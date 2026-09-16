//! Chat — internal conversation entity.
//!
//! A `Chat` holds the shared state for the conversation loop and tool
//! execution.  Use `spawn::spawn` to create a new chat; it returns a
//! `handle::ChatHandle` for external control plus the task's `JoinHandle`
//! so callers can supervise completion.

use crate::buf;
use crate::domain::StateManager;
use async_trait::async_trait;
use flux_core::{Message, ToolCall};
use flux_core::{ToolCtx, ToolPort, ToolRegistry};
use flux_store::Store;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::warn;

// ── Chat ────────────────────────────────────────────────────────────────────

/// Shared context the adapter's tool execution works over.
///
/// Cancellation rides the input channel as a plain [`LoopInput::Cancel`]
/// event; this struct only carries what the ports need.
pub(crate) struct Chat {
    pub(crate) id: String,
    /// Arc so the state tools (registered in this chat's registry) can
    /// share ownership with the ports.
    pub(crate) state_manager: Arc<StateManager>,
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) store: Arc<Store>,
}

impl Chat {
    // ── Persistence helpers ───────────────────────────────────────────

    /// Persist messages to the store, returning the assigned row ids (empty
    /// on failure — logged, non-fatal, the ids' consumers just skip).
    pub(crate) async fn persist_messages(&self, messages: &[Message]) -> Vec<i64> {
        if messages.is_empty() {
            return Vec::new();
        }
        match self.store.append_messages(&self.id, messages).await {
            Ok(ids) => ids,
            Err(e) => {
                warn!(chat_id = %self.id, error = %e, "failed to persist messages");
                Vec::new()
            }
        }
    }

    /// Central output truncation: results over the inline budget are stored
    /// whole (anchored to the producing tool call, readable via `buf_read`)
    /// and replaced by a bounded head plus a reference. One place covers
    /// every tool — bash, grep, read_file, MCP, and anything registered
    /// later. The reference IS the call id — self-describing and stable
    /// across rebuilds and restarts (the store persists it), and a fork
    /// copies the entries of the calls its copied transcript carries, so
    /// `buf_read` references stay resolvable in the new chat.
    pub(crate) async fn bounded_output(&self, call_id: &str, result: &str) -> String {
        // Byte fast path: bytes bound chars — results within the budget in
        // bytes are within it in chars, so the common case skips the O(n)
        // char walk entirely (every tool result passes through here).
        if result.len() <= buf::INLINE_BUDGET {
            return result.to_string();
        }
        let total = result.chars().count();
        if total <= buf::INLINE_BUDGET {
            return result.to_string();
        }
        buf::store_overflow(&self.store, &self.id, call_id, result).await;
        // Head slice at the budget's char boundary — one walk, no per-char
        // collect.
        let head_end = result
            .char_indices()
            .nth(buf::INLINE_BUDGET)
            .map_or(result.len(), |(i, _)| i);
        let mut out = String::with_capacity(head_end + 192);
        out.push_str(&result[..head_end]);
        out.push_str("\n\n--- output truncated (");
        out.push_str(&total.to_string());
        out.push_str(" chars total) — full output buffered under call \"");
        out.push_str(call_id);
        out.push_str("\"; read with buf_read {\"ref\": \"");
        out.push_str(call_id);
        out.push_str("\", \"offset\": ");
        out.push_str(&buf::INLINE_BUDGET.to_string());
        out.push_str(", \"limit\": ");
        out.push_str(&buf::BUF_PAGE_CHARS.to_string());
        out.push_str("} ---");
        out
    }
}

// ── Loop ports ───────────────────────────────────────────────────────────────

/// The kernel's tool abstraction: one uniform pipeline for every tool —
/// existence → ctx enrichment (the chat boundary — `workdir` /
/// `current_dir` — fills the ToolCtx so tools resolve paths against it) →
/// execute → output bounding. No approval phase — tools execute directly.
/// State tools live in the per-chat registry like any other tool.
#[async_trait]
impl ToolPort for Chat {
    async fn execute(&self, call: &ToolCall, args: HashMap<String, Value>, ctx: ToolCtx) -> String {
        // Existence first: a miss short-circuits before any enrichment.
        let Some(tool) = self.tools.get(&call.name) else {
            return format!("tool not found: {}", call.name);
        };
        // Boundary enrichment: the kernel builds the ctx boundary-agnostic;
        // the chat fills it from authoritative state (workdir fixed at
        // creation, current_dir read fresh per call — the same data the old
        // preprocessing layer consumed, now delivered as invocation context).
        let mut ctx = ctx;
        ctx.workdir = PathBuf::from(self.state_manager.workdir());
        ctx.current_dir = PathBuf::from(self.state_manager.get("current_dir").unwrap_or_default());
        let result = tool.call(args, ctx.clone()).await.unwrap_or_else(|e| {
            warn!(chat_id = %self.id, tool = %call.name, error = %e, "tool execution failed");
            format!("Error: {e}")
        });
        // The overflow reference IS the call id (ToolCtx carries the
        // kernel-assigned id) — anchored, persisted, never overwritten.
        self.bounded_output(&ctx.call_id, &result).await
    }
}

// ── ResolvedPin ─────────────────────────────────────────────────────────

/// A provider pin resolved to its instance + labels — the handoff from the
/// server's registry (which owns selection) to the chat layer's mutating
/// ops (`create_chat` / `switch_provider`) and the in-place rebuild (the
/// consumer re-begins its connection on the carried instance at the
/// machine gate). Instances are cheap (string assembly over a shared HTTP
/// client); the labels are the display/persist strings the UI and state
/// table carry.
#[derive(Clone)]
pub struct ResolvedPin {
    pub provider: Arc<dyn flux_core::Provider>,
    pub id: String,
    pub model: String,
}

// ── ChatInit ────────────────────────────────────────────────────────────────

/// Initialization data for [`crate::spawn::spawn`] (built by the session
/// layer's task lifecycle).
pub struct ChatInit {
    pub id: String,
    pub history: Vec<Message>,
    /// Pending-question registry for this chat's `question` tool.
    pub questions: Arc<crate::question::QuestionBoard>,
    /// The chat's resolved provider instance (model-pinned) — selection
    /// happened upstream (server registry at creation/swap/hydration);
    /// `begin` here is the chat layer's ONLY provider surface.
    pub provider: Arc<dyn flux_core::Provider>,
}
