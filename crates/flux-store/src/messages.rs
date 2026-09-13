//! Messages — load, append, and fork messages with tool calls.

use crate::Store;
use anyhow::{Context, Result};
use flux_core::Message;
use std::collections::HashMap;
use tracing::warn;

/// A persisted message with its store row id — the projection the wire
/// history snapshot uses (the client names messages by id, e.g. as a fork
/// point). The provider-facing context keeps using plain [`Message`] (the
/// id is persistence metadata, not transcript content).
pub struct StoredMessage {
    pub id: i64,
    pub message: Message,
}

/// Fork validation failure — the fork point is not a USER message row of
/// the source chat (the only forkable boundary: the copy ends awaiting
/// the model's reply).
#[derive(Debug, thiserror::Error)]
#[error("fork point is not a user message of the source chat")]
pub struct BadForkPoint;

/// The creation facts of a forked chat (the ops layer assembles the
/// `ChatInfo`/cached entry from these).
pub struct ForkedChat {
    pub created_at: String,
    pub workdir: String,
    pub provider: Option<String>,
    pub model: Option<String>,
}

impl Store {
    pub async fn load_messages(&self, chat_id: &str) -> Result<Vec<Message>> {
        Ok(self
            .load_messages_from(chat_id)
            .await?
            .into_iter()
            .map(|s| s.message)
            .collect())
    }

    /// Load every message WITH its row id — the history-snapshot path.
    pub async fn load_stored_messages(&self, chat_id: &str) -> Result<Vec<StoredMessage>> {
        self.load_messages_from(chat_id).await
    }

    async fn load_messages_from(&self, chat_id: &str) -> Result<Vec<StoredMessage>> {
        // 1. Load all messages for this chat (including reasoning_content)
        #[allow(clippy::type_complexity)]
        let msg_rows: Vec<(i64, String, String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT id, role, content, tool_call_id, reasoning_content
                 FROM messages
                 WHERE chat_id = ?1
                 ORDER BY id",
        )
        .bind(chat_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to query messages")?;

        // 2. Load tool calls for the matching assistant messages.
        let tc_rows: Vec<(i64, String, String, String)> = sqlx::query_as(
            "SELECT tc.message_id, tc.id, tc.name, tc.arguments
                 FROM tool_calls tc
                 JOIN messages m ON m.id = tc.message_id
                 WHERE m.chat_id = ?1
                 ORDER BY tc.message_id, tc.id",
        )
        .bind(chat_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to query tool_calls")?;

        // 3. Group tool calls by message_id — the row strings MOVE into
        //    the ToolCalls (no intermediate clone; the rows are consumed).
        let mut tool_calls_by_msg: HashMap<i64, Vec<flux_core::ToolCall>> = HashMap::new();
        for (msg_id, tc_id, name, arguments) in tc_rows {
            tool_calls_by_msg
                .entry(msg_id)
                .or_default()
                .push(flux_core::ToolCall {
                    id: tc_id,
                    name,
                    arguments,
                });
        }

        // 4. Assemble messages — the row fields move straight into each
        //    Message (sqlx already handed us owned Strings).
        let mut messages = Vec::with_capacity(msg_rows.len());
        for (msg_id, role, content, tool_call_id, reasoning_content) in msg_rows {
            let role = match role.as_str() {
                "system" => flux_core::Role::System,
                "user" => flux_core::Role::User,
                "assistant" => flux_core::Role::Assistant,
                "tool" => flux_core::Role::Tool,
                _ => {
                    warn!(role = %role, id = msg_id, "unknown role, skipping message");
                    // Drop the skipped row's tool-call entries too — they
                    // would otherwise linger for the rest of the load.
                    tool_calls_by_msg.remove(&msg_id);
                    continue;
                }
            };

            let tool_calls = tool_calls_by_msg.remove(&msg_id).unwrap_or_default();

            messages.push(StoredMessage {
                id: msg_id,
                message: Message {
                    role,
                    content,
                    reasoning_content,
                    tool_calls,
                    tool_call_id,
                },
            });
        }

        Ok(messages)
    }

    /// Append messages in one transaction and return the ASSIGNED row ids
    /// (in input order). The ids are the fork-point / correlation keys —
    /// the round consumer announces them so a client can name a message it
    /// just persisted (e.g. the live bubble's fork affordance) without
    /// waiting for the next history snapshot.
    pub async fn append_messages(&self, chat_id: &str, messages: &[Message]) -> Result<Vec<i64>> {
        if messages.is_empty() {
            return Ok(Vec::new());
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

        let mut ids = Vec::with_capacity(messages.len());
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
            ids.push(result.last_insert_rowid());

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
        Ok(ids)
    }

    /// Fork a conversation from a message: a NEW chat holding a copy of
    /// the source transcript up to but EXCLUDING `fork_point` (which must
    /// be a USER message row of the source). The fork point is the turn
    /// being REDONE: it re-enters the fork only when the user re-sends it
    /// (the client prefills the fork's composer with its content), so a
    /// fresh fork is a branch paused before its first turn — forking at
    /// the first user message copies nothing at all. The SOURCE chat is
    /// untouched: a fork is a read + create, so any viewer may trigger it
    /// (no lease gate).
    ///
    /// One transaction, five copies:
    ///   1. the chat row, carrying the fork provenance;
    ///   2. the transcript rows (fresh AUTOINCREMENT ids, old→new mapped);
    ///   3. the tool_calls rows, remapped onto the new message ids — the
    ///      call IDS themselves are model-generated and copied verbatim,
    ///      so history rendering and buf references keep working;
    ///   4. the buffered outputs of exactly those calls (the keep-set, one
    ///      statement) — `buf_read` references resolve in the fork;
    ///   5. the state rows a fork inherits (workdir pair + provider pin).
    pub async fn fork_chat(
        &self,
        source_chat_id: &str,
        fork_point: i64,
        new_chat_id: &str,
        new_name: &str,
    ) -> Result<ForkedChat, anyhow::Error> {
        let mut tx = self
            .pool
            .begin()
            .await
            .context("failed to begin fork transaction")?;

        // Validate the fork point INSIDE the transaction (no TOCTOU): it
        // must be an existing USER row of the source chat.
        let (role,): (String,) =
            sqlx::query_as("SELECT role FROM messages WHERE chat_id = ?1 AND id = ?2")
                .bind(source_chat_id)
                .bind(fork_point)
                .fetch_optional(&mut *tx)
                .await
                .context("failed to validate the fork point")?
                .ok_or(BadForkPoint)?;
        if role != "user" {
            return Err(BadForkPoint.into());
        }

        // 1. The new chat row carries the provenance; a fork's first
        //    activity is its creation.
        let (created_at,): (String,) = sqlx::query_as(
            "INSERT INTO chats (id, name, forked_from_chat, forked_from_message) \
             VALUES (?1, ?2, ?3, ?4) RETURNING created_at",
        )
        .bind(new_chat_id)
        .bind(new_name)
        .bind(source_chat_id)
        .bind(fork_point)
        .fetch_one(&mut *tx)
        .await
        .context("failed to insert the forked chat")?;
        sqlx::query("UPDATE chats SET last_activity_at = ?1 WHERE id = ?2")
            .bind(&created_at)
            .bind(new_chat_id)
            .execute(&mut *tx)
            .await
            .context("failed to stamp the forked chat's activity")?;

        // 2. The transcript rows, fresh ids, old→new mapped.
        #[allow(clippy::type_complexity)]
        let src_rows: Vec<(i64, String, String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT id, role, content, tool_call_id, reasoning_content \
             FROM messages WHERE chat_id = ?1 AND id < ?2 ORDER BY id",
        )
        .bind(source_chat_id)
        .bind(fork_point)
        .fetch_all(&mut *tx)
        .await
        .context("failed to read the source transcript")?;
        let mut id_map: HashMap<i64, i64> = HashMap::with_capacity(src_rows.len());
        for (old_id, role, content, tool_call_id, reasoning_content) in src_rows {
            let result = sqlx::query(
                "INSERT INTO messages (chat_id, role, content, tool_call_id, reasoning_content) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .bind(new_chat_id)
            .bind(&role)
            .bind(&content)
            .bind(&tool_call_id)
            .bind(&reasoning_content)
            .execute(&mut *tx)
            .await
            .context("failed to copy a transcript row")?;
            id_map.insert(old_id, result.last_insert_rowid());
        }

        // 3. Tool calls, remapped onto the new message ids.
        let tc_rows: Vec<(i64, String, String, String)> = sqlx::query_as(
            "SELECT tc.message_id, tc.id, tc.name, tc.arguments \
             FROM tool_calls tc JOIN messages m ON m.id = tc.message_id \
             WHERE m.chat_id = ?1 AND m.id < ?2 ORDER BY tc.message_id, tc.id",
        )
        .bind(source_chat_id)
        .bind(fork_point)
        .fetch_all(&mut *tx)
        .await
        .context("failed to read the source tool calls")?;
        for (old_msg_id, tc_id, name, arguments) in tc_rows {
            // Every source row ≤ point was copied above, so the map always
            // resolves (get() + expect would be equally total; indexed to
            // keep the hot loop simple).
            let new_msg_id = id_map[&old_msg_id];
            sqlx::query(
                "INSERT OR IGNORE INTO tool_calls (id, message_id, name, arguments) \
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(&tc_id)
            .bind(new_msg_id)
            .bind(&name)
            .bind(&arguments)
            .execute(&mut *tx)
            .await
            .context("failed to copy a tool call")?;
        }

        // 4. Buffered outputs of exactly the copied calls — the keep-set.
        sqlx::query(
            "INSERT OR IGNORE INTO buf_entries (chat_id, call_id, content) \
             SELECT ?1, call_id, content FROM buf_entries \
             WHERE chat_id = ?2 AND call_id IN (
                 SELECT tc.id FROM tool_calls tc \
                 JOIN messages m ON m.id = tc.message_id \
                 WHERE m.chat_id = ?2 AND m.id < ?3
             )",
        )
        .bind(new_chat_id)
        .bind(source_chat_id)
        .bind(fork_point)
        .execute(&mut *tx)
        .await
        .context("failed to copy the buffered outputs")?;

        // 5. Inherit the workdir pair + the provider pin. The key list is
        // explicit — a fork copies exactly what a fork needs, nothing
        // else the source's state may someday carry.
        let inherited: Vec<(String, String)> = sqlx::query_as(
            "SELECT key, value FROM state \
             WHERE chat_id = ?1 AND key IN ('workdir', 'current_dir', 'provider', 'model')",
        )
        .bind(source_chat_id)
        .fetch_all(&mut *tx)
        .await
        .context("failed to read the source state")?;
        let mut workdir: Option<String> = None;
        let mut provider: Option<String> = None;
        let mut model: Option<String> = None;
        for (key, value) in inherited {
            sqlx::query("INSERT OR REPLACE INTO state (chat_id, key, value) VALUES (?1, ?2, ?3)")
                .bind(new_chat_id)
                .bind(&key)
                .bind(&value)
                .execute(&mut *tx)
                .await
                .context("failed to copy a state row")?;
            match key.as_str() {
                "workdir" => workdir = Some(value),
                "provider" => provider = Some(value),
                "model" => model = Some(value),
                _ => {}
            }
        }
        // Every server-created chat carries a workdir (create persists it
        // atomically); its absence is store corruption, not a fork case.
        let workdir = workdir.ok_or_else(|| {
            anyhow::anyhow!("source chat {source_chat_id} has no persisted workdir")
        })?;

        tx.commit().await.context("failed to commit the fork")?;
        Ok(ForkedChat {
            created_at,
            workdir,
            provider,
            model,
        })
    }
}
