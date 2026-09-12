//! Scaffold orchestration: project basic content + feature decision log.
//!
//! The scaffold is the model's context at the start of every feature. It
//! is rebuilt from the WORKSPACE (source of truth), never from the
//! conversation transcript — archived history is deliberately absent so
//! it cannot interfere with the current feature.
//!
//! This module is a pure data-producer: `build_scaffold_text` assembles
//! the project context (built on the [`super::blocks`] compute engines).
//! No conversation-kind or driver logic lives here — the caller (the
//! `feature_done` tool, flux-chat) feeds the result to the restarter.

use flux_core::CoreError;
use flux_store::Store;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::blocks::{BuildContext, assemble, default_blocks};
use super::project_config::ProjectContext;

/// How project-level `include` lists merge over the server defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IncludeMode {
    /// Project files are appended after the server defaults (deduplicated).
    #[default]
    Append,
    /// Project files fully replace the server defaults.
    Replace,
}

/// Directory-tree limits (scaffold element).
#[derive(Debug, Clone)]
pub struct TreeLimits {
    pub enabled: bool,
    pub max_depth: usize,
    pub max_entries: usize,
}

/// Git status/diff limits (scaffold element).
#[derive(Debug, Clone)]
pub struct GitLimits {
    pub enabled: bool,
    pub max_lines: usize,
}

/// Convention-file limits (scaffold element).
#[derive(Debug, Clone)]
pub struct FileLimits {
    pub enabled: bool,
    pub max_chars: usize,
}

/// Project-profile analysis limits (scaffold element).
#[derive(Debug, Clone)]
pub struct AnalysisLimits {
    pub enabled: bool,
    /// Analyzer kinds to run (`cargo`/`node`/`python`/`go`/…); empty = all.
    pub kinds: Vec<String>,
}

/// Feature-decision-log limits (scaffold element).
#[derive(Debug, Clone)]
pub struct DecisionLimits {
    pub enabled: bool,
    pub count: i64,
}

/// Orchestration knobs. The server supplies the conservative built-in
/// defaults (there is no server config file); the project
/// (`<workdir>/.flux/config.toml`) overrides them with finer-grained
/// control (per-element toggles, budgets, preamble, feature-end tool name).
#[derive(Debug, Clone)]
pub struct ScaffoldConfig {
    /// How project `include` lists merge over the server defaults.
    pub include_mode: IncludeMode,
    /// Convention files read into the scaffold (relative to workdir).
    pub include: Vec<String>,
    /// Top-level directories excluded from the tree.
    pub exclude: Vec<String>,
    /// Directory-tree element.
    pub tree: TreeLimits,
    /// Git status/diff element.
    pub git: GitLimits,
    /// Convention-file element.
    pub files: FileLimits,
    /// Project-profile analysis element.
    pub analysis: AnalysisLimits,
    /// Feature-decision-log element.
    pub decisions: DecisionLimits,
    /// Optional context budget (approximate tokens). When set, scaffold
    /// elements are included by priority until the budget is consumed
    /// (preamble + project profile always fit; the rest are trimmed
    /// low-priority-first: decisions → tree → git → files).
    pub max_tokens: Option<usize>,
    /// Inline project engineering conventions (first scaffold message).
    pub preamble: Option<String>,
    /// Convention file whose content is injected after `preamble`.
    pub preamble_file: Option<String>,
    /// Project subdirectories to analyze (relative to workdir). Default
    /// `["."]` — the workdir itself. A monorepo with several projects
    /// lists them here; each project is judged and collected
    /// independently.
    pub projects: Vec<String>,
}

impl Default for ScaffoldConfig {
    fn default() -> Self {
        Self {
            include_mode: IncludeMode::Append,
            include: vec!["README.md".into(), "AGENTS.md".into(), "CLAUDE.md".into()],
            exclude: vec![
                "target".into(),
                "node_modules".into(),
                ".git".into(),
                "dist".into(),
                "build".into(),
                ".flux".into(),
            ],
            tree: TreeLimits {
                enabled: true,
                max_depth: 3,
                max_entries: 200,
            },
            git: GitLimits {
                enabled: true,
                max_lines: 200,
            },
            files: FileLimits {
                enabled: true,
                max_chars: 8_000,
            },
            analysis: AnalysisLimits {
                enabled: true,
                kinds: Vec::new(),
            },
            decisions: DecisionLimits {
                enabled: true,
                count: 5,
            },
            max_tokens: None,
            preamble: None,
            preamble_file: None,
            projects: vec![".".into()],
        }
    }
}

/// Build the orchestrated project context for a feature boundary — the
/// model's starting prompt for the next feature. Pure data production over
/// the [`super::blocks`] compute engines:
///
/// 1. the project-level config (`<workdir>/.flux/config.toml`) overrides
///    the server defaults (reloaded each call);
/// 2. the default information blocks assemble, order stable-first (prefix
///    cache), and trim to the token budget.
///
/// The result is the feature_done tool's orchestration text: the kernel
/// re-injects it as the next feature's first user message. No driver or
/// conversation-kind logic here.
pub async fn build_scaffold_text(
    store: Arc<Store>,
    chat_id: &str,
    workdir: &str,
    config: &ScaffoldConfig,
) -> Result<String, CoreError> {
    let proj = ProjectContext::load(workdir, config);
    let ctx = BuildContext {
        workdir,
        chat_id,
        store: &store,
        config: &proj.config,
    };
    let msgs = assemble(&default_blocks(), &ctx).await?;
    Ok(msgs
        .into_iter()
        .map(|m| m.content)
        .collect::<Vec<_>>()
        .join("\n\n"))
}
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Orchestrate once over a temp workdir + in-memory store.
    async fn build(workdir: &str, config: ScaffoldConfig) -> String {
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        build_scaffold_text(store, "c1", workdir, &config)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn disabled_elements_are_omitted() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "readme content").unwrap();
        let mut cfg = ScaffoldConfig::default();
        cfg.tree.enabled = false;
        cfg.git.enabled = false;
        cfg.decisions.enabled = false;
        let all = build(dir.path().to_str().unwrap(), cfg).await;
        assert!(all.contains("Project overview"), "analysis stays");
        assert!(all.contains("readme content"), "convention files stay");
        assert!(!all.contains("Project structure"), "tree omitted");
        assert!(!all.contains("git"), "git omitted");
        assert!(!all.contains("decision log"), "decisions omitted");
    }

    #[tokio::test]
    async fn budget_trims_low_priority_blocks() {
        let dir = tempdir().unwrap();
        // Big tree + big README so the budget bites.
        std::fs::create_dir_all(dir.path().join("src/a/b/c/d/e")).unwrap();
        std::fs::write(dir.path().join("README.md"), "x".repeat(10_000)).unwrap();
        let cfg = ScaffoldConfig {
            include: vec!["README.md".into()],
            max_tokens: Some(400),
            ..Default::default()
        };
        let all = build(dir.path().to_str().unwrap(), cfg).await;
        // Always-fit blocks survive the tiny budget…
        assert!(all.contains("Project overview"), "analysis always fits");
        // …the oversized convention file is trimmed (low priority), while
        // the small tree still fits inside the budget.
        assert!(
            !all.contains("Project convention file"),
            "README trimmed by budget"
        );
        assert!(
            all.contains("Project structure"),
            "small tree fits the budget"
        );
    }

    #[tokio::test]
    async fn replace_mode_drops_server_include() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "readme").unwrap();
        std::fs::write(dir.path().join("OWNERS.md"), "owners").unwrap();
        let cfg = ScaffoldConfig {
            include_mode: IncludeMode::Replace,
            include: vec!["OWNERS.md".into()],
            ..Default::default()
        };
        let all = build(dir.path().to_str().unwrap(), cfg).await;
        assert!(all.contains("OWNERS.md"));
        assert!(
            !all.contains("Project convention file `README.md`"),
            "replaced, not appended"
        );
    }

    #[test]
    fn stability_sort_puts_stable_content_first() {
        use crate::blocks::Stability;
        let mut pieces = [
            (Stability::Volatile, "git status"),
            (Stability::Measured(Some(100)), "old convention file"),
            (Stability::Fixed, "preamble"),
            (Stability::Measured(Some(200)), "recently changed file"),
            (Stability::Volatile, "decisions"),
        ];
        pieces.sort_by_key(|(s, _)| crate::blocks::stability_key(*s));
        let order: Vec<&str> = pieces.iter().map(|(_, t)| *t).collect();
        assert_eq!(
            order,
            vec![
                "preamble",
                "old convention file",
                "recently changed file",
                "git status",
                "decisions",
            ]
        );
    }

    #[test]
    fn unmeasured_content_ranks_after_measured() {
        use crate::blocks::Stability;
        let mut pieces = [
            (Stability::Measured(None), "unknown"),
            (Stability::Measured(Some(50)), "measured"),
            (Stability::Volatile, "volatile"),
        ];
        pieces.sort_by_key(|(s, _)| crate::blocks::stability_key(*s));
        let order: Vec<&str> = pieces.iter().map(|(_, t)| *t).collect();
        assert_eq!(order, vec!["measured", "unknown", "volatile"]);
    }
}
