use flux_core::{CoreError, ToolCtx};
use grep_matcher::Matcher;
use grep_regex::RegexMatcher;
use grep_searcher::{
    BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkContextKind, SinkMatch,
};
use ignore::{WalkBuilder, WalkState};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Maximum number of rendered rows (matches + context lines) across all
/// patterns before truncating.
const MAX_GREP_RESULTS: usize = 500;
/// Per-pattern soft cap in a MULTI-pattern call — one hot pattern must not
/// starve its siblings: once it fills its budget it is marked truncated and
/// the remaining patterns keep theirs. A single-pattern call keeps the full
/// 500 (nothing to starve).
const MAX_RESULTS_PER_PATTERN: usize = 200;
/// Maximum number of patterns per batch call (grep and glob alike).
const MAX_PATTERNS: usize = 8;
/// Maximum characters of a matched line before truncating. Match COUNT is
/// capped, but a single "line" can be a megabyte — minified bundles ship
/// whole programs on one line, and one match used to pull all of it into
/// the model context (observed: a 276k-token request from one grep).
const MAX_GREP_LINE_CHARS: usize = 500;
/// Maximum context lines rendered before/after each match (`context` is one
/// value standing in for rg's -A/-B/-C).
const MAX_CONTEXT: u8 = 5;
/// A file larger than this is skipped by grep instead of being read whole
/// into memory — a multi-GiB log or vendored bundle must not OOM the search.
const MAX_GREP_FILE_SIZE: u64 = 16 * 1024 * 1024;
/// Maximum number of glob matches before truncating.
const MAX_GLOB_RESULTS: usize = 500;

/// Render a matched line truncated to ~`budget` chars, keeping the MATCH
/// visible: up to 200 chars of leading context, the match, and trailing
/// content up to the budget. Omitted prefixes/suffixes are marked so the
/// model knows the line continues.
///
/// Byte-window implementation — no whole-line char-vector materialization
/// (the old version collected `Vec<char>` at 4 bytes/char for EVERY match
/// on a line; a minified bundle paid megabytes of allocation per match).
/// The window is computed in byte space at char boundaries, producing
/// exactly the char-space window the budget defines.
fn truncate_around_match(line: &str, match_byte: usize, budget: usize) -> String {
    const LEAD: usize = 200;
    // Fast path: byte length bounds char count, so a short line is whole.
    if line.len() <= budget {
        return line.to_string();
    }
    // Window start: up to LEAD chars before the match (0 if fewer exist).
    let start = line[..match_byte]
        .char_indices()
        .rev()
        .nth(LEAD - 1)
        .map_or(0, |(i, _)| i);
    // Window end: budget-LEAD chars forward from the match start (the whole
    // tail if fewer remain) — mirrors the char-space window end.
    let end = line[match_byte..]
        .char_indices()
        .nth(budget - LEAD)
        .map_or(line.len(), |(i, _)| match_byte + i);
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.push_str(&line[start..end]);
    if end < line.len() {
        out.push_str(&format!(
            "…[+{} chars truncated]",
            line[end..].chars().count()
        ));
    }
    out
}

/// Render a context line (no match inside) truncated to `budget` chars from
/// the head — the tail is marked so the model knows the line continues.
fn truncate_tail(line: &str, budget: usize) -> String {
    if line.chars().count() <= budget {
        return line.to_string();
    }
    let mut out: String = line.chars().take(budget).collect();
    out.push('…');
    out
}

/// Strip the line terminator the searcher may include in `SinkMatch::line`.
fn strip_eol(bytes: &[u8]) -> &[u8] {
    let mut b = bytes;
    if b.last() == Some(&b'\n') {
        b = &b[..b.len() - 1];
    }
    if b.last() == Some(&b'\r') {
        b = &b[..b.len() - 1];
    }
    b
}

// ---------------------------------------------------------------------------
// GrepTool
// ---------------------------------------------------------------------------

#[derive(Default, flux_macros::Tool, ::serde::Deserialize)]
#[tool(
    name = "grep",
    description = "Search for regex patterns in files under the given directory. Batch: 1..=8 patterns in one call. rg-style semantics — .gitignore/.ignore are respected, so ignored trees (target/, node_modules/) are skipped (an explicitly given FILE is still searched; for ignored directories use bash). Single-pattern output is flat 'path:line: text' (500-row cap); multi-pattern output is grouped per pattern ('== pattern \"x\" (N matches) ==') with per-pattern 200-row caps under the 500-row global cap; context lines (when context>0) render as 'path-line- text' and count toward the caps. An invalid pattern reports an error section without failing the rest (all invalid → error)."
)]
pub struct GrepTool {
    /// Regex patterns to search for (1..=8; duplicates are deduplicated).
    patterns: Vec<String>,
    /// Directory or file to search in. Defaults to the chat workdir. An explicit FILE is always searched; a directory respects .gitignore/.ignore.
    path: Option<String>,
    /// Optional gitignore-style glob filter on which files are searched (e.g. "*.rs", "src/**").
    include: Option<String>,
    /// Optional context lines (0-5) rendered before and after each match; counts toward the result caps. Defaults to 0.
    context: Option<u8>,
}

impl GrepTool {
    pub fn new() -> Self {
        Self::default()
    }

    async fn execute(&self, ctx: ToolCtx) -> Result<String, CoreError> {
        if self.patterns.is_empty() {
            return Err(CoreError::Tool(
                "patterns must not be empty — for a single pattern pass one element".into(),
            ));
        }
        if self.patterns.len() > MAX_PATTERNS {
            return Err(CoreError::Tool(format!(
                "too many patterns ({}) — pass at most {MAX_PATTERNS} per call",
                self.patterns.len()
            )));
        }
        // Resolve against the chat boundary — escapes are tool errors the
        // model sees and self-corrects.
        let dir = ctx.resolve(self.path.as_deref().unwrap_or("."))?;

        // Dedupe preserving order — a duplicated pattern would double-count
        // its quota and render its results twice.
        let mut patterns: Vec<String> = Vec::new();
        for p in &self.patterns {
            if !patterns.contains(p) {
                patterns.push(p.clone());
            }
        }
        let include = self.include.clone();
        let context = self.context.unwrap_or(0).min(MAX_CONTEXT) as usize;

        tokio::task::spawn_blocking(move || run_grep(&dir, &patterns, include.as_deref(), context))
            .await
            .map_err(|e| CoreError::Tool(format!("task panicked: {e}")))?
    }
}

/// One rendered output row, kept structured until the walk finishes so the
/// per-pattern results can be ordered deterministically (the parallel walk
/// delivers files in no fixed order): by path, then document order within
/// the file (before-context, match, after-context).
#[derive(Clone)]
struct Row {
    path: String,
    line_no: u64,
    /// 0 = before-context, 1 = match, 2 = after-context.
    rank: u8,
    /// Fully rendered row: `path:line: text` (match) or `path-line- text`.
    text: String,
}

#[derive(Clone)]
struct PatternState {
    rows: Vec<Row>,
    /// This pattern filled its per-pattern budget.
    truncated: bool,
}

#[derive(Clone)]
struct SearchState {
    /// Parallel to the deduped input patterns.
    patterns: Vec<PatternState>,
    /// Rendered rows across all patterns (the global cap counts rows, which
    /// with context>0 includes context lines).
    total: usize,
    /// Per-pattern budget: the full 500 for a single-pattern call, 200 when
    /// several patterns share the call.
    per_cap: usize,
    /// Oversized (>16MB) files skipped without reading.
    skipped: u64,
    /// Global cap hit.
    truncated: bool,
}

/// One sink per (pattern, file) search: renders matched and context lines
/// into the shared state under the per-pattern and global quotas.
struct GrepSink {
    state: Arc<Mutex<SearchState>>,
    matcher: Arc<RegexMatcher>,
    /// Index of this sink's pattern in the shared state.
    idx: usize,
    /// Display path relative to the search root.
    rel: String,
}

impl Sink for GrepSink {
    // io::Error implements SinkError out of the box; the sink never raises
    // errors itself — quota aborts travel as Ok(false), not errors.
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        let mut st = self.state.lock().unwrap();
        // Quota full — stop searching this file. The walk callback sees the
        // same state and quits the whole walk once the global cap is hit.
        if st.total >= MAX_GREP_RESULTS || st.patterns[self.idx].rows.len() >= st.per_cap {
            st.patterns[self.idx].truncated = true;
            return Ok(false);
        }
        let line_no = mat.line_number().unwrap_or(0);
        // Lossy-UTF8 first so match offsets and the rendered window live in
        // the same byte space (non-UTF8 lines render lossily instead of the
        // whole file being silently skipped like the old read_to_string).
        let line = String::from_utf8_lossy(strip_eol(mat.bytes())).into_owned();
        // Locate the match inside the line so the 500-char window centers on
        // it (a minified single-line bundle can hold the match at the tail).
        let offset = self
            .matcher
            .find(line.as_bytes())
            .ok()
            .flatten()
            .map(|m| m.start())
            .unwrap_or(0);
        let rendered = truncate_around_match(&line, offset, MAX_GREP_LINE_CHARS);
        st.patterns[self.idx].rows.push(Row {
            path: self.rel.clone(),
            line_no,
            rank: 1,
            text: format!("{}:{}: {}", self.rel, line_no, rendered),
        });
        st.total += 1;
        if st.total >= MAX_GREP_RESULTS {
            st.truncated = true;
        }
        if st.patterns[self.idx].rows.len() >= st.per_cap {
            st.patterns[self.idx].truncated = true;
        }
        Ok(true)
    }

    fn context(&mut self, _searcher: &Searcher, c: &SinkContext<'_>) -> Result<bool, Self::Error> {
        // Same quotas as matched: context rows count toward the caps so a
        // context-heavy call cannot balloon past the bound.
        let mut st = self.state.lock().unwrap();
        if st.total >= MAX_GREP_RESULTS || st.patterns[self.idx].rows.len() >= st.per_cap {
            return Ok(false);
        }
        let line = String::from_utf8_lossy(strip_eol(c.bytes())).into_owned();
        let text = truncate_tail(&line, MAX_GREP_LINE_CHARS);
        let line_no = c.line_number().unwrap_or(0);
        let rank = match c.kind() {
            SinkContextKind::Before => 0,
            SinkContextKind::After => 2,
            _ => 1,
        };
        st.patterns[self.idx].rows.push(Row {
            path: self.rel.clone(),
            line_no,
            rank,
            text: format!("{}-{}- {}", self.rel, line_no, text),
        });
        st.total += 1;
        Ok(true)
    }
}

/// The synchronous core: compile patterns, walk with the `ignore` crate's
/// parallel walker (gitignore semantics + no hidden-file filtering), search
/// each file once per valid pattern, render grouped by pattern.
fn run_grep(
    dir: &Path,
    patterns: &[String],
    include: Option<&str>,
    context: usize,
) -> Result<String, CoreError> {
    // Compile each pattern up front; a failure is recorded per pattern and
    // rendered as that pattern's error section (error isolation) — only an
    // all-invalid batch fails the call.
    let mut matchers: Vec<Option<Arc<RegexMatcher>>> = Vec::with_capacity(patterns.len());
    let mut errors: Vec<Option<String>> = Vec::with_capacity(patterns.len());
    for p in patterns {
        match RegexMatcher::new(p) {
            Ok(m) => {
                matchers.push(Some(Arc::new(m)));
                errors.push(None);
            }
            Err(e) => {
                matchers.push(None);
                errors.push(Some(format!("invalid regex: {e}")));
            }
        }
    }
    if matchers.iter().all(Option::is_none) {
        let msgs: Vec<String> = patterns
            .iter()
            .zip(&errors)
            .map(|(p, e)| format!("\"{p}\": {}", e.clone().unwrap_or_default()))
            .collect();
        return Err(CoreError::Tool(format!(
            "all {} pattern(s) failed to compile — {}",
            patterns.len(),
            msgs.join("; ")
        )));
    }

    let per_cap = if patterns.len() == 1 {
        MAX_GREP_RESULTS
    } else {
        MAX_RESULTS_PER_PATTERN
    };
    let state = Arc::new(Mutex::new(SearchState {
        patterns: patterns
            .iter()
            .map(|_| PatternState {
                rows: Vec::new(),
                truncated: false,
            })
            .collect(),
        total: 0,
        per_cap,
        skipped: 0,
        truncated: false,
    }));

    let mut builder = WalkBuilder::new(dir);
    builder
        // Current behavior preserved: dotfiles are searched (.env, .github/…).
        .hidden(false)
        // rg semantics: .gitignore/.ignore inside the tree plus .git/info/
        // exclude; global git ignore config stays out (predictability).
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        .require_git(true)
        // Symlinked directories are not followed — the walk cannot cycle or
        // escape (narrower than the old hand walk, which resolved symlinks
        // and re-checked containment).
        .follow_links(false);
    if let Some(inc) = include {
        let ov = ignore::overrides::OverrideBuilder::new(dir)
            .add(inc)
            .map_err(|e| CoreError::Tool(format!("invalid include glob: {e}")))?
            .build()
            .map_err(|e| CoreError::Tool(format!("invalid include glob: {e}")))?;
        builder.overrides(ov);
    }

    let matchers = Arc::new(matchers);
    let root = dir.to_path_buf();
    builder.build_parallel().run(|| {
        let state = Arc::clone(&state);
        let matchers = Arc::clone(&matchers);
        let root = root.clone();
        let mut searcher = {
            let mut b = SearcherBuilder::new();
            b.line_number(true)
                .binary_detection(BinaryDetection::quit(b'\x00'));
            if context > 0 {
                b.before_context(context).after_context(context);
            }
            b.build()
        };
        Box::new(move |result: Result<ignore::DirEntry, ignore::Error>| {
            let entry = match result {
                Ok(e) => e,
                Err(_) => return WalkState::Continue, // unreadable entry — skip
            };
            // Files only: the walker also yields the root and directories.
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return WalkState::Continue;
            }
            let path = entry.into_path();
            // Skip oversized files without reading them — a multi-GiB log or
            // vendored bundle must not be pulled whole into memory. The
            // file's own size, not the search root's, is the bound.
            if std::fs::metadata(&path)
                .map(|m| m.len() > MAX_GREP_FILE_SIZE)
                .unwrap_or(false)
            {
                state.lock().unwrap().skipped += 1;
                return WalkState::Continue;
            }
            {
                let st = state.lock().unwrap();
                if st.total >= MAX_GREP_RESULTS {
                    return WalkState::Quit;
                }
            }
            // Display paths relative to the search root; a single-file search
            // has no relative prefix (strip yields empty), so show the file
            // itself.
            let rel = match path.strip_prefix(&root) {
                Ok(r) if !r.as_os_str().is_empty() => r.to_string_lossy().into_owned(),
                _ => path.to_string_lossy().into_owned(),
            };
            for (i, m) in matchers.iter().enumerate() {
                let Some(matcher) = m else { continue };
                {
                    let st = state.lock().unwrap();
                    if st.total >= MAX_GREP_RESULTS || st.patterns[i].rows.len() >= st.per_cap {
                        continue;
                    }
                }
                let sink = GrepSink {
                    state: Arc::clone(&state),
                    matcher: Arc::clone(matcher),
                    idx: i,
                    rel: rel.clone(),
                };
                // Search errors are I/O-shaped (read/decode failures) — skip
                // the file like the old read_to_string Err path did. Quota
                // aborts travel as Ok(false), not errors.
                let _ = searcher.search_path(matcher.as_ref(), &path, sink);
            }
            if state.lock().unwrap().total >= MAX_GREP_RESULTS {
                WalkState::Quit
            } else {
                WalkState::Continue
            }
        })
    });

    let state = state.lock().unwrap().clone();
    Ok(render_grep(state, patterns, &errors))
}

fn render_grep(state: SearchState, patterns: &[String], errors: &[Option<String>]) -> String {
    let single = patterns.len() == 1;
    let mut parts: Vec<String> = Vec::new();

    for (i, ps) in state.patterns.iter().enumerate() {
        let mut rows = ps.rows.clone();
        rows.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then(a.line_no.cmp(&b.line_no))
                .then(a.rank.cmp(&b.rank))
        });
        if let Some(err) = &errors[i] {
            parts.push(format!("== pattern \"{}\" — {err} ==", patterns[i]));
            continue;
        }
        let mut body = String::new();
        if !single {
            if ps.truncated {
                body.push_str(&format!(
                    "== pattern \"{}\" (truncated at {} rows — narrow the pattern) ==",
                    patterns[i], state.per_cap
                ));
            } else {
                let n = rows.iter().filter(|r| r.rank == 1).count();
                body.push_str(&format!(
                    "== pattern \"{}\" ({} match{}) ==",
                    patterns[i],
                    n,
                    if n == 1 { "" } else { "es" }
                ));
            }
        }
        for row in &rows {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&row.text);
        }
        // Single-pattern truncation keeps the legacy trailing marker shape.
        if single && ps.truncated {
            body.push_str(&format!(
                "\n\n--- (grep results truncated at {} rows; further rows are not \
retrievable — narrow the pattern) ---",
                state.per_cap
            ));
        }
        parts.push(body);
    }
    if !single && state.truncated {
        parts.push(format!(
            "--- (grep results truncated at {MAX_GREP_RESULTS} rows; further rows are not \
retrievable — narrow the pattern) ---"
        ));
    }
    if state.skipped > 0 {
        parts.push(format!(
            "--- (skipped {} file(s) larger than {MAX_GREP_FILE_SIZE} bytes) ---",
            state.skipped
        ));
    }
    if parts.iter().all(String::is_empty) {
        return "No matches found.".into();
    }
    parts.join("\n\n")
}

// ---------------------------------------------------------------------------
// GlobTool
// ---------------------------------------------------------------------------

#[derive(Default, flux_macros::Tool, ::serde::Deserialize)]
#[tool(
    name = "glob",
    description = "Find files matching glob patterns (e.g. '**/*.rs', 'src/**'). Batch: 1..=8 patterns in one call — first matching pattern claims the file (results dedup across patterns); capped at 500 results. Scoped to the transient shell cwd (like bash); the model moves it with `state_set current_dir`."
)]
pub struct GlobTool {
    /// Glob patterns to match files against (relative to the chat's current working directory).
    patterns: Vec<String>,
}

impl GlobTool {
    pub fn new() -> Self {
        Self::default()
    }

    async fn execute(&self, ctx: ToolCtx) -> Result<String, CoreError> {
        // glob scopes to the transient shell cwd (like bash) — the model
        // moves it with `state_set current_dir`. The value is authoritative
        // (filled by the adapter from state); a missing one fails closed.
        if ctx.current_dir.as_os_str().is_empty() {
            return Err(CoreError::InvalidArguments(
                "current_dir is required".into(),
            ));
        }
        if self.patterns.is_empty() {
            return Err(CoreError::Tool(
                "patterns must not be empty — for a single pattern pass one element".into(),
            ));
        }
        if self.patterns.len() > MAX_PATTERNS {
            return Err(CoreError::Tool(format!(
                "too many patterns ({}) — pass at most {MAX_PATTERNS} per call",
                self.patterns.len()
            )));
        }
        let wd = ctx.current_dir.clone();

        // Dedupe preserving order (same rationale as grep's).
        let mut patterns: Vec<String> = Vec::new();
        for p in &self.patterns {
            if !patterns.contains(p) {
                patterns.push(p.clone());
            }
        }

        tokio::task::spawn_blocking(move || {
            // Compile all patterns up front — glob's output is a flat list
            // with no per-pattern error slot, so any invalid pattern fails
            // the call (grep's grouped output carries per-pattern error
            // sections and isolates instead — deliberate asymmetry).
            let mut compiled: Vec<glob::Pattern> = Vec::with_capacity(patterns.len());
            for p in &patterns {
                let pat = glob::Pattern::new(p)
                    .map_err(|e| CoreError::Tool(format!("invalid glob: {e}")))?;
                compiled.push(pat);
            }
            let opts = glob::MatchOptions {
                case_sensitive: true,
                require_literal_separator: false,
                require_literal_leading_dot: false,
            };

            let mut results: Vec<String> = Vec::new();
            crate::walk_dir(&wd, &mut |file_path| {
                if results.len() >= MAX_GLOB_RESULTS {
                    return Ok(false); // stop walking
                }
                let rel = file_path.strip_prefix(&wd).unwrap_or(file_path);
                // First matching pattern claims the file — duplicates across
                // patterns collapse into one row.
                for pat in &compiled {
                    if pat.matches_path_with(rel, opts) {
                        results.push(rel.display().to_string());
                        break;
                    }
                }
                Ok(true)
            })
            .map_err(|e| CoreError::Tool(format!("glob error: {e}")))?;

            let truncated = results.len() >= MAX_GLOB_RESULTS;
            if results.is_empty() {
                Ok("No files found.".into())
            } else if truncated {
                Ok(format!(
                    "{}\n\n--- (truncated at {MAX_GLOB_RESULTS} results) ---",
                    results.join("\n")
                ))
            } else {
                Ok(results.join("\n"))
            }
        })
        .await
        .map_err(|e| CoreError::Tool(format!("task panicked: {e}")))?
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;
    use crate::test_util::{boundary_ctx, processed, setup};
    use flux_core::Tool;
    use serde_json::json;
    use std::fs;

    #[tokio::test]
    async fn grep_finds_matching_lines() {
        let dir = setup();
        fs::write(
            dir.path().join("a.txt"),
            "hello world\nfoo bar\nhello again",
        )
        .unwrap();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![
                    ("patterns", json!(["hello"])),
                    ("path", json!(dir.path().join("a.txt").to_string_lossy())),
                ]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(result.contains("hello world"));
        assert!(result.contains("hello again"));
    }

    #[tokio::test]
    async fn grep_marks_truncated_results() {
        let dir = setup();
        // 501 matching lines in one file — one more than the 500-row cap.
        let content = (0..501)
            .map(|i| format!("hit {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(dir.path().join("a.txt"), content).unwrap();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![
                    ("patterns", json!(["hit"])),
                    ("path", json!(dir.path().join("a.txt").to_string_lossy())),
                ]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(
            result.contains("truncated at 500"),
            "global cap must be marked, got tail: {result:.400}"
        );
    }

    #[tokio::test]
    async fn grep_truncates_minified_single_line_matches() {
        // Window rendering keeps every row bounded — a minified bundle's
        // matched "line" must never inline whole (a single match used to
        // inline the entire line into the model context; observed: 276k-
        // token provider 400s).
        let dir = setup();
        let huge = format!("{}{}", "x".repeat(400_000), "needle trailing");
        fs::write(dir.path().join("bundle.js"), huge).unwrap();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![
                    ("patterns", json!(["needle"])),
                    (
                        "path",
                        json!(dir.path().join("bundle.js").to_string_lossy()),
                    ),
                ]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(
            result.chars().count() < 2000,
            "output must be bounded, got {} chars",
            result.chars().count()
        );
        assert!(
            result.contains("needle trailing"),
            "match must stay visible at the line tail"
        );
        assert!(result.contains('…'), "line-head omission must be marked");
        assert!(
            result.contains("bundle.js:1:"),
            "match location metadata must be present"
        );
    }

    #[tokio::test]
    async fn grep_wide_lines_are_windowed_around_the_match() {
        // Per-line windowing: a wide matched line is truncated AROUND the
        // match (not clipped to the line head), so the match context stays
        // visible. Total size moves to the central overflow buffer.
        let dir = setup();
        let line = "payload ".repeat(60); // ~480 chars per line
        let content = (0..20)
            .map(|i| format!("{line}hit{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(dir.path().join("a.txt"), content).unwrap();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![
                    ("patterns", json!(["hit"])),
                    ("path", json!(dir.path().join("a.txt").to_string_lossy())),
                ]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        // Each rendered line is bounded (~500-char window + path prefix).
        for line_out in result.lines() {
            assert!(
                line_out.chars().count() < 600,
                "per-line window must bound the render, got {}: {line_out:.80}",
                line_out.chars().count()
            );
            assert!(
                line_out.contains("hit"),
                "match must stay visible: {line_out:.80}"
            );
        }
    }

    #[test]
    fn truncate_around_match_keeps_match_visible() {
        let line = format!("{}needle trailing", "x".repeat(400_000));
        // The needle's final `n` sits at byte 400_000.
        let out = truncate_around_match(&line, 400_000, 500);
        assert!(
            out.contains("needle trailing"),
            "match must stay visible: {out:.120}"
        );
        assert!(
            out.starts_with('…'),
            "leading omission must be marked: {out:.120}"
        );
        // No tail marker — the needle's trailing content already ends the line (the window reached it naturally).
    }

    #[tokio::test]
    async fn grep_no_match() {
        let dir = setup();
        fs::write(dir.path().join("a.txt"), "foo bar").unwrap();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![
                    ("patterns", json!(["zzzzz"])),
                    ("path", json!(dir.path().join("a.txt").to_string_lossy())),
                ]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert_eq!(result, "No matches found.");
    }

    #[tokio::test]
    async fn grep_outside_boundary_is_denied() {
        // The boundary rides ToolCtx: an absolute path outside the chat
        // workdir is a resolve error the model sees.
        let dir = setup();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![("patterns", json!(["x"])), ("path", json!("/etc"))]),
                boundary_ctx(dir.path()),
            )
            .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("path escape"),
            "outside-boundary grep must be denied, got: {err}"
        );
    }

    #[tokio::test]
    async fn grep_multi_pattern_groups_by_pattern() {
        let dir = setup();
        fs::write(dir.path().join("a.txt"), "hello world\nfoo bar").unwrap();
        fs::write(dir.path().join("b.txt"), "hello again\nfoo baz").unwrap();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![("patterns", json!(["hello", "foo"]))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        // Grouped output: one section per pattern, rows sorted by path.
        assert!(
            result.contains("== pattern \"hello\" (2 matches) =="),
            "got: {result}"
        );
        assert!(result.contains("a.txt:1: hello world"));
        assert!(result.contains("b.txt:1: hello again"));
        assert!(result.contains("== pattern \"foo\" (2 matches) =="));
        assert!(result.contains("a.txt:2: foo bar"));
        assert!(result.contains("b.txt:2: foo baz"));
        // hello's section comes before foo's.
        let hello = result.find("== pattern \"hello\"").unwrap();
        let foo = result.find("== pattern \"foo\"").unwrap();
        assert!(hello < foo);
    }

    #[tokio::test]
    async fn grep_per_pattern_cap_does_not_starve_siblings() {
        let dir = setup();
        // 250 hits for the hot pattern (> the 200 per-pattern batch cap),
        // one for the rare one.
        let content = (0..250)
            .map(|i| format!("hit {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(dir.path().join("a.txt"), content).unwrap();
        fs::write(dir.path().join("b.txt"), "rare find").unwrap();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![("patterns", json!(["hit", "rare"]))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(
            result.contains("== pattern \"hit\" (truncated at 200 rows — narrow the pattern) =="),
            "hot pattern must be capped at 200, got: {result:.200}"
        );
        // The sibling keeps its budget and still reports.
        assert!(result.contains("== pattern \"rare\" (1 match) =="));
        assert!(result.contains("b.txt:1: rare find"));
    }

    #[tokio::test]
    async fn grep_invalid_pattern_is_isolated() {
        let dir = setup();
        fs::write(dir.path().join("a.txt"), "hello world").unwrap();
        let tool = GrepTool::new();
        // One invalid pattern: an error section, the valid one still searched.
        let result = tool
            .call(
                processed(vec![("patterns", json!(["(", "hello"]))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(
            result.contains("== pattern \"(\" — invalid regex"),
            "invalid pattern must render an error section, got: {result}"
        );
        assert!(result.contains("hello world"));
        // All invalid: the call fails with a consolidated error.
        let result = tool
            .call(
                processed(vec![("patterns", json!(["(", "["]))]),
                boundary_ctx(dir.path()),
            )
            .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("failed to compile"),
            "all-invalid batch must fail, got: {err}"
        );
    }

    #[tokio::test]
    async fn grep_respects_gitignore_but_explicit_files_are_searched() {
        let dir = setup();
        // A git repo whose .gitignore hides ignored/ — rg semantics: the
        // directory arg respects the rules, an explicit FILE arg does not.
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success()
        );
        fs::write(dir.path().join(".gitignore"), "ignored/\n").unwrap();
        fs::create_dir(dir.path().join("ignored")).unwrap();
        fs::write(dir.path().join("ignored/x.txt"), "needle secret").unwrap();
        fs::write(dir.path().join("kept.txt"), "needle here").unwrap();

        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![("patterns", json!(["needle"]))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(result.contains("kept.txt:1: needle here"), "got: {result}");
        assert!(
            !result.contains("ignored"),
            "gitignored tree must be skipped, got: {result}"
        );

        // An explicitly given FILE inside the ignored tree is searched.
        let result = tool
            .call(
                processed(vec![
                    ("patterns", json!(["needle"])),
                    (
                        "path",
                        json!(dir.path().join("ignored/x.txt").to_string_lossy()),
                    ),
                ]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(
            result.contains("needle secret"),
            "explicit file must bypass ignore rules, got: {result}"
        );
    }

    #[tokio::test]
    async fn grep_searches_hidden_files() {
        // hidden(false) preserves the old walk's behavior: dotfiles are
        // searched (.env, .github/… are common targets).
        let dir = setup();
        fs::write(dir.path().join(".env"), "needle in dotfile").unwrap();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![("patterns", json!(["needle"]))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(
            result.contains(".env:1: needle in dotfile"),
            "hidden files must be searched, got: {result}"
        );
    }

    #[tokio::test]
    async fn grep_does_not_follow_symlinked_dirs() {
        // follow_links(false): a symlinked dir inside the tree is not
        // entered (no cycles, no escape; narrower than the old walk).
        let dir = setup();
        fs::create_dir(dir.path().join("real")).unwrap();
        fs::write(dir.path().join("real/inside.txt"), "needle deep").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![("patterns", json!(["needle"]))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(
            !result.contains("link/"),
            "symlinked dirs must not be followed, got: {result}"
        );
        // The target itself stays reachable through its real path.
        assert!(
            result.contains("real/inside.txt:1: needle deep"),
            "got: {result}"
        );
    }

    #[tokio::test]
    async fn grep_skips_binary_files() {
        // BinaryDetection::quit(0): the search stops at the first NUL byte,
        // so content after it is never matched (was: whole-file silent skip
        // when read_to_string failed).
        let dir = setup();
        fs::write(dir.path().join("blob.bin"), b"abc\x00def\nneedle later\n").unwrap();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![("patterns", json!(["needle"]))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(
            !result.contains("needle"),
            "binary content past the NUL must not match, got: {result}"
        );
    }

    #[tokio::test]
    async fn grep_include_filters_files() {
        let dir = setup();
        fs::write(dir.path().join("a.rs"), "needle here").unwrap();
        fs::write(dir.path().join("b.txt"), "needle there").unwrap();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![
                    ("patterns", json!(["needle"])),
                    ("include", json!("*.rs")),
                ]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(result.contains("a.rs:1: needle here"), "got: {result}");
        assert!(
            !result.contains("b.txt"),
            "include must filter, got: {result}"
        );
    }

    #[tokio::test]
    async fn grep_context_lines_render_with_dash_separator() {
        let dir = setup();
        fs::write(dir.path().join("f.txt"), "one\ntwo\nmatch me\nfour\nfive").unwrap();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![
                    ("patterns", json!(["match"])),
                    ("path", json!(dir.path().join("f.txt").to_string_lossy())),
                    ("context", json!(1)),
                ]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(
            result.contains("f.txt-2- two"),
            "before-context, got: {result}"
        );
        assert!(result.contains("f.txt:3: match me"), "the match itself");
        assert!(result.contains("f.txt-4- four"), "after-context");
    }

    #[tokio::test]
    async fn grep_skips_oversized_files() {
        let dir = setup();
        fs::write(dir.path().join("small.txt"), "needle visible").unwrap();
        // One byte over the 16MB bound.
        let big = vec![b'x'; MAX_GREP_FILE_SIZE as usize + 1];
        fs::write(dir.path().join("big.log"), big).unwrap();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![("patterns", json!(["needle"]))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(
            result.contains("small.txt:1: needle visible"),
            "got: {result:.200}"
        );
        assert!(
            result.contains("skipped 1 file(s) larger than"),
            "oversized skip must be noted, got: {result:.400}"
        );
    }

    #[tokio::test]
    async fn grep_rejects_too_many_patterns() {
        let dir = setup();
        let patterns: Vec<String> = (0..MAX_PATTERNS + 1).map(|i| format!("p{i}")).collect();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![("patterns", json!(patterns))]),
                boundary_ctx(dir.path()),
            )
            .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("at most 8"),
            "pattern cap must be enforced, got: {err}"
        );
    }

    #[tokio::test]
    async fn grep_empty_patterns_rejected() {
        let dir = setup();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![("patterns", json!([]))]),
                boundary_ctx(dir.path()),
            )
            .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("must not be empty"),
            "empty patterns must be rejected, got: {err}"
        );
    }

    #[tokio::test]
    async fn glob_finds_files_relative_to_current_dir() {
        let dir = setup();
        fs::write(dir.path().join("a.rs"), "").unwrap();
        fs::write(dir.path().join("b.rs"), "").unwrap();
        fs::write(dir.path().join("c.txt"), "").unwrap();
        let tool = GlobTool::new();
        let result = tool
            .call(
                processed(vec![("patterns", json!(["*.rs"]))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(result.contains("a.rs"));
        assert!(result.contains("b.rs"));
        assert!(!result.contains("c.txt"));
    }

    #[tokio::test]
    async fn glob_multi_pattern_unions_and_dedups() {
        let dir = setup();
        fs::write(dir.path().join("a.rs"), "").unwrap();
        fs::write(dir.path().join("b.md"), "").unwrap();
        fs::write(dir.path().join("c.txt"), "").unwrap();
        let tool = GlobTool::new();
        let result = tool
            .call(
                processed(vec![("patterns", json!(["*.rs", "*.md"]))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(result.contains("a.rs"));
        assert!(result.contains("b.md"));
        assert!(!result.contains("c.txt"));

        // Overlapping patterns: the first match claims the file — a.rs is
        // claimed by "*.rs" and must not be listed again by "a.rs".
        let result = tool
            .call(
                processed(vec![("patterns", json!(["*.rs", "a.rs"]))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert_eq!(
            result.matches("a.rs").count(),
            1,
            "overlapping patterns must dedup, got: {result}"
        );
    }

    #[tokio::test]
    async fn glob_invalid_pattern_fails_fast() {
        let dir = setup();
        fs::write(dir.path().join("a.rs"), "").unwrap();
        let tool = GlobTool::new();
        let result = tool
            .call(
                processed(vec![("patterns", json!(["[", "*.rs"]))]),
                boundary_ctx(dir.path()),
            )
            .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("invalid glob"),
            "any invalid pattern must fail the call, got: {err}"
        );
    }

    #[tokio::test]
    async fn glob_requires_current_dir() {
        // current_dir rides ToolCtx now; a ctx without one fails closed
        // instead of silently falling back to the server's cwd.
        let tool = GlobTool::new();
        let result = tool
            .call(
                processed(vec![("patterns", json!(["*.rs"]))]),
                flux_core::ToolCtx::new(),
            )
            .await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, CoreError::InvalidArguments(_)),
            "missing current_dir should be InvalidArguments, got: {err}"
        );
    }

    #[tokio::test]
    async fn glob_scopes_to_subdirectory_of_current_dir() {
        // The pattern is relative to the ctx's current_dir; nested matches
        // come back as paths relative to it.
        let dir = setup();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "").unwrap();
        let tool = GlobTool::new();
        let result = tool
            .call(
                processed(vec![("patterns", json!(["src/**/*.rs"]))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(result.contains("src/lib.rs"), "got: {result}");
    }
}
