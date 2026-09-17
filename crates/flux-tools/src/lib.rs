//! Built-in tool implementations for Flux.
//!
//! Tools are organized by category:
//! - `fs` — file system (read, write, list)
//! - `search` — pattern matching (grep, glob)
//! - `shell` — command execution
//! - `skills` — Agent Skills packages (skill_list / skill_read, lazy loading)
//! - `ansi` — escape-sequence stripping (bash output hygiene only)
//! - `subprocess` — shared kill-hygiene subprocess runner (internal)
//!
//! The chat boundary (workdir / current_dir) reaches tools through
//! `ToolCtx` — path arguments resolve via `ToolCtx::resolve` against it;
//! there is no argument-preprocessing layer (see the trust model in
//! AGENTS.md).

mod ansi;
mod fs;
mod search;
mod shell;
mod skills;
mod subprocess;

#[cfg(test)]
pub(crate) mod test_util;

pub use fs::{
    EditFileTool, EditFilesTool, ListDirectoryTool, ReadFileTool, ReadFilesTool, ReplaceLinesTool,
    WriteFileTool,
};
pub use search::{GlobTool, GrepTool};
pub use shell::BashTool;
pub use skills::{
    SkillEntry, SkillListTool, catalog_for_workdir, catalog_section, discover_for_workdir,
    discover_skills, format_skill_activation, normalize_rel, read_skill_file, skill_content,
    validate_skill_dir,
};
pub use subprocess::arm_parent_death_signal;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::warn;

/// Format the output of a subprocess into a human-readable string.
/// Input is already decoded (`String::from_utf8_lossy` at the caller) and
/// ANSI-stripped where the caller's semantics require it (bash only).
pub(crate) fn format_command_output(
    stdout: &str,
    stderr: &str,
    status: Option<std::process::ExitStatus>,
) -> String {
    let mut result = String::new();
    if !stdout.is_empty() {
        result.push_str(stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("--- stderr ---\n");
        result.push_str(stderr);
    }
    let code = status.as_ref().and_then(|s| s.code());
    if result.is_empty() {
        result = if status.is_some() {
            format!("(exit code: {})", code.unwrap_or(-1))
        } else {
            "(no output)".to_string()
        };
    } else if let Some(c) = code.filter(|c| *c != 0) {
        // Non-zero WITH output: carry the code as a trailing marker so the
        // failure is visible without cross-referencing stderr's presence
        // (the model reads it; the web client's tool-card verdict sniffs
        // the exact same suffix).
        result.push_str(&format!("\n(exit code: {c})"));
    }
    result.trim_end().to_string()
}

/// Walk a directory tree non-recursively (explicit stack to avoid overflow).
/// Tracks visited paths to prevent infinite loops from symlink cycles.
/// Permission errors on individual directories are logged and skipped.
/// Entries whose canonical path falls outside the root (via symlinks) are
/// silently skipped so the walk cannot escape the workdir.
///
/// Per-entry cost is one `DirEntry::file_type()` (free on most filesystems —
/// readdir's d_type, no extra syscall). Only SYMLINKS pay a `canonicalize`
/// (realpath = a stat chain over every path component): a non-symlink entry
/// reached through a canonical parent is lexically inside the root and
/// cannot escape, so the old per-entry realpath (which dominated grep/glob
/// on large trees) is gone. Entries the callback receives are identical to
/// what the old canonicalize-per-entry walk produced: canonical parent +
/// plain component, or the symlink's resolved target.
pub(crate) fn walk_dir(
    root: &Path,
    f: &mut dyn FnMut(&Path) -> Result<bool, std::io::Error>,
) -> std::io::Result<()> {
    let root_real = root.canonicalize()?;
    if root_real.is_file() {
        f(&root_real)?;
        return Ok(());
    }
    let mut dirs: Vec<PathBuf> = vec![root_real.clone()];
    let mut visited: HashSet<PathBuf> = HashSet::new();
    while let Some(dir) = dirs.pop() {
        // Canonical-path dedup at pop time: `dirs` holds canonical paths
        // only (lexical under a canonical parent == canonical; symlinked
        // dirs are pushed as their resolved, containment-checked target).
        if !visited.insert(dir.clone()) {
            continue; // already visited — skip symlink cycle / duplicate
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                warn!("skipping unreadable directory {}: {e}", dir.display());
                continue;
            }
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let Ok(ft) = entry.file_type() else {
                continue; // racing delete / exotic fs — skip
            };
            let path = entry.path(); // canonical parent + plain name
            let (path, ft) = if ft.is_symlink() {
                // The only component that can move the path outside the
                // root (or create a cycle): resolve the target, re-check
                // containment, and classify the TARGET. Dangling links
                // cannot be classified — skip (matches the old
                // canonicalize-failure skip).
                let Ok(real) = path.canonicalize() else {
                    continue;
                };
                if !real.starts_with(&root_real) {
                    continue; // symlink escapes workdir — skip
                }
                let Ok(meta) = real.symlink_metadata() else {
                    continue; // racing delete — skip
                };
                (real, meta.file_type())
            } else {
                (path, ft)
            };
            if ft.is_dir() {
                dirs.push(path);
            } else if ft.is_file() && !f(&path)? {
                return Ok(());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::process::ExitStatusExt;
    use tempfile::tempdir;

    #[test]
    fn walk_dir_finds_files() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "a").unwrap();
        fs::write(dir.path().join("b.txt"), "b").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/c.txt"), "c").unwrap();

        let mut found = Vec::new();
        walk_dir(dir.path(), &mut |p| {
            found.push(p.file_name().unwrap().to_string_lossy().to_string());
            Ok(true)
        })
        .unwrap();

        assert!(found.contains(&"a.txt".to_string()));
        assert!(found.contains(&"b.txt".to_string()));
        assert!(found.contains(&"c.txt".to_string()));
    }

    #[test]
    fn walk_dir_skips_symlink_outside_workdir() {
        let dir = tempdir().unwrap();
        // Create a symlink inside workdir pointing outside
        let inside = dir.path().join("escape_link");
        std::os::unix::fs::symlink("/etc", &inside).unwrap();
        // Also create a normal file
        fs::write(dir.path().join("safe.txt"), "safe").unwrap();

        let mut found = Vec::new();
        walk_dir(dir.path(), &mut |p| {
            found.push(p.file_name().unwrap().to_string_lossy().to_string());
            Ok(true)
        })
        .unwrap();

        assert!(found.contains(&"safe.txt".to_string()));
        assert!(
            !found
                .iter()
                .any(|f| f.contains("passwd") || f.contains("hostname"))
        );
    }

    #[test]
    fn format_command_output_combines_stdout_stderr() {
        let out = format_command_output("hello", "world", None);
        assert!(out.contains("hello"));
        assert!(out.contains("world"));
        assert!(out.contains("--- stderr ---"));
    }

    #[test]
    fn format_command_output_shows_exit_code_when_empty() {
        let out = format_command_output("", "", Some(std::process::ExitStatus::default()));
        assert!(out.contains("exit code") || out == "(no output)");
    }

    #[test]
    fn format_command_output_appends_nonzero_exit_code_marker() {
        // A real ExitStatus for code 3 (Unix wait status = 3 << 8).
        let failed = std::process::ExitStatus::from_raw(3 << 8);
        let out = format_command_output("partial output", "", Some(failed));
        assert_eq!(out, "partial output\n(exit code: 3)");
        // Zero exit stays unmarked.
        let ok = std::process::ExitStatus::from_raw(0);
        assert_eq!(format_command_output("fine", "", Some(ok)), "fine");
    }
}
