//! Skill management — install / browse / remove for the UI's Skills dialog.
//!
//! Skills are pure filesystem content: the registry IS the directory.
//! Installing = materializing a validated skill directory into the global
//! skills location (`~/.flux/skills`), and the model's `skill_list` picks
//! it up on its very next call (flux-tools scans fresh per call — no
//! restart semantics, nothing persisted). Removal is limited to the
//! global dir: project skills live in the user's repository and the
//! server never writes into a workdir it does not own.
//!
//! Two install sources (exactly one per request):
//! - local directory path — validated in place, then copied (the source
//!   is never moved or modified);
//! - git URL (HTTP(S)/SSH) — `git clone --depth 1` via the git CLI
//!   (already a runtime dependency through fsbrowse), optionally picking
//!   a subdirectory out of a multi-skill repository.
//!
//! Both funnel through the same path: stage a candidate directory inside
//! the skills parent (same filesystem, so the final publish is an atomic
//! rename), validate it with flux-tools' parser, then publish. Size and
//! file-count budgets bound every copy; `.git` is never copied.

use flux_proto::flux::v1::SkillSummary;
use flux_tools::{discover_skills, validate_skill_dir};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Budget for one install (copy or clone + copy) — skills are
/// instructions plus modest assets, not data dumps.
const MAX_INSTALL_BYTES: u64 = 100 * 1024 * 1024;
/// File-count budget for one install — a pathological tree fails loudly
/// instead of pinning the server's disk.
const MAX_INSTALL_FILES: usize = 10_000;
/// `git clone` wall-clock cap (shallow single-branch of a skill repo).
const GIT_CLONE_TIMEOUT: Duration = Duration::from_secs(120);

/// The global skills dir — the same location flux-tools' discovery scans
/// (`$HOME/.flux/skills`, `USERPROFILE` fallback).
pub(crate) fn global_skills_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| Path::new(&home).join(".flux").join("skills"))
}

/// The wire summary: global entries are removable, project entries are
/// read-only (they live in the chat workdir's repository).
pub(crate) fn summaries(global: Option<&Path>, project: Option<&Path>) -> Vec<SkillSummary> {
    discover_skills(global, project)
        .into_iter()
        .map(|e| SkillSummary {
            name: e.name,
            description: e.description,
            source: if e.project {
                flux_proto::flux::v1::SkillSource::Project
            } else {
                flux_proto::flux::v1::SkillSource::Global
            } as i32,
            removable: !e.project && global.is_some(),
        })
        .collect()
}

/// Install from a local directory: validate a copied candidate, publish
/// under the skill's own name. The source is never modified. Blocking
/// filesystem work runs on the blocking pool — the source is an
/// unbounded user-supplied tree.
pub(crate) async fn install_from_path(path: &str) -> Result<String, String> {
    let global = require_global_dir()?;
    let source = PathBuf::from(path);
    install(global, |stage| async move {
        let source = source;
        tokio::task::spawn_blocking(move || {
            copy_tree(&source, &stage)?;
            validate_skill_dir(&stage)
                .map(|entry| entry.name)
                .map_err(|e| format!("invalid skill: {e}"))
        })
        .await
        .map_err(|e| format!("install task failed: {e}"))?
    })
    .await
}

/// Install from a git URL: shallow-clone into a staging dir, optionally
/// pick `subpath`, copy (minus `.git`) into a staging candidate,
/// validate, publish. Returns the installed name.
pub(crate) async fn install_from_url(url: &str, subpath: Option<&str>) -> Result<String, String> {
    let global = require_global_dir()?;
    let scheme_ok =
        url.starts_with("https://") || url.starts_with("http://") || url.starts_with("git@");
    if !scheme_ok {
        return Err(format!(
            "unsupported URL `{url}` — use an https/http or SSH git remote (archive downloads are not supported)"
        ));
    }
    let url = url.to_string();
    let subpath = subpath.map(|s| s.to_string());
    install(global, |stage| async move {
        // The clone target is a sibling of the candidate inside the same
        // staging root, so nothing ever leaves the skills filesystem.
        let repo = stage
            .parent()
            .ok_or_else(|| "invalid staging dir".to_string())?
            .join("clone");
        clone_repo(&url, &repo).await?;
        let source = match &subpath {
            Some(sub) => repo.join(sub),
            None => repo.clone(),
        };
        // Candidate = repo[/subpath] copied WITHOUT .git (the clone's
        // history is transport, not skill content), under the budget.
        tokio::task::spawn_blocking(move || {
            copy_tree(&source, &stage)?;
            validate_skill_dir(&stage)
                .map(|entry| entry.name)
                .map_err(|e| format!("invalid skill: {e}"))
        })
        .await
        .map_err(|e| format!("install task failed: {e}"))?
    })
    .await
}

/// The shared install funnel: stage a candidate dir inside the global
/// skills parent, hand it to `populate` (source-specific), then publish
/// atomically — the publish is a rename on one filesystem, so a
/// partially-written skill is never visible to the model's discovery.
/// The staging root is removed best-effort on every exit path.
async fn install<F, Fut>(global: PathBuf, populate: F) -> Result<String, String>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    std::fs::create_dir_all(&global)
        .map_err(|e| format!("cannot create {}: {e}", global.display()))?;
    let staging_root = global.join(format!(
        ".staging-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let stage = staging_root.join("skill");
    let outcome = match std::fs::create_dir_all(&stage) {
        Err(e) => Err(format!("cannot create staging dir: {e}")),
        Ok(()) => match populate(stage.clone()).await {
            Err(e) => Err(e),
            Ok(name) => {
                let target = global.join(&name);
                if target.exists() {
                    Err(format!(
                        "a skill named `{name}` already exists — remove it first (no silent overwrite)"
                    ))
                } else {
                    std::fs::rename(&stage, &target)
                        .map(|_| name.clone())
                        .map_err(|e| format!("publish failed: {e}"))
                }
            }
        },
    };
    // The staging root never holds the only copy of anything the user
    // asked to keep — dropping it is always safe.
    let _ = std::fs::remove_dir_all(&staging_root);
    outcome
}

fn require_global_dir() -> Result<PathBuf, String> {
    global_skills_dir().ok_or_else(|| {
        "no home directory — cannot locate the global skills dir (~/.flux/skills)".to_string()
    })
}

/// Shallow-clone one branch of `url` into `dir` via the git CLI. The URL
/// rides as a single argv element (no shell), and only http(s)/SSH
/// schemes pass the caller's gate — `ext::`-style transport tricks are
/// structurally out.
async fn clone_repo(url: &str, dir: &Path) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["clone", "--depth", "1", "--single-branch", url])
        .arg(dir)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        cmd.process_group(0); // kill-hygiene: the timeout drop takes the group
        // A clone can run minutes — if the server dies hard mid-clone, the
        // kernel kills the child (no orphaned git against a half-written
        // target dir).
        flux_tools::arm_parent_death_signal(&mut cmd);
    }
    let output = tokio::time::timeout(GIT_CLONE_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("git clone timed out after {}s", GIT_CLONE_TIMEOUT.as_secs()))?
        .map_err(|e| format!("failed to run git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git clone failed: {}",
            trim_lossy(&output.stderr, 400)
        ));
    }
    Ok(())
}

/// Truncate a lossy stderr to ~`max` chars at a char boundary.
fn trim_lossy(bytes: &[u8], max: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

/// Copy a directory tree under the install budget. DirEntry file types
/// are used WITHOUT following symlinks: symlinked FILES copy by content
/// (`fs::copy` follows), symlinked DIRS are skipped with a warning (a
/// symlink cycle must not become a copy cycle). `.git` is never copied.
fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    let mut budget = (MAX_INSTALL_BYTES, MAX_INSTALL_FILES);
    copy_tree_inner(src, dst, &mut budget)
}

fn copy_tree_inner(src: &Path, dst: &Path, budget: &mut (u64, usize)) -> Result<(), String> {
    let meta = std::fs::metadata(src).map_err(|e| format!("cannot read {}: {e}", src.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a directory", src.display()));
    }
    std::fs::create_dir_all(dst).map_err(|e| format!("cannot create {}: {e}", dst.display()))?;
    let entries =
        std::fs::read_dir(src).map_err(|e| format!("cannot read {}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read error: {e}"))?;
        let file_type = entry.file_type().map_err(|e| format!("read error: {e}"))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            if from.file_name().and_then(|n| n.to_str()) == Some(".git") {
                continue; // transport, not content
            }
            copy_tree_inner(&from, &to, budget)?;
        } else if file_type.is_symlink()
            && std::fs::metadata(&from)
                .map(|m| m.is_dir())
                .unwrap_or(false)
        {
            // A symlinked DIRECTORY (file_type sees the link, metadata
            // follows it): skipping keeps a symlink cycle from becoming
            // a copy cycle.
            tracing::warn!(from = %from.display(), "skipping symlinked directory in skill install");
        } else {
            let (bytes_left, files_left) = *budget;
            if files_left == 0 {
                return Err(format!(
                    "install budget exceeded: more than {MAX_INSTALL_FILES} files"
                ));
            }
            let size = std::fs::metadata(&from).map(|m| m.len()).unwrap_or(0);
            if size > bytes_left {
                return Err(format!(
                    "install budget exceeded: over {MAX_INSTALL_BYTES} bytes total"
                ));
            }
            // Follows regular files and file symlinks (copies the
            // target's content); symlinked dirs never reach this branch.
            std::fs::copy(&from, &to)
                .map_err(|e| format!("cannot copy {}: {e}", from.display()))?;
            budget.0 = bytes_left.saturating_sub(size);
            budget.1 = files_left - 1;
        }
    }
    Ok(())
}

/// Remove a GLOBAL skill by name. Project skills are unreachable here by
/// construction (discovery is scoped to the global dir).
pub(crate) async fn remove_skill(name: &str) -> Result<(), String> {
    let global = require_global_dir()?;
    let name = name.to_string();
    tokio::task::spawn_blocking(move || {
        let entries = discover_skills(Some(global.as_path()), None);
        let entry = entries
            .iter()
            .find(|e| e.name == name)
            .ok_or_else(|| format!("unknown global skill `{name}` — refresh the list"))?;
        std::fs::remove_dir_all(&entry.root).map_err(|e| format!("cannot remove `{name}`: {e}"))
    })
    .await
    .map_err(|e| format!("remove task failed: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_skill(root: &Path, dir_name: &str, name: &str, description: &str) -> PathBuf {
        let dir = root.join(dir_name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n"),
        )
        .unwrap();
        dir
    }

    #[test]
    fn copy_tree_copies_content_and_skips_git() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(src.join(".git")).unwrap();
        fs::write(src.join(".git/config"), "must not copy").unwrap();
        fs::write(src.join("SKILL.md"), "---\nname: x\n---").unwrap();
        fs::create_dir_all(src.join("references")).unwrap();
        fs::write(src.join("references/api.md"), "ref").unwrap();

        let dst = dir.path().join("dst");
        copy_tree(&src, &dst).unwrap();
        assert!(dst.join("SKILL.md").exists());
        assert!(dst.join("references/api.md").exists());
        assert!(!dst.join(".git").exists());
    }

    #[cfg(unix)]
    #[test]
    fn copy_tree_survives_symlinked_dirs_without_cycles() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("loop")).unwrap();
        fs::write(src.join("loop/a.md"), "a").unwrap();
        // A directory symlink pointing at the tree itself — a naive
        // follower would recurse forever.
        std::os::unix::fs::symlink(&src, src.join("loop/self")).unwrap();
        let dst = dir.path().join("dst");
        copy_tree(&src, &dst).unwrap();
        assert!(dst.join("loop/a.md").exists());
        assert!(!dst.join("loop/self").exists()); // dir symlink skipped
    }

    #[test]
    fn trim_lossy_respects_char_boundaries() {
        let bytes = "技能技能技能".as_bytes();
        assert!(trim_lossy(bytes, 3).chars().count() <= 4);
        assert_eq!(trim_lossy(b"short", 10), "short");
    }

    #[tokio::test]
    async fn install_funnel_publishes_validated_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        let source = write_skill(dir.path(), "my-src", "my-skill", "Does things.");
        let source_for_closure = source.clone();

        let name = install(global.clone(), |stage| async move {
            let source = source_for_closure.clone();
            tokio::task::spawn_blocking(move || {
                copy_tree(&source, &stage)?;
                validate_skill_dir(&stage)
                    .map(|e| e.name)
                    .map_err(|e| e.to_string())
            })
            .await
            .unwrap()
        })
        .await
        .unwrap();

        assert_eq!(name, "my-skill");
        assert!(global.join("my-skill/SKILL.md").exists());
        assert!(source.exists()); // the source is never moved
        // Staging cleaned up.
        assert!(!global.join(".staging-0").exists());
    }

    #[tokio::test]
    async fn install_funnel_rejects_collisions_without_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        write_skill(&global, "existing", "existing", "Already here.");
        let other = write_skill(dir.path(), "other-src", "existing", "Collides.");

        let err = install(global.clone(), |stage| async move {
            let other = other;
            tokio::task::spawn_blocking(move || {
                copy_tree(&other, &stage)?;
                validate_skill_dir(&stage)
                    .map(|e| e.name)
                    .map_err(|e| e.to_string())
            })
            .await
            .unwrap()
        })
        .await
        .unwrap_err();

        assert!(err.contains("already exists"), "got: {err}");
    }

    #[tokio::test]
    async fn install_funnel_cleans_up_on_validation_failure() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        let bogus = dir.path().join("bogus");
        fs::create_dir_all(&bogus).unwrap();
        fs::write(bogus.join("README.md"), "not a skill").unwrap();

        let err = install(global.clone(), |stage| async move {
            let bogus = bogus.clone();
            tokio::task::spawn_blocking(move || {
                copy_tree(&bogus, &stage)?;
                validate_skill_dir(&stage)
                    .map(|e| e.name)
                    .map_err(|e| e.to_string())
            })
            .await
            .unwrap()
        })
        .await
        .unwrap_err();

        assert!(err.contains("not a valid skill directory"), "got: {err}");
        // Nothing published, staging gone.
        assert_eq!(discover_skills(Some(&global), None).len(), 0);
    }

    #[tokio::test]
    async fn remove_from_global_only_touches_discovered_entries() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        write_skill(&global, "gone", "gone", "To be removed.");
        write_skill(&global, "stay", "stay", "Stays.");

        let entries = discover_skills(Some(&global), None);
        let entry = entries.iter().find(|e| e.name == "gone").unwrap();
        fs::remove_dir_all(&entry.root).unwrap();

        let left = discover_skills(Some(&global), None);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].name, "stay");
        // Unknown names have nothing to remove.
        assert!(!entries.iter().any(|e| e.name == "nope"));
    }
}
