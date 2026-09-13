//! Chat metadata — CRUD operations on the `chats` table.

use crate::{ChatSummary, Store};
use anyhow::{Context, Result};

impl Store {
    /// Insert a brand-new chat and return the DB-generated `created_at`.
    /// `last_activity_at` starts equal to it (a chat's first activity is
    /// its creation) — both carry the same DEFAULT, so one INSERT suffices.
    pub async fn insert_chat(&self, chat_id: &str, name: &str) -> Result<String> {
        let (created_at,): (String,) =
            sqlx::query_as("INSERT INTO chats (id, name) VALUES (?1, ?2) RETURNING created_at")
                .bind(chat_id)
                .bind(name)
                .fetch_one(&self.pool)
                .await
                .context("failed to insert chat")?;
        Ok(created_at)
    }

    pub async fn rename_chat(&self, chat_id: &str, name: &str) -> Result<()> {
        sqlx::query("UPDATE chats SET name = ?1 WHERE id = ?2")
            .bind(name)
            .bind(chat_id)
            .execute(&self.pool)
            .await
            .context("failed to rename chat")?;
        Ok(())
    }

    pub async fn delete_chat(&self, chat_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM chats WHERE id = ?1")
            .bind(chat_id)
            .execute(&self.pool)
            .await
            .context("failed to delete chat")?;
        Ok(())
    }

    /// One query for the chat list plus its `state`-table metadata (`kind`,
    /// `workdir`, `provider`, `model`) — LEFT JOINs on the keyed rows, so
    /// startup cache population and `ChatInfo` rendering need no per-chat
    /// round trips. Ordered by LAST ACTIVITY (the sidebar is a recency
    /// list, not a creation log; creation order is the fallback for rows
    /// the backfill somehow missed).
    pub async fn list_chats(&self) -> Result<Vec<ChatSummary>> {
        let rows = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
            ),
        >(
            "SELECT c.id, c.name, c.created_at, c.last_activity_at, c.forked_from_chat, w.value, p.value, m.value \
             FROM chats c \
             LEFT JOIN state w ON w.chat_id = c.id AND w.key = 'workdir' \
             LEFT JOIN state p ON p.chat_id = c.id AND p.key = 'provider' \
             LEFT JOIN state m ON m.chat_id = c.id AND m.key = 'model' \
             ORDER BY COALESCE(c.last_activity_at, c.created_at) DESC",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to query chats")?;

        Ok(rows
            .into_iter()
            .map(
                |(
                    chat_id,
                    name,
                    created_at,
                    last_activity_at,
                    forked_from_chat,
                    workdir,
                    provider,
                    model,
                )| {
                    // A row predating the backfill falls back to creation time.
                    let activity = last_activity_at.unwrap_or_else(|| created_at.clone());
                    ChatSummary {
                        chat_id,
                        name,
                        created_at,
                        last_activity_at: activity,
                        forked_from_chat,
                        workdir,
                        provider,
                        model,
                    }
                },
            )
            .collect())
    }
}
