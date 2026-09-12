//! Runtime state — per-chat key-value persistence for StateManager.

use crate::Store;
use anyhow::{Context, Result};
use std::collections::HashMap;

impl Store {
    /// Load all persisted state entries for a given chat.
    pub async fn load_state(&self, chat_id: &str) -> Result<HashMap<String, String>> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT key, value FROM state WHERE chat_id = ?1")
                .bind(chat_id)
                .fetch_all(&self.pool)
                .await
                .context("failed to query state")?;

        let mut map = HashMap::new();
        for (k, v) in rows {
            map.insert(k, v);
        }
        Ok(map)
    }

    /// Persist a single state entry for a given chat (upsert).
    pub async fn save_state_entry(&self, chat_id: &str, key: &str, value: &str) -> Result<()> {
        sqlx::query("INSERT OR REPLACE INTO state (chat_id, key, value) VALUES (?1, ?2, ?3)")
            .bind(chat_id)
            .bind(key)
            .bind(value)
            .execute(&self.pool)
            .await
            .context("failed to save state entry")?;
        Ok(())
    }

    /// Save multiple state entries in ONE transaction — all-or-nothing.
    /// For entries that form a single semantic unit (the swap's
    /// provider+model pin pair, the creation's workdir+current_dir pair):
    /// a torn pair (provider persisted, model stale) would survive a
    /// respawn with mismatched halves. Entries with DIFFERENT failure
    /// policies stay separate calls — this helper has one outcome.
    pub async fn save_state_entries(&self, chat_id: &str, entries: &[(&str, &str)]) -> Result<()> {
        let mut tx = self
            .pool
            .begin()
            .await
            .context("failed to begin state transaction")?;
        for (key, value) in entries {
            sqlx::query("INSERT OR REPLACE INTO state (chat_id, key, value) VALUES (?1, ?2, ?3)")
                .bind(chat_id)
                .bind(key)
                .bind(value)
                .execute(&mut *tx)
                .await
                .with_context(|| format!("failed to save state entry {key}"))?;
        }
        tx.commit()
            .await
            .context("failed to commit state transaction")
    }
}
