//! Messages — load and append messages with tool calls.

use crate::Store;
use anyhow::{Context, Result};
use flux_core::Message;
use std::collections::HashMap;
use tracing::warn;

/// A persisted message with its store row id — the projection the wire
/// history snapshot uses (the client names messages by id, e.g. as a
/// rebase base). The provider-facing context keeps using plain
/// [`Message`] (the id is persistence metadata, not transcript content).
pub struct StoredMessage {
    pub id: i64,
    pub message: Message,
}

impl Store {
    pub async fn load_messages(&self, chat_id: &str) -> Result<Vec<Message>> {
        Ok(self
            .load_messages_from(chat_id, None)
            .await?
            .into_iter()
            .map(|s| s.message)
            .collect())
    }

    /// Load every message WITH its row id — the history-snapshot path.
    pub async fn load_stored_messages(&self, chat_id: &str) -> Result<Vec<StoredMessage>> {
        self.load_messages_from(chat_id, None).await
    }

    /// Load only the messages above `after_id` (exclusive) — the live
    /// context of a feature chat after a rebuild. `after_id = None` loads
    /// everything.
    pub async fn load_messages_after(&self, chat_id: &str, after_id: i64) -> Result<Vec<Message>> {
        Ok(self
            .load_messages_from(chat_id, Some(after_id))
            .await?
            .into_iter()
            .map(|s| s.message)
            .collect())
    }

    /// The largest message id for a chat — the context base persisted at
    /// rebuild time.
    pub async fn max_message_id(&self, chat_id: &str) -> Result<i64> {
        let max: Option<i64> =
            sqlx::query_scalar("SELECT MAX(id) FROM messages WHERE chat_id = ?1")
                .bind(chat_id)
                .fetch_one(&self.pool)
                .await
                .context("failed to query max message id")?;
        Ok(max.unwrap_or(0))
    }

    async fn load_messages_from(
        &self,
        chat_id: &str,
        after_id: Option<i64>,
    ) -> Result<Vec<StoredMessage>> {
        // 1. Load all messages for this chat (including reasoning_content)
        #[allow(clippy::type_complexity)]
        let msg_rows: Vec<(i64, String, String, Option<String>, Option<String>)> = match after_id {
            Some(base) => sqlx::query_as(
                "SELECT id, role, content, tool_call_id, reasoning_content
                     FROM messages
                     WHERE chat_id = ?1 AND id > ?2
                     ORDER BY id",
            )
            .bind(chat_id)
            .bind(base)
            .fetch_all(&self.pool)
            .await
            .context("failed to query messages")?,
            None => sqlx::query_as(
                "SELECT id, role, content, tool_call_id, reasoning_content
                     FROM messages
                     WHERE chat_id = ?1
                     ORDER BY id",
            )
            .bind(chat_id)
            .fetch_all(&self.pool)
            .await
            .context("failed to query messages")?,
        };

        // 2. Load tool calls for the matching assistant messages. With a
        //    base, scope the join to the same window so archived tool calls
        //    never leak into the live context.
        let tc_rows: Vec<(i64, String, String, String)> = match after_id {
            Some(base) => sqlx::query_as(
                "SELECT tc.message_id, tc.id, tc.name, tc.arguments
                     FROM tool_calls tc
                     JOIN messages m ON m.id = tc.message_id
                     WHERE m.chat_id = ?1 AND m.id > ?2
                     ORDER BY tc.message_id, tc.id",
            )
            .bind(chat_id)
            .bind(base)
            .fetch_all(&self.pool)
            .await
            .context("failed to query tool_calls")?,
            None => sqlx::query_as(
                "SELECT tc.message_id, tc.id, tc.name, tc.arguments
                     FROM tool_calls tc
                     JOIN messages m ON m.id = tc.message_id
                     WHERE m.chat_id = ?1
                     ORDER BY tc.message_id, tc.id",
            )
            .bind(chat_id)
            .fetch_all(&self.pool)
            .await
            .context("failed to query tool_calls")?,
        };

        // 3. Group tool calls by message_id
        let mut tool_calls_by_msg: HashMap<i64, Vec<flux_core::ToolCall>> = HashMap::new();
        for (msg_id, tc_id, name, arguments) in &tc_rows {
            tool_calls_by_msg
                .entry(*msg_id)
                .or_default()
                .push(flux_core::ToolCall {
                    id: tc_id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                });
        }

        // 4. Assemble messages
        let mut messages = Vec::new();
        for (msg_id, role, content, tool_call_id, reasoning_content) in &msg_rows {
            let role = match role.as_str() {
                "system" => flux_core::Role::System,
                "user" => flux_core::Role::User,
                "assistant" => flux_core::Role::Assistant,
                "tool" => flux_core::Role::Tool,
                _ => {
                    warn!(role = %role, id = msg_id, "unknown role, skipping message");
                    // Drop the skipped row's tool-call entries too — they
                    // would otherwise linger for the rest of the load.
                    tool_calls_by_msg.remove(msg_id);
                    continue;
                }
            };

            let tool_calls = tool_calls_by_msg.remove(msg_id).unwrap_or_default();

            messages.push(StoredMessage {
                id: *msg_id,
                message: Message {
                    role,
                    content: content.clone(),
                    reasoning_content: reasoning_content.clone(),
                    tool_calls,
                    tool_call_id: tool_call_id.clone(),
                },
            });
        }

        Ok(messages)
    }

    pub async fn append_messages(&self, chat_id: &str, messages: &[Message]) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .context("failed to begin transaction")?;

        // Ensure chat row exists (may have been created by another session)
        sqlx::query("INSERT OR IGNORE INTO chats (id) VALUES (?1)")
            .bind(chat_id)
            .execute(&mut *tx)
            .await?;

        for msg in messages {
            let role = msg.role.to_string();
            let result = sqlx::query(
                "INSERT INTO messages (chat_id, role, content, tool_call_id, reasoning_content)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .bind(chat_id)
            .bind(&role)
            .bind(&msg.content)
            .bind(&msg.tool_call_id)
            .bind(&msg.reasoning_content)
            .execute(&mut *tx)
            .await
            .context("failed to insert message")?;

            if role == "assistant" && !msg.tool_calls.is_empty() {
                let message_id = result.last_insert_rowid();
                for tc in &msg.tool_calls {
                    sqlx::query(
                        "INSERT OR IGNORE INTO tool_calls (id, message_id, name, arguments)
                         VALUES (?1, ?2, ?3, ?4)",
                    )
                    .bind(&tc.id)
                    .bind(message_id)
                    .bind(&tc.name)
                    .bind(&tc.arguments)
                    .execute(&mut *tx)
                    .await
                    .context("failed to insert tool_call")?;
                }
            }
        }

        // Touch the chat's activity stamp in the SAME transaction — the
        // sidebar sorts by it, so it must move atomically with the append.
        sqlx::query("UPDATE chats SET last_activity_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?1")
            .bind(chat_id)
            .execute(&mut *tx)
            .await
            .context("failed to touch chat activity")?;

        tx.commit()
            .await
            .context("failed to commit message append")?;
        Ok(())
    }
}
