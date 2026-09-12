//! The chat workdir boundary — path resolution against a fixed sandbox root.
//!
//! The boundary is **functional confinement** (the chat's project scope on
//! a multi-project server), not a security boundary: an escape is a tool
//! error the model sees and self-corrects, and real isolation comes from
//! the OS or a container. The resolution logic lives here because both the
//! built-in tools (flux-tools) and the chat-owned `state_set` tool
//! (flux-chat) resolve against the same boundary — one implementation, no
//! drift.

use crate::CoreError;
use std::path::{Component, Path, PathBuf};
/// Resolve a path relative to a boundary working directory, preventing
/// traversal escapes.
///
/// Absolute inputs must land inside the boundary; relative inputs join it.
/// Existing paths canonicalize directly. Non-existent paths resolve via the
/// deepest existing ancestor plus an appended tail — which explicitly
/// rejects `..`/`.` tail components and dangling symlinks (a dangling link
/// must be denied: a later `fs::write` would follow the link and create the
/// file outside the boundary).
pub(crate) fn resolve_path(workdir: &Path, input: &str) -> Result<PathBuf, CoreError> {
    let path = Path::new(input);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workdir.join(path)
    };
    // Try canonicalize (path exists). Fall back to resolving the deepest
    // existing ancestor and appending the missing tail (write_file may write
    // a new file several levels under a not-yet-existing directory).
    let canonical = match resolved.canonicalize() {
        Ok(c) => c,
        Err(_) => {
            let mut existing = resolved.as_path();
            let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
            while existing.canonicalize().is_err() {
                // A dangling symlink (lstat succeeds, canonicalize fails —
                // both are ENOENT) must be denied: a later `fs::write` would
                // follow the link and create the file outside the sandbox.
                // `symlink_metadata` does not follow links, so it tells
                // "exists" apart from "missing component".
                if existing.symlink_metadata().is_ok() {
                    return Err(CoreError::Tool(format!(
                        "dangling symlink in path: {input}"
                    )));
                }
                // Explicitly reject `..`/`.`/root terminators. `starts_with`
                // does NOT normalize `..` — a joined
                // `/tmp/X/a/../../../../etc` still starts with `/tmp/X` — so
                // containment must not rely on the incidental `None` that
                // `file_name()` returns for a `..`-terminated path. Reject
                // the component explicitly, or a later refactor of this walk
                // could reopen the nonexistent-path escape.
                let (name, parent) = match (existing.components().next_back(), existing.parent()) {
                    (Some(Component::Normal(n)), Some(p)) => (n, p),
                    _ => return Err(CoreError::Tool(format!("parent path error: {input}"))),
                };
                tail.push(name);
                existing = parent;
            }
            let base = existing
                .canonicalize()
                .map_err(|e| CoreError::Tool(format!("parent path error: {e}")))?;
            if !base.starts_with(workdir) {
                return Err(CoreError::Tool(format!("path escape attempt: {input}")));
            }
            let mut joined = base.to_path_buf();
            for name in tail.iter().rev() {
                joined.push(name);
            }
            joined
        }
    };
    if !canonical.starts_with(workdir) {
        return Err(CoreError::Tool(format!("path escape attempt: {input}")));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // ── Moved verbatim from flux-tools (the function's original home) ──

    #[test]
    fn resolve_path_relative() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("test.txt"), "content").unwrap();
        let resolved = resolve_path(dir.path(), "test.txt").unwrap();
        assert!(resolved.ends_with("test.txt"));
    }

    #[test]
    fn resolve_path_absolute_within_workdir() {
        let dir = tempfile::tempdir().unwrap();
        let abs = dir.path().join("test.txt");
        fs::write(&abs, "content").unwrap();
        let resolved = resolve_path(dir.path(), abs.to_str().unwrap()).unwrap();
        assert!(resolved.ends_with("test.txt"));
    }

    #[test]
    fn resolve_path_prevents_absolute_outside_workdir() {
        let dir = tempfile::tempdir().unwrap();
        // /etc/passwd exists and is outside the temp dir.
        let result = resolve_path(dir.path(), "/etc/passwd");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("path escape") || err.contains("path error"),
            "should reject path outside workdir, got: {err}"
        );
    }

    #[test]
    fn resolve_path_nonexistent_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let result = resolve_path(dir.path(), "nonexistent.txt");
        // Non-existent files are now allowed — the parent is resolved and
        // the filename is appended (needed for write_file on new paths).
        assert!(result.is_ok());
        assert!(result.unwrap().ends_with("nonexistent.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_path_rejects_dangling_symlink() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        // Dangling symlink: the target does not exist. Its canonicalize
        // failure is the same NotFound as "a missing component" — the
        // walk-up MUST use symlink_metadata to tell them apart, or a write
        // follows the link and creates files outside the boundary.
        let outside = dir.path().join("outside_target.txt");
        let link = dir.path().join("link");
        symlink(&outside, &link).unwrap();
        let result = resolve_path(dir.path(), "link");
        assert!(
            result.is_err(),
            "dangling symlink must be denied, got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_path_rejects_dangling_symlink_in_tail_ancestor() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        // The dangling link is an intermediate ancestor of the multi-segment tail: /wd/link/sub/new.txt
        let outside = dir.path().join("outside_target.txt");
        let link = dir.path().join("link");
        symlink(&outside, &link).unwrap();
        let result = resolve_path(dir.path(), "link/sub/new.txt");
        assert!(
            result.is_err(),
            "dangling symlink ancestor must be denied, got: {result:?}"
        );
    }

    #[test]
    fn resolve_path_empty_input_resolves_to_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_path(dir.path(), "").unwrap();
        assert_eq!(resolved, dir.path().canonicalize().unwrap());
    }

    #[test]
    fn resolve_path_dot_resolves_to_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_path(dir.path(), ".").unwrap();
        assert_eq!(resolved, dir.path().canonicalize().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_path_rejects_dotdot_escape_on_nonexistent_tail() {
        // `..` in the nonexistent tail must be rejected explicitly —
        // `starts_with` does not normalize it, so the lexical containment
        // check alone would pass `/wd/a/../../../etc`.
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("a")).unwrap();
        let result = resolve_path(dir.path(), "a/../../../etc/passwd-new");
        assert!(result.is_err(), "dotdot tail escape must be denied");
    }

    // ── Adapted from the deleted flux-tools preprocess tests: the
    //    expansion behaviors the tools now get via ToolCtx::resolve ──

    #[test]
    fn resolve_relative_file_lands_inside_boundary() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("f.txt"), "x").unwrap();
        let resolved = resolve_path(dir.path(), "f.txt").unwrap();
        assert!(resolved.starts_with(dir.path()));
        assert!(resolved.ends_with("f.txt"));
    }

    #[test]
    fn resolve_hard_denies_escape() {
        let dir = tempfile::tempdir().unwrap();
        let result = resolve_path(dir.path(), "../../etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn resolve_symlinked_file_inside_boundary_canonicalizes() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("real.txt"), "x").unwrap();
        symlink(dir.path().join("real.txt"), dir.path().join("alias")).unwrap();
        // An in-boundary symlink canonicalizes to its target (still inside).
        let resolved = resolve_path(dir.path(), "alias").unwrap();
        assert!(resolved.starts_with(dir.path()));
        assert!(resolved.ends_with("real.txt"));
    }

    #[test]
    fn resolve_accepts_relative_path_with_intermediate_dir() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        let resolved = resolve_path(dir.path(), "src/main.rs").unwrap();
        assert_eq!(
            resolved,
            dir.path().join("src/main.rs").canonicalize().unwrap()
        );
    }

    #[test]
    fn resolve_path_error_is_a_tool_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_path(dir.path(), "/etc/passwd").unwrap_err();
        assert!(matches!(err, crate::CoreError::Tool(_)));
    }
}
