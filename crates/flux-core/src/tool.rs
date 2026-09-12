use crate::CoreError;
use crate::types::ToolDefinition;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio_util::sync::CancellationToken;

// ── Tool trait ──────────────────────────────────────────────────────────────

/// Canonical name of the output-buffer reader tool — the cross-crate name
/// contract between flux-chat (which registers the per-chat tool) and
/// flux-tools (which wires its preprocess entry).
pub const BUF_READ_TOOL: &str = "buf_read";

/// Canonical name of the question tool — the per-chat tool that lets the
/// model ask the user a question mid-round (the approval prompt's
/// ecological successor: round-blocking, lease-holder-answered, host
/// QuickPick — but the content is entirely agent-produced).
pub const QUESTION_TOOL: &str = "question";

/// Per-invocation context handed to every tool execution.
///
/// Carries the kernel-owned cancellation token for the current tool
/// flight and the chat's sandbox boundary. The kernel builds the ctx with
/// the cancel token and call id only (it is boundary-agnostic); the
/// ToolPort adapter fills `workdir` / `current_dir` from the chat's state
/// before `tool.call` — the boundary reaches tools as invocation context,
/// not as injected arguments.
///
/// The kernel cancels `cancel` when the user interrupts the round; a tool
/// that sees `cancel.is_cancelled()` should stop promptly (kill children,
/// release resources) and return whatever partial result it has. Tools
/// never self-report cancellation — the kernel marks the result as
/// interrupted uniformly.
#[derive(Clone)]
pub struct ToolCtx {
    /// Cancelled by the kernel when the user interrupts the round.
    pub cancel: CancellationToken,
    /// The kernel-assigned tool-call id of this invocation. Tools that
    /// need to correlate out-of-band replies (the `question` tool pairs
    /// its host prompt with the pending call) read it from here.
    pub call_id: String,
    /// The chat's sandbox boundary — the canonical workdir carried at
    /// `chat_create`. Empty = not yet filled (kernel-side default); tools
    /// that need the boundary fail closed on it via [`ToolCtx::resolve`].
    pub workdir: std::path::PathBuf,
    /// The chat's transient shell cwd — canonical, always inside `workdir`
    /// (the `state_set` tool enforces that at the write point). bash runs
    /// here; glob scopes its search here. Empty = not yet filled.
    pub current_dir: std::path::PathBuf,
}

impl ToolCtx {
    pub fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            call_id: String::new(),
            workdir: std::path::PathBuf::new(),
            current_dir: std::path::PathBuf::new(),
        }
    }

    /// Resolve `input` against the chat boundary ([`Self::workdir`]).
    ///
    /// Absolute inputs must land inside the boundary; relative inputs join
    /// it. Existing paths canonicalize; non-existent paths resolve via the
    /// deepest existing ancestor plus tail (see `boundary::resolve_path`).
    /// An escape is a tool error — the model sees it and self-corrects.
    pub fn resolve(&self, input: &str) -> Result<std::path::PathBuf, CoreError> {
        if self.workdir.as_os_str().is_empty() {
            return Err(CoreError::InvalidArguments(
                "no workdir boundary on tool context".into(),
            ));
        }
        crate::boundary::resolve_path(&self.workdir, input)
    }
}

impl Default for ToolCtx {
    fn default() -> Self {
        Self::new()
    }
}

/// A tool that can be invoked by the LLM.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name (e.g. "read_file").
    fn name(&self) -> &str;

    /// Human-readable description shown to the LLM.
    fn description(&self) -> &str;

    /// JSON Schema describing the tool's parameters.
    fn schema(&self) -> serde_json::Value;

    /// Execute the tool with the given arguments. The ToolPort adapter has
    /// already filled the ctx with the chat's boundary (`workdir` /
    /// `current_dir`) — resolve any path argument through
    /// [`ToolCtx::resolve`] instead of trusting raw LLM-supplied locations;
    /// the tool is otherwise a pure executor and never touches the state
    /// store.
    ///
    /// Cooperative cancellation: when `ctx.cancel` fires (user interrupt),
    /// the tool should stop promptly and return its partial result. A tool
    /// that ignores the token is force-terminated by the kernel after a
    /// grace period (its future is dropped, so Drop-based cleanup still
    /// runs). Tools never mention cancellation in their result — the
    /// kernel marks interrupted results uniformly.
    async fn call(
        &self,
        arguments: HashMap<String, Value>,
        ctx: ToolCtx,
    ) -> Result<String, CoreError>;
}

// ── ToolRegistry ────────────────────────────────────────────────────────────

/// A thread-safe collection of tools with O(1) lookup by name.
///
/// Duplicate tool names replace the previous entry.
pub struct ToolRegistry {
    tools: RwLock<HashMap<String, Arc<dyn Tool>>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
        }
    }

    /// Register a tool (replaces any existing tool with the same name).
    pub fn register(&self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        let mut tools = recovered(self.tools.write());
        if tools.contains_key(&name) {
            tracing::warn!(tool = %name, "duplicate tool name, replacing previous registration");
        }
        tools.insert(name, tool);
    }

    /// Register a tool unless its name is already taken; returns `false`
    /// on collision without touching the existing entry.
    ///
    /// Callers registering external tools (MCP) use this so a name clash
    /// can never silently replace a built-in tool and inherit its approval
    /// policy.
    pub fn register_if_absent(&self, tool: Arc<dyn Tool>) -> bool {
        let name = tool.name().to_string();
        let mut tools = recovered(self.tools.write());
        if tools.contains_key(&name) {
            return false;
        }
        tools.insert(name, tool);
        true
    }

    /// Find a tool by name (returns a cloned `Arc` for async usage).
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        recovered(self.tools.read()).get(name).cloned()
    }

    /// Replace the whole contents with `other`'s. The in-place engine
    /// rebuild's registry refresh: the round consumer re-assembles a
    /// registry from the CURRENT global truth (assemble_tools) and swaps
    /// this chat's lookup surface atomically under the write lock — tool
    /// calls between rebuilds never observe a half-swapped set.
    pub fn replace_with(&self, other: &ToolRegistry) {
        let snapshot = recovered(other.tools.read()).clone();
        *recovered(self.tools.write()) = snapshot;
    }

    /// Remove a tool by name; returns whether it existed.
    ///
    /// The removal counterpart of `register` — tool-set sources that come
    /// and go (an MCP server shutting down) unregister exactly the names
    /// they registered, so a hot tool-set reload never leaks stale entries.
    pub fn unregister(&self, name: &str) -> bool {
        recovered(self.tools.write()).remove(name).is_some()
    }

    /// All registered tools (cloned Arcs) — used to build per-chat registries.
    pub fn entries(&self) -> Vec<Arc<dyn Tool>> {
        recovered(self.tools.read()).values().cloned().collect()
    }

    /// All registered tool definitions.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        recovered(self.tools.read())
            .values()
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.schema(),
            })
            .collect()
    }
}

/// Recover the lock after poisoning instead of panicking the caller.
///
/// The registry never panics while holding the lock (plain map inserts),
/// so a poisoned lock is always the residue of a panic elsewhere — the
/// map itself is still consistent enough to keep using.
fn recovered<T>(lock: std::sync::LockResult<T>) -> T {
    lock.unwrap_or_else(|e| e.into_inner())
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct FakeTool {
        name: &'static str,
        description: &'static str,
        schema: serde_json::Value,
    }

    #[async_trait]
    impl Tool for FakeTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            self.description
        }
        fn schema(&self) -> serde_json::Value {
            self.schema.clone()
        }
        async fn call(
            &self,
            _arguments: HashMap<String, Value>,
            _ctx: ToolCtx,
        ) -> Result<String, CoreError> {
            Ok("ok".into())
        }
    }

    impl FakeTool {
        fn new(name: &'static str, description: &'static str, schema: serde_json::Value) -> Self {
            Self {
                name,
                description,
                schema,
            }
        }
    }

    #[test]
    fn empty_registry() {
        let r = ToolRegistry::new();
        assert!(r.definitions().is_empty());
        assert!(r.get("nonexistent").is_none());
    }

    #[test]
    fn add_and_get() {
        let r = ToolRegistry::new();
        r.register(Arc::new(FakeTool::new(
            "my_tool",
            "does stuff",
            json!({"type":"object"}),
        )));
        assert_eq!(r.get("my_tool").unwrap().name(), "my_tool");
    }

    #[test]
    fn duplicate_replaces() {
        let r = ToolRegistry::new();
        r.register(Arc::new(FakeTool::new(
            "a",
            "first",
            json!({"type":"object"}),
        )));
        r.register(Arc::new(FakeTool::new(
            "a",
            "second",
            json!({"type":"object"}),
        )));
        assert_eq!(r.get("a").unwrap().description(), "second");
    }

    #[test]
    fn unregister_removes_exactly_the_named_tool() {
        let r = ToolRegistry::new();
        r.register(Arc::new(FakeTool::new(
            "a",
            "first",
            json!({"type":"object"}),
        )));
        r.register(Arc::new(FakeTool::new(
            "b",
            "second",
            json!({"type":"object"}),
        )));
        assert!(r.unregister("a"), "existing tool unregisters");
        assert!(!r.unregister("a"), "second unregister reports absence");
        assert!(r.get("a").is_none());
        assert!(r.get("b").is_some(), "other tools untouched");
        assert!(r.definitions().iter().all(|d| d.name != "a"));
        // A removed name is free again — re-registration works.
        assert!(r.register_if_absent(Arc::new(FakeTool::new(
            "a",
            "third",
            json!({"type":"object"}),
        ))));
    }

    #[test]
    fn register_if_absent_rejects_collision() {
        let r = ToolRegistry::new();
        assert!(r.register_if_absent(Arc::new(FakeTool::new(
            "a",
            "first",
            json!({"type":"object"}),
        ))));
        // A colliding registration must be a no-op — the first tool stays.
        assert!(!r.register_if_absent(Arc::new(FakeTool::new(
            "a",
            "second",
            json!({"type":"object"}),
        ))));
        assert_eq!(r.get("a").unwrap().description(), "first");
    }

    #[test]
    fn definitions_only_includes_registered_tools() {
        let r = ToolRegistry::new();
        r.register(Arc::new(FakeTool::new("a", "", json!({"type":"object"}))));
        let defs = r.definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "a");
    }

    #[test]
    fn entries_returns_all_registered_tools() {
        let r = ToolRegistry::new();
        r.register(Arc::new(FakeTool::new(
            "a",
            "first",
            json!({"type":"object"}),
        )));
        r.register(Arc::new(FakeTool::new(
            "b",
            "second",
            json!({"type":"object"}),
        )));
        let mut names: Vec<String> = r.entries().iter().map(|t| t.name().to_string()).collect();
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn default_creates_empty() {
        assert!(ToolRegistry::default().definitions().is_empty());
    }

    #[test]
    fn registry_survives_poisoned_lock() {
        let registry = Arc::new(ToolRegistry::new());
        // Poison the lock: a panic while holding the write guard.
        let raiser = {
            let registry = Arc::clone(&registry);
            std::thread::spawn(move || {
                let _guard = registry.tools.write().unwrap();
                panic!("simulated panic while holding the lock");
            })
        };
        assert!(raiser.join().is_err());
        // The registry must remain fully usable — reads and writes both
        // recover from the poison instead of panicking the caller.
        registry.register(Arc::new(FakeTool::new(
            "a",
            "works",
            json!({"type":"object"}),
        )));
        assert_eq!(registry.get("a").unwrap().name(), "a");
        assert_eq!(registry.definitions().len(), 1);
        assert_eq!(registry.entries().len(), 1);
        assert!(!registry.register_if_absent(Arc::new(FakeTool::new(
            "a",
            "second",
            json!({"type":"object"}),
        ))));
    }
}
