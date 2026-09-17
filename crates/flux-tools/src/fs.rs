use flux_core::{CoreError, ToolCtx};
use std::collections::HashMap;
use tokio::io::AsyncBufReadExt;

/// Default line limit when no explicit limit is provided.
const DEFAULT_READ_LIMIT: usize = 2000;
/// Hard cap on lines read to prevent context overflow.
const MAX_READ_LIMIT: usize = 5000;
/// Cap on items per multi-item fs tool call — bounds the fan-out of one
/// invocation (output amplification is further bounded by the central
/// overflow buffer).
const MAX_ITEMS: usize = 16;

// ---------------------------------------------------------------------------
// ReadFileTool
// ---------------------------------------------------------------------------

#[derive(Default, flux_macros::Tool, ::serde::Deserialize)]
#[tool(
    name = "read_file",
    description = "Read the contents of a file. Omit ranges to read the whole file (2000 lines default, 5000 max, 10MB truncation notice). Pass ranges to read several windows of this file in ONE call, e.g. ranges: [{\"offset\": 1, \"limit\": 100}, {\"offset\": 500, \"limit\": 100}] — each window keeps the offset/limit semantics (1-based offset, line-count limit) and its continuation footer; with 2+ windows each segment is headed by `=== path ===`."
)]
pub struct ReadFileTool {
    /// Path to the file to read.
    file_path: String,
    /// Optional windows to read instead of the whole file. Omitted or empty = whole file (at most 16 windows).
    ranges: Option<Vec<ReadRange>>,
}

#[derive(flux_macros::ToolItem, ::serde::Deserialize)]
pub struct ReadRange {
    /// Line number to start reading from (1-based).
    offset: Option<usize>,
    /// Maximum number of lines to read.
    limit: Option<usize>,
}

impl ReadFileTool {
    pub fn new() -> Self {
        Self::default()
    }

    async fn execute(&self, ctx: ToolCtx) -> Result<String, CoreError> {
        // Resolve against the chat boundary — escapes are tool errors the
        // model sees and self-corrects.
        let file_path = ctx.resolve(&self.file_path)?;
        let path = file_path.as_path();

        // No ranges (or an empty list) = the whole file — the bare output
        // read_file has always returned.
        let Some(ranges) = self.ranges.as_ref().filter(|r| !r.is_empty()) else {
            return read_file_segment(path, 1, None).await;
        };
        if ranges.len() > MAX_ITEMS {
            return Err(CoreError::Tool(format!(
                "too many windows ({}) — read at most {MAX_ITEMS} per call and split the rest across further calls",
                ranges.len()
            )));
        }
        // A single window keeps the bare output (no header) — the old
        // offset/limit shape verbatim.
        if ranges.len() == 1 {
            let r = &ranges[0];
            return read_file_segment(path, r.offset.unwrap_or(1).max(1), r.limit).await;
        }
        // 2+ windows: one `=== path ===` section per window, each keeping
        // read_file's footer semantics. A failure fails the call — same
        // path, so a stat/open error affects every window identically
        // (offsets past EOF are not errors: read_file_segment reports them
        // inline). Cancellation lands between windows; the partial result
        // stays useful, the kernel marks the round interrupted.
        let mut sections: Vec<String> = Vec::with_capacity(ranges.len());
        for r in ranges {
            if ctx.cancel.is_cancelled() {
                break;
            }
            let content = read_file_segment(path, r.offset.unwrap_or(1).max(1), r.limit).await?;
            sections.push(format!("=== {} ===\n{}", path.display(), content));
        }
        Ok(sections.join("\n\n"))
    }
}

/// Read one file segment — the single-file core of `read_file` (whole-file
/// reads and every `ranges` window go through it): 10MB guard, streamed line
/// window, continuation/truncation footers. `path` must already be resolved
/// against the chat boundary.
async fn read_file_segment(
    path: &std::path::Path,
    offset: usize,
    limit_arg: Option<usize>,
) -> Result<String, CoreError> {
    let file_str = path.to_string_lossy();

    // Check file size before reading
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|e| CoreError::Tool(format!("failed to stat {file_str}: {e}")))?;
    const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10MB
    let file_size = metadata.len();
    let truncated = file_size > MAX_FILE_SIZE;

    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| CoreError::Tool(format!("failed to open {file_str}: {e}")))?;
    let mut reader = tokio::io::BufReader::new(file);

    let limit = limit_arg
        .map(|l| l.min(MAX_READ_LIMIT))
        .unwrap_or(DEFAULT_READ_LIMIT);
    if limit == 0 {
        // Zero means zero: reading one line and reporting "lines 1-1"
        // would mislead the model about the file's contents.
        return Ok(String::new());
    }

    let mut lines: Vec<String> = Vec::with_capacity(limit);
    let mut total = 0usize;
    let mut line_buf = String::new();
    loop {
        line_buf.clear();
        let n = reader
            .read_line(&mut line_buf)
            .await
            .map_err(|e| CoreError::Tool(format!("failed to read {file_str}: {e}")))?;
        if n == 0 {
            break; // EOF
        }
        total += 1;
        if total >= offset {
            // Strip the trailing newline like BufRead::lines does
            if line_buf.ends_with('\n') {
                line_buf.pop();
                if line_buf.ends_with('\r') {
                    line_buf.pop();
                }
            }
            // Per-line truncation moved to the central overflow buffer
            // (Chat::bounded_output → buf_read) — a minified "line" is
            // buffered and paged, not clipped.
            lines.push(std::mem::take(&mut line_buf));
        }
        if lines.len() >= limit {
            break;
        }
    }

    // If the read stopped at the line limit, drain the remaining lines
    // (counting only, not storing) so `total` reflects the real line
    // count and the continuation footer fires when lines are unread.
    // Skipped for oversized files: their footer uses the 10MB notice,
    // and draining a multi-GB file would defeat the streaming design.
    // read_until into a reusable byte buffer skips the per-line UTF-8
    // validation read_line pays when appending to a String — the bytes
    // are discarded, only the count matters.
    if !truncated && lines.len() >= limit {
        let mut drain_buf = Vec::new();
        loop {
            drain_buf.clear();
            let n = reader
                .read_until(b'\n', &mut drain_buf)
                .await
                .map_err(|e| CoreError::Tool(format!("failed to read {file_str}: {e}")))?;
            if n == 0 {
                break;
            }
            total += 1;
        }
    }

    let start = offset;
    // end is the last SHOWN line (inclusive); start+len would be one
    // past it, and `total > end` would then stay false when exactly one
    // line remains unread, silently dropping it.
    // Guard `lines.is_empty()` first: offset beyond the file's line count
    // (or an empty file) leaves `lines` empty, and `start + 0 - 1` would
    // underflow to a descending interval like "showing lines 2-1".
    if lines.is_empty() {
        // No lines shown — nothing to report as a range. Truncated files
        // still get their size notice; otherwise emit a clear note about
        // the offset being past the end (or an empty read for an empty file).
        if truncated {
            return Ok(format!(
                "--- (file truncated at 10MB; offset {start} is beyond the file's line count, nothing shown) ---"
            ));
        }
        if total == 0 {
            return Ok(String::new()); // empty file
        }
        return Ok(format!(
            "--- (offset {start} is beyond the file's {total} line(s); nothing shown) ---"
        ));
    }
    let end = start + lines.len() - 1;
    let mut result = lines.join("\n");

    if truncated {
        result.push_str(&format!(
            "\n\n--- (file truncated at 10MB, showing lines {start}-{end}) ---"
        ));
    } else if total > end {
        result.push_str(&format!("\n\n--- (lines {start}-{end} of {total}) ---"));
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// EditFileTool — exact text replacement (str_replace)
// ---------------------------------------------------------------------------

/// Hard cap on files an edit tool will read/modify — keep splices and
/// match scans bounded (mirrors read_file's 10MB truncation notice).
const MAX_EDIT_SIZE: u64 = 10 * 1024 * 1024;

/// Read a file for editing: whole bytes, UTF-8 only, size-guarded. Creation
/// is `write_file`'s job — an edit tool that silently creates files turns
/// typos into stray files.
async fn read_for_edit(path: &std::path::Path) -> Result<String, CoreError> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| CoreError::Tool(format!("failed to read {}: {e}", path.display())))?;
    if bytes.len() as u64 > MAX_EDIT_SIZE {
        return Err(CoreError::Tool(format!(
            "{} exceeds the 10MB edit limit — use bash for bulk changes",
            path.display()
        )));
    }
    String::from_utf8(bytes).map_err(|_| {
        CoreError::Tool(format!(
            "{} is not valid UTF-8 (binary file?) — text edit tools only handle text",
            path.display()
        ))
    })
}

/// Convert LF text to the file's CRLF convention (idempotent for already-
/// CRLF input: normalize first, then re-emit CRLF).
fn to_crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// CRLF-tolerant matching: `read_file` strips `\r` from line ends, so a
/// model copying text verbatim never emits `\r` — multi-line `old_string`
/// can never match a CRLF file in raw space. Returns the LF-normalized
/// content; the edit splices in this space and the result is re-emitted
/// through [`to_crlf`], so untouched lines keep their exact endings.
fn normalize_crlf(content: &str) -> String {
    let bytes = content.as_bytes();
    let mut norm: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
            i += 1; // drop the CR, keep the LF
            continue;
        }
        norm.push(bytes[i]);
        i += 1;
    }
    // Byte-identical UTF-8 copy minus CR bytes — validity is preserved.
    String::from_utf8(norm).expect("byte-identical UTF-8 copy without CR bytes is valid UTF-8")
}

/// Cheap nearest-region hint for a failed exact match: the file line whose
/// trimmed form shares the longest common prefix with the old text's first
/// non-empty line (the dominant failure modes — content drift since the
/// last read, or an indentation mismatch). One pass, O(line length).
fn nearest_line_hint(content: &str, old_string: &str) -> String {
    let Some(anchor) = old_string.lines().map(str::trim).find(|l| !l.is_empty()) else {
        return String::new();
    };
    let anchor = anchor.as_bytes();
    let mut best: (usize, usize, &str) = (0, 0, ""); // (score, 1-based line, trimmed line)
    for (i, line) in content.lines().enumerate() {
        let t = line.trim();
        let score = t
            .as_bytes()
            .iter()
            .zip(anchor)
            .take_while(|(a, b)| a == b)
            .count();
        if score > best.0 {
            best = (score, i + 1, t);
        }
    }
    if best.0 == 0 {
        return String::new();
    }
    let shown: String = best.2.chars().take(120).collect();
    format!("closest line {}: {shown}", best.1)
}

/// One exact-text replacement to apply to the file — the item of
/// `edit_file`'s `edits` list.
#[derive(flux_macros::ToolItem, ::serde::Deserialize)]
struct EditOp {
    /// Exact text to replace, copied verbatim from a recent read (no line-number prefixes). Must be unique in the file unless replace_all is true.
    old_string: String,
    /// Replacement text. An empty string deletes the matched text.
    new_string: String,
    /// Replace every occurrence of old_string instead of requiring a unique match.
    replace_all: Option<bool>,
}

/// Outcome of one applied [`EditOp`], for the result text.
struct EditOutcome {
    /// 1-based line of the first match, at this op's apply time (later ops
    /// see the content earlier ops left behind, so the number is real).
    first_line: usize,
    /// Occurrences replaced (1, or more under `replace_all`).
    count: usize,
}

/// Apply `ops` sequentially to `work`: each op's match scan runs on the
/// content the previous ops left behind, so a later op may target text an
/// earlier one introduced. Fails on the first op with no match (with the
/// nearest-line hint) or an ambiguous non-`replace_all` match; `Err` carries
/// the failing op's position in the slice. Matching/splicing happens in the
/// caller's work space (raw or LF-normalized — see the CRLF gate at the call
/// sites).
fn apply_edit_ops(
    mut work: String,
    ops: &[&EditOp],
) -> Result<(String, Vec<EditOutcome>), (usize, CoreError)> {
    let mut outcomes = Vec::with_capacity(ops.len());
    for (pos, op) in ops.iter().enumerate() {
        let matches: Vec<(usize, usize)> = work
            .match_indices(&op.old_string)
            .map(|(s, _)| (s, s + op.old_string.len()))
            .collect();

        if matches.is_empty() {
            let hint = nearest_line_hint(&work, &op.old_string);
            let hint = if hint.is_empty() {
                String::new()
            } else {
                format!(" {hint}.")
            };
            return Err((
                pos,
                CoreError::Tool(format!(
                    "no exact match for old_string ({} chars).{hint} Re-read the file with read_file and copy the text verbatim, including whitespace",
                    op.old_string.chars().count()
                )),
            ));
        }
        if matches.len() > 1 && !op.replace_all.unwrap_or(false) {
            return Err((
                pos,
                CoreError::Tool(format!(
                    "old_string matched {} locations — it must be unique. Include more surrounding lines to disambiguate, or pass replace_all=true",
                    matches.len()
                )),
            ));
        }

        let first_line = work[..matches[0].0].matches('\n').count() + 1;
        let count = matches.len();
        // Splice back-to-front so earlier offsets stay valid (match_indices
        // yields non-overlapping, ascending ranges).
        for (start, end) in matches.iter().rev() {
            work.replace_range(start..end, &op.new_string);
        }
        outcomes.push(EditOutcome { first_line, count });
    }
    Ok((work, outcomes))
}

#[derive(Default, flux_macros::Tool, ::serde::Deserialize)]
#[tool(
    name = "edit_file",
    description = "Edit a file by replacing exact text snippets — one file per call, 1..=16 edits applied in listed order, each matching the content the previous edit left (a later edit may target text an earlier one introduced). Copy old_string VERBATIM from a recent read (no line-number prefixes); it must match the file content exactly and appear exactly once, or pass replace_all=true. The file is written once, atomically: one failed edit leaves the file untouched (with the nearest-line hint). To create a file or rewrite one entirely use write_file; to edit by line numbers use replace_lines."
)]
pub struct EditFileTool {
    /// Path to the file to edit (one file per call).
    file_path: String,
    /// Edits to apply in listed order (1..=16).
    edits: Vec<EditOp>,
}

impl EditFileTool {
    pub fn new() -> Self {
        Self::default()
    }

    async fn execute(&self, ctx: ToolCtx) -> Result<String, CoreError> {
        if self.edits.is_empty() {
            return Err(CoreError::Tool(
                "edits must not be empty — pass at least one edit".into(),
            ));
        }
        if self.edits.len() > MAX_ITEMS {
            return Err(CoreError::Tool(format!(
                "too many edits ({}) — apply at most {MAX_ITEMS} per call and split the rest across further calls",
                self.edits.len()
            )));
        }

        // Upfront per-item validation — nothing is read or written until
        // every edit is individually well-formed.
        let mut seen: HashMap<(&str, &str, Option<bool>), usize> = HashMap::new();
        for (i, edit) in self.edits.iter().enumerate() {
            if edit.old_string.is_empty() {
                return Err(CoreError::Tool(format!(
                    "edit {}: old_string must not be empty — use write_file to create a file or replace_lines to insert at a line",
                    i + 1
                )));
            }
            if edit.old_string == edit.new_string {
                return Err(CoreError::Tool(format!(
                    "edit {}: old_string and new_string are identical — nothing to replace",
                    i + 1
                )));
            }
            if let Some(&first) = seen.get(&(
                edit.old_string.as_str(),
                edit.new_string.as_str(),
                edit.replace_all,
            )) {
                return Err(CoreError::Tool(format!(
                    "edit {}: exact duplicate of edit {} (same old_string/new_string/replace_all) — remove the duplicate",
                    i + 1,
                    first + 1
                )));
            }
            seen.insert(
                (
                    edit.old_string.as_str(),
                    edit.new_string.as_str(),
                    edit.replace_all,
                ),
                i,
            );
        }

        // Resolve against the chat boundary — escapes are tool errors the
        // model sees and self-corrects.
        let file_path = ctx.resolve(&self.file_path)?;
        let path = file_path.as_path();

        let content = read_for_edit(path).await?;

        // Working space: raw bytes for LF files; LF-normalized when a
        // multi-line LF-only old_string meets a CRLF file (the model's
        // verbatim copy never carries \r — read_file strips it). Any edit
        // carrying \r keeps the whole file in raw space — a deliberate
        // raw-space edit wins over normalization. Line structure is
        // identical in both spaces, so match counts, hint lines, and
        // reported line numbers are all taken from here; the spliced
        // result is re-emitted as CRLF so untouched lines keep their
        // exact endings.
        let crlf = content.contains("\r\n")
            && self.edits.iter().any(|op| op.old_string.contains('\n'))
            && self.edits.iter().all(|op| !op.old_string.contains('\r'));
        let work = if crlf {
            normalize_crlf(&content)
        } else {
            content
        };

        let op_refs: Vec<&EditOp> = self.edits.iter().collect();
        // Whole-call atomicity: the ops apply to an in-memory string and
        // the file is written once at the end — one failed edit leaves
        // the file untouched, reported with the edit's 1-based index in
        // the call's own edits list.
        let (out, outcomes) = apply_edit_ops(work, &op_refs).map_err(|(pos, e)| {
            CoreError::Tool(format!("edit {}: {e}. File untouched.", pos + 1))
        })?;
        let out = if crlf { to_crlf(&out) } else { out };
        tokio::fs::write(path, &out).await.map_err(|e| {
            CoreError::Tool(format!("failed to write {}: {e}", file_path.display()))
        })?;

        // Report: one line per edit, 1-based in the call's edits list.
        let mut report = format!(
            "Applied {} edit(s). File now {} lines.",
            self.edits.len(),
            out.lines().count()
        );
        for (k, outcome) in outcomes.iter().enumerate() {
            if outcome.count == 1 {
                report.push_str(&format!(
                    "\n  edit {}: replaced 1 occurrence at line {}",
                    k + 1,
                    outcome.first_line
                ));
            } else {
                report.push_str(&format!(
                    "\n  edit {}: replaced {} occurrences (replace_all)",
                    k + 1,
                    outcome.count
                ));
            }
        }
        Ok(report)
    }
}

// ---------------------------------------------------------------------------
// WriteFileTool — create / full overwrite (the old edit_file behavior)
// ---------------------------------------------------------------------------

#[derive(Default, flux_macros::Tool, ::serde::Deserialize)]
#[tool(
    name = "write_file",
    description = "Create a file (with any missing parent directories) or fully overwrite its contents. For partial edits of existing files prefer edit_file (exact text replacement) or replace_lines (line-range splice)."
)]
pub struct WriteFileTool {
    /// Path to the file to write.
    file_path: String,
    /// The full new content to write to the file.
    content: String,
}

impl WriteFileTool {
    pub fn new() -> Self {
        Self::default()
    }

    async fn execute(&self, ctx: ToolCtx) -> Result<String, CoreError> {
        // Resolve against the chat boundary — escapes are tool errors the
        // model sees and self-corrects.
        let file_path = ctx.resolve(&self.file_path)?;
        let path = file_path.as_path();

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| CoreError::Tool(format!("failed to create parent dirs: {e}")))?;
        }

        tokio::fs::write(path, &self.content).await.map_err(|e| {
            CoreError::Tool(format!("failed to write {}: {e}", file_path.display()))
        })?;

        Ok(format!("Successfully wrote {}", file_path.display()))
    }
}

// ---------------------------------------------------------------------------
// ReplaceLinesTool — line-range splice (secondary; line numbers drift)
// ---------------------------------------------------------------------------

#[derive(Default, flux_macros::Tool, ::serde::Deserialize)]
#[tool(
    name = "replace_lines",
    description = "Replace a range of lines in a file (1-based, inclusive — the same numbering read_file reports). Use only right after reading the file: line numbers shift with every edit. Deletion: pass an empty content. Pure insertion: start_line = end_line + 1 — e.g. (5,4) inserts before line 5; (total_lines+1, total_lines) appends at the end. Prefer edit_file for exact text replacement."
)]
pub struct ReplaceLinesTool {
    /// Path to the file to edit.
    file_path: String,
    /// First line of the range (1-based, inclusive).
    start_line: usize,
    /// Last line of the range (1-based, inclusive). start_line = end_line + 1 inserts before start_line without removing lines.
    end_line: usize,
    /// Replacement content for the range. Empty deletes the range.
    content: String,
}

impl ReplaceLinesTool {
    pub fn new() -> Self {
        Self::default()
    }

    async fn execute(&self, ctx: ToolCtx) -> Result<String, CoreError> {
        // Resolve against the chat boundary — escapes are tool errors the
        // model sees and self-corrects.
        let file_path = ctx.resolve(&self.file_path)?;
        let path = file_path.as_path();

        let content = read_for_edit(path).await?;
        let lines: Vec<&str> = content.split_inclusive('\n').collect();
        let total = lines.len();

        if self.start_line == 0 {
            return Err(CoreError::Tool(
                "start_line is 1-based (read_file's numbering) — line 0 does not exist".into(),
            ));
        }
        // The empty range (start = end + 1) is a pure insert; anything looser
        // is a miscount the model should see instead of a silent splice.
        if self.end_line + 1 < self.start_line {
            return Err(CoreError::Tool(format!(
                "end_line ({}) must be >= start_line ({}) - 1 — an empty range (start_line = end_line + 1) inserts before start_line",
                self.end_line, self.start_line
            )));
        }
        if self.end_line > total {
            return Err(CoreError::Tool(format!(
                "end_line {} is beyond the file's {total} line(s) — re-read the file (line numbers shift with every edit)",
                self.end_line
            )));
        }

        let before: String = lines[..self.start_line - 1].concat();
        let after: String = lines[self.end_line..].concat();

        // The file's line-ending convention governs the spliced-in lines.
        let eol: &str = if content.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let mut replacement = self.content.clone();
        if !replacement.is_empty() && eol == "\r\n" && !replacement.contains('\r') {
            replacement = to_crlf(&replacement);
        }
        // Newline hygiene at the splice boundaries: the result must stay
        // line-structured whether or not the replacement carries a trailing
        // newline and whether the range abuts the file's end.
        if !replacement.is_empty() {
            if !after.is_empty() {
                // Mid-file splice: the next existing line must not glue on.
                if !replacement.ends_with(eol) {
                    replacement.push_str(eol);
                }
            } else if before.ends_with(eol) {
                // Appending to a newline-terminated file: keep it terminated.
                if !replacement.ends_with(eol) {
                    replacement.push_str(eol);
                }
            } else if !before.is_empty() {
                // Appending after a last line without a terminator: start
                // the new range on its own line.
                replacement.insert_str(0, eol);
            }
        }

        let out = format!("{before}{replacement}{after}");
        tokio::fs::write(path, &out).await.map_err(|e| {
            CoreError::Tool(format!("failed to write {}: {e}", file_path.display()))
        })?;

        let replaced = self.end_line + 1 - self.start_line;
        Ok(format!(
            "Replaced lines {}-{} ({} line(s)). File now {} line(s).",
            self.start_line,
            self.end_line,
            replaced,
            out.lines().count()
        ))
    }
}

// ---------------------------------------------------------------------------
// ListDirectoryTool
// ---------------------------------------------------------------------------

#[derive(Default, flux_macros::Tool, ::serde::Deserialize)]
#[tool(
    name = "list_directory",
    description = "List the contents of a directory. Defaults to the current working directory."
)]
pub struct ListDirectoryTool {
    /// Path to the directory to list. Defaults to current directory.
    path: Option<String>,
}

impl ListDirectoryTool {
    pub fn new() -> Self {
        Self::default()
    }

    async fn execute(&self, ctx: ToolCtx) -> Result<String, CoreError> {
        // Resolve against the chat boundary — escapes are tool errors the
        // model sees and self-corrects. Missing path lists the boundary itself.
        let dir = ctx.resolve(self.path.as_deref().unwrap_or("."))?;

        let mut entries: Vec<String> = Vec::new();
        let mut read_dir = tokio::fs::read_dir(dir)
            .await
            .map_err(|e| CoreError::Tool(format!("failed to read directory: {e}")))?;

        while let Some(entry) = read_dir
            .next_entry()
            .await
            .map_err(|e| CoreError::Tool(format!("read entry: {e}")))?
        {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "." || name == ".." {
                continue;
            }
            let file_type = entry
                .file_type()
                .await
                .map_err(|e| CoreError::Tool(format!("stat: {e}")))?;
            if file_type.is_dir() {
                entries.push(format!("{name}/"));
            } else {
                entries.push(name);
            }
        }

        entries.sort();
        Ok(entries.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{boundary_ctx, processed, setup};
    use flux_core::Tool;
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;

    fn read_args(path: &std::path::Path) -> HashMap<String, Value> {
        processed(vec![("file_path", json!(path.to_string_lossy()))])
    }

    #[tokio::test]
    async fn read_file_minified_line_returned_whole() {
        // Per-line truncation moved to the central overflow buffer
        // (Chat::bounded_output → buf_read): the tool returns the line WHOLE
        // (the model pages it via buf_read; re-reading with offset/limit
        // also works for files).
        let dir = setup();
        let path = dir.path().join("bundle.js");
        fs::write(&path, format!("tail{}", "x".repeat(400_000))).unwrap();
        let tool = ReadFileTool::new();
        let result = tool
            .call(
                processed(vec![("file_path", json!(path.to_string_lossy()))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert_eq!(
            result.chars().count(),
            400_004,
            "the tool must not truncate — central layer owns that now"
        );
        assert!(result.contains("tail"));
    }

    #[tokio::test]
    async fn read_file_wide_lines_pass_through_for_central_buffering() {
        // Wide lines pass through whole — the central overflow buffer bounds
        // what reaches the model context (head) and pages the rest.
        let dir = setup();
        let path = dir.path().join("wide.txt");
        let line = "z".repeat(1500);
        let content = (0..300)
            .map(|i| format!("{line} {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, content).unwrap();
        let tool = ReadFileTool::new();
        let result = tool
            .call(
                processed(vec![("file_path", json!(path.to_string_lossy()))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        // First line intact, last line numbered (not truncated), no truncation markers.
        assert!(result.contains(&"z".repeat(1500)));
        assert!(result.contains("299"));
        assert!(!result.contains("truncated"));
    }

    #[tokio::test]
    async fn read_file_reads_content() {
        let dir = setup();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello world").unwrap();
        let tool = ReadFileTool::new();
        let result = tool
            .call(read_args(&path), boundary_ctx(dir.path()))
            .await
            .unwrap();
        assert_eq!(result, "hello world");
    }

    #[tokio::test]
    async fn read_file_offset_beyond_eof_in_oversized_file_no_descending_interval() {
        // A >10MB file whose actual line count is below the requested offset:
        // no lines are shown, and the truncation footer must not print a
        // descending interval ("showing lines 2-1"). Regression: `end =
        // start + lines.len() - 1` underflowed to `start - 1` when `lines`
        // was empty.
        let dir = setup();
        let path = dir.path().join("huge_single_line.txt");
        // One line larger than the 10MB cap → truncated, but with few lines.
        let content = "x".repeat(10 * 1024 * 1024 + 4096);
        fs::write(&path, &content).unwrap();
        let tool = ReadFileTool::new();
        let result = tool
            .call(
                processed(vec![
                    ("file_path", json!(path.to_string_lossy())),
                    ("ranges", json!([{ "offset": 2 }])),
                ]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(
            result.contains("file truncated at 10MB"),
            "expected truncation notice, got: {result:.200}"
        );
        assert!(
            !result.contains("showing lines 2-1"),
            "must not print a descending interval, got: {result:.200}"
        );
    }

    #[tokio::test]
    async fn read_file_large_truncates() {
        let dir = setup();
        let path = dir.path().join("big.txt");
        // Write a file larger than the streaming read would output
        let content = (0..3000)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, &content).unwrap();
        let tool = ReadFileTool::new();
        let args = processed(vec![
            ("file_path", json!(path.to_string_lossy())),
            ("ranges", json!([{ "limit": 100 }])),
        ]);
        let result = tool.call(args, boundary_ctx(dir.path())).await.unwrap();
        let lines: Vec<&str> = result.lines().collect();
        // Should have at most 100 content lines + possibly a footer
        assert!(lines.len() <= 102);
    }

    #[tokio::test]
    async fn read_file_limit_truncation_footer() {
        let dir = setup();
        let path = dir.path().join("many.txt");
        let content = (0..3000)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, &content).unwrap();
        let tool = ReadFileTool::new();
        let result = tool
            .call(read_args(&path), boundary_ctx(dir.path()))
            .await
            .unwrap();
        // Default limit (2000) is hit; the footer must report remaining lines
        // and end at the last SHOWN line (not one past it).
        assert!(
            result.contains("--- (lines 1-2000 of 3000) ---"),
            "expected continuation footer, got: {result:.200}"
        );
        assert!(
            !result.contains("line 2000"),
            "read should stop at the limit, got: {result:.200}"
        );
    }

    #[tokio::test]
    async fn read_file_boundary_exactly_limit_plus_one() {
        // The file has exactly limit+1 lines: the footer must still fire so
        // the model knows the last line is unread. Regression: `end` was one
        // past the last shown line, making `total > end` false at the
        // boundary and silently dropping the final line.
        let dir = setup();
        let path = dir.path().join("boundary.txt");
        let content = (1..=6)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, &content).unwrap();
        let tool = ReadFileTool::new();
        let args = processed(vec![
            ("file_path", json!(path.to_string_lossy())),
            ("ranges", json!([{ "limit": 5 }])),
        ]);
        let result = tool.call(args, boundary_ctx(dir.path())).await.unwrap();
        assert!(
            result.contains("--- (lines 1-5 of 6) ---"),
            "boundary footer must fire, got: {result}"
        );
        assert!(
            !result.contains("line 6"),
            "read should stop at the limit, got: {result}"
        );
    }

    #[tokio::test]
    async fn read_file_offset_and_limit() {
        let dir = setup();
        let path = dir.path().join("numbered.txt");
        let content = (1..=50)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, &content).unwrap();
        let tool = ReadFileTool::new();
        let args = processed(vec![
            ("file_path", json!(path.to_string_lossy())),
            ("ranges", json!([{ "offset": 10, "limit": 5 }])),
        ]);
        let result = tool.call(args, boundary_ctx(dir.path())).await.unwrap();
        assert!(
            result.contains("line 10"),
            "should start at line 10, got: {result}"
        );
        assert!(
            result.contains("line 14"),
            "should include line 14, got: {result}"
        );
    }

    #[tokio::test]
    async fn read_file_numeric_offset_survives_json_string_pipeline() {
        // The chat layer serializes the LLM's arguments as a JSON string and
        // parses them into a HashMap<String, Value>. Numeric offsets must
        // survive untouched (no string conversion in the pipeline).
        let dir = setup();
        let path = dir.path().join("numbered.txt");
        let content = (1..=20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, &content).unwrap();

        let raw = json!({
            "file_path": path.to_string_lossy(),
            "ranges": [{ "offset": 8, "limit": 3 }],
        })
        .to_string();
        let args: HashMap<String, Value> = serde_json::from_str(&raw).unwrap();

        let tool = ReadFileTool::new();
        let result = tool.call(args, boundary_ctx(dir.path())).await.unwrap();
        assert_eq!(result.lines().next(), Some("line 8"), "got: {result}");
        assert!(result.contains("line 10"), "got: {result}");
        assert!(!result.contains("line 11"), "got: {result}");
    }

    #[tokio::test]
    async fn read_file_limit_zero_returns_no_lines() {
        let dir = setup();
        let path = dir.path().join("small.txt");
        fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let tool = ReadFileTool::new();
        let args = processed(vec![
            ("file_path", json!(path.to_string_lossy())),
            ("ranges", json!([{ "limit": 0 }])),
        ]);
        let result = tool.call(args, boundary_ctx(dir.path())).await.unwrap();
        assert_eq!(result, "", "limit 0 must yield no lines, got: {result}");
    }

    #[tokio::test]
    async fn read_file_over_10mb_marks_truncated() {
        let dir = setup();
        let path = dir.path().join("huge.txt");
        // Build a file just over the 10MB cap with short lines
        let mut content = String::with_capacity(10 * 1024 * 1024 + 4096);
        while content.len() <= 10 * 1024 * 1024 {
            content.push_str("line of padding\n");
        }
        fs::write(&path, &content).unwrap();
        let tool = ReadFileTool::new();
        let result = tool
            .call(read_args(&path), boundary_ctx(dir.path()))
            .await
            .unwrap();
        assert!(
            result.contains("file truncated at 10MB"),
            "expected truncation notice, got: {result:.200}"
        );
        assert!(
            result.contains("showing lines 1-2000"),
            "truncation footer must end at the last shown line, got: {result:.200}"
        );
    }

    #[tokio::test]
    async fn read_file_nonexistent_inside_boundary_fails_at_open() {
        // A non-existent path inside the boundary RESOLVES (the ancestor
        // walk allows it — edit_file needs that) and then fails at open:
        // the error is the tool's result string, not a round-level block.
        let dir = setup();
        let tool = ReadFileTool::new();
        let result = tool
            .call(
                read_args(Path::new("nonexistent/path")),
                boundary_ctx(dir.path()),
            )
            .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("failed to stat"),
            "nonexistent file must fail at stat, got: {err}"
        );
    }

    #[tokio::test]
    async fn read_file_outside_boundary_is_denied() {
        // The boundary rides ToolCtx now: an absolute path outside the
        // chat workdir is a resolve error the model sees.
        let dir = setup();
        let tool = ReadFileTool::new();
        let result = tool
            .call(
                read_args(Path::new("/etc/passwd")),
                boundary_ctx(dir.path()),
            )
            .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("path escape"),
            "outside-boundary read must be denied, got: {err}"
        );
    }

    #[tokio::test]
    async fn read_file_requires_file_path() {
        let tool = ReadFileTool::new();
        // file_path is required by the schema — serde must reject its absence.
        let result = tool
            .call(processed(vec![]), flux_core::ToolCtx::new())
            .await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, CoreError::InvalidArguments(_)),
            "missing file_path should be InvalidArguments, got: {err}"
        );
    }

    #[tokio::test]
    async fn write_file_creates_and_reads() {
        let dir = setup();
        let path = dir.path().join("new.txt");
        let tool = WriteFileTool::new();
        let input = processed(vec![
            ("file_path", json!(path.to_string_lossy())),
            ("content", json!("new content")),
        ]);
        let result = tool.call(input, boundary_ctx(dir.path())).await.unwrap();
        assert!(result.contains("Successfully wrote"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "new content");
    }

    #[tokio::test]
    async fn write_file_creates_missing_parent_directories() {
        let dir = setup();
        let tool = WriteFileTool::new();
        let input = processed(vec![
            (
                "file_path",
                json!(dir.path().join("newdir/nested/f.txt").to_string_lossy()),
            ),
            ("content", json!("deep content")),
        ]);
        let result = tool.call(input, boundary_ctx(dir.path())).await;
        assert!(
            result.is_ok(),
            "write_file must create missing parents, got: {result:?}"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("newdir/nested/f.txt")).unwrap(),
            "deep content"
        );
    }

    // ── edit_file (str_replace) ──

    async fn edit(
        path: &Path,
        old: &str,
        new: &str,
        replace_all: bool,
    ) -> Result<String, CoreError> {
        let tool = EditFileTool::new();
        let mut op = json!({ "old_string": old, "new_string": new });
        if replace_all {
            op["replace_all"] = json!(true);
        }
        // The boundary is the file's own directory for these tests.
        let dir = path.parent().unwrap();
        tool.call(
            processed(vec![
                ("file_path", json!(path.to_string_lossy())),
                ("edits", json!([op])),
            ]),
            boundary_ctx(dir),
        )
        .await
    }

    #[tokio::test]
    async fn edit_file_replaces_unique_match_and_reports_position() {
        let dir = setup();
        let path = dir.path().join("code.rs");
        fs::write(&path, "fn main() {\n    let x = 1;\n    let y = 2;\n}\n").unwrap();
        let result = edit(&path, "let y = 2;", "let y = 3;", false)
            .await
            .unwrap();
        assert!(result.contains("line 3"), "got: {result}");
        assert!(result.contains("File now 4 lines"), "got: {result}");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("let y = 3;"));
        assert!(
            content.contains("let x = 1;"),
            "untouched bytes must survive"
        );
        assert!(content.ends_with('\n'), "trailing newline must survive");
    }

    #[tokio::test]
    async fn edit_file_no_match_reports_nearest_line_hint() {
        let dir = setup();
        let path = dir.path().join("code.rs");
        fs::write(&path, "fn main() {\n    let x = 1;\n}\n").unwrap();
        // The model's copy drifted (x = 2 in its memory).
        let err = edit(&path, "let x = 2;", "let x = 3;", false)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no exact match"), "got: {msg}");
        assert!(
            msg.contains("closest line 2"),
            "hint must land on the drifted line, got: {msg}"
        );
    }

    #[tokio::test]
    async fn edit_file_ambiguous_match_requires_replace_all() {
        let dir = setup();
        let path = dir.path().join("code.rs");
        fs::write(&path, "foo();\nbar();\nfoo();\n").unwrap();
        let err = edit(&path, "foo();", "baz();", false).await.unwrap_err();
        assert!(
            err.to_string().contains("matched 2 locations"),
            "got: {err}"
        );
        let result = edit(&path, "foo();", "baz();", true).await.unwrap();
        assert!(
            result.contains("replaced 2 occurrences (replace_all)"),
            "got: {result}"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "baz();\nbar();\nbaz();\n"
        );
    }

    #[tokio::test]
    async fn edit_file_rejects_missing_file_and_empty_old() {
        let dir = setup();
        // No creation on edit — that's write_file's job.
        let missing = dir.path().join("missing.txt");
        let err = edit(&missing, "a", "b", false).await.unwrap_err();
        assert!(err.to_string().contains("failed to read"), "got: {err}");

        let path = dir.path().join("f.txt");
        fs::write(&path, "hello").unwrap();
        let err = edit(&path, "", "b", false).await.unwrap_err();
        assert!(
            err.to_string().contains("old_string must not be empty"),
            "got: {err}"
        );
        let err = edit(&path, "hello", "hello", false).await.unwrap_err();
        assert!(err.to_string().contains("identical"), "got: {err}");
    }

    #[tokio::test]
    async fn edit_file_crlf_file_matches_lf_copy_and_preserves_endings() {
        let dir = setup();
        let path = dir.path().join("win.txt");
        fs::write(&path, "alpha\r\nbeta\r\ngamma\r\n").unwrap();
        // The model's read_file-derived copy never carries \r.
        let result = edit(&path, "alpha\nbeta", "ALPHA\nBETA", false)
            .await
            .unwrap();
        assert!(result.contains("line 1"), "got: {result}");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "ALPHA\r\nBETA\r\ngamma\r\n",
            "replacement must adopt the file's CRLF convention"
        );
    }

    // ── replace_lines ──

    async fn splice(
        path: &Path,
        start: usize,
        end: usize,
        content: &str,
    ) -> Result<String, CoreError> {
        let tool = ReplaceLinesTool::new();
        let dir = path.parent().unwrap();
        tool.call(
            processed(vec![
                ("file_path", json!(path.to_string_lossy())),
                ("start_line", json!(start)),
                ("end_line", json!(end)),
                ("content", json!(content)),
            ]),
            boundary_ctx(dir),
        )
        .await
    }

    #[tokio::test]
    async fn replace_lines_swaps_a_range() {
        let dir = setup();
        let path = dir.path().join("f.txt");
        fs::write(&path, "one\ntwo\nthree\nfour\n").unwrap();
        let result = splice(&path, 2, 3, "TWO\nTHREE\nTWO-AND-A-HALF")
            .await
            .unwrap();
        assert!(result.contains("File now 5 line(s)"), "got: {result}");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "one\nTWO\nTHREE\nTWO-AND-A-HALF\nfour\n",
            "the splice must end with a newline when more lines follow"
        );
    }

    #[tokio::test]
    async fn replace_lines_deletes_and_inserts() {
        let dir = setup();
        let path = dir.path().join("f.txt");
        // Deletion: empty content removes the range.
        fs::write(&path, "one\ntwo\nthree\n").unwrap();
        splice(&path, 2, 2, "").await.unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "one\nthree\n");
        // Pure insert before line 2: start = end + 1.
        splice(&path, 2, 1, "one-and-a-half").await.unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "one\none-and-a-half\nthree\n"
        );
        // Append: (total+1, total).
        splice(&path, 4, 3, "four").await.unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "one\none-and-a-half\nthree\nfour\n"
        );
    }

    #[tokio::test]
    async fn replace_lines_rejects_out_of_range_and_inverted() {
        let dir = setup();
        let path = dir.path().join("f.txt");
        fs::write(&path, "one\ntwo\n").unwrap();
        let err = splice(&path, 1, 5, "x").await.unwrap_err();
        assert!(
            err.to_string().contains("beyond the file's 2 line(s)"),
            "got: {err}"
        );
        let err = splice(&path, 5, 2, "x").await.unwrap_err();
        assert!(
            err.to_string().contains("must be >= start_line"),
            "got: {err}"
        );
        let err = splice(&path, 0, 0, "x").await.unwrap_err();
        assert!(err.to_string().contains("1-based"), "got: {err}");
    }

    #[tokio::test]
    async fn list_directory_lists_entries() {
        let dir = setup();
        fs::write(dir.path().join("a.txt"), "a").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        let tool = ListDirectoryTool::new();
        let result = tool
            .call(
                processed(vec![("path", json!(dir.path().to_string_lossy()))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(result.contains("a.txt"));
        assert!(result.contains("sub/"));
    }

    #[tokio::test]
    async fn list_directory_nonexistent() {
        let dir = setup();
        let tool = ListDirectoryTool::new();
        // Non-existent directory → read_dir should fail
        let result = tool
            .call(
                processed(vec![(
                    "path",
                    json!(dir.path().join("nonexistent_dir_xyz").to_string_lossy()),
                )]),
                boundary_ctx(dir.path()),
            )
            .await;
        assert!(result.is_err());
    }

    // ── read_file: multi-window reads ──

    #[tokio::test]
    async fn read_file_multiple_windows_are_headed_and_footed() {
        let dir = setup();
        fs::write(dir.path().join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let tool = ReadFileTool::new();
        let result = tool
            .call(
                processed(vec![
                    (
                        "file_path",
                        json!(dir.path().join("a.txt").to_string_lossy()),
                    ),
                    (
                        "ranges",
                        json!([{ "offset": 1, "limit": 2 }, { "offset": 3 }]),
                    ),
                ]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        // 2+ windows: one `=== path ===` section per window, each keeping
        // read_file's footer semantics.
        assert!(result.contains("=== "), "got: {result}");
        assert!(result.contains("--- (lines 1-2 of 3) ---"), "got: {result}");
        assert!(result.contains("alpha"), "got: {result}");
        assert!(result.contains("gamma"), "got: {result}");
        // Window 2's content comes after window 1's.
        let w1 = result.find("alpha").unwrap();
        let w2 = result.find("gamma").unwrap();
        assert!(w1 < w2);
    }

    #[tokio::test]
    async fn read_file_single_window_keeps_the_bare_output() {
        let dir = setup();
        let content = (1..=10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(dir.path().join("n.txt"), content).unwrap();
        let tool = ReadFileTool::new();
        let result = tool
            .call(
                processed(vec![
                    (
                        "file_path",
                        json!(dir.path().join("n.txt").to_string_lossy()),
                    ),
                    ("ranges", json!([{ "offset": 3, "limit": 2 }])),
                ]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(result.contains("line 3"), "got: {result}");
        assert!(result.contains("line 4"), "got: {result}");
        assert!(!result.contains("line 5"), "got: {result}");
        // One window = the old offset/limit shape: no section header.
        assert!(!result.contains("=== "), "got: {result}");
    }

    #[tokio::test]
    async fn read_file_empty_ranges_mean_whole_file() {
        let dir = setup();
        fs::write(dir.path().join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let tool = ReadFileTool::new();
        let result = tool
            .call(
                processed(vec![
                    (
                        "file_path",
                        json!(dir.path().join("a.txt").to_string_lossy()),
                    ),
                    ("ranges", json!([])),
                ]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert_eq!(result, "alpha\nbeta\ngamma");
    }

    #[tokio::test]
    async fn read_file_window_failure_fails_the_call() {
        // One file per call: a stat/open error is path-level — every window
        // would fail identically, so the call fails instead of rendering
        // per-window error sections.
        let dir = setup();
        let tool = ReadFileTool::new();
        let result = tool
            .call(
                processed(vec![
                    (
                        "file_path",
                        json!(dir.path().join("missing.txt").to_string_lossy()),
                    ),
                    ("ranges", json!([{ "offset": 1 }, { "offset": 2 }])),
                ]),
                boundary_ctx(dir.path()),
            )
            .await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("failed to stat"), "got: {err}");
    }

    #[tokio::test]
    async fn read_file_over_window_cap_rejected() {
        let dir = setup();
        fs::write(dir.path().join("a.txt"), "alpha\n").unwrap();
        let tool = ReadFileTool::new();
        let over: Vec<Value> = (0..MAX_ITEMS + 1).map(|_| json!({ "offset": 1 })).collect();
        let err = tool
            .call(
                processed(vec![
                    (
                        "file_path",
                        json!(dir.path().join("a.txt").to_string_lossy()),
                    ),
                    ("ranges", json!(over)),
                ]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too many windows"), "got: {err}");
    }

    // ── edit_file: multi-edit calls ──

    fn edits_args(path: &Path, ops: Value) -> HashMap<String, Value> {
        processed(vec![
            ("file_path", json!(path.to_string_lossy())),
            ("edits", ops),
        ])
    }

    #[tokio::test]
    async fn edit_file_applies_edits_in_order() {
        let dir = setup();
        let path = dir.path().join("code.txt");
        fs::write(&path, "fn a() {}\nfn b() {}\n").unwrap();
        let tool = EditFileTool::new();
        let result = tool
            .call(
                edits_args(
                    &path,
                    json!([
                        { "old_string": "fn a() {}", "new_string": "fn a() -> u32 { 1 }" },
                        { "old_string": "fn a() -> u32 { 1 }", "new_string": "fn a() -> u32 { 2 }" },
                    ]),
                ),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        let written = fs::read_to_string(&path).unwrap();
        // Edit 2 matched the text edit 1 introduced — sequential semantics.
        assert!(written.contains("fn a() -> u32 { 2 }"), "got: {written}");
        assert!(result.contains("Applied 2 edit(s)"), "got: {result}");
        assert!(
            result.contains("edit 2: replaced 1 occurrence"),
            "got: {result}"
        );
    }

    #[tokio::test]
    async fn edit_file_failed_edit_leaves_the_file_untouched_and_reports_the_index() {
        let dir = setup();
        let path = dir.path().join("f.txt");
        fs::write(&path, "a\nb\nc\n").unwrap();
        let tool = EditFileTool::new();
        // Edit 2 fails → the whole call rolls back (the file is written
        // once, at the end), and the error carries the edit's 1-based index
        // in the call's own edits list.
        let err = tool
            .call(
                edits_args(
                    &path,
                    json!([
                        { "old_string": "a", "new_string": "A" },
                        { "old_string": "zzz", "new_string": "Z" },
                    ]),
                ),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("edit 2:"), "got: {err}");
        assert!(err.to_string().contains("no exact match"), "got: {err}");
        assert!(err.to_string().contains("File untouched"), "got: {err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\nb\nc\n");
    }

    #[tokio::test]
    async fn edit_file_crlf_edit_keeps_the_file_in_raw_space() {
        // A \r-carrying old_string opts the whole file out of LF
        // normalization — a deliberate raw-space edit wins over the CRLF
        // gate, and untouched lines keep their exact endings.
        let dir = setup();
        let path = dir.path().join("win.txt");
        fs::write(&path, "one\r\ntwo\r\nthree\r\n").unwrap();
        let tool = EditFileTool::new();
        let result = tool
            .call(
                edits_args(
                    &path,
                    json!([{ "old_string": "one\r\ntwo", "new_string": "ONE\r\nTWO" }]),
                ),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert_eq!(written, "ONE\r\nTWO\r\nthree\r\n", "got: {written:?}");
        assert!(result.contains("Applied 1 edit(s)"), "got: {result}");
    }

    #[tokio::test]
    async fn edit_file_rejects_empty_identical_and_duplicate_edits() {
        let dir = setup();
        let path = dir.path().join("f.txt");
        fs::write(&path, "x\n").unwrap();
        let tool = EditFileTool::new();

        let err = tool
            .call(edits_args(&path, json!([])), boundary_ctx(dir.path()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("must not be empty"), "got: {err}");

        let err = tool
            .call(
                edits_args(&path, json!([{ "old_string": "", "new_string": "y" }])),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("edit 1: old_string must not be empty"),
            "got: {err}"
        );

        let err = tool
            .call(
                edits_args(&path, json!([{ "old_string": "x", "new_string": "x" }])),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("identical"), "got: {err}");

        let dup = json!([
            { "old_string": "x", "new_string": "y" },
            { "old_string": "x", "new_string": "y" },
        ]);
        let err = tool
            .call(edits_args(&path, dup), boundary_ctx(dir.path()))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("exact duplicate of edit 1"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn edit_file_over_cap_rejected() {
        let dir = setup();
        let path = dir.path().join("f.txt");
        fs::write(&path, "x\n").unwrap();
        let tool = EditFileTool::new();
        // Cap precedes per-item validation: 17 identical edits report the
        // cap, not the duplicate.
        let over: Vec<Value> = (0..MAX_ITEMS + 1)
            .map(|_| json!({ "old_string": "x", "new_string": "y" }))
            .collect();
        let err = tool
            .call(edits_args(&path, json!(over)), boundary_ctx(dir.path()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too many edits"), "got: {err}");
    }

    #[tokio::test]
    async fn edit_file_missing_required_fields_is_invalid_arguments() {
        let dir = setup();
        let tool = EditFileTool::new();
        // `edits` is required by the schema — serde must reject its absence.
        let err = tool
            .call(processed(vec![]), boundary_ctx(dir.path()))
            .await
            .unwrap_err();
        assert!(
            matches!(err, CoreError::InvalidArguments(_)),
            "missing edits should be InvalidArguments, got: {err}"
        );
    }
}
