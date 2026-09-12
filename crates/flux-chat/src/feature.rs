//! Feature-mode tooling — the `feature_done` tool that a feature chat
//! registers. It is the pure mechanism's orchestration face: when the
//! model announces a completed feature, the tool records the decision and
//! returns the orchestrated project context that the kernel then re-injects
//! as the next feature's first message (via the cancel→rebase→inject
//! sequence in flux-loop).
//!
//! No conversation-kind logic lives here beyond the tool itself: the
//! `ChatKind` and per-chat registration are decided at assembly (spawn.rs).

use crate::domain::StateManager;
use async_trait::async_trait;
use flux_core::{CoreError, FEATURE_DONE_TOOL, Tool};
use flux_store::Store;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

/// Announce a completed feature and emit the orchestrated project context
/// that becomes the next feature's starting prompt.
pub(crate) struct FeatureDoneTool {
    store: Arc<Store>,
    chat_id: String,
    state_manager: Arc<StateManager>,
}

impl FeatureDoneTool {
    pub(crate) fn new(store: Arc<Store>, chat_id: String, state: Arc<StateManager>) -> Self {
        Self {
            store,
            chat_id,
            state_manager: state,
        }
    }
}

#[async_trait]
impl Tool for FeatureDoneTool {
    fn name(&self) -> &str {
        FEATURE_DONE_TOOL
    }
    fn description(&self) -> &str {
        "Announce that the current feature is complete. After the call: the feature's summary is written to the engineering decision log and the conversation context is re-scaffolded — the next feature starts from a clean context using the scaffold text as its starting point. Before calling, use `summary` to describe what this feature did, which files changed, and the key decisions."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "Completion summary of this feature: what was done, which files changed, key decisions. Written to the decision log and used as input for the next feature's scaffold context."
                }
            },
            "required": ["summary"]
        })
    }
    async fn call(
        &self,
        args: HashMap<String, Value>,
        _ctx: flux_core::ToolCtx,
    ) -> Result<String, CoreError> {
        let summary = args
            .get("summary")
            .and_then(|v| v.as_str())
            .ok_or_else(|| CoreError::InvalidArguments("feature_done requires 'summary'".into()))?;

        // Record the completed feature into the decision log.
        if let Err(e) = self.store.append_feature_log(&self.chat_id, summary).await {
            tracing::warn!(chat_id = %self.chat_id, error = %e, "failed to record feature");
            return Err(CoreError::Internal(format!(
                "feature_done: failed to record: {e}"
            )));
        }

        // Record nothing here — the decision log entry was written above;
        // orchestrate the full project scaffold as the next feature's
        // starting context (project profile, tree, git status, convention
        // files, recent decisions).
        let workdir = self.state_manager.workdir().to_string();
        let context = match flux_context::build_scaffold_text(
            self.store.clone(),
            &self.chat_id,
            &workdir,
            &flux_context::ScaffoldConfig::default(),
        )
        .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(chat_id = %self.chat_id, error = %e, "scaffold build failed; continuing");
                format!("(scaffold failed: {e})")
            }
        };

        Ok(format!(
            "Feature completed: {summary}\n\nEngineering context for the next feature:\n{context}",
        ))
    }
}
