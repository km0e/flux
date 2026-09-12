//! MCP-server management — the launch list's validation + persistence +
//! LIVE application.
//!
//! The list lives ONLY in the server database (the server has no config
//! file); every mutation is persist-FIRST (the row is the launch list — a
//! failed connect is retried at the next process start), then applied to
//! the running process: the manager spawns/connects the child, registers
//! its tools into the global registry (reserved-name + collision rules),
//! and tracks the exact names it registered so a removal unregisters
//! precisely those. After a successful apply the caller fans an engine
//! rebuild out to the chats (each respawn re-assembles from the CURRENT
//! global registry), so a change lands without a process restart.
//!
//! Summaries redact the env values (keys only) — secrets parity with the
//! provider api_key.

use flux_core::{Tool, ToolRegistry};
use flux_proto::flux::v1::McpServerSummary;
use flux_store::Store;
use flux_store::mcp::McpServerRow;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// One live MCP server: the RAII session guard (dropping it shuts the
/// child down) and the tool names it registered into the global registry
/// (a removal unregisters exactly those — never a stale name more).
struct McpEntry {
    _guard: Box<dyn McpSessionGuard>,
    tool_names: Vec<String>,
}

/// Opaque live-connection guard. `flux_mcp::McpSession` implements it (its
/// Drop shuts the child down); tests substitute a no-op.
pub trait McpSessionGuard: Send + Sync {}
impl McpSessionGuard for flux_mcp::McpSession {}

/// The result of one connect: the guard + the tools to register. The tools
/// arrive pre-wrapped as registry entries so tests can inject fakes
/// without a real MCP peer.
pub struct ConnectedServer {
    pub guard: Box<dyn McpSessionGuard>,
    pub tools: Vec<Arc<dyn Tool>>,
}

/// The connect function (injection seam for tests).
pub type Connect = Arc<dyn Fn(flux_mcp::McpServerConfig) -> ConnectFuture + Send + Sync>;
pub type ConnectFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<ConnectedServer>> + Send>>;

/// The production connect: spawn the child, handshake, list tools, wrap
/// each as a registry tool bound to the peer.
pub fn production_connect() -> Connect {
    Arc::new(|cfg| {
        Box::pin(async move {
            let (session, peer, metas) = flux_mcp::connect_with_peer(&cfg).await?;
            let tools = metas
                .into_iter()
                .map(|meta| {
                    Arc::new(flux_mcp::McpToolWrapper::from_rmcp_tool(meta, peer.clone()))
                        as Arc<dyn Tool>
                })
                .collect();
            Ok(ConnectedServer {
                guard: Box::new(session),
                tools,
            })
        })
    })
}

/// The live MCP surface: launch-list rows are the truth on disk, this map
/// is the truth in the process. Holds the global tool registry it
/// registers into (the same Arc the engine spawns assemble from).
pub struct McpManager {
    entries: RwLock<HashMap<String, McpEntry>>,
    registry: Arc<ToolRegistry>,
    connect: Connect,
}

impl McpManager {
    pub fn new(registry: Arc<ToolRegistry>, connect: Connect) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            registry,
            connect,
        }
    }

    /// Startup restore: connect every launch-list row concurrently (one
    /// hung server costs only its own init timeout) and register the
    /// connected ones. A row that fails is SKIPPED with a warning — it
    /// stays fixable from the UI and retried at the next restart.
    pub async fn restore(&self, store: &Arc<Store>) {
        let Ok(rows) = store.list_mcp_servers().await else {
            tracing::warn!("failed to load MCP servers; skipping");
            return;
        };
        let connects = rows.into_iter().map(|row| async move {
            tracing::info!(id = %row.id, command = %row.command, "connecting to MCP");
            let cfg = flux_mcp::McpServerConfig {
                command: row.command.clone(),
                args: row.args,
                env: row.env,
            };
            let result = (self.connect)(cfg).await;
            (row.id, result)
        });
        for (id, result) in futures_util::future::join_all(connects).await {
            match result {
                Ok(connected) => {
                    let names = register_tools(&self.registry, &id, &connected.tools);
                    tracing::info!(id = %id, tools = names.len(), "MCP connected");
                    self.entries.write().unwrap().insert(
                        id,
                        McpEntry {
                            _guard: connected.guard,
                            tool_names: names,
                        },
                    );
                }
                Err(e) => {
                    tracing::warn!(id = %id, error = %e, "MCP server failed to start; skipping");
                }
            }
        }
    }

    /// Apply one persisted add: connect + register into the global
    /// registry + record the entry. Failure = the row stays on disk (the
    /// next restart retries) and the error rides back to the UI inline.
    pub async fn apply_add(&self, id: &str, row: &McpServerRow) -> anyhow::Result<()> {
        if self.entries.read().unwrap().contains_key(id) {
            anyhow::bail!("MCP server '{id}' is already running");
        }
        let cfg = flux_mcp::McpServerConfig {
            command: row.command.clone(),
            args: row.args.clone(),
            env: row.env.clone(),
        };
        let connected = (self.connect)(cfg).await?;
        let names = register_tools(&self.registry, id, &connected.tools);
        self.entries.write().unwrap().insert(
            id.to_string(),
            McpEntry {
                _guard: connected.guard,
                tool_names: names,
            },
        );
        Ok(())
    }

    /// Apply one persisted remove: unregister exactly the names the server
    /// registered and drop the session (RAII shuts the child down).
    /// Best-effort: an entry that never came up (a failed startup connect)
    /// is not an error — the row is gone either way. Returns whether a
    /// live tool set actually changed (the caller rebuilds the engines
    /// only then).
    pub fn apply_remove(&self, id: &str) -> anyhow::Result<bool> {
        let Some(entry) = self.entries.write().unwrap().remove(id) else {
            return Ok(false);
        };
        for name in &entry.tool_names {
            self.registry.unregister(name);
        }
        Ok(!entry.tool_names.is_empty())
    }
}

/// Register the server's tools into the global registry under the
/// collision rules (reserved chat-owned names and existing tools are
/// skipped — an external tool can never shadow them). Returns the names
/// that REGISTERED (the removal set).
fn register_tools(registry: &ToolRegistry, id: &str, tools: &[Arc<dyn Tool>]) -> Vec<String> {
    tools
        .iter()
        .filter(|tool| {
            let registered = !flux_chat::reserved::is_reserved_tool_name(tool.name())
                && registry.register_if_absent(Arc::clone(tool));
            if !registered {
                tracing::error!(
                    server = %id,
                    tool = %tool.name(),
                    "MCP tool name collides with a built-in/reserved tool; skipping",
                );
            }
            registered
        })
        .map(|tool| tool.name().to_string())
        .collect()
}

// ── Launch-list persistence (unchanged semantics — persist-first) ───────────

/// Validate + persist a new MCP server. Persist-first: a successful return
/// means the row is durable (the live apply is the caller's next step).
pub(crate) async fn add(
    store: &Arc<Store>,
    mut row: McpServerRow,
) -> anyhow::Result<McpServerSummary> {
    row.id = row.id.trim().to_string();
    row.command = row.command.trim().to_string();
    anyhow::ensure!(!row.id.is_empty(), "MCP server id must not be empty");
    anyhow::ensure!(
        !row.command.is_empty(),
        "MCP server '{}' command must not be empty",
        row.id
    );
    if !store.insert_mcp_server(&row).await? {
        anyhow::bail!("duplicate MCP server id: {}", row.id);
    }
    Ok(summary(row))
}

/// Remove one MCP server row (the live apply is the caller's next step).
pub(crate) async fn remove(store: &Arc<Store>, id: &str) -> anyhow::Result<()> {
    if !store.delete_mcp_server(id).await? {
        anyhow::bail!("unknown MCP server id: {id}");
    }
    Ok(())
}

/// The launch list for the wire (`mcp_list` reply / broadcast), sorted by
/// id, env values redacted to keys.
pub(crate) async fn summaries(store: &Arc<Store>) -> anyhow::Result<Vec<McpServerSummary>> {
    let mut out: Vec<McpServerSummary> = store
        .list_mcp_servers()
        .await?
        .into_iter()
        .map(summary)
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

fn summary(row: McpServerRow) -> McpServerSummary {
    let mut env_keys: Vec<String> = row.env.into_keys().collect();
    env_keys.sort();
    McpServerSummary {
        id: row.id,
        command: row.command,
        args: row.args,
        env_keys,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    async fn test_store() -> Arc<Store> {
        Arc::new(Store::open_in_memory().await.unwrap())
    }

    fn row(id: &str, command: &str) -> McpServerRow {
        McpServerRow {
            id: id.to_string(),
            command: command.to_string(),
            args: vec!["-y".to_string()],
            env: HashMap::from([("B_KEY".to_string(), "v2".to_string())]),
        }
    }

    fn fake_tools(names: &[&str]) -> Vec<Arc<dyn Tool>> {
        struct Fake(String);
        #[async_trait::async_trait]
        impl Tool for Fake {
            fn name(&self) -> &str {
                &self.0
            }
            fn description(&self) -> &str {
                "fake"
            }
            fn schema(&self) -> serde_json::Value {
                serde_json::json!({"type": "object"})
            }
            async fn call(
                &self,
                _args: HashMap<String, serde_json::Value>,
                _ctx: flux_core::ToolCtx,
            ) -> Result<String, flux_core::CoreError> {
                Ok("ok".into())
            }
        }
        names
            .iter()
            .map(|n| Arc::new(Fake(n.to_string())) as Arc<dyn Tool>)
            .collect()
    }

    /// A connect that succeeds for `ok` ids with the given tools and fails
    /// for everything else (the spawn-failure path).
    fn fake_connect(ok_tools: Vec<(&'static str, Vec<Arc<dyn Tool>>)>) -> Connect {
        let map: HashMap<String, Vec<Arc<dyn Tool>>> = ok_tools
            .into_iter()
            .map(|(id, tools)| (id.to_string(), tools))
            .collect();
        Arc::new(move |cfg: flux_mcp::McpServerConfig| {
            let tools = map.get(&cfg.command).cloned();
            Box::pin(async move {
                let tools = tools.ok_or_else(|| anyhow::anyhow!("spawn failed"))?;
                Ok(ConnectedServer {
                    guard: Box::new(NoopGuard),
                    tools,
                })
            }) as ConnectFuture
        })
    }

    struct NoopGuard;
    impl McpSessionGuard for NoopGuard {}

    #[tokio::test]
    async fn add_validates_and_redacts() {
        let store = test_store().await;
        // Empty id / empty command reject before any write.
        let err = add(&store, row("  ", "npx")).await.unwrap_err();
        assert!(err.to_string().contains("id must not be empty"));
        let err = add(&store, row("a", "   ")).await.unwrap_err();
        assert!(err.to_string().contains("command must not be empty"));
        // A valid add trims, persists, and the summary carries env KEYS
        // only — never the values.
        let sum = add(&store, row("  fs  ", "npx")).await.unwrap();
        assert_eq!(sum.id, "fs");
        assert_eq!(sum.env_keys, vec!["B_KEY"]);
        assert_eq!(summaries(&store).await.unwrap().len(), 1);
        // Duplicate id rejects.
        let err = add(&store, row("fs", "npx")).await.unwrap_err();
        assert!(err.to_string().contains("duplicate MCP server id"));
    }

    #[tokio::test]
    async fn remove_round_trip() {
        let store = test_store().await;
        add(&store, row("fs", "npx")).await.unwrap();
        remove(&store, "fs").await.unwrap();
        assert!(summaries(&store).await.unwrap().is_empty());
        let err = remove(&store, "fs").await.unwrap_err();
        assert!(err.to_string().contains("unknown MCP server id"));
    }

    #[tokio::test]
    async fn apply_add_registers_tools_and_apply_remove_unregisters_exactly_them() {
        let store = test_store().await;
        let registry = Arc::new(ToolRegistry::new());
        registry.register(Arc::new(Builtin));
        let manager = McpManager::new(
            Arc::clone(&registry),
            fake_connect(vec![(
                "echo",
                fake_tools(&["mcp_echo", "bash", "state_get"]),
            )]),
        );

        add(&store, row("echo", "echo")).await.unwrap();
        manager
            .apply_add("echo", &row("echo", "echo"))
            .await
            .unwrap();

        // mcp_echo registered; the built-in and the reserved name skipped.
        assert!(registry.get("mcp_echo").is_some());
        assert!(registry.get("bash").is_some(), "built-in untouched");
        assert!(registry.get("state_get").is_none(), "reserved name skipped");

        // A duplicate live add rejects without touching the registry.
        let err = manager
            .apply_add("echo", &row("echo", "echo"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already running"));

        // Remove unregisters EXACTLY the registered names.
        assert!(manager.apply_remove("echo").unwrap());
        assert!(registry.get("mcp_echo").is_none());
        assert!(registry.get("bash").is_some());
        // A second remove reports nothing changed (the entry is gone).
        assert!(!manager.apply_remove("echo").unwrap());
    }

    #[tokio::test]
    async fn apply_add_failure_keeps_the_row_and_the_registry_clean() {
        let store = test_store().await;
        let registry = Arc::new(ToolRegistry::new());
        let manager = McpManager::new(Arc::clone(&registry), fake_connect(vec![])); // everything fails

        add(&store, row("bad", "bad")).await.unwrap();
        let err = manager
            .apply_add("bad", &row("bad", "bad"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("spawn failed"));
        // The row stays (the next restart retries); the registry is clean.
        assert_eq!(summaries(&store).await.unwrap().len(), 1);
        assert!(registry.entries().is_empty());
    }

    #[tokio::test]
    async fn restore_skips_failed_rows_and_registers_connected_ones() {
        let store = test_store().await;
        add(&store, row("good", "good")).await.unwrap();
        add(&store, row("dead", "dead")).await.unwrap();
        let registry = Arc::new(ToolRegistry::new());
        let manager = McpManager::new(
            Arc::clone(&registry),
            fake_connect(vec![("good", fake_tools(&["mcp_a"]))]),
        );
        manager.restore(&store).await;
        assert!(registry.get("mcp_a").is_some());
        // The failed row stays on the launch list (retry at next restart).
        assert_eq!(summaries(&store).await.unwrap().len(), 2);
    }

    struct Builtin;
    #[async_trait::async_trait]
    impl Tool for Builtin {
        fn name(&self) -> &str {
            "bash"
        }
        fn description(&self) -> &str {
            "built-in"
        }
        fn schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn call(
            &self,
            _args: HashMap<String, serde_json::Value>,
            _ctx: flux_core::ToolCtx,
        ) -> Result<String, flux_core::CoreError> {
            Ok("ok".into())
        }
    }
}
