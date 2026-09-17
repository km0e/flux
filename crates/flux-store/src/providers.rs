//! Provider-registry persistence.
//!
//! The registry's ONLY home — the config file carries no providers. One
//! row per provider: a pure endpoint (id, interface type, url, api_key)
//! with NO model (the model is a required per-chat pin). The server
//! hydrates its in-memory registry from this table at startup; mutations
//! (the UI's provider_add / provider_remove) write here FIRST and only
//! then touch memory, so a failed write never leaves a phantom entry.

use super::Store;
use anyhow::Result;

/// One provider-registry row — a pure endpoint quadruple. `protocol` maps
/// the `type` column (the wire name; `type` is a Rust keyword). `url` /
/// `api_key` are optional: an absent url means the OpenAI default base
/// url (filled by the registry at construction).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRow {
    pub id: String,
    pub protocol: String,
    pub url: Option<String>,
    pub api_key: Option<String>,
}

impl Store {
    /// All registered providers (unsorted — ordering is a consumer concern).
    pub async fn list_providers(&self) -> Result<Vec<ProviderRow>> {
        let rows: Vec<(String, String, Option<String>, Option<String>)> =
            sqlx::query_as("SELECT id, type, url, api_key FROM providers")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|(id, protocol, url, api_key)| ProviderRow {
                id,
                protocol,
                url,
                api_key,
            })
            .collect())
    }

    /// Insert one provider row. Returns `Ok(false)` on a duplicate `id`
    /// (the UNIQUE constraint is the race guard for concurrent adds — the
    /// registry pre-checks, this catches the interleaving); other database
    /// errors propagate.
    pub async fn insert_provider(&self, row: &ProviderRow) -> Result<bool> {
        match sqlx::query("INSERT INTO providers (id, type, url, api_key) VALUES (?1, ?2, ?3, ?4)")
            .bind(&row.id)
            .bind(&row.protocol)
            .bind(&row.url)
            .bind(&row.api_key)
            .execute(&self.pool)
            .await
        {
            Ok(_) => Ok(true),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete one provider row. Returns whether the row existed.
    pub async fn delete_provider(&self, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM providers WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// One provider row by id — the registry's edit path reads the CURRENT
    /// row here (the api_key tri-state merges against it; the key never
    /// lives in memory outside the ctor closure).
    pub async fn get_provider(&self, id: &str) -> Result<Option<ProviderRow>> {
        let row: Option<(String, String, Option<String>, Option<String>)> =
            sqlx::query_as("SELECT id, type, url, api_key FROM providers WHERE id = ?1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(id, protocol, url, api_key)| ProviderRow {
            id,
            protocol,
            url,
            api_key,
        }))
    }

    /// Overwrite one provider row (url / api_key / type; the id is the
    /// WHERE key, never a written column). Returns whether the row
    /// existed — the race guard for a concurrent delete between the
    /// registry's read and this write.
    pub async fn update_provider(&self, row: &ProviderRow) -> Result<bool> {
        let result =
            sqlx::query("UPDATE providers SET type = ?2, url = ?3, api_key = ?4 WHERE id = ?1")
                .bind(&row.id)
                .bind(&row.protocol)
                .bind(&row.url)
                .bind(&row.api_key)
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_store() -> Store {
        Store::open_in_memory().await.unwrap()
    }

    fn row(id: &str) -> ProviderRow {
        ProviderRow {
            id: id.to_string(),
            protocol: "openai".to_string(),
            url: Some(format!("https://{id}.example.com/v1")),
            api_key: Some("sk-test".to_string()),
        }
    }

    #[tokio::test]
    async fn providers_round_trip() {
        let store = test_store().await;
        assert!(store.list_providers().await.unwrap().is_empty());
        store.insert_provider(&row("main")).await.unwrap();
        store
            .insert_provider(&ProviderRow {
                id: "local".to_string(),
                protocol: "openai".to_string(),
                url: None,
                api_key: None,
            })
            .await
            .unwrap();
        let mut providers = store.list_providers().await.unwrap();
        providers.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(providers.len(), 2);
        // The optionality round-trips: absent url/api_key stay None.
        assert_eq!(
            providers[0],
            ProviderRow {
                id: "local".to_string(),
                protocol: "openai".to_string(),
                url: None,
                api_key: None,
            }
        );
        assert_eq!(providers[1], row("main"));
    }

    #[tokio::test]
    async fn duplicate_id_reports_false() {
        let store = test_store().await;
        assert!(store.insert_provider(&row("main")).await.unwrap());
        assert!(!store.insert_provider(&row("main")).await.unwrap());
    }

    #[tokio::test]
    async fn delete_reports_existence() {
        let store = test_store().await;
        store.insert_provider(&row("main")).await.unwrap();
        assert!(store.delete_provider("main").await.unwrap());
        assert!(!store.delete_provider("main").await.unwrap());
        assert!(store.list_providers().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn update_replaces_fields_id_stays_the_key() {
        let store = test_store().await;
        store.insert_provider(&row("main")).await.unwrap();
        // Full-row overwrite: type / url / api_key all land; the id is the
        // WHERE key — a renamed row would be a different row.
        assert!(
            store
                .update_provider(&ProviderRow {
                    id: "main".to_string(),
                    protocol: "openai".to_string(),
                    url: Some("https://new.example.com/v1".to_string()),
                    api_key: Some("sk-new".to_string()),
                })
                .await
                .unwrap()
        );
        assert_eq!(
            store.get_provider("main").await.unwrap().unwrap(),
            ProviderRow {
                id: "main".to_string(),
                protocol: "openai".to_string(),
                url: Some("https://new.example.com/v1".to_string()),
                api_key: Some("sk-new".to_string()),
            }
        );
    }

    #[tokio::test]
    async fn update_unknown_id_reports_false() {
        let store = test_store().await;
        assert!(!store.update_provider(&row("ghost")).await.unwrap());
    }

    #[tokio::test]
    async fn get_provider_round_trips_optionality() {
        let store = test_store().await;
        assert!(store.get_provider("main").await.unwrap().is_none());
        store
            .insert_provider(&ProviderRow {
                id: "main".to_string(),
                protocol: "openai".to_string(),
                url: None,
                api_key: None,
            })
            .await
            .unwrap();
        let got = store.get_provider("main").await.unwrap().unwrap();
        assert_eq!(got.id, "main");
        assert_eq!(got.url, None);
        assert_eq!(got.api_key, None);
    }
}
