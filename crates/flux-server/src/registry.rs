//! ProviderRegistry — the SERVER's provider management, as a plain struct.
//!
//! Selection (which provider, which model) and instance building live
//! here; the chat layer receives resolved `Arc<dyn Provider>` instances
//! and only ever calls `begin` over its own materials.
//!
//! The registry's ONLY home is the server database (`providers` table) —
//! the config file carries no providers; the UI manages the registry over
//! the WS (`provider_add` / `provider_remove`). Startup hydrates the
//! in-memory map from the store (`hydrate`); every mutation persists
//! FIRST and touches memory second, so a failed write never leaves a
//! phantom entry. A server with zero providers is legal: `chat_create`
//! rejects unknown pins with `invalid_request` until the UI adds one.
//!
//! Instances are CHEAP (string assembly over a shared HTTP client — no
//! network state of their own), which is what makes per-chat pins and
//! mid-conversation model overrides inexpensive: a fresh instance per
//! resolution, model baked in.

use anyhow::Context;
use flux_core::Provider;
use flux_provider::OpenAiParams;
use flux_store::Store;
use flux_store::models::ModelRow;
use flux_store::providers::ProviderRow;
use std::sync::Arc;

use crate::models_dev::ModelsDev;

/// The provider-constructor closure type — builds a cheap provider
/// instance per resolution, pinned to the given model. The model is
/// ALWAYS explicit: registry entries are pure endpoints (id, type, url,
/// api_key) and carry no model of their own. `params` carries the saved
/// model's generation knobs (default = none); they bake into the request
/// at connection begin.
pub(crate) type ProviderCtor =
    Arc<dyn Fn(&str, OpenAiParams) -> anyhow::Result<Arc<dyn Provider>> + Send + Sync>;

/// One saved model in memory: the raw params JSON (wire-verbatim) plus its
/// lenient parse into request knobs, and the models.dev meta snapshot.
#[derive(Clone, Debug)]
pub(crate) struct ModelEntryMem {
    params_raw: serde_json::Value,
    params: OpenAiParams,
    meta: serde_json::Value,
}

/// One registered provider slot: the constructor plus the EFFECTIVE base
/// url (baked at registration) for the `provider_list` summaries. There is
/// no configured default model to remember — selection is always a
/// per-chat decision. The api_key is never exposed through the summaries.
#[derive(Clone)]
pub(crate) struct ProviderSlot {
    pub(crate) ctor: ProviderCtor,
    pub(crate) url: String,
}

pub(crate) struct ProviderRegistry {
    /// std RwLock: guards are never held across an await (`list_models`
    /// clones the ctor and drops the guard before the network call).
    providers: std::sync::RwLock<std::collections::HashMap<String, ProviderSlot>>,
    /// The LOCAL saved models (the model registry): (provider, model) →
    /// entry. Hydrated from the store; mutated persist-first like the
    /// providers map.
    models: std::sync::RwLock<std::collections::HashMap<(String, String), ModelEntryMem>>,
    /// The lazy models.dev client (saved-model auto-fill). Constructed
    /// here — the registry owns every model-catalog concern.
    models_dev: ModelsDev,
    /// The registry's persistence home. Mutations write here first.
    store: Arc<Store>,
    /// Shared HTTP client the provider instances are built over.
    client: reqwest::Client,
}

impl ProviderRegistry {
    pub(crate) fn new(store: Arc<Store>, client: reqwest::Client) -> Self {
        Self {
            providers: std::sync::RwLock::new(std::collections::HashMap::new()),
            models: std::sync::RwLock::new(std::collections::HashMap::new()),
            models_dev: ModelsDev::new(client.clone()),
            store,
            client,
        }
    }

    /// Load the persisted registry into memory (startup). A row whose
    /// protocol the server no longer supports is skipped with a warning —
    /// DB rows are server-written, so this is defensive, never expected.
    pub(crate) async fn hydrate(&self) -> anyhow::Result<usize> {
        let rows = self.store.list_providers().await?;
        let mut slots = std::collections::HashMap::new();
        for row in rows {
            match self.slot(&row) {
                Ok(slot) => {
                    slots.insert(row.id, slot);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "skipping persisted provider row");
                }
            }
        }
        let count = slots.len();
        *self.providers.write().unwrap() = slots;
        // Saved models hydrate alongside — a row whose params JSON no
        // longer parses keeps its meta but contributes no request knobs
        // (DB rows are server-written, so this is defensive, never
        // expected).
        let model_rows = self.store.list_models().await?;
        let mut models = std::collections::HashMap::new();
        for row in model_rows {
            let entry = self.model_entry(&row);
            models.insert((row.provider_id, row.model_id), entry);
        }
        *self.models.write().unwrap() = models;
        Ok(count)
    }

    /// Build the in-memory entry for one persisted row: params parsed
    /// leniently (unknown keys preserved on the raw value, ignored by the
    /// request builder), meta verbatim. Malformed JSON degrades to `{}` —
    /// a hand-edited row must not take the registry down.
    fn model_entry(&self, row: &ModelRow) -> ModelEntryMem {
        let params_raw: serde_json::Value =
            serde_json::from_str(&row.params).unwrap_or(serde_json::json!({}));
        let params: OpenAiParams = serde_json::from_value(params_raw.clone()).unwrap_or_default();
        let meta: serde_json::Value =
            serde_json::from_str(&row.meta).unwrap_or(serde_json::json!({}));
        ModelEntryMem {
            params_raw,
            params,
            meta,
        }
    }

    /// Build one slot from an endpoint row: validates the protocol and
    /// bakes the effective base url.
    fn slot(&self, row: &ProviderRow) -> anyhow::Result<ProviderSlot> {
        anyhow::ensure!(
            row.protocol == "openai",
            "provider '{}' has unknown protocol type '{}'; supported: \"openai\"",
            row.id,
            row.protocol
        );
        let url = row
            .url
            .clone()
            .unwrap_or_else(flux_provider::openai::default_base_url);
        let cfg = flux_provider::OpenAiConfig {
            base_url: url.clone(),
            api_key: row.api_key.clone(),
        };
        let client = self.client.clone();
        Ok(ProviderSlot {
            ctor: Arc::new(move |model: &str, params: OpenAiParams| {
                Ok(Arc::new(flux_provider::OpenAiProvider::new(
                    cfg.clone(),
                    model.to_string(),
                    params,
                    client.clone(),
                )?) as Arc<dyn Provider>)
            }),
            url,
        })
    }

    /// Register a new provider: validate → persist → insert. Saving never
    /// validates connectivity — a bad endpoint surfaces at the next
    /// round / model probe, in band.
    pub(crate) async fn add(
        &self,
        mut row: ProviderRow,
    ) -> anyhow::Result<flux_proto::flux::v1::ProviderSummary> {
        row.id = row.id.trim().to_string();
        anyhow::ensure!(!row.id.is_empty(), "provider id must not be empty");
        // Normalize optional fields (the UI sends "" for unset; a url
        // carries no surrounding whitespace).
        row.url = row.url.and_then(|u| {
            let t = u.trim().to_string();
            if t.is_empty() { None } else { Some(t) }
        });
        row.api_key = row.api_key.filter(|k| !k.is_empty());
        let slot = self.slot(&row)?; // protocol validation lives here
        if self.providers.read().unwrap().contains_key(&row.id) {
            anyhow::bail!("duplicate provider id: {}", row.id);
        }
        // Persist FIRST: a failed write leaves memory untouched. A racing
        // concurrent add loses the PK constraint (Ok(false)) — map it to
        // the same duplicate-id error the pre-check produces.
        if !self
            .store
            .insert_provider(&row)
            .await
            .context("failed to persist provider")?
        {
            anyhow::bail!("duplicate provider id: {}", row.id);
        }
        self.providers
            .write()
            .unwrap()
            .insert(row.id.clone(), slot.clone());
        Ok(flux_proto::flux::v1::ProviderSummary {
            id: row.id,
            url: slot.url,
        })
    }

    /// Remove a registered provider. Chats pinned to it keep their pin and
    /// fail on the next round naming the dead pin (recovery:
    /// `chat_provider`). Persist-first like `add`.
    pub(crate) async fn remove(&self, id: &str) -> anyhow::Result<()> {
        if !self.providers.read().unwrap().contains_key(id) {
            anyhow::bail!("unknown provider id: {id}");
        }
        self.store.delete_provider(id).await?;
        self.providers.write().unwrap().remove(id);
        Ok(())
    }

    /// Resolve one request's pin into a ready-to-use instance: unknown ids
    /// reject, and an empty model rejects (no default exists to fill in —
    /// the wire types require the field, so an empty string is the only
    /// under-specified shape left). There is no omitted-id path —
    /// selection is always explicit.
    pub(crate) fn instance(
        &self,
        id: &str,
        model: &str,
    ) -> anyhow::Result<(Arc<dyn Provider>, String, String)> {
        let ctor = {
            let providers = self.providers.read().unwrap();
            providers
                .get(id)
                .with_context(|| format!("unknown provider id: {id}"))?
                .ctor
                .clone()
        };
        let model = model.trim();
        anyhow::ensure!(
            !model.is_empty(),
            "model is required — providers carry no default; pin one explicitly"
        );
        // A saved row's knobs ride along; an unsaved model string runs on
        // the upstream defaults (the registry is a convenience, never a
        // gate — model strings stay free-form).
        let params = {
            let models = self.models.read().unwrap();
            models
                .get(&(id.to_string(), model.to_string()))
                .map(|m| m.params.clone())
                .unwrap_or_default()
        };
        let provider = (ctor)(model, params)?;
        Ok((provider, id.to_string(), model.to_string()))
    }

    /// Whether the models.dev client is exposed (the session layer drives
    /// explicit syncs through it).
    pub(crate) fn models_dev(&self) -> &ModelsDev {
        &self.models_dev
    }

    /// The effective base url of one provider (the models.dev host
    /// heuristic anchors on it). Unknown provider: None.
    pub(crate) fn provider_url(&self, id: &str) -> Option<String> {
        self.providers
            .read()
            .unwrap()
            .get(id)
            .map(|s| s.url.clone())
    }

    /// Whether a saved model row exists.
    pub(crate) async fn model_exists(&self, provider: &str, model: &str) -> anyhow::Result<bool> {
        Ok(self
            .store
            .list_models()
            .await?
            .iter()
            .any(|r| r.provider_id == provider && r.model_id == model))
    }

    /// The stored meta snapshot of one row (`{}` when absent).
    pub(crate) async fn model_meta(
        &self,
        provider: &str,
        model: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let meta = self
            .store
            .list_models()
            .await?
            .iter()
            .find(|r| r.provider_id == provider && r.model_id == model)
            .map(|r| r.meta.clone())
            .unwrap_or_else(|| "{}".to_string());
        Ok(serde_json::from_str(&meta).unwrap_or(serde_json::json!({})))
    }

    /// Persist one saved model (create or edit — the caller resolved the
    /// create-path enrichment already; this writes BOTH columns verbatim)
    /// and refresh the memory map. The provider must exist (the FK also
    /// enforces it; this gives the UI a readable error).
    pub(crate) async fn save_model(
        &self,
        provider: &str,
        model: &str,
        params: &serde_json::Value,
        meta: &serde_json::Value,
    ) -> anyhow::Result<()> {
        validate_model_params(params)?;
        anyhow::ensure!(
            self.providers.read().unwrap().contains_key(provider),
            "unknown provider id: {provider}"
        );
        let row = ModelRow {
            provider_id: provider.to_string(),
            model_id: model.to_string(),
            params: serde_json::to_string(params)?,
            meta: serde_json::to_string(meta)?,
        };
        // Persist FIRST, memory second (the providers-map discipline).
        self.store.upsert_model(&row).await?;
        let entry = self.model_entry(&row);
        self.models
            .write()
            .unwrap()
            .insert((provider.to_string(), model.to_string()), entry);
        Ok(())
    }

    /// Remove one saved model (persist-first). An unknown row errors.
    pub(crate) async fn remove_model(&self, provider: &str, model: &str) -> anyhow::Result<()> {
        if !self
            .models
            .read()
            .unwrap()
            .contains_key(&(provider.to_string(), model.to_string()))
        {
            anyhow::bail!("unknown model: {provider}/{model}");
        }
        self.store.delete_model(provider, model).await?;
        self.models
            .write()
            .unwrap()
            .remove(&(provider.to_string(), model.to_string()));
        Ok(())
    }

    /// Rewritten meta for one row (the models.dev sync path) — persist +
    /// memory. A row that vanished mid-sync reports Ok(false).
    pub(crate) async fn update_model_meta(
        &self,
        provider: &str,
        model: &str,
        meta: &serde_json::Value,
    ) -> anyhow::Result<bool> {
        let changed = self
            .store
            .update_model_meta(provider, model, &serde_json::to_string(meta)?)
            .await?;
        if changed
            && let Some(entry) = self
                .models
                .write()
                .unwrap()
                .get_mut(&(provider.to_string(), model.to_string()))
        {
            entry.meta = meta.clone();
        }
        Ok(changed)
    }

    /// All (provider, model) saved pairs — the sync scope iterator.
    pub(crate) async fn saved_model_pairs(&self) -> anyhow::Result<Vec<(String, String)>> {
        Ok(self
            .store
            .list_models()
            .await?
            .into_iter()
            .map(|r| (r.provider_id, r.model_id))
            .collect())
    }

    /// Saved-model summaries for the wire (`model_list` reply / `models`
    /// broadcast), sorted by (provider, model). Params + meta verbatim.
    pub(crate) fn model_summaries(&self) -> Vec<flux_proto::flux::v1::ModelSummary> {
        let models = self.models.read().unwrap();
        let mut out: Vec<flux_proto::flux::v1::ModelSummary> = models
            .iter()
            .map(
                |((provider, model), entry)| flux_proto::flux::v1::ModelSummary {
                    provider: provider.clone(),
                    model: model.clone(),
                    // Verbatim JSON passthrough (the proto fields are string-
                    // encoded open objects — byte-for-byte is the contract).
                    params_json: entry.params_raw.to_string(),
                    meta_json: entry.meta.to_string(),
                },
            )
            .collect();
        out.sort_by(|a, b| {
            (a.provider.clone(), a.model.clone()).cmp(&(b.provider.clone(), b.model.clone()))
        });
        out
    }

    /// Registry summaries for the wire (`provider_list` reply payload),
    /// sorted by id. Ids + effective urls — the api_key NEVER leaves the
    /// server.
    pub(crate) fn summaries(&self) -> Vec<flux_proto::flux::v1::ProviderSummary> {
        let mut out: Vec<flux_proto::flux::v1::ProviderSummary> = self
            .providers
            .read()
            .unwrap()
            .iter()
            .map(|(id, slot)| flux_proto::flux::v1::ProviderSummary {
                id: id.clone(),
                url: slot.url.clone(),
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Probe one provider's upstream model catalog (the OpenAI-compatible
    /// `GET /models`). The probe failure rides the result in band — never
    /// the global error channel.
    pub(crate) async fn list_models(
        &self,
        id: &str,
    ) -> (String, Result<Vec<flux_core::ModelInfo>, String>) {
        let ctor = {
            let providers = self.providers.read().unwrap();
            match providers.get(id) {
                Some(slot) => slot.ctor.clone(),
                None => return (id.to_string(), Err(format!("unknown provider id: {id}"))),
            }
        };
        // The catalog probe is model-agnostic (`GET /models` never
        // references the pin) — build the probe instance with the empty
        // placeholder rather than inventing a model; probes carry no
        // generation params.
        let provider = match (ctor)("", OpenAiParams::default()) {
            Ok(p) => p,
            Err(e) => return (id.to_string(), Err(e.to_string())),
        };
        match provider.list_models().await {
            Ok(models) => (id.to_string(), Ok(models)),
            Err(e) => (id.to_string(), Err(e.to_string())),
        }
    }
}

/// Light validation of the client-submitted params: an object whose known
/// keys carry sane values. Unknown keys pass through (preserved on the
/// row, ignored by the request builder — forward compatibility).
fn validate_model_params(params: &serde_json::Value) -> anyhow::Result<()> {
    let obj = params
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("params must be a JSON object"))?;
    for (key, value) in obj {
        match key.as_str() {
            "temperature" => {
                let t = value
                    .as_f64()
                    .with_context(|| "temperature must be a number")?;
                anyhow::ensure!(
                    (0.0..=2.0).contains(&t),
                    "temperature must be within 0..=2, got {t}"
                );
            }
            "top_p" => {
                let p = value.as_f64().with_context(|| "top_p must be a number")?;
                anyhow::ensure!(p > 0.0 && p <= 1.0, "top_p must be within (0, 1], got {p}");
            }
            "max_tokens" | "context_length" => {
                let n = value
                    .as_u64()
                    .with_context(|| format!("{key} must be a positive integer"))?;
                anyhow::ensure!(n > 0, "{key} must be positive");
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn registry_with(ids: &[&str]) -> ProviderRegistry {
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let registry = ProviderRegistry::new(store, reqwest::Client::new());
        for id in ids {
            registry
                .add(ProviderRow {
                    id: id.to_string(),
                    protocol: "openai".to_string(),
                    url: Some(format!("http://{id}.invalid/v1")),
                    api_key: Some("sk-test".to_string()),
                })
                .await
                .unwrap();
        }
        registry
    }

    #[tokio::test]
    async fn instance_resolves_id_and_model_label() {
        let reg = registry_with(&["a", "b"]).await;
        // The pin is fully explicit: the label IS the requested model —
        // no configured default fills anything in.
        let (_, id, label) = reg.instance("b", "custom").unwrap();
        assert_eq!(id, "b");
        assert_eq!(label, "custom");
    }

    #[tokio::test]
    async fn instance_rejects_unknown_ids() {
        let reg = registry_with(&["a"]).await;
        let err = match reg.instance("no-such", "m") {
            Ok(_) => panic!("unknown id must reject"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("unknown provider id"));
    }

    #[tokio::test]
    async fn instance_rejects_an_empty_model() {
        // No default exists to fill in — an empty pin is an explicit error,
        // never a silent fallback.
        let reg = registry_with(&["a"]).await;
        let err = match reg.instance("a", "  ") {
            Ok(_) => panic!("empty model must reject"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("model is required"));
    }

    #[tokio::test]
    async fn summaries_are_sorted_and_carry_the_effective_url() {
        let reg = registry_with(&["b", "a"]).await;
        let sums = reg.summaries();
        let ids: Vec<&str> = sums.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
        assert_eq!(sums[0].url, "http://a.invalid/v1");
    }

    #[tokio::test]
    async fn add_validates_the_input() {
        let reg = registry_with(&[]).await;
        let row = |id: &str, protocol: &str| ProviderRow {
            id: id.to_string(),
            protocol: protocol.to_string(),
            url: None,
            api_key: None,
        };
        // Empty id (after trim).
        let err = reg.add(row("  ", "openai")).await.unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
        // Unknown protocol.
        let err = reg.add(row("x", "anthropic")).await.unwrap_err();
        assert!(err.to_string().contains("unknown protocol type"));
        // A missing url falls back to the shared default.
        let sum = reg.add(row("main", "openai")).await.unwrap();
        assert_eq!(sum.url, flux_provider::openai::default_base_url());
        // Duplicate id — the pre-check path.
        let err = reg.add(row("main", "openai")).await.unwrap_err();
        assert!(err.to_string().contains("duplicate provider id"));
    }

    #[tokio::test]
    async fn add_then_remove_round_trip() {
        let reg = registry_with(&["main"]).await;
        reg.remove("main").await.unwrap();
        assert!(reg.summaries().is_empty());
        // Gone from memory AND the store — re-adding works.
        let err = reg.remove("main").await.unwrap_err();
        assert!(err.to_string().contains("unknown provider id"));
        reg.add(ProviderRow {
            id: "main".to_string(),
            protocol: "openai".to_string(),
            url: None,
            api_key: None,
        })
        .await
        .unwrap();
        assert_eq!(reg.summaries().len(), 1);
    }

    #[tokio::test]
    async fn hydrate_restores_the_persisted_registry() {
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let reg = ProviderRegistry::new(store.clone(), reqwest::Client::new());
        reg.add(ProviderRow {
            id: "main".to_string(),
            protocol: "openai".to_string(),
            url: Some("http://main.invalid/v1".to_string()),
            api_key: None,
        })
        .await
        .unwrap();
        // A fresh registry over the SAME store sees the persisted row.
        let reg2 = ProviderRegistry::new(store, reqwest::Client::new());
        assert_eq!(reg2.hydrate().await.unwrap(), 1);
        let sums = reg2.summaries();
        assert_eq!(sums[0].id, "main");
        assert_eq!(sums[0].url, "http://main.invalid/v1");
    }

    // ── Model registry ────────────────────────────────────────────────

    #[tokio::test]
    async fn model_save_requires_a_known_provider_and_valid_params() {
        let reg = registry_with(&["a"]).await;
        // Unknown provider.
        let err = reg
            .save_model("nope", "m", &serde_json::json!({}), &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown provider id"));
        // Bad params.
        let err = reg
            .save_model(
                "a",
                "m",
                &serde_json::json!({"temperature": 5.0}),
                &serde_json::json!({}),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("within 0..=2"));
        // Non-object params.
        let err = reg
            .save_model("a", "m", &serde_json::json!(3), &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("JSON object"));
    }

    #[tokio::test]
    async fn model_save_then_instance_bakes_params() {
        let reg = registry_with(&["a"]).await;
        reg.save_model(
            "a",
            "gpt-4o",
            &serde_json::json!({"temperature": 0.3, "max_tokens": 4096}),
            &serde_json::json!({"name": "GPT-4o"}),
        )
        .await
        .unwrap();
        // The pin resolves through a saved row's params; an unsaved model
        // string keeps running on the defaults.
        let (_, _, _) = reg.instance("a", "gpt-4o").unwrap();
        let (_, _, _) = reg.instance("a", "unsaved").unwrap();
        // The summaries carry params + meta verbatim, sorted.
        let sums = reg.model_summaries();
        assert_eq!(sums.len(), 1);
        assert_eq!(sums[0].model, "gpt-4o");
        let params: serde_json::Value = serde_json::from_str(&sums[0].params_json).unwrap();
        assert_eq!(params["temperature"], 0.3);
        let meta: serde_json::Value = serde_json::from_str(&sums[0].meta_json).unwrap();
        assert_eq!(meta["name"], "GPT-4o");
    }

    #[tokio::test]
    async fn model_remove_then_summaries_shrink() {
        let reg = registry_with(&["a"]).await;
        reg.save_model("a", "m", &serde_json::json!({}), &serde_json::json!({}))
            .await
            .unwrap();
        reg.remove_model("a", "m").await.unwrap();
        assert!(reg.model_summaries().is_empty());
        // Removing again errors (unknown row).
        let err = reg.remove_model("a", "m").await.unwrap_err();
        assert!(err.to_string().contains("unknown model"));
    }

    #[tokio::test]
    async fn model_rows_survive_a_fresh_hydrate() {
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let reg = ProviderRegistry::new(store.clone(), reqwest::Client::new());
        reg.add(ProviderRow {
            id: "main".to_string(),
            protocol: "openai".to_string(),
            url: None,
            api_key: Some("sk-test".to_string()),
        })
        .await
        .unwrap();
        reg.save_model(
            "main",
            "m",
            &serde_json::json!({"context_length": 32768}),
            &serde_json::json!({"source": "models.dev"}),
        )
        .await
        .unwrap();
        // A fresh registry over the SAME store sees the row (and its
        // params ride instance resolution).
        let reg2 = ProviderRegistry::new(store, reqwest::Client::new());
        reg2.hydrate().await.unwrap();
        assert_eq!(reg2.model_summaries().len(), 1);
        let (_, _, _) = reg2.instance("main", "m").unwrap();
    }

    #[tokio::test]
    async fn list_models_reports_failures_in_band() {
        let reg = registry_with(&["a"]).await;
        let (id, result) = reg.list_models("no-such").await;
        assert_eq!(id, "no-such");
        assert!(result.is_err());
    }
}
