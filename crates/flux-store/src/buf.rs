//! Buffered tool outputs — per-chat persistence for the overflow buffer.
//!
//! Each entry is ANCHORED to the tool call that produced it (keyed by the
//! kernel-assigned call id): the truncated result in the transcript points
//! at the same id, so the reference is self-describing and stable across
//! engine rebuilds AND process restarts. Entries are never overwritten —
//! a call id maps to exactly one output — and they die when their tool
//! call disappears from the model's view: the GC (at a rebase or a
//! feature boundary) deletes every entry whose call id has no tool
//! message above the persisted `context_base`. Chat deletion cascades.

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
    /// reference no longer resolves (never existed, evicted by the GC, or
    /// an old-format reference from before call-id anchoring).
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

    /// Delete every buffered entry whose tool call has NO tool message
    /// above `base_message_id` — the entries the model can no longer
    /// reference after a rebase / feature boundary archived their calls.
    /// One statement: the keep-set never crosses into the application.
    /// Returns the number of entries deleted.
    pub async fn gc_buf_entries(&self, chat_id: &str, base_message_id: i64) -> Result<u64> {
        let result = sqlx::query(
            "DELETE FROM buf_entries WHERE chat_id = ?1 AND call_id NOT IN (
                     SELECT tool_call_id FROM messages
                     WHERE chat_id = ?1 AND id > ?2 AND tool_call_id IS NOT NULL
                 )",
        )
        .bind(chat_id)
        .bind(base_message_id)
        .execute(&self.pool)
        .await
        .context("failed to gc buf entries")?;
        Ok(result.rows_affected())
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
    async fn gc_deletes_entries_below_the_base_and_keeps_live_ones() {
        let store = test_store().await;
        seed_chat(&store, "c").await;
        // Two tool calls land in the transcript (ids 1..4: call_old then
        // call_live, each a user turn + a tool result).
        store
            .append_messages(
                "c",
                &[
                    flux_core::Message::user("go"),
                    flux_core::Message::tool("call_old", "old result"),
                ],
            )
            .await
            .unwrap();
        store
            .append_messages(
                "c",
                &[
                    flux_core::Message::user("more"),
                    flux_core::Message::tool("call_live", "live result"),
                ],
            )
            .await
            .unwrap();
        store
            .save_buf_entry("c", "call_old", "OLD OUTPUT")
            .await
            .unwrap();
        store
            .save_buf_entry("c", "call_live", "LIVE OUTPUT")
            .await
            .unwrap();

        // Rebase to above message 2 (call_old archived, call_live kept):
        // exactly the archived call's entry dies.
        let deleted = store.gc_buf_entries("c", 2).await.unwrap();
        assert_eq!(deleted, 1);
        assert!(
            store
                .load_buf_entry("c", "call_old")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .load_buf_entry("c", "call_live")
                .await
                .unwrap()
                .as_deref(),
            Some("LIVE OUTPUT")
        );

        // Idempotent: a second GC at the same base deletes nothing.
        assert_eq!(store.gc_buf_entries("c", 2).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn gc_covers_calls_without_entries_and_entries_without_calls() {
        let store = test_store().await;
        seed_chat(&store, "c").await;
        // An entry whose call never reached the transcript (a mid-round
        // crash orphan) — the GC sweeps it at any base.
        store.save_buf_entry("c", "orphan", "junk").await.unwrap();
        store
            .append_messages("c", &[flux_core::Message::user("hi")])
            .await
            .unwrap();
        assert_eq!(store.gc_buf_entries("c", 1).await.unwrap(), 1);
        assert!(store.load_buf_entry("c", "orphan").await.unwrap().is_none());
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
