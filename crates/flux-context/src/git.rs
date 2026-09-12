//! Git helpers — status, history, and change-frequency measurement.
//!
//! The change-frequency signals drive the scaffold's prefix-stable
//! ordering: `git log --format=%ct` gives "when did this path last
//! change", which the info blocks use to rank content stable-first.

use std::path::Path;
use std::time::Duration;

/// Git status + diff stat, best-effort and capped (used by the scaffold).
pub async fn git_status(workdir: &str, max_lines: usize) -> String {
    let mut out = String::new();
    for args in [
        vec!["-C", workdir, "status", "--short"],
        vec!["-C", workdir, "diff", "--stat"],
    ] {
        let Ok(child) = tokio::process::Command::new("git")
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            return out;
        };
        let Ok(output) =
            tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await
        else {
            return out;
        };
        let Ok(output) = output else { return out };
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines().take(max_lines) {
            out.push_str(line);
            out.push('\n');
        }
        if out.len() >= max_lines * 40 {
            break;
        }
    }
    out
}

/// Current branch via `git -C workdir branch --show-current`, best-effort.
pub fn git_branch(workdir: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["-C", workdir, "branch", "--show-current"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

/// Stability measurement: the last-change timestamp of a path.
///
/// Primary signal = git history (`git log -1 --format=%ct -- <path>` — the
/// last commit touching the path). Fallback = file mtime (untracked /
/// non-git workspaces). `None` when neither is available (path missing).
///
/// Used by the scaffold builder to order blocks stable-first so the
/// provider's prefix cache keeps hitting across feature rebuilds: content
/// that has not changed in months stays in the cached prefix; content that
/// churns (recent commits / fresh mtimes) moves to the tail where its
/// variation only invalidates itself.
pub fn last_change_ts(workdir: &str, path: &str) -> Option<i64> {
    if let Some(ts) = git_last_commit_ts(workdir, path) {
        return Some(ts);
    }
    let p = Path::new(workdir).join(path);
    let meta = p.metadata().ok()?;
    let modified = meta.modified().ok()?;
    modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

/// The last commit touching `path` (or the whole repo for "."), seconds.
pub fn git_last_commit_ts(workdir: &str, path: &str) -> Option<i64> {
    let output = std::process::Command::new("git")
        .args(["-C", workdir, "log", "-1", "--format=%ct", "--", path])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    s.parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn last_change_ts_reads_git_history() {
        // Skip when git is unavailable (e.g. minimal CI images).
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            return;
        }
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .output()
            .unwrap();
        let commit = |msg: &str, date: &str| {
            std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"])
                .current_dir(root)
                .output()
                .unwrap();
            std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "-q",
                    "-m",
                    msg,
                ])
                .env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_DATE", date)
                .current_dir(root)
                .output()
                .unwrap();
        };
        std::fs::write(root.join("stable.txt"), "v1").unwrap();
        commit("first", "2024-01-01T00:00:00Z");
        std::fs::write(root.join("churn.txt"), "v1").unwrap();
        commit("second", "2025-01-01T00:00:00Z");

        let stable = last_change_ts(root.to_str().unwrap(), "stable.txt").unwrap();
        let churn = last_change_ts(root.to_str().unwrap(), "churn.txt").unwrap();
        assert!(
            stable < churn,
            "older commit must rank as more stable: {stable} vs {churn}"
        );
    }
}
