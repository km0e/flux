//! Filesystem browsing for the UI's workdir picker (any directory
//! the server's user can read is selectable).
//!
//! Two pure helpers back the `FsList` / `FsRead` Connect RPCs:
//! [`list_dir`] produces a directories-first listing (the picker's
//! navigation + contents preview), [`preview_file`] a bounded, lossy
//! head of one file (the picker's file preview pane). Both fail into a
//! carried `String` error instead of a Result so the RPC layer can ship
//! them straight into the response's inline `error` — UI browsing never
//! touches the global error channel.
//!
//! Trust posture: the server runs with the starting user's
//! permissions; the connection is same-origin browser trust — the shell
//! tool can already reach every path these helpers can. Real isolation
//! comes from the OS or a container, not this layer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Git working-tree status of a listed entry (absent = clean / not a
/// repo). Directories aggregate their descendants — the strongest signal
/// wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitStatus {
    Modified,
    Added,
    Untracked,
    Conflicted,
}

/// One entry kind — dirs navigable, files previewable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FsEntryKind {
    Dir,
    File,
}

/// One directory-listing entry.
#[derive(Debug)]
pub(crate) struct FsEntry {
    pub(crate) name: String,
    pub(crate) kind: FsEntryKind,
    pub(crate) size: Option<u64>,
    pub(crate) git: Option<GitStatus>,
}

/// Budget for `fs_read` previews. Large enough that typical source files
/// (README, configs, most code) preview WHOLE — the truncation flag only
/// fires for genuinely huge files — while staying a single WS frame
/// (~256 KB JSON). The frontend renders the honest "first N of M" hint.
pub(crate) const PREVIEW_BYTES: usize = 256 * 1024;

/// The default start directory for the picker: `$HOME` (where projects
/// live), falling back to the process cwd.
pub(crate) fn default_start_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// Outcome of a browse operation — either a listing or the failure the UI
/// shows inline.
pub(crate) enum Listing {
    Ok {
        path: String,
        parent: Option<String>,
        entries: Vec<FsEntry>,
    },
    Err(String),
}

/// List `path` (or the default start dir when `None`) directories-first.
pub(crate) fn list_dir(path: Option<&str>) -> Listing {
    let Some(target) = path.map(str::trim).filter(|p| !p.is_empty()) else {
        return to_listing(&default_start_dir());
    };
    to_listing(Path::new(target))
}

fn to_listing(dir: &Path) -> Listing {
    // Canonicalize so the picker's display path is stable across symlinks
    // and `..` segments (and so `parent` navigation never loops).
    let canonical = match dir.canonicalize() {
        Ok(p) => p,
        Err(e) => return Listing::Err(format!("{}: {e}", dir.display())),
    };
    if !canonical.is_dir() {
        return Listing::Err(format!("not a directory: {}", canonical.display()));
    }
    let parent = canonical.parent().map(|p| p.to_string_lossy().into_owned());
    let mut dirs: Vec<FsEntry> = Vec::new();
    let mut files: Vec<FsEntry> = Vec::new();
    let read = match std::fs::read_dir(&canonical) {
        Ok(r) => r,
        Err(e) => return Listing::Err(format!("{}: {e}", canonical.display())),
    };
    // Git working-tree status per entry (empty outside a repo / on any git
    // failure — the tree renders plain). A listed dir that is itself
    // untracked marks every entry: nothing under an untracked dir is tracked.
    let (git, listed_dir_untracked) = git_entry_status(&canonical);
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // Metadata follows symlinks: a linked directory stays navigable.
        let meta = match entry.metadata() {
            Ok(m) => m,
            // Unresolvable entry (broken link / racing delete) — skip, the
            // listing must stay complete for the paths it does show.
            Err(_) => continue,
        };
        let (kind, size) = if meta.is_dir() {
            (FsEntryKind::Dir, None)
        } else {
            (FsEntryKind::File, Some(meta.len()))
        };
        let git_status = if listed_dir_untracked {
            Some(GitStatus::Untracked)
        } else {
            git.get(&name).copied()
        };
        let entry = FsEntry {
            git: git_status,
            name,
            kind,
            size,
        };
        if kind == FsEntryKind::Dir {
            dirs.push(entry);
        } else {
            files.push(entry);
        }
    }
    let by_name = |a: &FsEntry, b: &FsEntry| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    };
    dirs.sort_by(by_name);
    files.sort_by(by_name);
    dirs.extend(files);
    Listing::Ok {
        path: canonical.to_string_lossy().into_owned(),
        parent,
        entries: dirs,
    }
}

/// Git working-tree status for one directory's entries, best-effort.
///
/// `git status --porcelain=v1 -z --untracked-files=normal --no-renames -- .`
/// (run with `-C <dir>`) reports repo-root-relative paths, bounded to the
/// LISTED subtree by the `.` pathspec — a deep listing never scans the
/// whole repo, and collapsed untracked dirs (`dir/`) keep node_modules-
/// scale trees from exploding the output. Each path maps to the direct
/// child of `<dir>` that contains it: files take their own state;
/// directories aggregate their descendants (strongest signal wins — see
/// [`GitStatus`]). A listed dir that is ITSELF untracked (collapsed to
/// `?? prefix/`) marks every entry untracked — by definition nothing under
/// an untracked dir can be tracked. Any failure (not a repo, no git
/// binary) yields an empty map — the listing never fails on status.
/// Returns the per-entry map plus the listed-dir-untracked flag.
fn git_entry_status(dir: &Path) -> (HashMap<String, GitStatus>, bool) {
    let Some(toplevel) = run_git(dir, &["rev-parse", "--show-toplevel"]) else {
        return (HashMap::new(), false);
    };
    let toplevel = PathBuf::from(toplevel.trim());
    // Paths in the status output are repo-root-relative; the listed dir's
    // offset inside the repo lets each path land on its direct child here.
    let prefix = dir.strip_prefix(&toplevel).unwrap_or(dir);
    let Some(raw) = run_git(
        dir,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
            "--no-renames",
            "--",
            ".",
        ],
    ) else {
        return (HashMap::new(), false);
    };

    let mut out: HashMap<String, GitStatus> = HashMap::new();
    let mut listed_dir_untracked = false;
    // NUL-separated records: `XY <path>\0` (XY = index + worktree columns;
    // collapsed untracked dirs end with `/`).
    for record in raw.split('\0').filter(|r| r.len() > 3) {
        let (xy, path) = record.split_at(2);
        let path = path.strip_prefix(' ').unwrap_or(path);
        let Some(status) = porcelain_status(xy) else {
            continue;
        };
        // A collapsed untracked dir that IS the listed dir (or one of its
        // ancestors) means the whole subtree is untracked — per-entry
        // records don't exist for it (verified against git: the pathspec
        // collapses to the deepest untracked dir inside it).
        if status == GitStatus::Untracked
            && path.ends_with('/')
            && prefix.starts_with(Path::new(path.trim_end_matches('/')))
        {
            listed_dir_untracked = true;
            continue;
        }
        let Ok(rel) = Path::new(path).strip_prefix(prefix) else {
            continue;
        };
        // First component = the direct-child entry this change lives under.
        let Some(name) = rel.components().next() else {
            continue;
        };
        let name = name.as_os_str().to_string_lossy().into_owned();
        merge_status(&mut out, name, status);
    }
    (out, listed_dir_untracked)
}

/// One porcelain XY pair → the entry status. Index column first (`git
/// status --porcelain` docs); unmerged pairs are exactly DD/AU/UD/UA/DU/AA/UU.
fn porcelain_status(xy: &str) -> Option<GitStatus> {
    let x = xy.as_bytes()[0];
    let y = xy.as_bytes()[1];
    let unmerged = x == b'U' || y == b'U' || (x == b'A' && y == b'A') || (x == b'D' && y == b'D');
    if unmerged {
        return Some(GitStatus::Conflicted);
    }
    if x == b'?' {
        return Some(GitStatus::Untracked);
    }
    if x == b'A' {
        return Some(GitStatus::Added);
    }
    if matches!(x, b'M' | b'R' | b'C' | b'T') || matches!(y, b'M' | b'T') {
        return Some(GitStatus::Modified);
    }
    // Deleted (staged or worktree) — the file itself is gone from the
    // listing; it only reaches here through a directory aggregate.
    if x == b'D' || y == b'D' {
        return Some(GitStatus::Modified);
    }
    None
}

/// Aggregate one status into an entry's slot — the strongest signal wins
/// (conflicted > modified > untracked > added).
fn merge_status(map: &mut HashMap<String, GitStatus>, name: String, status: GitStatus) {
    let rank = |s: GitStatus| match s {
        GitStatus::Conflicted => 3,
        GitStatus::Modified => 2,
        GitStatus::Untracked => 1,
        GitStatus::Added => 0,
    };
    match map.get(&name) {
        Some(existing) if rank(*existing) >= rank(status) => {}
        _ => {
            map.insert(name, status);
        }
    }
}

/// Run a git command in `dir`, capturing stdout. `None` on any failure
/// (no git binary, not a repo, non-zero exit) — the listing never fails on
/// status. Synchronous, like every other filesystem touch in this module.
fn run_git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Outcome of a preview read — content or the failure the UI shows inline.
/// `size` is the file's byte size (the truncated hint needs the total).
pub(crate) struct Preview {
    pub content: String,
    pub truncated: bool,
    pub size: u64,
}

/// Read a bounded head of `path` for the preview pane. Binary content
/// (NUL byte in the read window) becomes a placeholder note — the preview
/// is for recognizing files, not dumping them. The budget cut is
/// UTF-8-boundary-safe and, when the buffer contains a newline, line-aligned
/// (a truncated tail never starts mid-line/mid-character).
pub(crate) fn preview_file(path: &str) -> Result<Preview, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
    if meta.is_dir() {
        return Err(format!("not a file: {path}"));
    }
    let size = meta.len();
    let mut file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    use std::io::Read;
    // Read one byte beyond the budget to know whether content was cut.
    let mut buf = vec![0u8; PREVIEW_BYTES + 1];
    let n = file.read(&mut buf).map_err(|e| format!("{path}: {e}"))?;
    buf.truncate(n);
    let truncated = n > PREVIEW_BYTES;
    buf.truncate(PREVIEW_BYTES);
    if buf.contains(&0) {
        return Ok(Preview {
            content: "(binary file — preview unavailable)".to_string(),
            truncated: false,
            size,
        });
    }
    if truncated {
        // Never end mid-character: walk back over UTF-8 continuation bytes
        // (0b10xxxxxx); if we then land on a multi-byte LEAD byte whose
        // continuation was cut off by the budget, drop that char too.
        let mut len = buf.len();
        while len > 0 && (buf[len - 1] & 0xC0) == 0x80 {
            len -= 1;
        }
        if len > 0 && (buf[len - 1] & 0x80) != 0 {
            len -= 1;
        }
        buf.truncate(len);
        // Prefer ending on a line boundary (there is always one in a
        // text file of this size; a no-newline blob keeps the char cut).
        if let Some(pos) = buf.iter().rposition(|&b| b == b'\n') {
            buf.truncate(pos + 1);
        }
    }
    Ok(Preview {
        content: String::from_utf8_lossy(&buf).into_owned(),
        truncated,
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn lists_directories_first_with_sizes() {
        let dir = std::env::temp_dir().join(format!("flux-fs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(&dir.join("b.txt"), b"hello");
        write(&dir.join("a_dir/x"), b"x");
        write(&dir.join("a.txt"), b"z".repeat(10).as_slice());

        match list_dir(dir.to_str()) {
            Listing::Ok {
                path,
                parent,
                entries,
            } => {
                assert_eq!(path, dir.canonicalize().unwrap().to_str().unwrap());
                assert!(parent.is_some());
                let kinds: Vec<_> = entries.iter().map(|e| e.kind).collect();
                assert_eq!(
                    kinds,
                    [FsEntryKind::Dir, FsEntryKind::File, FsEntryKind::File]
                );
                assert_eq!(entries[0].name, "a_dir");
                assert_eq!(entries[1].name, "a.txt");
                assert_eq!(entries[1].size, Some(10));
                assert_eq!(entries[2].size, Some(5));
            }
            Listing::Err(e) => panic!("unexpected listing error: {e}"),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unresolvable_path_is_a_carried_error() {
        let missing = std::env::temp_dir().join("flux-fs-missing-nope");
        match list_dir(missing.to_str()) {
            Listing::Err(e) => assert!(e.contains("No such file") || e.contains("no such file")),
            Listing::Ok { .. } => panic!("missing path must not list"),
        }
    }

    #[test]
    fn file_path_is_an_error_not_a_listing() {
        let dir = std::env::temp_dir().join(format!("flux-fs-f-{}", std::process::id()));
        write(&dir, b"x");
        assert!(matches!(list_dir(dir.to_str()), Listing::Err(_)));
        std::fs::remove_file(&dir).unwrap();
    }

    #[test]
    fn default_start_dir_exists() {
        assert!(default_start_dir().is_dir());
    }

    #[test]
    fn preview_truncates_at_the_budget() {
        let dir = std::env::temp_dir().join(format!("flux-fs-p-{}", std::process::id()));
        let big = dir.join("big.txt");
        write(&big, &b"a".repeat(PREVIEW_BYTES + 100));
        let p = preview_file(big.to_str().unwrap()).unwrap();
        assert!(p.truncated);
        assert_eq!(p.content.len(), PREVIEW_BYTES);
        assert_eq!(p.size, (PREVIEW_BYTES + 100) as u64);
        let small = dir.join("small.txt");
        write(&small, b"tiny");
        let p = preview_file(small.to_str().unwrap()).unwrap();
        assert!(!p.truncated);
        assert_eq!(p.content, "tiny");
        assert_eq!(p.size, 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn preview_cut_is_line_aligned_and_utf8_safe() {
        let dir = std::env::temp_dir().join(format!("flux-fs-l-{}", std::process::id()));
        // Two newline-terminated lines inside the budget, then a tail of
        // multibyte chars (é = 2 bytes) crossing it — the cut must fall on
        // the last newline, never mid-line or mid-character.
        let mut body = vec![b'a'; PREVIEW_BYTES - 16];
        body.extend_from_slice(b"\nbbbbbbbbbb\n");
        body.extend("ééé ééé — tail beyond the budget".repeat(64).as_bytes());
        let f = dir.join("lines.txt");
        write(&f, &body);
        let p = preview_file(f.to_str().unwrap()).unwrap();
        assert!(p.truncated);
        // The cut lands on the last newline inside the budget.
        assert!(p.content.ends_with('\n'));
        assert!(std::str::from_utf8(p.content.as_bytes()).is_ok());
        assert!(
            !p.content.contains('\u{fffd}'),
            "no replacement char: cut is boundary-safe"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn preview_flags_binary_content() {
        let dir = std::env::temp_dir().join(format!("flux-fs-b-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("bin.dat");
        write(&bin, &[0x89, 0x50, 0x4e, 0x47, 0x00, 0x0d]);
        let p = preview_file(bin.to_str().unwrap()).unwrap();
        assert!(p.content.contains("binary"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn preview_of_a_directory_is_an_error() {
        let dir = std::env::temp_dir().join(format!("flux-fs-d-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(preview_file(dir.to_str().unwrap()).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ── Git working-tree status ────────────────────────────────────────

    /// Skip the git-dependent tests when git is unavailable (minimal CI).
    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// A real repo under a tempdir: one committed file, one modified, one
    /// staged-new, one untracked (nested so the parent dir aggregates).
    fn seeded_repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        std::fs::write(root.join("clean.txt"), "v1").unwrap();
        std::fs::write(root.join("modified.txt"), "v1").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "seed"]);
        std::fs::write(root.join("modified.txt"), "v2").unwrap();
        std::fs::write(root.join("staged.txt"), "new").unwrap();
        git(&["add", "staged.txt"]);
        std::fs::create_dir_all(root.join("new/nested")).unwrap();
        std::fs::write(root.join("new/nested/deep.txt"), "untracked").unwrap();
        (dir, root.to_path_buf())
    }

    #[test]
    fn listing_outside_a_repo_carries_no_git_status() {
        let dir = std::env::temp_dir().join(format!("flux-fs-nogit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        match list_dir(dir.to_str()) {
            Listing::Ok { entries, .. } => {
                assert!(entries.iter().all(|e| e.git.is_none()));
            }
            Listing::Err(e) => panic!("unexpected listing error: {e}"),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn git_status_marks_files_and_aggregates_directories() {
        if !git_available() {
            return;
        }
        let (_keep, root) = seeded_repo();
        match list_dir(root.to_str()) {
            Listing::Ok { entries, .. } => {
                let status_of = |name: &str| {
                    entries
                        .iter()
                        .find(|e| e.name == name)
                        .unwrap_or_else(|| panic!("entry {name} missing"))
                        .git
                };
                assert_eq!(status_of("clean.txt"), None);
                assert_eq!(status_of("modified.txt"), Some(GitStatus::Modified));
                assert_eq!(status_of("staged.txt"), Some(GitStatus::Added));
                // The untracked FILE inside new/nested is below the listed
                // root — only the ancestor dir on this level shows, and it
                // aggregates the untracked signal.
                assert_eq!(status_of("new"), Some(GitStatus::Untracked));
            }
            Listing::Err(e) => panic!("unexpected listing error: {e}"),
        }
    }

    #[test]
    fn git_status_marks_deep_subtree_entries_when_expanded() {
        if !git_available() {
            return;
        }
        let (_keep, root) = seeded_repo();
        // Expanding `new/` lists the nested dir; expanding `new/nested/`
        // lists the untracked file itself — the per-entry marking must
        // survive the lazy-load depth (paths are relative to the LISTED
        // dir, not the repo root). The collapsed-untracked-dir rule makes
        // every entry under an untracked dir untracked.
        let nested = root.join("new/nested");
        match list_dir(nested.to_str()) {
            Listing::Ok { entries, .. } => {
                let deep = entries
                    .iter()
                    .find(|e| e.name == "deep.txt")
                    .expect("deep.txt listed");
                assert_eq!(deep.git, Some(GitStatus::Untracked));
            }
            Listing::Err(e) => panic!("unexpected listing error: {e}"),
        }
    }

    #[test]
    fn git_status_of_a_collapsed_untracked_dir_marks_every_entry() {
        if !git_available() {
            return;
        }
        // node_modules-scale case: a wholly untracked dir collapses to ONE
        // porcelain record; listing INSIDE it must still mark all entries
        // (nothing under an untracked dir can be tracked).
        let (_keep, root) = seeded_repo();
        let untracked = root.join("new");
        match list_dir(untracked.to_str()) {
            Listing::Ok { entries, .. } => {
                assert!(!entries.is_empty());
                assert!(
                    entries.iter().all(|e| e.git == Some(GitStatus::Untracked)),
                    "all entries under an untracked dir are untracked"
                );
            }
            Listing::Err(e) => panic!("unexpected listing error: {e}"),
        }
    }

    #[test]
    fn porcelain_status_maps_xy_pairs() {
        assert_eq!(porcelain_status("??"), Some(GitStatus::Untracked));
        assert_eq!(porcelain_status("A "), Some(GitStatus::Added));
        assert_eq!(porcelain_status("M "), Some(GitStatus::Modified));
        assert_eq!(porcelain_status(" M"), Some(GitStatus::Modified));
        assert_eq!(porcelain_status("R "), Some(GitStatus::Modified));
        assert_eq!(porcelain_status(" D"), Some(GitStatus::Modified));
        assert_eq!(porcelain_status("UU"), Some(GitStatus::Conflicted));
        assert_eq!(porcelain_status("AA"), Some(GitStatus::Conflicted));
        assert_eq!(porcelain_status("DU"), Some(GitStatus::Conflicted));
    }

    #[test]
    fn merge_status_keeps_the_strongest_signal() {
        let mut m = HashMap::new();
        merge_status(&mut m, "d".into(), GitStatus::Added);
        merge_status(&mut m, "d".into(), GitStatus::Modified);
        assert_eq!(m["d"], GitStatus::Modified, "modified outranks added");
        merge_status(&mut m, "d".into(), GitStatus::Untracked);
        assert_eq!(m["d"], GitStatus::Modified);
        merge_status(&mut m, "d".into(), GitStatus::Conflicted);
        assert_eq!(m["d"], GitStatus::Conflicted, "conflicted wins");
    }
}
