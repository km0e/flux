//! Information collectors — "how to gather this piece of information" as
//! a pluggable trait.
//!
//! A collector reads ONE data source (directory tree, git status, a
//! convention file, the decision log…) and produces the formatted content
//! with its measured stability. Blocks ([`crate::blocks::InfoBlock`]) are
//! collector-composing orchestrators: a block may reuse collectors
//! directly or chain several. Adding a new information source = one
//! struct.

use crate::blocks::{BlockContent, BuildContext, Stability, path_stability};
use crate::git::git_status;
use crate::scaffold::ScaffoldConfig;
use async_trait::async_trait;
use flux_core::CoreError;
use std::path::Path;

/// A collector of one information source.
#[async_trait]
pub trait InfoCollector: Send + Sync {
    /// Stable id — used for config toggles, logging, and hash keys.
    fn id(&self) -> &str;

    /// Collect 0..N content pieces. `Ok(vec![])` = source absent or
    /// disabled.
    async fn collect(&self, ctx: &BuildContext<'_>) -> Result<Vec<BlockContent>, CoreError>;
}

/// Directory tree — the structural map.
pub struct TreeCollector;

#[async_trait]
impl InfoCollector for TreeCollector {
    fn id(&self) -> &str {
        "tree"
    }
    async fn collect(&self, ctx: &BuildContext<'_>) -> Result<Vec<BlockContent>, CoreError> {
        let cfg = ctx.config;
        if !cfg.tree.enabled {
            return Ok(Vec::new());
        }
        let projects = &cfg.projects;
        if projects.len() == 1 && projects[0] == "." {
            // Single-project (default) keeps the classic one-section shape.
            let mut tree = String::new();
            walk_tree(Path::new(ctx.workdir), 0, cfg, &mut tree, &mut 0usize);
            return Ok(vec![BlockContent {
                text: format!(
                    "# Current project context\n\nWorkdir: `{}`\n\n## Project structure (top {} levels, cap {} entries):\n```\n{}\n```",
                    ctx.workdir,
                    cfg.tree.max_depth,
                    cfg.tree.max_entries,
                    tree.trim_end()
                ),
                stability: path_stability(ctx, "."),
            }]);
        }
        // Multi-project workspace: one independent content piece per
        // configured root, each measured by its own path history — the
        // stable projects' trees rank before the churning ones.
        let mut out = Vec::new();
        for proj in projects {
            let mut tree = String::new();
            walk_tree(
                &Path::new(ctx.workdir).join(proj),
                0,
                cfg,
                &mut tree,
                &mut 0usize,
            );
            if tree.trim().is_empty() {
                continue;
            }
            out.push(BlockContent {
                text: format!(
                    "# Project structure: `{proj}`\n```\n{}\n```",
                    tree.trim_end()
                ),
                stability: path_stability(ctx, proj),
            });
        }
        Ok(out)
    }
}

/// Git status/diff — the live workspace state. Volatile.
pub struct GitStatusCollector;

#[async_trait]
impl InfoCollector for GitStatusCollector {
    fn id(&self) -> &str {
        "git"
    }
    async fn collect(&self, ctx: &BuildContext<'_>) -> Result<Vec<BlockContent>, CoreError> {
        if !ctx.config.git.enabled {
            return Ok(Vec::new());
        }
        let git = git_status(ctx.workdir, ctx.config.git.max_lines).await;
        if git.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![BlockContent {
            text: format!("## Workspace status (git)\n```\n{}\n```", git.trim_end()),
            stability: Stability::Volatile,
        }])
    }
}

/// One convention file — measured by its own path history.
pub struct FileCollector {
    pub file: String,
}

#[async_trait]
impl InfoCollector for FileCollector {
    fn id(&self) -> &str {
        "file"
    }
    async fn collect(&self, ctx: &BuildContext<'_>) -> Result<Vec<BlockContent>, CoreError> {
        if !ctx.config.files.enabled {
            return Ok(Vec::new());
        }
        let path = Path::new(ctx.workdir).join(&self.file);
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Ok(Vec::new()); // absent convention file — fine
        };
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }
        let content: String = content.chars().take(ctx.config.files.max_chars).collect();
        Ok(vec![BlockContent {
            text: format!(
                "# Project convention file `{}`\n```\n{}\n```",
                self.file,
                content.trim_end()
            ),
            stability: path_stability(ctx, &self.file),
        }])
    }
}

/// Last-feature decisions. Volatile.
pub struct DecisionLogCollector;

#[async_trait]
impl InfoCollector for DecisionLogCollector {
    fn id(&self) -> &str {
        "decisions"
    }
    async fn collect(&self, ctx: &BuildContext<'_>) -> Result<Vec<BlockContent>, CoreError> {
        let cfg = ctx.config;
        if !cfg.decisions.enabled {
            return Ok(Vec::new());
        }
        let decisions = ctx
            .store
            .list_feature_logs(ctx.chat_id, cfg.decisions.count)
            .await
            .map_err(|e| CoreError::Internal(format!("feature log read failed: {e}")))?;
        if decisions.is_empty() {
            return Ok(Vec::new());
        }
        let body = decisions
            .iter()
            .map(|d| format!("- ({}) {}", d.created_at, d.summary))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(vec![BlockContent {
            text: format!("## Recently completed features (decision log)\n{body}"),
            stability: Stability::Volatile,
        }])
    }
}

/// Recursive, capped, sorted directory tree renderer.
fn walk_tree(
    dir: &Path,
    depth: usize,
    config: &ScaffoldConfig,
    out: &mut String,
    count: &mut usize,
) {
    if depth > config.tree.max_depth || *count >= config.tree.max_entries {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<(String, bool)> = entries
        .filter_map(|e| e.ok())
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let is_dir = e.path().is_dir();
            (name, is_dir)
        })
        .collect();
    names.sort();
    for (name, is_dir) in names {
        if *count >= config.tree.max_entries {
            return;
        }
        let path = dir.join(&name);
        if is_dir && config.exclude.iter().any(|x| x == &name) {
            continue;
        }
        out.push_str(&format!(
            "{}{}{}\n",
            "  ".repeat(depth),
            name,
            if is_dir { "/" } else { "" }
        ));
        *count += 1;
        if is_dir {
            walk_tree(&path, depth + 1, config, out, count);
        }
    }
}
