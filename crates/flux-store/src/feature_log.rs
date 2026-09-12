//! Feature decision log — records completed features so the scaffold
//! builder can seed the next feature with last-feature decisions.

use crate::{FeatureLogEntry, Store};
use anyhow::{Context, Result};

impl Store {
    /// Append a completed-feature record (called by the `feature_done`
    /// tool).
    pub async fn append_feature_log(&self, chat_id: &str, summary: &str) -> Result<()> {
        sqlx::query("INSERT INTO feature_log (chat_id, summary) VALUES (?1, ?2)")
            .bind(chat_id)
            .bind(summary)
            .execute(&self.pool)
            .await
            .context("failed to append feature log")?;
        Ok(())
    }

    /// The most recent feature records for a chat, newest first.
    pub async fn list_feature_logs(
        &self,
        chat_id: &str,
        limit: i64,
    ) -> Result<Vec<FeatureLogEntry>> {
        let rows = sqlx::query_as::<_, (i64, String, String)>(
            "SELECT id, summary, created_at FROM feature_log \
             WHERE chat_id = ?1 ORDER BY id DESC LIMIT ?2",
        )
        .bind(chat_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to query feature log")?;

        Ok(rows
            .into_iter()
            .map(|(id, summary, created_at)| FeatureLogEntry {
                id,
                summary,
                created_at,
            })
            .collect())
    }
}
