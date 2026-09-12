//! # flux-context — feature-mode context orchestration.
//!
//! The "code engineering mode" layer: what the model sees at the start of
//! every feature is a **project scaffold** — project profile, tree, git
//! status, convention files, last-feature decisions — with NO conversation
//! history. This crate owns:
//!
//! - [`scaffold`] — the entry point: [`scaffold::build_scaffold_text`] is a
//!   pure function (store + workdir + config → scaffold text) with no
//!   driver or trait coupling; the caller (the `feature_done` tool) owns
//!   what happens with the result;
//! - [`detector`] — project judgment: the [`ProjectDetector`] trait, one
//!   implementation per language/build system (cargo, node, python, go,
//!   generic);
//! - [`collector`] — information gathering: the [`InfoCollector`] trait,
//!   one implementation per data source (tree, git status, convention
//!   file, decision log);
//! - [`blocks`] — the orchestration layer: [`InfoBlock`]s (a collector +
//!   budget priority) and the stable-first assembler;
//! - [`git`] — git helpers: status, history, change-frequency measurement.
//!
//! The `detector`/`collector` modules are the growth points for the
//! information-collection system (multi-language support, build systems,
//! CI…) and may split into their own crate (`flux-project`) once they
//! outgrow this layout.

pub mod blocks;
pub mod collector;
pub mod detector;
pub mod git;
#[cfg(test)]
mod probe_test;
pub mod project_config;
pub mod scaffold;

pub use blocks::{
    BlockContent, BuildContext, InfoBlock, Stability, content_stability, path_stability,
};
pub use collector::InfoCollector;
pub use detector::{ProjectDetector, ProjectProfile, analyze_project};
pub use git::last_change_ts;
pub use project_config::{
    AnalysisOverrides, DecisionsOverrides, FilesOverrides, GitOverrides, ProjectConfig,
    ProjectContext, ProjectContextConfig, TreeOverrides,
};
pub use scaffold::{
    AnalysisLimits, DecisionLimits, FileLimits, GitLimits, IncludeMode, ScaffoldConfig, TreeLimits,
    build_scaffold_text,
};
