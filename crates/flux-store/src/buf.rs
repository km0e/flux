//! Buffered tool outputs — per-chat persistence for the overflow buffer.
//!
//! Each entry is ANCHORED to the tool call that produced it (keyed by the
//! kernel-assigned call id): the truncated result in the transcript points
//! at the same id, so the reference is self-describing and stable across
//! engine rebuilds AND process restarts. Entries are never overwritten —
//! a call id maps to exactly one output — and they die when their tool
//! call disappears from the model's view: entries live exactly as long
//! as their chat (a transcript only grows), and a FORK copies the
//! entries its copied transcript carries. Chat deletion cascades.

use crate::Store;
use anyhow::{Context, Result};

impl Store {
    /// Upsert one buffered output (the write-through of
    /// `Chat::bounded_output`).
    pub async fn save_buf_entry(&self, chat_id: &str, call_id: &str, content: &str) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO buf_entries (chat_id, call_id, content) VALUES (?1, ?2, ?3)",
        )
        .bind(chat_id)
        .bind(call_id)
        .bind(content)
        .execute(&self.pool)
        .await
        .context("failed to save buf entry")?;
        Ok(())
    }

    /// Load one buffered output (`buf_read`'s read-through). `None` = the
    /// reference no longer resolves (never existed, or the chat was
    /// deleted).
    pub async fn load_buf_entry(&self, chat_id: &str, call_id: &str) -> Result<Option<String>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT content FROM buf_entries WHERE chat_id = ?1 AND call_id = ?2")
                .bind(chat_id)
                .bind(call_id)
                .fetch_optional(&self.pool)
                .await
                .context("failed to load buf entry")?;
        Ok(row.map(|(content,)| content))
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;

    async fn test_store() -> Store {
        Store::open_in_memory().await.unwrap()
    }

    async fn seed_chat(store: &Store, id: &str) {
        store.insert_chat(id, "t").await.unwrap();
    }

    #[tokio::test]
    async fn save_and_load_round_trip() {
        let store = test_store().await;
        seed_chat(&store, "c").await;
        store.save_buf_entry("c", "call_1", "hello").await.unwrap();
        assert_eq!(
            store
                .load_buf_entry("c", "call_1")
                .await
                .unwrap()
                .as_deref(),
            Some("hello")
        );
        // Upsert: a call id maps to exactly one output — re-storing
        // REPLACES (only the overflow path writes, once per call).
        store
            .save_buf_entry("c", "call_1", "hello v2")
            .await
            .unwrap();
        assert_eq!(
            store
                .load_buf_entry("c", "call_1")
                .await
                .unwrap()
                .as_deref(),
            Some("hello v2")
        );
        // Unknown ref → None (the buf_read error path).
        assert!(store.load_buf_entry("c", "nope").await.unwrap().is_none());
        // Per-chat isolation.
        seed_chat(&store, "d").await;
        assert!(store.load_buf_entry("d", "call_1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn fork_copies_the_copied_calls_entries() {
        let store = test_store().await;
        seed_chat(&store, "c").await;
        // A fork inherits the workdir pair — a server-created chat always
        // carries one.
        store
            .save_state_entries("c", &[("workdir", "/tmp"), ("current_dir", "/tmp")])
            .await
            .unwrap();
        // Transcript: user + tool result (call_old) + user (id 3); a
        // buffered output for the call and one orphan (no call anywhere).
        store
            .append_messages(
                "c",
                &[
                    flux_core::Message::user("go"),
                    // The assistant turn is what carries the tool_calls
                    // row (the buf keep-set joins on it).
                    flux_core::Message {
                        role: flux_core::Role::Assistant,
                        content: String::new(),
                        reasoning_content: None,
                        tool_calls: vec![flux_core::ToolCall {
                            id: "call_old".into(),
                            name: "bash".into(),
                            arguments: "{}".into(),
                        }],
                        tool_call_id: None,
                    },
                    flux_core::Message::tool("call_old", "old result"),
                    flux_core::Message::user("more"),
                ],
            )
            .await
            .unwrap();
        store
            .save_buf_entry("c", "call_old", "OLD OUTPUT")
            .await
            .unwrap();
        store.save_buf_entry("c", "orphan", "junk").await.unwrap();

        // Fork at the LAST user turn (id 4): the copy carries the whole
        // tool exchange, so its entry rides along; the orphan does not.
        // The source keeps everything.
        store.fork_chat("c", 4, "f", "c (fork)").await.unwrap();
        assert_eq!(
            store
                .load_buf_entry("f", "call_old")
                .await
                .unwrap()
                .as_deref(),
            Some("OLD OUTPUT")
        );
        assert!(store.load_buf_entry("f", "orphan").await.unwrap().is_none());
        assert_eq!(
            store
                .load_buf_entry("c", "call_old")
                .await
                .unwrap()
                .as_deref(),
            Some("OLD OUTPUT")
        );
        assert_eq!(
            store
                .load_buf_entry("c", "orphan")
                .await
                .unwrap()
                .as_deref(),
            Some("junk")
        );
    }

    #[tokio::test]
    async fn chat_deletion_cascades_buf_entries() {
        let store = test_store().await;
        seed_chat(&store, "c").await;
        store
            .save_buf_entry("c", "call_1", "content")
            .await
            .unwrap();
        store.delete_chat("c").await.unwrap();
        assert!(store.load_buf_entry("c", "call_1").await.unwrap().is_none());
    }
}
