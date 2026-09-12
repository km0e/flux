//! Pluggable scaffold information blocks — the orchestration layer over
//! the collectors.
//!
//! An [`InfoBlock`] is a [`InfoCollector`] (how to gather the information)
//! plus an `always` flag (budget priority). The orchestrator
//! ([`assemble`]) builds the registered blocks, orders the content
//! stable-first (prefix cache), and trims to the token budget. Adding a
//! new information type = implementing one collector (or composing
//! existing ones) and registering the block — zero orchestrator changes.
//!
//! Stability bases (see [`Stability`]): path history (git log → mtime),
//! content hash measured across builds, or constants.

use crate::collector::{
    DecisionLogCollector, FileCollector, GitStatusCollector, InfoCollector, TreeCollector,
};
use crate::detector::analyze_project;
use crate::git::last_change_ts;
use crate::scaffold::ScaffoldConfig;
use async_trait::async_trait;
use flux_core::{CoreError, Message};
use flux_store::Store;
use std::path::Path;
use std::sync::Arc;

/// What a block can touch while building.
pub struct BuildContext<'a> {
    pub workdir: &'a str,
    pub chat_id: &'a str,
    pub store: &'a Arc<Store>,
    pub config: &'a ScaffoldConfig,
}

/// One produced piece of scaffold content with its measured stability.
pub struct BlockContent {
    pub text: String,
    pub stability: Stability,
}

/// Measured prefix-stability of one piece of content.
///
/// Ordering: [`Stability::Fixed`] < [`Stability::Measured`] (oldest change
/// first; missing measurement = newest) < [`Stability::Volatile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stability {
    Fixed,
    /// Last-change timestamp (unix seconds). `None` = unmeasurable —
    /// treated as "changed just now".
    Measured(Option<i64>),
    Volatile,
}

/// A pluggable scaffold information block: the orchestration unit. It may
/// compose one or more [`InfoCollector`]s (or collect directly) and adds a
/// budget priority. `always` blocks survive budget trimming.
#[async_trait]
pub trait InfoBlock: Send + Sync {
    /// Stable identifier — config toggles, logging, hash keys.
    fn id(&self) -> &str;

    /// Whether the content must fit regardless of the token budget.
    fn always(&self) -> bool {
        false
    }

    /// Produce 0..N content pieces.
    async fn collect(&self, ctx: &BuildContext<'_>) -> Result<Vec<BlockContent>, CoreError>;
}

// ── Stability measurement helpers ───────────────────────────────────────────

/// PathHistory basis: last-change timestamp from git history, falling back
/// to the path's mtime.
pub fn path_stability(ctx: &BuildContext<'_>, path: &str) -> Stability {
    Stability::Measured(last_change_ts(ctx.workdir, path))
}

/// ContentHash basis: persist `hash:last_changed_ts` in the chat state; a
/// changed hash updates the timestamp to now (content changed since the
/// last build), an identical hash keeps the old timestamp — the measured
/// "when did this exact content last change" signal.
pub async fn content_stability(ctx: &BuildContext<'_>, id: &str, content: &str) -> Stability {
    Stability::Measured(content_hash_ts(ctx, id, content).await)
}

async fn content_hash_ts(ctx: &BuildContext<'_>, id: &str, content: &str) -> Option<i64> {
    let key = format!("scaffold_hash_{id}");
    let hash = fx_hash(content);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let state = ctx.store.load_state(ctx.chat_id).await.ok()?;
    let prev = state.get(&key).and_then(|v| {
        let (h, t) = v.split_once(':')?;
        Some((h.to_string(), t.parse::<i64>().ok()?))
    });
    match prev {
        Some((h, t)) if h == format!("{hash:016x}") => Some(t),
        _ => {
            let _ = ctx
                .store
                .save_state_entry(ctx.chat_id, &key, &format!("{hash:016x}:{now}"))
                .await;
            Some(now)
        }
    }
}

/// xxhash-style 64-bit mix (deterministic, dependency-free).
fn fx_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

// ── Built-in blocks ─────────────────────────────────────────────────────────

/// Preamble + preamble_file (project engineering conventions). Fixed.
pub struct PreambleBlock;

#[async_trait]
impl InfoBlock for PreambleBlock {
    fn id(&self) -> &str {
        "preamble"
    }
    fn always(&self) -> bool {
        true
    }
    async fn collect(&self, ctx: &BuildContext<'_>) -> Result<Vec<BlockContent>, CoreError> {
        let cfg = ctx.config;
        let mut conventions = String::new();
        if let Some(preamble) = &cfg.preamble {
            conventions.push_str(preamble);
            conventions.push('\n');
        }
        if let Some(file) = &cfg.preamble_file {
            let path = Path::new(ctx.workdir).join(file);
            if let Ok(content) = std::fs::read_to_string(&path) {
                let content: String = content.chars().take(cfg.files.max_chars).collect();
                conventions.push_str("\n```\n");
                conventions.push_str(content.trim_end());
                conventions.push_str("\n```");
            }
        }
        Ok(if conventions.trim().is_empty() {
            Vec::new()
        } else {
            vec![BlockContent {
                text: format!("# Project engineering conventions (`.flux`)\n{conventions}"),
                stability: Stability::Fixed,
            }]
        })
    }
}

/// Project profile (type / commands / entry points / git branch) — from
/// the project detectors. Fixed in the single-project shape; with a
/// multi-project workspace (`config.projects`) every root is judged
/// independently and produces its own content-hash-measured piece, so all
/// profiles compete in the same stable-first ordering.
pub struct ProfileBlock;

#[async_trait]
impl InfoBlock for ProfileBlock {
    fn id(&self) -> &str {
        "analysis"
    }
    fn always(&self) -> bool {
        true
    }
    async fn collect(&self, ctx: &BuildContext<'_>) -> Result<Vec<BlockContent>, CoreError> {
        if !ctx.config.analysis.enabled {
            return Ok(Vec::new());
        }
        let projects = &ctx.config.projects;
        if projects.len() == 1 && projects[0] == "." {
            // Single-project (default) keeps the classic one-section shape.
            let profile = analyze_project(ctx.workdir);
            return Ok(vec![BlockContent {
                text: format!("# Project overview\n{}", profile.render()),
                stability: Stability::Fixed,
            }]);
        }
        // Multi-project workspace: one profile piece per root, each
        // measured by content hash — a profile whose facts changed (e.g.
        // a branch switch) ranks as fresh and moves down the prefix.
        let mut out = Vec::new();
        for proj in projects {
            let dir = Path::new(ctx.workdir).join(proj);
            let profile = analyze_project(dir.to_str().unwrap_or(proj));
            let text = format!("# Project overview: `{proj}`\n{}", profile.render());
            let stability = content_stability(ctx, &format!("profile:{proj}"), &text).await;
            out.push(BlockContent { text, stability });
        }
        Ok(out)
    }
}

/// Convention files — composes a [`FileCollector`] per include entry, so
/// each file is measured by its own path history.
pub struct ConventionFilesBlock;

#[async_trait]
impl InfoBlock for ConventionFilesBlock {
    fn id(&self) -> &str {
        "files"
    }
    async fn collect(&self, ctx: &BuildContext<'_>) -> Result<Vec<BlockContent>, CoreError> {
        if !ctx.config.files.enabled {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for file in &ctx.config.include {
            out.extend(FileCollector { file: file.clone() }.collect(ctx).await?);
        }
        Ok(out)
    }
}

/// Directory tree — delegates to the tree collector.
pub struct TreeBlock;

#[async_trait]
impl InfoBlock for TreeBlock {
    fn id(&self) -> &str {
        "tree"
    }
    async fn collect(&self, ctx: &BuildContext<'_>) -> Result<Vec<BlockContent>, CoreError> {
        TreeCollector.collect(ctx).await
    }
}

/// Git status — delegates to the git collector.
pub struct GitStatusBlock;

#[async_trait]
impl InfoBlock for GitStatusBlock {
    fn id(&self) -> &str {
        "git"
    }
    async fn collect(&self, ctx: &BuildContext<'_>) -> Result<Vec<BlockContent>, CoreError> {
        GitStatusCollector.collect(ctx).await
    }
}

/// Last-feature decisions — delegates to the decision-log collector.
pub struct DecisionsBlock;

#[async_trait]
impl InfoBlock for DecisionsBlock {
    fn id(&self) -> &str {
        "decisions"
    }
    async fn collect(&self, ctx: &BuildContext<'_>) -> Result<Vec<BlockContent>, CoreError> {
        DecisionLogCollector.collect(ctx).await
    }
}

/// The default block set — the built-in information computing engines.
pub fn default_blocks() -> Vec<Arc<dyn InfoBlock>> {
    vec![
        Arc::new(PreambleBlock),
        Arc::new(ProfileBlock),
        Arc::new(ConventionFilesBlock),
        Arc::new(TreeBlock),
        Arc::new(GitStatusBlock),
        Arc::new(DecisionsBlock),
    ]
}

/// Assemble the scaffold messages from a block list: build → stable-first
/// sort → budget trim. Consumed by [`crate::scaffold::build_scaffold_text`].
pub async fn assemble(
    blocks: &[Arc<dyn InfoBlock>],
    ctx: &BuildContext<'_>,
) -> Result<Vec<Message>, CoreError> {
    let mut pieces: Vec<(BlockContent, bool, String)> = Vec::new();
    for block in blocks {
        let id = block.id().to_string();
        let always = block.always();
        let contents = block.collect(ctx).await?;
        for piece in contents {
            pieces.push((piece, always, id.clone()));
        }
    }
    // Stable-first sort.
    pieces.sort_by_key(|(piece, _, _)| stability_key(piece.stability));
    // Budget trim (always pieces survive).
    let budget = ctx.config.max_tokens;
    let mut used = 0usize;
    let mut msgs = Vec::new();
    for (piece, always, _id) in pieces {
        let est = estimate_tokens(&piece.text);
        if let Some(b) = budget {
            if used + est > b && !always {
                continue;
            }
            used += est;
        }
        msgs.push(Message::user(piece.text));
    }
    Ok(msgs)
}

/// Sort key: Fixed(0) < Measured(ts asc, missing = newest) < Volatile(2).
pub(crate) fn stability_key(s: Stability) -> (u8, i64) {
    match s {
        Stability::Fixed => (0, i64::MIN),
        Stability::Measured(ts) => (1, ts.unwrap_or(i64::MAX)),
        Stability::Volatile => (2, 0),
    }
}

/// Rough token estimate for budget allocation: CJK chars ≈ 2 tokens,
/// everything else ≈ 0.3 token/char, plus a per-message overhead.
fn estimate_tokens(text: &str) -> usize {
    let (mut cjk, mut other) = (0usize, 0usize);
    for c in text.chars() {
        if ('\u{4e00}'..='\u{9fff}').contains(&c) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    cjk * 2 + other * 30 / 100 + 8
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn content_hash_stability_measures_actual_change() {
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let workdir = tempdir().unwrap().path().to_string_lossy().into_owned();
        let ctx = BuildContext {
            workdir: &workdir,
            chat_id: "c1",
            store: &store,
            config: &ScaffoldConfig::default(),
        };
        // First build: content unknown → timestamp = now.
        let first = content_stability(&ctx, "test_block", "version 1").await;
        let Stability::Measured(Some(t1)) = first else {
            panic!("expected measured timestamp");
        };
        // Same content on the next build → the SAME timestamp (unchanged).
        let second = content_stability(&ctx, "test_block", "version 1").await;
        assert_eq!(second, first, "unchanged content keeps its stability");
        // Changed content → a fresh (>=) timestamp.
        let third = content_stability(&ctx, "test_block", "version 2").await;
        let Stability::Measured(Some(t3)) = third else {
            panic!("expected measured timestamp");
        };
        assert!(t3 >= t1, "changed content must look newer");
        // Independent blocks don't collide: churning another block must
        // not touch this block's record.
        let _ = content_stability(&ctx, "other_block", "version 2").await;
        let again = content_stability(&ctx, "test_block", "version 1").await;
        assert_eq!(again, first, "per-block hash keys");
    }

    #[test]
    fn fx_hash_is_deterministic() {
        assert_eq!(fx_hash("same"), fx_hash("same"));
        assert_ne!(fx_hash("a"), fx_hash("b"));
    }

    #[test]
    fn stability_sort_puts_stable_content_first() {
        let mut pieces = [
            (Stability::Volatile, "git status"),
            (Stability::Measured(Some(100)), "old convention file"),
            (Stability::Fixed, "preamble"),
            (Stability::Measured(Some(200)), "recently changed file"),
            (Stability::Volatile, "decisions"),
        ];
        pieces.sort_by_key(|(s, _)| stability_key(*s));
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
        let mut pieces = [
            (Stability::Measured(None), "unknown"),
            (Stability::Measured(Some(50)), "measured"),
            (Stability::Volatile, "volatile"),
        ];
        pieces.sort_by_key(|(s, _)| stability_key(*s));
        let order: Vec<&str> = pieces.iter().map(|(_, t)| *t).collect();
        assert_eq!(order, vec!["measured", "unknown", "volatile"]);
    }

    #[tokio::test]
    async fn multi_project_profile_and_tree_produce_independent_pieces() {
        use crate::collector::TreeCollector;
        let dir = tempdir().unwrap();
        // Two sub-projects: a stable cargo crate and a churning node app.
        let root = dir.path();
        std::fs::create_dir_all(root.join("crates/core/src")).unwrap();
        std::fs::write(root.join("crates/core/Cargo.toml"), "[package]\n").unwrap();
        std::fs::create_dir_all(root.join("clients/app")).unwrap();
        std::fs::write(
            root.join("clients/app/package.json"),
            r#"{"scripts":{"test":"vitest"}}"#,
        )
        .unwrap();
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let config = ScaffoldConfig {
            projects: vec!["crates/core".into(), "clients/app".into()],
            ..Default::default()
        };
        let ctx = BuildContext {
            workdir: root.to_str().unwrap(),
            chat_id: "c1",
            store: &store,
            config: &config,
        };

        // Profiles: one piece per project, each judged by its own detector.
        let profiles = ProfileBlock.collect(&ctx).await.unwrap();
        assert_eq!(profiles.len(), 2);
        assert!(profiles[0].text.contains("crates/core"));
        assert!(profiles[0].text.contains("rust (cargo)"));
        assert!(profiles[1].text.contains("clients/app"));
        assert!(profiles[1].text.contains("node (npm)"));
        // Content-hash measured (not Fixed) in the multi-project shape.
        assert!(matches!(profiles[0].stability, Stability::Measured(_)));

        // Trees: one piece per project, measured by each path's history.
        let trees = TreeCollector.collect(&ctx).await.unwrap();
        assert_eq!(trees.len(), 2);
        assert!(trees[0].text.contains("crates/core"));
        assert!(trees[1].text.contains("clients/app"));
        assert!(matches!(trees[0].stability, Stability::Measured(_)));

        // All pieces compete in one stable-first ordering.
        let mut pieces = Vec::new();
        for p in profiles {
            pieces.push((p, false, "analysis".to_string()));
        }
        for t in trees {
            pieces.push((t, false, "tree".to_string()));
        }
        pieces.sort_by_key(|(piece, _, _)| stability_key(piece.stability));
        // Sorted by measured timestamp (oldest change first).
        let ts: Vec<Option<i64>> = pieces
            .iter()
            .map(|(piece, _, _)| match piece.stability {
                Stability::Measured(t) => t,
                _ => None,
            })
            .collect();
        for w in ts.windows(2) {
            if let (Some(a), Some(b)) = (w[0], w[1]) {
                assert!(a <= b, "pieces must sort stable-first");
            }
        }
    }
}
