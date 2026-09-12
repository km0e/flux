//! Local model-registry persistence.
//!
//! One row per (provider, model): the user's SAVED model with editable
//! request params and an optional models.dev metadata snapshot. The two
//! JSON columns carry SEPARATE write authority — `params` is written by
//! the client's `model_save` (edits never touch `meta`), `meta` only by
//! the server's import/refresh path (models.dev matches) — so an edit can
//! never clobber enrichment and a refresh can never clobber user values.
//!
//! The API-catalog probe results are NOT stored here: the catalog stays
//! probe-transient (frontend cache); importing copies a row into this
//! table. Provider deletion cascades (the FK), matching the rule that a
//! chat pinned to a dead provider fails its next round anyway.

use super::Store;
use anyhow::Result;

/// One saved-model row. `params` / `meta` are opaque JSON blobs at this
/// layer (the registry parses `params` into `OpenAiParams`; `meta` is
/// display-only and forwarded verbatim to the wire).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRow {
    pub provider_id: String,
    pub model_id: String,
    pub params: String,
    pub meta: String,
}

impl Store {
    /// All saved models (unsorted — ordering is a consumer concern).
    pub async fn list_models(&self) -> Result<Vec<ModelRow>> {
        let rows: Vec<(String, String, String, String)> =
            sqlx::query_as("SELECT provider_id, model_id, params, meta FROM models")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|(provider_id, model_id, params, meta)| ModelRow {
                provider_id,
                model_id,
                params,
                meta,
            })
            .collect())
    }

    /// Insert or update one saved model. UPSERT semantics: `model_save` is
    /// both the create and the edit path (conflict-avoidance for imports is
    /// the CALLER's job — it checks existence first and skips). `params`
    /// replaces the stored value; `meta` is preserved untouched on update
    /// (only the refresh path writes it, via [`Store::update_model_meta`]).
    pub async fn upsert_model(&self, row: &ModelRow) -> Result<bool> {
        let result = sqlx::query(
            "INSERT INTO models (provider_id, model_id, params, meta) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(provider_id, model_id) DO UPDATE SET params = excluded.params",
        )
        .bind(&row.provider_id)
        .bind(&row.model_id)
        .bind(&row.params)
        .bind(&row.meta)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Rewrite ONLY the metadata snapshot (the models.dev refresh path).
    /// Params stay exactly as the user left them.
    pub async fn update_model_meta(
        &self,
        provider_id: &str,
        model_id: &str,
        meta: &str,
    ) -> Result<bool> {
        let result =
            sqlx::query("UPDATE models SET meta = ?3 WHERE provider_id = ?1 AND model_id = ?2")
                .bind(provider_id)
                .bind(model_id)
                .bind(meta)
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Delete one saved model. Returns whether the row existed.
    pub async fn delete_model(&self, provider_id: &str, model_id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM models WHERE provider_id = ?1 AND model_id = ?2")
            .bind(provider_id)
            .bind(model_id)
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

    async fn with_provider() -> Store {
        let store = test_store().await;
        store
            .insert_provider(&super::super::providers::ProviderRow {
                id: "main".to_string(),
                protocol: "openai".to_string(),
                url: None,
                api_key: None,
            })
            .await
            .unwrap();
        store
    }

    fn row(provider: &str, model: &str) -> ModelRow {
        ModelRow {
            provider_id: provider.to_string(),
            model_id: model.to_string(),
            params: r#"{"temperature":0.7}"#.to_string(),
            meta: r#"{"name":"GPT-4o"}"#.to_string(),
        }
    }

    #[tokio::test]
    async fn models_round_trip() {
        let store = with_provider().await;
        assert!(store.list_models().await.unwrap().is_empty());
        store.upsert_model(&row("main", "gpt-4o")).await.unwrap();
        store
            .upsert_model(&row("main", "deepseek-chat"))
            .await
            .unwrap();
        let mut rows = store.list_models().await.unwrap();
        rows.sort_by(|a, b| a.model_id.cmp(&b.model_id));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].model_id, "deepseek-chat");
        assert_eq!(rows[0].params, r#"{"temperature":0.7}"#);
    }

    #[tokio::test]
    async fn upsert_edits_params_only_and_preserves_meta() {
        let store = with_provider().await;
        store.upsert_model(&row("main", "gpt-4o")).await.unwrap();
        // The edit path replaces params; meta must survive untouched.
        store
            .upsert_model(&ModelRow {
                provider_id: "main".into(),
                model_id: "gpt-4o".into(),
                params: "{}".to_string(),
                meta: r#"{"name":"SHOULD NOT LAND"}"#.to_string(),
            })
            .await
            .unwrap();
        let rows = store.list_models().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].params, "{}");
        assert_eq!(rows[0].meta, r#"{"name":"GPT-4o"}"#);
    }

    #[tokio::test]
    async fn meta_refresh_writes_meta_only() {
        let store = with_provider().await;
        store.upsert_model(&row("main", "gpt-4o")).await.unwrap();
        let changed = store
            .update_model_meta(
                "main",
                "gpt-4o",
                r#"{"name":"Refreshed","cost":{"input":2.5}}"#,
            )
            .await
            .unwrap();
        assert!(changed);
        let rows = store.list_models().await.unwrap();
        assert_eq!(rows[0].params, r#"{"temperature":0.7}"#);
        assert!(rows[0].meta.contains("Refreshed"));
        // An unknown row reports not-changed.
        assert!(
            !store
                .update_model_meta("main", "no-such", "{}")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn delete_reports_existence() {
        let store = with_provider().await;
        store.upsert_model(&row("main", "gpt-4o")).await.unwrap();
        assert!(store.delete_model("main", "gpt-4o").await.unwrap());
        assert!(!store.delete_model("main", "gpt-4o").await.unwrap());
        assert!(store.list_models().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn provider_deletion_cascades() {
        let store = with_provider().await;
        store.upsert_model(&row("main", "gpt-4o")).await.unwrap();
        store.delete_provider("main").await.unwrap();
        assert!(store.list_models().await.unwrap().is_empty());
    }
}
