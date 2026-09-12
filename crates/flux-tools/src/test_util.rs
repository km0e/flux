//! Shared test utilities for flux-tools tool tests.

use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use tempfile::TempDir;

/// Raw-args helper: builds the exact argument map a test passes to
/// `tool.call`. Tools resolve paths against the ctx boundary — pass
/// relative paths and pair with [`boundary_ctx`].
pub(crate) fn processed(pairs: Vec<(&str, Value)>) -> HashMap<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// A `ToolCtx` with the chat boundary filled: `workdir` = `dir`,
/// `current_dir` = `dir` (the create_chat initialization).
pub(crate) fn boundary_ctx(dir: &Path) -> flux_core::ToolCtx {
    flux_core::ToolCtx {
        workdir: dir.to_path_buf(),
        current_dir: dir.to_path_buf(),
        ..Default::default()
    }
}

/// Fresh temporary directory for tool tests.
pub(crate) fn setup() -> TempDir {
    TempDir::new().unwrap()
}
