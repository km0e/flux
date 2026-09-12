//! Built-in tool implementations for Flux.
//!
//! Tools are organized by category:
//! - `fs` — file system (read, write, list)
//! - `search` — pattern matching (grep, glob)
//! - `shell` — command execution
//! - `rust` — Rust/Cargo project tooling
//! - `skills` — Agent Skills packages (skill_list / skill_read, lazy loading)
//! - `subprocess` — shared kill-hygiene subprocess runner (internal)
//!
//! The chat boundary (workdir / current_dir) reaches tools through
//! `ToolCtx` — path arguments resolve via `ToolCtx::resolve` against it;
//! there is no argument-preprocessing layer (see the trust model in
//! AGENTS.md).

mod fs;
mod rust;
mod search;
mod shell;
mod skills;
mod subprocess;

#[cfg(test)]
pub(crate) mod test_util;

pub use fs::{EditFileTool, ListDirectoryTool, ReadFileTool, ReplaceLinesTool, WriteFileTool};
pub use rust::{RustInitTool, RustVerifyTool};
pub use search::{GlobTool, GrepTool};
pub use shell::BashTool;
pub use skills::{SkillEntry, SkillListTool, SkillReadTool, discover_skills, validate_skill_dir};

use flux_core::CoreError;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::warn;

/// Format the output of a subprocess into a human-readable string.
pub(crate) fn format_command_output(
    stdout: &[u8],
    stderr: &[u8],
    status: Option<std::process::ExitStatus>,
) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    let mut result = String::new();
    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("--- stderr ---\n");
        result.push_str(&stderr);
    }
    if result.is_empty() {
        result = if let Some(s) = status {
            format!("(exit code: {})", s.code().unwrap_or(-1))
        } else {
            "(no output)".to_string()
        };
    }
    result.trim_end().to_string()
}

/// Require a non-empty workdir — the sandbox boundary. Restores the
/// pre-derive contract where a missing workdir was `InvalidArguments`
/// instead of silently falling back to the server process's cwd.
pub(crate) fn require_workdir(wd: &Path) -> Result<(), CoreError> {
    if wd.as_os_str().is_empty() {
        return Err(CoreError::InvalidArguments("workdir is required".into()));
    }
    Ok(())
}

/// Walk a directory tree non-recursively (explicit stack to avoid overflow).
/// Tracks visited paths to prevent infinite loops from symlink cycles.
/// Permission errors on individual directories are logged and skipped.
/// Entries whose canonical path falls outside the root (via symlinks) are
/// silently skipped so the walk cannot escape the workdir.
pub(crate) fn walk_dir(
    root: &Path,
    f: &mut dyn FnMut(&Path) -> Result<bool, std::io::Error>,
) -> std::io::Result<()> {
    let root_real = root.canonicalize()?;
    if root_real.is_file() {
        f(&root_real)?;
        return Ok(());
    }
    let mut dirs: Vec<std::path::PathBuf> = vec![root_real.clone()];
    let mut visited: HashSet<PathBuf> = HashSet::new();
    while let Some(dir) = dirs.pop() {
        // Resolve the canonical path to detect symlink cycles
        let real = match dir.canonicalize() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if !visited.insert(real.clone()) {
            continue; // already visited — skip symlink cycle
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                warn!("skipping unreadable directory {}: {e}", dir.display());
                continue;
            }
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            // Symlink containment check
            let real = match path.canonicalize() {
                Ok(p) => p,
                Err(_) => continue,
            };
            if !real.starts_with(&root_real) {
                continue; // symlink escapes workdir — skip
            }
            if real.is_dir() {
                dirs.push(real);
            } else if real.is_file() && !f(&real)? {
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
    use tempfile::tempdir;

    #[cfg(unix)]
    #[test]
    fn resolve_path_rejects_dangling_symlink() {
        // The boundary-resolution security tests live in flux-core
        // (boundary module — the function's new home); this one stays as a
        // flux-tools-visible smoke check of the migration.
        use std::os::unix::fs::symlink;
        let dir = tempdir().unwrap();
        let outside = dir.path().join("outside_target.txt");
        let link = dir.path().join("link");
        symlink(&outside, &link).unwrap();
        let ctx = flux_core::ToolCtx {
            workdir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let result = ctx.resolve("link");
        assert!(
            result.is_err(),
            "dangling symlink must be denied, got: {result:?}"
        );
    }

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
        let out = format_command_output(b"hello", b"world", None);
        assert!(out.contains("hello"));
        assert!(out.contains("world"));
        assert!(out.contains("--- stderr ---"));
    }

    #[test]
    fn format_command_output_shows_exit_code_when_empty() {
        let out = format_command_output(b"", b"", Some(std::process::ExitStatus::default()));
        assert!(out.contains("exit code") || out == "(no output)");
    }
}
