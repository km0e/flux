//! Models.dev catalog integration — the auto-fill source for saved models.
//!
//! models.dev publishes a public catalog of LLM metadata (context window,
//! max output, capability flags, pricing) at `https://models.dev/api.json`
//! — a plain GET, no auth, no user data attached. The server fetches it
//! LAZILY (never at startup — a deployment that never saves a model never
//! calls out), caches in memory with a TTL, and single-flights concurrent
//! fetches. Matching a (base_url, model id) pair is BEST-EFFORT: a miss or
//! a fetch failure never blocks a save/import — the row simply lands
//! without enrichment.
//!
//! Matching is two-layer: a static host→provider table disambiguates which
//! models.dev section to search first (registry ids like "main" never
//! match models.dev provider keys; base urls often do), then the model id
//! is matched exactly with two normalization fallbacks (a `vendor/` prefix
//! and a `:tag` suffix are stripped). Determinism: the global fallback
//! walks provider keys in sorted order.

use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const MODELS_DEV_URL: &str = "https://models.dev/api.json";
/// Catalog TTL — the upstream catalog changes slowly; a day is plenty.
const CATALOG_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// One models.dev model entry — only the fields the meta snapshot keeps.
/// Unknown fields are ignored (forward-compatible); absent capability
/// booleans default false (conservative display).
#[derive(Debug, Clone, Deserialize)]
pub struct ModelEntry {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub tool_call: bool,
    /// Whether the model supports temperature control at all (models.dev
    /// carries no default VALUE — the knob stays user-set).
    #[serde(default)]
    pub temperature: bool,
    #[serde(default)]
    pub attachment: bool,
    #[serde(default)]
    pub knowledge: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub limit: Option<Limit>,
    #[serde(default)]
    pub cost: Option<Cost>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Limit {
    #[serde(default)]
    pub context: Option<u64>,
    #[serde(default)]
    pub output: Option<u64>,
}

/// USD per million tokens.
#[derive(Debug, Clone, Deserialize)]
pub struct Cost {
    #[serde(default)]
    pub input: Option<f64>,
    #[serde(default)]
    pub output: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
}

/// The parsed catalog: provider key → model id → entry.
pub type Catalog = HashMap<String, HashMap<String, ModelEntry>>;

struct Cached {
    fetched_at: Instant,
    catalog: Catalog,
}

/// The lazy models.dev client. Cheap to clone conceptually — one instance
/// lives on the provider registry.
pub struct ModelsDev {
    client: reqwest::Client,
    cache: tokio::sync::RwLock<Option<Cached>>,
    /// Single-flight guard: concurrent fetchers share one round-trip (the
    /// loser re-reads the cache after the winner swaps it in).
    fetch_lock: tokio::sync::Mutex<()>,
}

impl ModelsDev {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            cache: tokio::sync::RwLock::new(None),
            fetch_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// The catalog, fresh per the TTL (or force-refreshed for an explicit
    /// user sync). The fetch failure surfaces as Err — the CALLER decides
    /// it is non-fatal.
    pub async fn catalog(&self, force: bool) -> Result<Catalog, String> {
        if !force {
            let cached = self.cache.read().await;
            if let Some(c) = cached
                .as_ref()
                .filter(|c| c.fetched_at.elapsed() < CATALOG_TTL)
            {
                return Ok(c.catalog.clone());
            }
        }
        let _guard = self.fetch_lock.lock().await;
        // Re-check under the lock: a concurrent fetcher may have refreshed.
        {
            let cached = self.cache.read().await;
            if let Some(c) = cached
                .as_ref()
                .filter(|c| !force && c.fetched_at.elapsed() < CATALOG_TTL)
            {
                return Ok(c.catalog.clone());
            }
        }
        let response = self
            .client
            .get(MODELS_DEV_URL)
            .send()
            .await
            .map_err(|e| format!("models.dev request failed: {e}"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| format!("models.dev read failed: {e}"))?;
        if !status.is_success() {
            let excerpt: String = body.chars().take(256).collect();
            return Err(format!("models.dev HTTP {status}: {excerpt}"));
        }
        let catalog = parse_catalog(&body)?;
        *self.cache.write().await = Some(Cached {
            fetched_at: Instant::now(),
            catalog: catalog.clone(),
        });
        Ok(catalog)
    }

    /// Resolve one (base_url, model id) pair into the meta snapshot to
    /// store. Ok(None) = no match; Err = the catalog fetch failed.
    pub async fn meta_for(
        &self,
        base_url: &str,
        model_id: &str,
        force: bool,
    ) -> Result<Option<serde_json::Value>, String> {
        let catalog = self.catalog(force).await?;
        Ok(match_entry(&catalog, base_url, model_id)
            .map(|(provider_key, id, entry)| build_meta(&provider_key, &id, entry)))
    }
}

/// Parse the catalog body. The envelope is `{provider_key: {models: {...}}}`
/// — sections without a `models` map are skipped, not fatal.
fn parse_catalog(body: &str) -> Result<Catalog, String> {
    let raw: HashMap<String, ProviderSection> =
        serde_json::from_str(body).map_err(|e| format!("models.dev parse: {e}"))?;
    Ok(raw.into_iter().map(|(k, v)| (k, v.models)).collect())
}

#[derive(Debug, Deserialize)]
struct ProviderSection {
    #[serde(default)]
    models: HashMap<String, ModelEntry>,
}

/// Host → models.dev provider key. A SMALL table of confident, stable
/// mappings only — a registry entry with an unknown base url simply falls
/// through to the global id search, and a key that no longer exists in the
/// catalog does the same (the table is a preference, never a gate).
fn provider_for_host(base_url: &str) -> Option<&'static str> {
    const HOSTS: &[(&str, &str)] = &[
        ("api.openai.com", "openai"),
        ("api.anthropic.com", "anthropic"),
        ("api.deepseek.com", "deepseek"),
        ("openrouter.ai", "openrouter"),
        ("api.groq.com", "groq"),
        ("api.x.ai", "xai"),
        ("api.mistral.ai", "mistral"),
        ("api.together.xyz", "together"),
        ("api.together.ai", "together"),
        ("api.fireworks.ai", "fireworks-ai"),
        ("api.moonshot.cn", "moonshotai"),
        ("generativelanguage.googleapis.com", "google"),
    ];
    let host = base_url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()?
        .to_ascii_lowercase();
    HOSTS.iter().find(|(h, _)| *h == host).map(|(_, key)| *key)
}

/// Candidate id forms, broadest last: exact → vendor-prefix stripped →
/// tag-suffix stripped → both.
fn id_candidates(model_id: &str) -> Vec<&str> {
    let mut out = vec![model_id];
    if let Some(idx) = model_id.find('/') {
        out.push(&model_id[idx + 1..]);
    }
    if let Some(idx) = model_id.find(':') {
        out.push(&model_id[..idx]);
    }
    if let Some(idx) = model_id.find('/') {
        let tail = &model_id[idx + 1..];
        if let Some(c) = tail.find(':') {
            out.push(&tail[..c]);
        }
    }
    out
}

/// Match (base_url, model id) → (provider key, matched id, entry).
/// Deterministic: host-scoped section first, then the global walk in
/// sorted provider order; exact id before case-insensitive before
/// normalized variants.
/// Returns (provider key, matched model id, entry) — the key/id are owned
/// (candidates derived from `model_id` do not borrow from the catalog).
pub fn match_entry<'a>(
    catalog: &'a Catalog,
    base_url: &str,
    model_id: &str,
) -> Option<(String, String, &'a ModelEntry)> {
    let host_provider = provider_for_host(base_url);
    let mut keys: Vec<&String> = catalog.keys().collect();
    keys.sort();
    // Host provider first, then everything else alphabetically.
    if let Some(hp) = host_provider {
        keys.sort_by_key(|k| (*k != hp, (*k).clone()));
    }
    for candidate in id_candidates(model_id) {
        for key in &keys {
            let section = &catalog[*key];
            if let Some(entry) = section.get(candidate) {
                return Some(((*key).clone(), candidate.to_string(), entry));
            }
            if let Some((id, entry)) = section
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(candidate))
            {
                return Some(((*key).clone(), id.clone(), entry));
            }
        }
    }
    None
}

/// Build the meta snapshot the row stores. Display-only data plus the
/// matching key (the refresh path re-matches from it, and the UI shows the
/// source badge) — never a request input.
fn build_meta(provider_key: &str, model_id: &str, entry: &ModelEntry) -> serde_json::Value {
    let mut meta = serde_json::Map::new();
    if let Some(n) = &entry.name {
        meta.insert("name".into(), json!(n));
    }
    meta.insert("reasoning".into(), json!(entry.reasoning));
    meta.insert("tool_call".into(), json!(entry.tool_call));
    meta.insert("temperature".into(), json!(entry.temperature));
    meta.insert("attachment".into(), json!(entry.attachment));
    if let Some(l) = &entry.limit {
        if let Some(c) = l.context {
            meta.insert("context_length".into(), json!(c));
        }
        if let Some(o) = l.output {
            meta.insert("max_output".into(), json!(o));
        }
    }
    if let Some(c) = &entry.cost {
        let mut cost = serde_json::Map::new();
        for (key, value) in [
            ("input", c.input),
            ("output", c.output),
            ("cache_read", c.cache_read),
            ("cache_write", c.cache_write),
        ] {
            if let Some(v) = value {
                cost.insert(key.into(), json!(v));
            }
        }
        if !cost.is_empty() {
            meta.insert("cost".into(), serde_json::Value::Object(cost));
        }
    }
    if let Some(k) = &entry.knowledge {
        meta.insert("knowledge".into(), json!(k));
    }
    if let Some(r) = &entry.release_date {
        meta.insert("release_date".into(), json!(r));
    }
    meta.insert("source".into(), json!("models.dev"));
    meta.insert(
        "models_dev".into(),
        json!({"provider": provider_key, "model": model_id}),
    );
    serde_json::Value::Object(meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog() -> Catalog {
        let body = json!({
            "openai": {"models": {
                "gpt-4o": {"name": "GPT-4o", "reasoning": false, "tool_call": true,
                            "temperature": true, "attachment": true,
                            "limit": {"context": 128000, "output": 16384},
                            "cost": {"input": 2.5, "output": 10.0, "cache_read": 1.25}},
                "o1": {"name": "o1", "reasoning": true, "temperature": false,
                       "limit": {"context": 200000, "output": 100000}}
            }},
            "deepseek": {"models": {
                "deepseek-chat": {"name": "DeepSeek-V3", "tool_call": true,
                                  "limit": {"context": 65536}}
            }},
            "openrouter": {"models": {
                "deepseek/deepseek-chat": {"name": "OR DeepSeek", "limit": {"context": 65536}}
            }}
        });
        parse_catalog(&body.to_string()).unwrap()
    }

    #[test]
    fn host_scoped_match_wins() {
        let cat = catalog();
        // api.deepseek.com → the deepseek section, not openrouter's copy.
        let (key, id, entry) =
            match_entry(&cat, "https://api.deepseek.com/v1", "deepseek-chat").unwrap();
        assert_eq!(key, "deepseek");
        assert_eq!(id, "deepseek-chat");
        assert_eq!(entry.name.as_deref(), Some("DeepSeek-V3"));
    }

    #[test]
    fn vendor_prefix_and_tag_suffix_normalize() {
        let cat = catalog();
        let (_, id, _) = match_entry(&cat, "https://x.invalid/v1", "vendor/gpt-4o").unwrap();
        assert_eq!(id, "gpt-4o");
        let (_, id, _) = match_entry(&cat, "https://x.invalid/v1", "gpt-4o:latest").unwrap();
        assert_eq!(id, "gpt-4o");
        // The tag-stripped variant hits openrouter's exact catalog id.
        let (_, id, _) =
            match_entry(&cat, "https://x.invalid/v1", "deepseek/deepseek-chat:free").unwrap();
        assert_eq!(id, "deepseek/deepseek-chat");
    }

    #[test]
    fn global_fallback_is_sorted_and_deterministic() {
        let cat = catalog();
        let (key, _, _) = match_entry(&cat, "https://unknown.invalid/v1", "gpt-4o").unwrap();
        assert_eq!(key, "openai");
    }

    #[test]
    fn miss_is_none_never_fatal() {
        let cat = catalog();
        assert!(match_entry(&cat, "https://api.openai.com/v1", "no-such-model").is_none());
    }

    #[test]
    fn meta_snapshot_carries_display_and_match_keys() {
        let cat = catalog();
        let (_, _, entry) = match_entry(&cat, "https://api.openai.com/v1", "gpt-4o").unwrap();
        let meta = build_meta("openai", "gpt-4o", entry);
        assert_eq!(meta["name"], "GPT-4o");
        assert_eq!(meta["context_length"], 128000);
        assert_eq!(meta["max_output"], 16384);
        assert_eq!(meta["cost"]["input"], 2.5);
        // cache_write absent upstream → omitted from the snapshot.
        assert!(meta["cost"].get("cache_write").is_none());
        assert_eq!(meta["source"], "models.dev");
        assert_eq!(meta["models_dev"]["provider"], "openai");
    }

    #[test]
    fn capability_flags_default_false() {
        let cat = catalog();
        let (_, _, entry) = match_entry(&cat, "https://api.openai.com/v1", "gpt-4o").unwrap();
        assert!(entry.temperature);
        assert!(!entry.reasoning);
        // o1: reasoning model without temperature support.
        let (_, _, o1) = match_entry(&cat, "https://api.openai.com/v1", "o1").unwrap();
        assert!(o1.reasoning);
        assert!(!o1.temperature);
    }
}
