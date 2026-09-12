use crate::walk_dir;
use flux_core::{CoreError, ToolCtx};

/// Maximum number of grep matches before truncating.
const MAX_GREP_RESULTS: usize = 500;
/// Maximum characters of a matched line before truncating. Match COUNT is
/// capped, but a single "line" can be a megabyte — minified bundles ship
/// whole programs on one line, and one match used to pull all of it into
/// the model context (observed: a 276k-token request from one grep).
const MAX_GREP_LINE_CHARS: usize = 500;
/// Render a matched line truncated to ~`budget` chars, keeping the MATCH
/// visible: up to 200 chars of leading context, the match, and trailing
/// content up to the budget. Omitted prefixes/suffixes are marked so the
/// model knows the line continues.
fn truncate_around_match(line: &str, match_byte: usize, budget: usize) -> String {
    const LEAD: usize = 200;
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= budget {
        return line.to_string();
    }
    // Byte offset → char index.
    let mut match_char = 0usize;
    for (bi, _) in line.char_indices() {
        if bi >= match_byte {
            break;
        }
        match_char += 1;
    }
    let start = match_char.saturating_sub(LEAD);
    let end = (match_char + (budget - LEAD)).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push_str(&format!("…[+{} chars truncated]", chars.len() - end));
    }
    out
}

/// Maximum number of glob matches before truncating.
const MAX_GLOB_RESULTS: usize = 500;
/// A file larger than this is skipped by grep instead of being read whole
/// into memory — a multi-GiB log or vendored bundle must not OOM the search.
const MAX_GREP_FILE_SIZE: u64 = 16 * 1024 * 1024;

// ---------------------------------------------------------------------------
// GrepTool
// ---------------------------------------------------------------------------

#[derive(Default, flux_macros::Tool, ::serde::Deserialize)]
#[tool(
    name = "grep",
    description = "Search for a regex pattern in files under the given directory."
)]
pub struct GrepTool {
    /// Regex pattern to search for.
    pattern: String,
    /// Directory or file to search in. Defaults to the chat workdir.
    path: Option<String>,
}

impl GrepTool {
    pub fn new() -> Self {
        Self::default()
    }

    async fn execute(&self, ctx: ToolCtx) -> Result<String, CoreError> {
        // Resolve against the chat boundary — escapes are tool errors the
        // model sees and self-corrects.
        let dir = ctx.resolve(self.path.as_deref().unwrap_or("."))?;

        let pattern_owned = self.pattern.clone();
        tokio::task::spawn_blocking(move || {
            let re = regex::Regex::new(&pattern_owned)
                .map_err(|e| CoreError::Tool(format!("invalid regex: {e}")))?;

            let mut results = Vec::new();
            let mut truncated = false;
            let mut skipped = 0u64;
            walk_dir(&dir, &mut |file_path| {
                // Skip oversized files without reading them — a multi-GiB log
                // or vendored bundle would otherwise be pulled whole into
                // memory (read_to_string). The file's own size, not the
                // search root's, is the bound.
                if file_path
                    .metadata()
                    .map(|m| m.len() > MAX_GREP_FILE_SIZE)
                    .unwrap_or(false)
                {
                    skipped += 1;
                    return Ok(true);
                }
                if let Ok(content) = std::fs::read_to_string(file_path) {
                    for (i, line) in content.lines().enumerate() {
                        if re.is_match(line) {
                            if results.len() >= MAX_GREP_RESULTS {
                                truncated = true;
                                return Ok(false);
                            }
                            // Display paths relative to the search root; a
                            // single-file search has no relative prefix, so
                            // show the file itself.
                            let rel = match file_path.strip_prefix(&dir) {
                                Ok(rel) if !rel.as_os_str().is_empty() => rel.to_path_buf(),
                                _ => file_path.to_path_buf(),
                            };
                            // Per-line cap at a char boundary — a matched
                            // minified line is truncated AROUND THE MATCH
                            // (leading + trailing context), never inlined
                            // whole and never clipped to the line head
                            // (the match may live at the tail of a huge
                            // single-line bundle).
                            let match_byte = re.find(line).map(|m| m.start());
                            let text = truncate_around_match(
                                line,
                                match_byte.unwrap_or(0),
                                MAX_GREP_LINE_CHARS,
                            );
                            results.push(format!("{}:{}: {}", rel.display(), i + 1, text));
                        }
                    }
                }
                Ok(true)
            })
            .map_err(|e| CoreError::Tool(format!("grep error: {e}")))?;

            let skip_note = if skipped > 0 {
                format!(
                    "\n\n--- (skipped {skipped} file(s) larger than {MAX_GREP_FILE_SIZE} bytes) ---"
                )
            } else {
                String::new()
            };
            if results.is_empty() {
                if skipped > 0 {
                    Ok(format!("No matches found.{skip_note}"))
                } else {
                    Ok("No matches found.".into())
                }
            } else {
                let mut out = results.join("\n");
                if truncated {
                    out.push_str(&format!(
                        "\n\n--- (grep results truncated at {MAX_GREP_RESULTS} matches; \
further matches are not retrievable — narrow the pattern) ---"
                    ));
                }
                out.push_str(&skip_note);
                Ok(out)
            }
        })
        .await
        .map_err(|e| CoreError::Tool(format!("task panicked: {e}")))?
    }
}

// ---------------------------------------------------------------------------
// GlobTool
// ---------------------------------------------------------------------------

#[derive(Default, flux_macros::Tool, ::serde::Deserialize)]
#[tool(
    name = "glob",
    description = "Find files matching glob patterns (e.g. '**/*.rs', 'src/**'). Limited to 500 results."
)]
pub struct GlobTool {
    /// Glob pattern to match files against (relative to the chat's current
    /// working directory).
    pattern: String,
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
        let wd = ctx.current_dir.clone();
        let pattern_owned = self.pattern.clone();
        tokio::task::spawn_blocking(move || {
            let pattern = glob::Pattern::new(&pattern_owned)
                .map_err(|e| CoreError::Tool(format!("invalid glob: {e}")))?;

            let mut results = Vec::new();
            walk_dir(&wd, &mut |file_path| {
                let rel = file_path.strip_prefix(&wd).unwrap_or(file_path);
                if pattern.matches_path_with(
                    rel,
                    glob::MatchOptions {
                        case_sensitive: true,
                        require_literal_separator: false,
                        require_literal_leading_dot: false,
                    },
                ) {
                    if results.len() >= MAX_GLOB_RESULTS {
                        return Ok(false); // stop walking
                    }
                    results.push(rel.display().to_string());
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
                    ("pattern", json!("hello")),
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
        // 501 matching lines in one file — one more than MAX_GREP_RESULTS.
        let content = (0..501)
            .map(|i| format!("hit {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(dir.path().join("a.txt"), content).unwrap();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![
                    ("pattern", json!("hit")),
                    ("path", json!(dir.path().join("a.txt").to_string_lossy())),
                ]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(
            result.contains("truncated"),
            "truncated results must be marked, got: {result:.200}"
        );
    }

    #[tokio::test]
    async fn grep_truncates_minified_single_line_matches() {
        // Regression (context overflow): minified bundles ship whole programs
        // on ONE line — a single match used to inline the entire line into
        // the model context (observed: 276k-token provider 400s).
        let dir = setup();
        let huge = format!("{}{}", "x".repeat(400_000), "needle trailing");
        fs::write(dir.path().join("bundle.js"), huge).unwrap();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![
                    ("pattern", json!("needle")),
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
                    ("pattern", json!("hit")),
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
                    ("pattern", json!("zzzzz")),
                    ("path", json!(dir.path().join("a.txt").to_string_lossy())),
                ]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(result.contains("No matches found."));
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
                processed(vec![("pattern", json!("*.rs"))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(result.contains("a.rs"));
        assert!(result.contains("b.rs"));
        assert!(!result.contains("c.txt"));
    }

    #[tokio::test]
    async fn glob_requires_current_dir() {
        // current_dir rides ToolCtx now; a ctx without one fails closed
        // instead of silently falling back to the server's cwd.
        let tool = GlobTool::new();
        let result = tool
            .call(
                processed(vec![("pattern", json!("*.rs"))]),
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
                processed(vec![("pattern", json!("src/**/*.rs"))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(result.contains("src/lib.rs"), "got: {result}");
    }

    #[tokio::test]
    async fn grep_outside_boundary_is_denied() {
        // The boundary rides ToolCtx now: an absolute path outside the
        // chat workdir is a resolve error the model sees.
        let dir = setup();
        let tool = GrepTool::new();
        let result = tool
            .call(
                processed(vec![("pattern", json!("x")), ("path", json!("/etc"))]),
                boundary_ctx(dir.path()),
            )
            .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("path escape"),
            "outside-boundary grep must be denied, got: {err}"
        );
    }
}
