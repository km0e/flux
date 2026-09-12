//! Project-level context config — `<workdir>/.flux/config.toml`.
//!
//! The `.flux/` directory lives in the project root and holds project
//! configuration that travels with the codebase (the server itself has no
//! config file — CLI flags + the DB; `.flux/` is the project's own voice).
//! Today it carries the context-orchestration overrides — FINER-GRAINED
//! than the server layer: per-element toggles, a context budget, preamble
//! (inline + file), and the feature-end tool name. Future project content
//! (conventions, decisions) will live alongside it.

use crate::scaffold::{IncludeMode, ScaffoldConfig};
use serde::Deserialize;
use std::path::Path;
use tracing::warn;

/// Path of the project-level config, relative to the chat workdir.
pub const PROJECT_CONFIG_PATH: &str = ".flux/config.toml";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
    pub context: ProjectContextConfig,
}

/// The `[context]` section of `.flux/config.toml` — the project's detailed
/// orchestration overrides. Scalars replace the server default; list
/// fields append (or replace, per `include_mode`); element sub-configs
/// override their individual fields. Every field is optional: absent =
/// keep the server default.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProjectContextConfig {
    /// `append` (default) | `replace` — how `include` merges.
    pub include_mode: Option<IncludeMode>,
    /// Convention files (relative to workdir). Appended or replaced per
    /// `include_mode`.
    pub include: Vec<String>,
    /// Extra tree exclusions (appended after the server defaults).
    pub exclude: Vec<String>,
    /// Directory-tree element overrides.
    pub tree: Option<TreeOverrides>,
    /// Git status/diff element overrides.
    pub git: Option<GitOverrides>,
    /// Convention-file element overrides.
    pub files: Option<FilesOverrides>,
    /// Project-profile analysis element overrides.
    pub analysis: Option<AnalysisOverrides>,
    /// Feature-decision-log element overrides.
    pub decisions: Option<DecisionsOverrides>,
    /// Optional context budget in approximate tokens. When set, the
    /// scaffold includes high-priority blocks first and trims
    /// low-priority ones (decisions → tree → git → files) to fit.
    pub max_tokens: Option<usize>,
    /// Inline project engineering conventions.
    pub preamble: Option<String>,
    /// Convention file injected after the inline preamble.
    pub preamble_file: Option<String>,
    /// Project subdirectories to analyze (relative to workdir). Default
    /// `["."]` — the workdir itself. In a multi-project workspace this
    /// lists each project root; every entry is judged (detectors) and
    /// collected (tree, profile) independently.
    pub projects: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TreeOverrides {
    pub enabled: Option<bool>,
    pub max_depth: Option<usize>,
    pub max_entries: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct GitOverrides {
    pub enabled: Option<bool>,
    pub max_lines: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FilesOverrides {
    pub enabled: Option<bool>,
    pub max_chars: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AnalysisOverrides {
    pub enabled: Option<bool>,
    pub kinds: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DecisionsOverrides {
    pub enabled: Option<bool>,
    pub count: Option<i64>,
}

/// Loaded project context: the merged config plus the project preamble.
#[derive(Debug, Clone)]
pub struct ProjectContext {
    pub config: ScaffoldConfig,
}

impl ProjectContext {
    /// Read `<workdir>/.flux/config.toml` and merge over `base`.
    ///
    /// - Absent file → `base` untouched.
    /// - Parse failure → warn + `base` (a broken project config must not
    ///   block conversations; the server-side fail-fast rule is about
    ///   auth/safety, this is orchestration quality).
    pub fn load(workdir: &str, base: &ScaffoldConfig) -> Self {
        let path = Path::new(workdir).join(PROJECT_CONFIG_PATH);
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(_) => {
                return Self {
                    config: base.clone(),
                };
            }
        };
        let parsed: ProjectConfig = match toml::from_str(&raw) {
            Ok(p) => p,
            Err(e) => {
                warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to parse project config; using server defaults"
                );
                return Self {
                    config: base.clone(),
                };
            }
        };
        Self {
            config: merge(base, &parsed.context),
        }
    }
}

/// Merge the project overrides onto the server defaults.
fn merge(base: &ScaffoldConfig, pc: &ProjectContextConfig) -> ScaffoldConfig {
    let mut c = base.clone();

    if let Some(m) = pc.include_mode {
        c.include_mode = m;
    }
    match c.include_mode {
        IncludeMode::Replace => c.include = pc.include.clone(),
        IncludeMode::Append => {
            for f in &pc.include {
                if !c.include.contains(f) {
                    c.include.push(f.clone());
                }
            }
        }
    }
    for e in &pc.exclude {
        if !c.exclude.contains(e) {
            c.exclude.push(e.clone());
        }
    }

    if let Some(t) = &pc.tree {
        if let Some(v) = t.enabled {
            c.tree.enabled = v;
        }
        if let Some(v) = t.max_depth {
            c.tree.max_depth = v;
        }
        if let Some(v) = t.max_entries {
            c.tree.max_entries = v;
        }
    }
    if let Some(g) = &pc.git {
        if let Some(v) = g.enabled {
            c.git.enabled = v;
        }
        if let Some(v) = g.max_lines {
            c.git.max_lines = v;
        }
    }
    if let Some(f) = &pc.files {
        if let Some(v) = f.enabled {
            c.files.enabled = v;
        }
        if let Some(v) = f.max_chars {
            c.files.max_chars = v;
        }
    }
    if let Some(a) = &pc.analysis {
        if let Some(v) = a.enabled {
            c.analysis.enabled = v;
        }
        if let Some(v) = &a.kinds {
            c.analysis.kinds = v.clone();
        }
    }
    if let Some(d) = &pc.decisions {
        if let Some(v) = d.enabled {
            c.decisions.enabled = v;
        }
        if let Some(v) = d.count {
            c.decisions.count = v;
        }
    }

    if let Some(v) = pc.max_tokens {
        c.max_tokens = Some(v);
    }
    if let Some(p) = &pc.preamble {
        c.preamble = Some(p.clone());
    }
    if let Some(f) = &pc.preamble_file {
        c.preamble_file = Some(f.clone());
    }
    if let Some(p) = &pc.projects {
        if p.is_empty() {
            c.projects = vec![".".into()];
        } else {
            c.projects = p.clone();
        }
    }

    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_project(dir: &tempfile::TempDir, toml: &str) {
        let flux = dir.path().join(".flux");
        fs::create_dir_all(&flux).unwrap();
        fs::write(flux.join("config.toml"), toml).unwrap();
    }

    #[test]
    fn absent_config_keeps_base() {
        let dir = tempdir().unwrap();
        let base = ScaffoldConfig::default();
        let ctx = ProjectContext::load(dir.path().to_str().unwrap(), &base);
        assert_eq!(ctx.config.include, base.include);
        assert!(ctx.config.preamble.is_none());
    }

    #[test]
    fn append_mode_dedups_and_scalars_replace() {
        let dir = tempdir().unwrap();
        write_project(
            &dir,
            r#"
[context]
include = ["docs/CONTRIBUTING.md"]
exclude = ["vendor"]
max_tokens = 2000
preamble = "Use TDD."
"#,
        );
        let base = ScaffoldConfig::default();
        let ctx = ProjectContext::load(dir.path().to_str().unwrap(), &base);
        // append: merge + dedupe
        assert!(ctx.config.include.contains(&"docs/CONTRIBUTING.md".into()));
        assert!(ctx.config.include.contains(&"README.md".into()));
        // exclude: merge + dedupe
        assert!(ctx.config.exclude.contains(&"vendor".into()));
        assert!(ctx.config.exclude.contains(&"target".into()));
        assert_eq!(ctx.config.max_tokens, Some(2000));
        assert_eq!(ctx.config.preamble.as_deref(), Some("Use TDD."));
    }

    #[test]
    fn replace_mode_replaces_include() {
        let dir = tempdir().unwrap();
        write_project(
            &dir,
            r#"
[context]
include_mode = "replace"
include = ["docs/CONTRIBUTING.md"]
"#,
        );
        let base = ScaffoldConfig::default();
        let ctx = ProjectContext::load(dir.path().to_str().unwrap(), &base);
        assert_eq!(ctx.config.include, vec!["docs/CONTRIBUTING.md".to_string()]);
        // other fields untouched
        assert_eq!(ctx.config.tree.max_depth, base.tree.max_depth);
    }

    #[test]
    fn element_overrides_apply_per_field() {
        let dir = tempdir().unwrap();
        write_project(
            &dir,
            r#"
[context]
tree = { enabled = false }
git = { max_lines = 50 }
files = { max_chars = 4000 }
analysis = { enabled = true, kinds = ["cargo"] }
decisions = { count = 3 }
projects = ["crates/a", "clients/vscode"]
preamble_file = "docs/ENGINEERING.md"
"#,
        );
        let base = ScaffoldConfig::default();
        let ctx = ProjectContext::load(dir.path().to_str().unwrap(), &base);
        assert!(!ctx.config.tree.enabled);
        assert_eq!(ctx.config.git.max_lines, 50);
        assert_eq!(ctx.config.files.max_chars, 4000);
        assert_eq!(ctx.config.analysis.kinds, vec!["cargo".to_string()]);
        assert_eq!(ctx.config.decisions.count, 3);
        assert_eq!(
            ctx.config.preamble_file.as_deref(),
            Some("docs/ENGINEERING.md")
        );
        // uncovered fields keep the server defaults
        assert_eq!(ctx.config.tree.max_depth, base.tree.max_depth);
        assert!(ctx.config.git.enabled);
    }

    #[test]
    fn empty_projects_falls_back_to_workdir() {
        let dir = tempdir().unwrap();
        write_project(&dir, "[context]\nprojects = []\n");
        let base = ScaffoldConfig::default();
        let ctx = ProjectContext::load(dir.path().to_str().unwrap(), &base);
        assert_eq!(ctx.config.projects, vec![".".to_string()]);
    }

    #[test]
    fn broken_config_falls_back_to_base() {
        let dir = tempdir().unwrap();
        write_project(&dir, "[context\nnope");
        let base = ScaffoldConfig::default();
        let ctx = ProjectContext::load(dir.path().to_str().unwrap(), &base);
        assert_eq!(ctx.config.tree.max_depth, base.tree.max_depth);
        assert!(ctx.config.preamble.is_none());
    }
}
