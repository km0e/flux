//! Project detection — "what kind of project is this" as a pluggable trait.
//!
//! Each [`ProjectDetector`] recognizes one project kind (by marker files)
//! and describes it (kind label, build/test commands, entry points,
//! workspace members). Detectors are tried by priority; the first match
//! wins. Adding a language = implementing one struct and registering it.

use crate::git::{git_branch, git_last_commit_ts};
use std::path::Path;

/// What kind of project this is, how it is built and tested — the
/// structural facts the model needs to work in the workspace.
#[derive(Debug, Default, Clone)]
pub struct ProjectProfile {
    /// "rust (cargo)" / "node (npm)" / "python (pyproject)" / "go" / "unknown".
    pub kind: String,
    /// Suggested test/build commands (best-effort, deduplicated).
    pub commands: Vec<String>,
    /// Entry points the model may want to read first.
    pub entry_points: Vec<String>,
    /// Cargo workspace members (best-effort parse of `[workspace] members`).
    pub workspace: Vec<String>,
    pub has_git: bool,
    pub branch: Option<String>,
    /// Last commit touching the workspace (seconds) — the tree block's
    /// stability proxy.
    pub last_change: Option<i64>,
}

impl ProjectProfile {
    /// One-line rendering for the scaffold message.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("Type: {}\n", self.kind));
        if !self.commands.is_empty() {
            out.push_str(&format!("Common commands: {}\n", self.commands.join(", ")));
        }
        if !self.entry_points.is_empty() {
            out.push_str(&format!(
                "Entry/key files: {}\n",
                self.entry_points.join(", ")
            ));
        }
        if !self.workspace.is_empty() {
            out.push_str(&format!(
                "Workspace members: {}\n",
                self.workspace.join(", ")
            ));
        }
        if let Some(b) = &self.branch {
            out.push_str(&format!("Git branch: {b}\n"));
        }
        out.trim_end().to_string()
    }
}

/// A project-kind detector: recognizes a marker and produces the profile.
pub trait ProjectDetector: Send + Sync {
    /// Stable id ("cargo", "node", "python", "go", "generic").
    fn id(&self) -> &str;

    /// Detection priority — higher wins when several markers coexist
    /// (e.g. a repo with both package.json and pyproject.toml). The
    /// generic fallback must have the lowest priority.
    fn priority(&self) -> u8 {
        50
    }

    /// Whether this detector matches the workspace.
    fn detect(&self, workdir: &Path) -> bool;

    /// The profile for a matching workspace.
    fn describe(&self, workdir: &Path) -> ProjectProfile;
}

/// Rust / Cargo.
pub struct CargoDetector;

impl ProjectDetector for CargoDetector {
    fn id(&self) -> &str {
        "cargo"
    }
    fn priority(&self) -> u8 {
        100
    }
    fn detect(&self, workdir: &Path) -> bool {
        workdir.join("Cargo.toml").exists()
    }
    fn describe(&self, workdir: &Path) -> ProjectProfile {
        let mut p = ProjectProfile {
            kind: "rust (cargo)".into(),
            ..Default::default()
        };
        p.commands.push("cargo test".into());
        p.commands.push("cargo build".into());
        if let Ok(text) = std::fs::read_to_string(workdir.join("Cargo.toml")) {
            p.workspace = extract_cargo_members(&text);
            if p.workspace.is_empty() && text.contains("[workspace]") {
                p.workspace.push("(root crate)".into());
            }
        }
        for entry in ["src/main.rs", "src/lib.rs"] {
            if workdir.join(entry).exists() {
                p.entry_points.push(entry.into());
            }
        }
        if workdir.join("src").is_dir() {
            p.entry_points.push("src/".into());
        }
        p
    }
}

/// Node / npm.
pub struct NodeDetector;

impl ProjectDetector for NodeDetector {
    fn id(&self) -> &str {
        "node"
    }
    fn priority(&self) -> u8 {
        90
    }
    fn detect(&self, workdir: &Path) -> bool {
        workdir.join("package.json").exists()
    }
    fn describe(&self, workdir: &Path) -> ProjectProfile {
        let mut p = ProjectProfile {
            kind: "node (npm)".into(),
            ..Default::default()
        };
        p.commands.push("npm test".into());
        if let Ok(text) = std::fs::read_to_string(workdir.join("package.json"))
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
            && let Some(scripts) = v.get("scripts").and_then(|s| s.as_object())
        {
            for key in ["test", "build", "lint", "typecheck"] {
                if scripts.contains_key(key) {
                    p.commands.push(format!("npm run {key}"));
                }
            }
        }
        for entry in [
            "src/index.ts",
            "src/index.tsx",
            "src/index.js",
            "index.ts",
            "index.js",
        ] {
            if workdir.join(entry).exists() {
                p.entry_points.push(entry.into());
            }
        }
        p
    }
}

/// Python.
pub struct PythonDetector;

impl ProjectDetector for PythonDetector {
    fn id(&self) -> &str {
        "python"
    }
    fn priority(&self) -> u8 {
        80
    }
    fn detect(&self, workdir: &Path) -> bool {
        workdir.join("pyproject.toml").exists() || workdir.join("requirements.txt").exists()
    }
    fn describe(&self, workdir: &Path) -> ProjectProfile {
        let mut p = ProjectProfile {
            kind: "python".into(),
            ..Default::default()
        };
        p.commands.push("pytest".into());
        for entry in ["main.py", "app.py", "src/"] {
            if workdir.join(entry).exists() {
                p.entry_points.push(entry.into());
            }
        }
        p
    }
}

/// Go.
pub struct GoDetector;

impl ProjectDetector for GoDetector {
    fn id(&self) -> &str {
        "go"
    }
    fn priority(&self) -> u8 {
        70
    }
    fn detect(&self, workdir: &Path) -> bool {
        workdir.join("go.mod").exists()
    }
    fn describe(&self, workdir: &Path) -> ProjectProfile {
        let mut p = ProjectProfile {
            kind: "go".into(),
            ..Default::default()
        };
        p.commands.push("go test ./...".into());
        if workdir.join("cmd/").exists() {
            p.entry_points.push("cmd/".into());
        }
        p
    }
}

/// Generic fallback — matches everything (lowest priority), contributes
/// common directory entry points only.
pub struct GenericDetector;

impl ProjectDetector for GenericDetector {
    fn id(&self) -> &str {
        "generic"
    }
    fn priority(&self) -> u8 {
        0
    }
    fn detect(&self, _workdir: &Path) -> bool {
        true
    }
    fn describe(&self, workdir: &Path) -> ProjectProfile {
        let mut p = ProjectProfile {
            kind: "unknown".into(),
            ..Default::default()
        };
        for entry in ["src/", "lib/", "tests/", "docs/"] {
            if workdir.join(entry).exists() {
                p.entry_points.push(entry.into());
            }
        }
        p
    }
}

/// The default detector set — the built-in project judges. Extensible:
/// register additional detectors to recognize more languages/build systems.
pub fn default_detectors() -> Vec<Box<dyn ProjectDetector>> {
    vec![
        Box::new(CargoDetector),
        Box::new(NodeDetector),
        Box::new(PythonDetector),
        Box::new(GoDetector),
        Box::new(GenericDetector),
    ]
}

/// Analyze the workspace: run the detectors by priority, take the first
/// match, and fill in the git facts shared by every project kind.
pub fn analyze_project(workdir: &str) -> ProjectProfile {
    let path = Path::new(workdir);
    let mut detectors = default_detectors();
    // Highest priority first — the most specific detector wins (generic
    // falls back last).
    detectors.sort_by_key(|d| std::cmp::Reverse(d.priority()));
    let mut profile = detectors
        .iter()
        .find(|d| d.detect(path))
        .map(|d| d.describe(path))
        .unwrap_or_else(|| ProjectProfile {
            kind: "unknown".into(),
            ..Default::default()
        });
    fill_git_facts(path, &mut profile);
    profile
}

/// Shared git facts: repo presence, current branch, last commit time.
fn fill_git_facts(workdir: &Path, profile: &mut ProjectProfile) {
    if !workdir.join(".git").exists() {
        return;
    }
    profile.has_git = true;
    profile.branch = git_branch(workdir.to_str().unwrap_or(""));
    profile.last_change = git_last_commit_ts(workdir.to_str().unwrap_or(""), ".");
}

/// Best-effort parse of `[workspace] members = [...]` from a Cargo.toml.
fn extract_cargo_members(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_workspace = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("[workspace]") {
            in_workspace = true;
            continue;
        }
        if !in_workspace {
            continue;
        }
        if t.starts_with('[') {
            break; // next section ends the workspace block
        }
        let trimmed = t
            .strip_prefix("members")
            .or_else(|| t.strip_prefix("default-members"))
            .map(|s| s.trim())
            .unwrap_or("");
        let trimmed = trimmed.trim_start_matches('=').trim();
        if trimmed.is_empty() {
            continue;
        }
        // strip the enclosing [ ... ] of an inline array
        let inner = trimmed
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(trimmed);
        if !inner.is_empty() {
            for part in inner.split(',') {
                let part = part.trim().trim_matches(['"', '\'']);
                if !part.is_empty() {
                    out.push(part.to_string());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn detects_cargo_project_with_workspace() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n\n[package]\nname = \"x\"\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        let profile = analyze_project(dir.path().to_str().unwrap());
        assert_eq!(profile.kind, "rust (cargo)");
        assert!(profile.commands.contains(&"cargo test".into()));
        assert!(profile.workspace.contains(&"crates/a".into()));
        assert!(profile.entry_points.contains(&"src/".into()));
    }

    #[test]
    fn detects_node_project_scripts() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"test":"vitest","build":"esbuild"}}"#,
        )
        .unwrap();
        let profile = analyze_project(dir.path().to_str().unwrap());
        assert_eq!(profile.kind, "node (npm)");
        assert!(profile.commands.contains(&"npm run test".into()));
        assert!(profile.commands.contains(&"npm run build".into()));
    }

    #[test]
    fn priority_picks_most_specific_detector() {
        // A repo with both markers: cargo wins over generic.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        fs::write(dir.path().join("src"), "").unwrap();
        let profile = analyze_project(dir.path().to_str().unwrap());
        assert_eq!(profile.kind, "rust (cargo)");
    }

    #[test]
    fn unknown_project_is_unknown() {
        let dir = tempdir().unwrap();
        let profile = analyze_project(dir.path().to_str().unwrap());
        assert_eq!(profile.kind, "unknown");
        assert!(!profile.has_git);
    }

    #[test]
    fn python_project_detected() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("requirements.txt"), "requests\n").unwrap();
        let profile = analyze_project(dir.path().to_str().unwrap());
        assert_eq!(profile.kind, "python");
        assert!(profile.commands.contains(&"pytest".into()));
    }
}
