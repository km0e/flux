//! buf.rs — Buffered overflow for oversized tool outputs, ANCHORED TO
//! THEIR TOOL CALLS and persisted per chat.
//!
//! When a tool's rendered output exceeds the inline budget
//! (`Chat::bounded_output`), the full content is stored keyed by the
//! call id (`ToolCtx::call_id`) and the tool returns a bounded head plus
//! a reference; the model reads the rest through [`BufReadTool`] with
//! char-based paging. Char paging is deliberate: buffered output is
//! arbitrary — it may be a single megabyte "line" (minified bundle), and
//! line-based paging would inherit exactly the line-length problem the
//! buffer exists to solve.
//!
//! Lifetime: entries are NEVER overwritten (a call id maps to exactly
//! one output) and never wiped by new activity — a chat's transcript only
//! grows, so an entry lives exactly as long as its chat (FK cascade), and
//! a fork copies the entries its copied transcript carries. The store is
//! the only truth: entries survive engine rebuilds AND process restarts.

use async_trait::async_trait;
use flux_core::BUF_READ_TOOL;
use flux_core::{CoreError, Tool, ToolCtx};
use flux_store::Store;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// Hard cap on a single buffered entry (chars). Beyond this the tail is
/// dropped with an in-buffer marker — a 16MB subprocess dump must not sit
/// in the store whole when the model can only ever page a fraction of it.
const BUF_ENTRY_MAX_CHARS: usize = 1_000_000;
/// Default and maximum page size for `buf_read` (chars). The maximum is
/// deliberately at/below the inline budget (8000) so a buffer page can
/// never trigger the overflow path itself (no recursion).
pub(crate) const BUF_PAGE_CHARS: usize = 6000;

/// Write-through: store the full output of one tool call, keyed by the
/// call id. Content beyond [`BUF_ENTRY_MAX_CHARS`] is dropped with an
/// in-buffer marker. A store failure is logged and swallowed — the tool
/// result already points at the reference; a failed write surfaces later
/// as a `buf_read` miss, which the model recovers from by re-running the
/// tool.
pub(crate) async fn store_overflow(store: &Store, chat_id: &str, call_id: &str, content: &str) {
    // Byte-length pre-filter: chars ≤ bytes, so content within the cap in
    // BYTES is within the cap in chars — the common case skips the O(n)
    // char walk entirely.
    let stored = if content.len() > BUF_ENTRY_MAX_CHARS {
        let char_count = content.chars().count();
        if char_count > BUF_ENTRY_MAX_CHARS {
            // One walk to the cap's char boundary, then a memcpy slice —
            // no per-char collect.
            let head_end = content
                .char_indices()
                .nth(BUF_ENTRY_MAX_CHARS)
                .map_or(content.len(), |(i, _)| i);
            let dropped = char_count - BUF_ENTRY_MAX_CHARS;
            format!(
                "{}\n\n--- (buffered output capped at {BUF_ENTRY_MAX_CHARS} chars; {dropped} chars dropped) ---",
                &content[..head_end]
            )
        } else {
            content.to_string()
        }
    } else {
        content.to_string()
    };
    if let Err(e) = store.save_buf_entry(chat_id, call_id, &stored).await {
        tracing::warn!(chat_id, call_id, error = %e, "failed to persist buffered tool output");
    }
}

/// `buf_read` — page through a previously buffered (truncated) tool
/// output, addressed by the tool call id that produced it.
#[derive(Debug, Clone)]
pub struct BufReadTool {
    store: Arc<Store>,
    chat_id: String,
}

impl BufReadTool {
    pub(crate) fn new(store: Arc<Store>, chat_id: String) -> Self {
        Self { store, chat_id }
    }
}

#[async_trait]
impl Tool for BufReadTool {
    fn name(&self) -> &str {
        BUF_READ_TOOL
    }

    fn description(&self) -> &str {
        "Read a page of a previously buffered (truncated) tool output. Char-based paging: \
         offset/limit are CHAR offsets, not lines — buffered output may be a single huge line. \
         Entries are keyed by the tool call id that produced them and persist for the chat."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "ref": {
                    "type": "string",
                    "description": "Tool call id whose output was truncated (stated in the truncated result)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Char offset to read from (default 0)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Chars to read (default 6000)"
                }
            },
            "required": ["ref"],
            "additionalProperties": false
        })
    }

    async fn call(&self, args: HashMap<String, Value>, _ctx: ToolCtx) -> Result<String, CoreError> {
        let Some(r) = args.get("ref").and_then(|v| v.as_str()) else {
            return Err(CoreError::InvalidArguments(
                "buf_read requires a 'ref' argument".into(),
            ));
        };
        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(BUF_PAGE_CHARS as u64)
            .clamp(1, BUF_PAGE_CHARS as u64) as usize;
        let Some(content) = self
            .store
            .load_buf_entry(&self.chat_id, r)
            .await
            .map_err(|e| CoreError::Tool(format!("buf_read failed: {e}")))?
        else {
            return Err(CoreError::Tool(format!(
                "unknown buffer reference '{r}' — references are per-conversation; \
                 re-run the original tool to regenerate"
            )));
        };
        let total = content.chars().count();
        let start = offset.min(total);
        let end = (start + limit).min(total);
        // Byte window via one bounded walk — the page itself is then a
        // memcpy. (The old `chars().skip(start)` walked `start` chars PER
        // READ; sequential paging through a full 1M-char entry no longer
        // rescans the prefix per page.)
        let mut start_byte = content.len();
        let mut end_byte = content.len();
        if start < total {
            for (i, (b, _)) in content.char_indices().enumerate() {
                if i == start {
                    start_byte = b;
                    if end == total {
                        break;
                    }
                }
                if i == end {
                    end_byte = b;
                    break;
                }
            }
        }
        let slice = &content[start_byte..end_byte];
        let footer = if end < total {
            format!("--- (chars {start}-{end} of {total} — continue with offset: {end}) ---")
        } else {
            format!("--- (end of buffer, {total} chars) ---")
        };
        Ok(format!(
            "[{r} chars {start}..{end} of {total}]\n{slice}\n\n{footer}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_store::Store;

    async fn store_with_chat() -> (Arc<Store>, String) {
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        store.insert_chat("test-chat", "t").await.unwrap();
        (store, "test-chat".to_string())
    }

    #[tokio::test]
    async fn write_through_read_pages_round_trip() {
        let (store, chat_id) = store_with_chat().await;
        store_overflow(&store, &chat_id, "call_1", "hello world").await;
        let tool = BufReadTool::new(Arc::clone(&store), chat_id);
        let page = tool
            .call(
                HashMap::from([
                    ("ref".to_string(), json!("call_1")),
                    ("limit".to_string(), json!(5)),
                ]),
                ToolCtx::new(),
            )
            .await
            .unwrap();
        assert!(page.contains("hello"));
        assert!(page.contains("chars 0..5 of 11"));
        assert!(page.contains("continue with offset: 5"));
        assert!(page.starts_with("[call_1 chars 0..5"));
    }

    #[tokio::test]
    async fn entry_cap_drops_the_tail_with_marker() {
        let (store, chat_id) = store_with_chat().await;
        store_overflow(&store, &chat_id, "call_1", &"x".repeat(1_200_000)).await;
        let stored = store
            .load_buf_entry(&chat_id, "call_1")
            .await
            .unwrap()
            .unwrap();
        assert!(stored.contains("buffered output capped"));
        assert!(stored.contains("200000 chars dropped"));
    }

    #[tokio::test]
    async fn entries_persist_across_rebuilds_and_are_never_wiped_by_new_tools() {
        // Two "engines" over the same store (a rebuild is just a new tool
        // set over the same chat id): the second engine's buf_read still
        // resolves the first engine's entry, and new tool activity does
        // NOT supersede it (no generation wipe exists anymore).
        let (store, chat_id) = store_with_chat().await;
        store_overflow(&store, &chat_id, "call_1", "OLD OUTPUT").await;

        let engine2 = BufReadTool::new(Arc::clone(&store), chat_id.clone());
        let page = engine2
            .call(
                HashMap::from([("ref".to_string(), json!("call_1"))]),
                ToolCtx::new(),
            )
            .await
            .unwrap();
        assert!(page.contains("OLD OUTPUT"));

        // New overflow lands alongside — the old entry survives.
        store_overflow(&store, &chat_id, "call_2", "NEW OUTPUT").await;
        assert!(
            engine2
                .call(
                    HashMap::from([("ref".to_string(), json!("call_1"))]),
                    ToolCtx::new()
                )
                .await
                .unwrap()
                .contains("OLD OUTPUT")
        );
    }

    #[tokio::test]
    async fn unknown_ref_is_graceful_with_the_regen_hint() {
        let (store, chat_id) = store_with_chat().await;
        let tool = BufReadTool::new(store, chat_id);
        let err = tool
            .call(
                HashMap::from([("ref".to_string(), json!("call_99"))]),
                ToolCtx::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown buffer reference"));
        assert!(err.to_string().contains("re-run the original tool"));
    }

    #[tokio::test]
    async fn missing_ref_is_invalid_arguments() {
        let (store, chat_id) = store_with_chat().await;
        let tool = BufReadTool::new(store, chat_id);
        let err = tool.call(HashMap::new(), ToolCtx::new()).await.unwrap_err();
        assert!(matches!(err, CoreError::InvalidArguments(_)));
    }
}
