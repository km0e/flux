//! Per-chat state store with built-in `state_get` / `state_set` tools.
//!
//! ## The boundary key is read-only state
//!
//! `workdir` is the chat's sandbox boundary — carried at `chat_create`,
//! held by the `StateManager` as a fixed field, and surfaced read-only
//! through `state_get`. A movable boundary would be no boundary, so the
//! write path refuses it structurally: `set("workdir", …)` is an error at
//! the single write point — there is no preprocessing layer to
//! bypass anymore. Every other key is open by design: `state_set` accepts
//! any key (the schema `enum` lists only the known keys as LLM guidance,
//! not a whitelist); `current_dir` is canonicalized and confined to the
//! boundary at the write point; arbitrary keys are inert data. `state_get`
//! on an unknown key returns the empty string rather than an error,
//! symmetric with the open write side.
//!
//! ## Structure
//!
//! - `StateManager` — per-chat in-memory key-value store backed by SQLite.
//!   Created via `StateManager::for_chat`, persists on every `set()`.
//! - `StateGetTool` / `StateSetTool` — registry `Tool` implementations
//!   bound to this chat's `StateManager`; assembled per chat by
//!   `spawn::assemble_tools`.
//!
//! ## Usage sketch
//!
//! ```ignore
//! let sm = StateManager::for_chat(store, "chat-1", &initial_entries);
//! sm.set("current_dir", "/path".into()).await?;  // auto-persisted to SQLite
//! sm.set("workdir", "/elsewhere".into()).await?; // Err — read-only
//! ```

use async_trait::async_trait;
use flux_core::CoreError;
use flux_core::Tool;
use flux_store::Store;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::warn;

pub const INITIAL_STATE: &[(&str, &str)] = &[
    (
        "workdir",
        "Current working directory — sandbox boundary (read-only, fixed at chat creation); default base for file/search/shell tools",
    ),
    (
        "current_dir",
        "Transient shell cwd — defaults to workdir; affects bash and glob",
    ),
];

// ── StateManager ────────────────────────────────────────────────────────────

/// Per-chat key-value store backed by SQLite.
///
/// Each `Chat` owns its own `StateManager`.  Mutable state is loaded from
/// the store on construction and persisted automatically on every `set()`
/// call. The chat's sandbox boundary (`workdir`) is NOT mutable state: it
/// is extracted from the persisted entries into a fixed field at load and
/// served read-only by `get("workdir")` — the write path refuses it.
///
/// Handles:
/// - get / set (CRUD with auto-persist)
/// - `state_get` / `state_set` tool execution
/// - the authoritative boundary for the per-chat `ToolCtx` enrichment
#[derive(Debug)]
pub(crate) struct StateManager {
    /// The chat's sandbox boundary — canonical (create_chat canonicalized
    /// it before persisting), carried at `chat_create`, never writable.
    workdir: String,
    /// A sync mutex (not tokio's): the chat loop is the only accessor, so
    /// contention never happens, and the guard is never held across `.await`.
    /// `Chat` must stay `Send` for `tokio::spawn`.
    entries: Mutex<HashMap<String, String>>,
    store: Arc<Store>,
    chat_id: String,
}

impl StateManager {
    /// Create a `StateManager` for a specific chat.
    ///
    /// Loads any previously persisted state from the store, then populates
    /// initial keys with empty strings as defaults. The persisted `workdir`
    /// entry (written by `create_chat`) becomes the fixed boundary field;
    /// it never re-enters the mutable map.
    ///
    /// `initial` — `key → description` (used only for key names, values start empty).
    pub async fn for_chat(
        store: Arc<Store>,
        chat_id: &str,
        initial: &HashMap<String, String>,
    ) -> Self {
        let mut entries: HashMap<String, String> =
            initial.keys().map(|k| (k.clone(), String::new())).collect();

        let mut workdir = String::new();
        match store.load_state(chat_id).await {
            Ok(persisted) => {
                for (k, v) in persisted {
                    if k == "workdir" {
                        workdir = v;
                    } else {
                        entries.insert(k, v);
                    }
                }
            }
            Err(e) => warn!(chat_id = %chat_id, error = %e, "failed to load persisted state"),
        }

        Self {
            workdir,
            entries: Mutex::new(entries),
            store,
            chat_id: chat_id.to_string(),
        }
    }

    /// The chat's sandbox boundary (canonical; possibly empty only for
    /// chats constructed outside `create_chat` — tests).
    pub(crate) fn workdir(&self) -> &str {
        &self.workdir
    }

    // ── CRUD ──

    /// Read a state value. The boundary key serves the fixed field —
    /// read-only by construction.
    pub fn get(&self, key: &str) -> Option<String> {
        if key == "workdir" {
            return Some(self.workdir.clone());
        }
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned()
    }

    /// Write a value (creates the key if missing). Persisted automatically.
    ///
    /// The boundary key is refused at this single write point: a movable
    /// workdir would be no boundary. The refusal is a tool error —
    /// the model sees it and self-corrects.
    pub async fn set(&self, key: &str, value: String) -> Result<(), String> {
        if key == "workdir" {
            return Err("workdir is fixed for this chat — it is carried at chat creation".into());
        }
        {
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(e) = entries.get_mut(key) {
                *e = value.clone();
            } else {
                entries.insert(key.to_owned(), value.clone());
            }
        } // guard released before the await
        // Auto-persist
        if let Err(e) = self
            .store
            .save_state_entry(&self.chat_id, key, &value)
            .await
        {
            warn!(chat_id = %self.chat_id, key = %key, error = %e, "failed to persist state");
        }
        Ok(())
    }
}

// ── State tools ─────────────────────────────────────────────────────────────

/// Canonical names of the built-in state tools — the single source of truth
/// for the tool `name()` impls below and for the reserved-name check in
/// `approvals::is_reserved_tool_name`. Order matches the tool definitions:
/// index 0 is [`StateGetTool`], index 1 is [`StateSetTool`].
pub const STATE_TOOL_NAMES: &[&str] = &["state_get", "state_set"];

/// `state_get` as a registry tool — bound to this chat's [`StateManager`].
pub(crate) struct StateGetTool {
    state: Arc<StateManager>,
    schema: Value,
}

impl StateGetTool {
    pub(crate) fn new(state: Arc<StateManager>, descriptions: &HashMap<String, String>) -> Self {
        Self {
            state,
            schema: state_get_schema(descriptions),
        }
    }
}

#[async_trait]
impl Tool for StateGetTool {
    fn name(&self) -> &str {
        STATE_TOOL_NAMES[0]
    }
    fn description(&self) -> &str {
        "Read a value from shared state."
    }
    fn schema(&self) -> Value {
        self.schema.clone()
    }
    async fn call(
        &self,
        args: HashMap<String, Value>,
        _ctx: flux_core::ToolCtx,
    ) -> Result<String, CoreError> {
        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| CoreError::Tool("state_get requires a 'key' argument".into()))?;
        Ok(self.state.get(key).unwrap_or_default())
    }
}

/// `state_set` as a registry tool — bound to this chat's [`StateManager`].
pub(crate) struct StateSetTool {
    state: Arc<StateManager>,
    schema: Value,
}

impl StateSetTool {
    pub(crate) fn new(state: Arc<StateManager>, descriptions: &HashMap<String, String>) -> Self {
        Self {
            state,
            schema: state_set_schema(descriptions),
        }
    }
}

#[async_trait]
impl Tool for StateSetTool {
    fn name(&self) -> &str {
        STATE_TOOL_NAMES[1]
    }
    fn description(&self) -> &str {
        "Write a value to shared state."
    }
    fn schema(&self) -> Value {
        self.schema.clone()
    }
    async fn call(
        &self,
        args: HashMap<String, Value>,
        ctx: flux_core::ToolCtx,
    ) -> Result<String, CoreError> {
        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| CoreError::Tool("state_set requires a 'key' argument".into()))?;
        let value = args
            .get("value")
            .and_then(|v| v.as_str())
            .ok_or_else(|| CoreError::Tool("state_set requires a 'value' argument".into()))?;
        // The boundary confinement lives at this write point: current_dir
        // is canonicalized and confined to the chat boundary here, so the
        // stored value and every later use agree on the resolved path. A
        // lexical `starts_with` on a raw symlinked path (macOS /Users)
        // would wrongly deny later reads, and a raw current_dir could be
        // swapped for an escaping symlink after the write.
        let value = if key == "current_dir" {
            if value.trim().is_empty() {
                return Err(CoreError::Tool("current_dir cannot be empty".into()));
            }
            // Resolve against the boundary (containment; also normalizes
            // relative inputs), then canonicalize to require existence —
            // a nonexistent cwd would fail at spawn time anyway.
            let resolved = ctx.resolve(value)?;
            let canonical = resolved.canonicalize().map_err(|e| {
                CoreError::Tool(format!("current_dir does not resolve: {value}: {e}"))
            })?;
            canonical.to_string_lossy().into_owned()
        } else {
            value.to_string()
        };
        self.state.set(key, value).await.map_err(CoreError::Tool)?;
        Ok("OK".into())
    }
}

// ── Schema generation ──────────────────────────────────────────────────────

fn state_key_descriptions(descriptions: &HashMap<String, String>) -> String {
    descriptions
        .iter()
        .map(|(k, desc)| format!("`{k}` — {desc}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn state_keys(descriptions: &HashMap<String, String>) -> Vec<String> {
    descriptions.keys().cloned().collect()
}

fn state_get_schema(descriptions: &HashMap<String, String>) -> Value {
    json!({
        "type": "object",
        "properties": {
            "key": {
                "type": "string",
                "description": state_key_descriptions(descriptions),
                "enum": state_keys(descriptions),
            }
        },
        "required": ["key"],
    })
}

fn state_set_schema(descriptions: &HashMap<String, String>) -> Value {
    json!({
        "type": "object",
        "properties": {
            "key": {
                "type": "string",
                "description": state_key_descriptions(descriptions),
                "enum": state_keys(descriptions),
            },
            "value": { "type": "string", "description": "Value to store" },
        },
        "required": ["key", "value"],
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_store() -> Arc<Store> {
        Arc::new(Store::open_in_memory().await.unwrap())
    }

    fn test_initial() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("workdir".into(), "Working directory".into());
        m
    }

    #[tokio::test]
    async fn get_set() {
        let store = test_store().await;
        let sm = StateManager::for_chat(store, "test-chat", &HashMap::new()).await;
        assert_eq!(sm.get("x"), None);
        sm.set("x", "hello".into()).await.unwrap();
        assert_eq!(sm.get("x"), Some("hello".into()));
        sm.set("x", "world".into()).await.unwrap();
        assert_eq!(sm.get("x"), Some("world".into()));
    }

    #[tokio::test]
    async fn set_refuses_workdir_at_the_write_point() {
        // The boundary is read-only state: the refusal lives at the single
        // write point — there is no preprocessing layer to bypass anymore.
        let store = test_store().await;
        let sm = StateManager::for_chat(store, "test-chat", &HashMap::new()).await;
        let err = sm
            .set("workdir", "/elsewhere".into())
            .await
            .expect_err("workdir must be refused");
        assert!(err.contains("fixed for this chat"), "{err}");
    }

    #[tokio::test]
    async fn workdir_persists_readonly_from_create() {
        // create_chat persists workdir before the StateManager exists; the
        // load extracts it into the fixed field and get() serves it.
        let store = test_store().await;
        store.insert_chat("chat-1", "Chat").await.unwrap();
        store
            .save_state_entry("chat-1", "workdir", "/custom/path")
            .await
            .unwrap();
        let sm = StateManager::for_chat(store, "chat-1", &HashMap::new()).await;
        assert_eq!(sm.workdir(), "/custom/path");
        assert_eq!(sm.get("workdir"), Some("/custom/path".into()));
    }

    #[tokio::test]
    async fn state_persisted_on_set() {
        let store = test_store().await;
        store.insert_chat("chat-1", "Chat").await.unwrap();
        let sm = StateManager::for_chat(store.clone(), "chat-1", &HashMap::new()).await;

        sm.set("foo", "bar".into()).await.unwrap();

        // Load a fresh StateManager for the same chat — should see persisted value
        let sm2 = StateManager::for_chat(store, "chat-1", &HashMap::new()).await;
        assert_eq!(sm2.get("foo"), Some("bar".into()));
    }

    #[tokio::test]
    async fn state_scoped_per_chat() {
        let store = test_store().await;
        store.insert_chat("chat-a", "A").await.unwrap();
        store.insert_chat("chat-b", "B").await.unwrap();

        let sm_a = StateManager::for_chat(store.clone(), "chat-a", &HashMap::new()).await;
        sm_a.set("shared-key", "value-a".into()).await.unwrap();

        let sm_b = StateManager::for_chat(store.clone(), "chat-b", &HashMap::new()).await;
        sm_b.set("shared-key", "value-b".into()).await.unwrap();

        assert_eq!(sm_a.get("shared-key"), Some("value-a".into()));
        assert_eq!(sm_b.get("shared-key"), Some("value-b".into()));
    }

    #[tokio::test]
    async fn initial_keys_loaded_as_empty() {
        let store = test_store().await;
        let sm = StateManager::for_chat(store, "chat-1", &test_initial()).await;
        // Initial keys exist but are empty until set (workdir serves the
        // fixed field, empty for a test chat with no persisted boundary).
        assert_eq!(sm.get("workdir"), Some(String::new()));
        assert_eq!(sm.get("current_dir"), None);
    }

    #[tokio::test]
    async fn state_reads_survive_lock_poisoning() {
        // Following this file's existing construction pattern (see the
        // initial_keys_loaded_as_empty test):
        // `std::sync::MutexGuard` is `!Send`, so the poisoning thread must
        // lock the mutex itself — share the manager via `Arc` for that.
        let sm =
            Arc::new(StateManager::for_chat(test_store().await, "chat-1", &test_initial()).await);
        let poisoned = Arc::clone(&sm);
        std::thread::spawn(move || {
            let _held = poisoned.entries.lock().unwrap();
            panic!("poison the mutex");
        })
        .join()
        .ok();
        // get/snapshot must recover via into_inner instead of panicking.
        assert_eq!(sm.get("workdir"), Some(String::new()));
        // Arbitrary keys are still readable; the boundary is a fixed field.
        assert_eq!(sm.get("current_dir"), None);
    }

    #[tokio::test]
    async fn current_dir_stays_writable_state() {
        // Only the boundary is read-only: current_dir remains ordinary
        // mutable state (the model moves it to steer bash/glob).
        let store = test_store().await;
        let sm = StateManager::for_chat(store, "c1", &test_initial()).await;
        sm.set("current_dir", "/w".into()).await.unwrap();
        assert_eq!(sm.get("current_dir"), Some("/w".into()));
    }

    #[tokio::test]
    async fn state_get_tool_reads_value() {
        let store = test_store().await;
        let sm = Arc::new(StateManager::for_chat(store, "test-chat", &HashMap::new()).await);
        sm.set("foo", "bar".into()).await.unwrap();
        let tool = StateGetTool::new(sm, &HashMap::new());
        let out = tool
            .call(
                HashMap::from([("key".into(), Value::String("foo".into()))]),
                flux_core::ToolCtx::new(),
            )
            .await
            .unwrap();
        assert_eq!(out, "bar");
    }

    #[tokio::test]
    async fn state_get_tool_missing_key_errors() {
        let store = test_store().await;
        let sm = Arc::new(StateManager::for_chat(store, "test-chat", &HashMap::new()).await);
        let tool = StateGetTool::new(sm, &HashMap::new());
        let err = tool
            .call(HashMap::new(), flux_core::ToolCtx::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("'key'"), "{err}");
    }

    #[tokio::test]
    async fn state_set_tool_writes_and_persists() {
        let store = test_store().await;
        store.insert_chat("chat-1", "Chat").await.unwrap();
        let sm = Arc::new(StateManager::for_chat(store.clone(), "chat-1", &HashMap::new()).await);
        let tool = StateSetTool::new(sm.clone(), &HashMap::new());
        let out = tool
            .call(
                HashMap::from([
                    ("key".into(), Value::String("foo".into())),
                    ("value".into(), Value::String("bar".into())),
                ]),
                flux_core::ToolCtx::new(),
            )
            .await
            .unwrap();
        assert_eq!(out, "OK");
        assert_eq!(sm.get("foo").as_deref(), Some("bar"));
        // Persisted: a fresh manager sees the value.
        let sm2 = StateManager::for_chat(store, "chat-1", &HashMap::new()).await;
        assert_eq!(sm2.get("foo").as_deref(), Some("bar"));
    }

    #[tokio::test]
    async fn state_set_tool_refuses_workdir() {
        // The boundary is read-only state — the tool surfaces the write-point
        // refusal as a tool error the model sees and self-corrects.
        let store = test_store().await;
        let sm = Arc::new(StateManager::for_chat(store, "test-chat", &HashMap::new()).await);
        let tool = StateSetTool::new(sm.clone(), &HashMap::new());
        let err = tool
            .call(
                HashMap::from([
                    ("key".into(), Value::String("workdir".into())),
                    ("value".into(), Value::String("/elsewhere".into())),
                ]),
                flux_core::ToolCtx::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("fixed for this chat"), "{err}");
        assert_eq!(sm.get("workdir"), Some(String::new()));
    }

    #[tokio::test]
    async fn state_set_tool_canonicalizes_current_dir_against_boundary() {
        // The old state_set preprocessing moved into the tool: current_dir
        // is resolved against the ctx boundary and canonicalized (must
        // exist) at the write point.
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let store = test_store().await;
        let sm = Arc::new(StateManager::for_chat(store, "test-chat", &HashMap::new()).await);
        let tool = StateSetTool::new(sm.clone(), &HashMap::new());
        let ctx = flux_core::ToolCtx {
            workdir: dir.path().to_path_buf(),
            current_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        tool.call(
            HashMap::from([
                ("key".into(), Value::String("current_dir".into())),
                ("value".into(), Value::String("sub".into())),
            ]),
            ctx,
        )
        .await
        .unwrap();
        // Stored canonical: a relative input lands on the absolute path.
        assert_eq!(
            sm.get("current_dir").as_deref(),
            Some(sub.to_str().unwrap())
        );
    }

    #[tokio::test]
    async fn state_set_tool_denies_current_dir_outside_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store().await;
        let sm = Arc::new(StateManager::for_chat(store, "test-chat", &HashMap::new()).await);
        let tool = StateSetTool::new(sm.clone(), &HashMap::new());
        let ctx = flux_core::ToolCtx {
            workdir: dir.path().to_path_buf(),
            current_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let err = tool
            .call(
                HashMap::from([
                    ("key".into(), Value::String("current_dir".into())),
                    ("value".into(), Value::String("/etc".into())),
                ]),
                ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("path escape"), "{err}");
    }

    #[tokio::test]
    async fn state_set_tool_denies_nonexistent_current_dir() {
        // current_dir must exist (it becomes a spawn cwd) — canonicalize
        // after the boundary resolve requires that.
        let dir = tempfile::tempdir().unwrap();
        let store = test_store().await;
        let sm = Arc::new(StateManager::for_chat(store, "test-chat", &HashMap::new()).await);
        let tool = StateSetTool::new(sm.clone(), &HashMap::new());
        let ctx = flux_core::ToolCtx {
            workdir: dir.path().to_path_buf(),
            current_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let err = tool
            .call(
                HashMap::from([
                    ("key".into(), Value::String("current_dir".into())),
                    ("value".into(), Value::String("no/such/dir".into())),
                ]),
                ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("does not resolve"), "{err}");
    }

    #[tokio::test]
    async fn state_set_tool_missing_value_errors() {
        let store = test_store().await;
        let sm = Arc::new(StateManager::for_chat(store, "test-chat", &HashMap::new()).await);
        let tool = StateSetTool::new(sm, &HashMap::new());
        let err = tool
            .call(
                HashMap::from([("key".into(), Value::String("foo".into()))]),
                flux_core::ToolCtx::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("'value'"), "{err}");
    }

    #[test]
    fn state_tool_schemas_include_key_descriptions() {
        let desc = HashMap::from([("workdir".into(), "Working directory".into())]);
        let get_schema = state_get_schema(&desc);
        let keys = get_schema["properties"]["key"]["enum"].as_array().unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0], "workdir");
        let set_schema = state_set_schema(&desc);
        assert!(set_schema["properties"]["value"].is_object());
    }
}
