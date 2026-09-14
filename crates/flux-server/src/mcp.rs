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
use flux_proto::flux::v1::McpState;
use flux_store::Store;
use flux_store::mcp::McpServerRow;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Respawn backoff: base × 2^(attempt-1), capped. No attempt limit — the
/// row is durable, the supervisor is mechanical, and the cap IS the storm
/// protection. Test-injectable via the crate-visible fields.
pub(crate) const BACKOFF_BASE: Duration = Duration::from_secs(30);
pub(crate) const BACKOFF_MAX: Duration = Duration::from_secs(600);

/// One live MCP server: the tool names it registered into the global
/// registry (a removal unregisters precisely those — never a stale name
/// more), the row config its respawn needs, and the supervisor-visible
/// state. The session handle itself is OWNED by the supervisor task (it
/// awaits rmcp's death signal); the entry holds a cancel capability so a
/// removal can kill the child from here.
struct McpEntry {
    row: McpServerRow,
    tool_names: Vec<String>,
    state: McpLiveState,
    cancel: Arc<dyn Fn() + Send + Sync>,
    /// Re-lists the live session's tools (the tools/list_changed path);
    /// None for test fakes.
    relist: Option<flux_mcp::RelistFn>,
}

/// Supervisor-visible liveness (rendered into the wire summary).
#[derive(Clone, Copy, PartialEq)]
enum McpLiveState {
    Running,
    Backoff,
}

impl From<McpLiveState> for McpState {
    fn from(s: McpLiveState) -> Self {
        match s {
            McpLiveState::Running => McpState::Running,
            McpLiveState::Backoff => McpState::Backoff,
        }
    }
}

/// Supervisor events — consumed in main with the SAME machinery the
/// mutation path uses (broadcast + restart_all_chats), never a second
/// mechanism.
#[derive(Debug)]
pub enum McpEvent {
    /// A state transition worth re-rendering (Running ↔ Backoff).
    Changed(String),
    /// A respawn succeeded with a live tool set — the chats must rebuild
    /// (the old tool wrappers bind to the DEAD peer).
    ToolsChanged(String),
    /// A rate-limited logging notification from one server — forwarded
    /// to the UI (toast for warning+, notification center for all).
    Notice {
        id: String,
        level: String,
        message: String,
    },
}

/// Opaque live-session handle, owned by the supervisor task. Death
/// detection is rmcp's own `RunningService::waiting()`; teardown is its
/// cancellation token. Tests substitute scripted fakes.
pub trait McpSessionHandle: Send + 'static {
    /// Resolves when the session's serve loop ends: `Closed` = the child
    /// died (transport input closed), `Cancelled` = our cancel,
    /// `Failed` = task-level failure. A second call reports `Failed`.
    fn quit(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = flux_mcp::McpQuit> + Send + '_>>;
}

impl McpSessionHandle for flux_mcp::McpSession {
    fn quit(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = flux_mcp::McpQuit> + Send + '_>> {
        Box::pin(flux_mcp::McpSession::quit(self))
    }
}

/// The result of one connect: the session handle (moved into the
/// supervisor), a cancel capability for the entry (removal from outside
/// the supervisor), and the tools to register. The tools arrive
/// pre-wrapped as registry entries so tests can inject fakes without a
/// real MCP peer.
pub struct ConnectedServer {
    pub handle: Box<dyn McpSessionHandle>,
    pub cancel: Arc<dyn Fn() + Send + Sync>,
    pub tools: Vec<Arc<dyn Tool>>,
    /// Re-lists the live session's current tool set (the
    /// tools/list_changed path). None for test fakes.
    pub relist: Option<flux_mcp::RelistFn>,
}

/// The connect function (injection seam for tests). The sink receives
/// the server notifications flux forwards (tools/list_changed, logging);
/// a test fake can drop it.
pub type Connect = Arc<
    dyn Fn(
            flux_mcp::McpServerConfig,
            tokio::sync::mpsc::UnboundedSender<flux_mcp::ServerNotice>,
        ) -> ConnectFuture
        + Send
        + Sync,
>;
pub type ConnectFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<ConnectedServer>> + Send>>;

/// The production connect: spawn the child, handshake, list tools, wrap
/// each as a registry tool bound to the peer.
pub fn production_connect() -> Connect {
    Arc::new(|cfg, notices| {
        Box::pin(async move {
            let (session, external_cancel, peer, metas) =
                flux_mcp::connect_with_peer(&cfg, Some(notices)).await?;
            let tools = metas
                .into_iter()
                .map(|meta| {
                    Arc::new(flux_mcp::McpToolWrapper::from_rmcp_tool(meta, peer.clone()))
                        as Arc<dyn Tool>
                })
                .collect();
            Ok(ConnectedServer {
                handle: Box::new(session),
                // Removal (from the ops thread, while the supervisor owns
                // the handle) cancels the SAME inner token.
                cancel: external_cancel,
                tools,
                relist: Some(flux_mcp::relist_for_peer(&peer)),
            })
        })
    })
}

/// The live MCP surface: launch-list rows are the truth on disk, this map
/// is the truth in the process. Holds the global tool registry it
/// registers into (the same Arc the engine spawns assemble from) and the
/// event channel the supervisor reports through.
pub struct McpManager {
    entries: RwLock<HashMap<String, McpEntry>>,
    registry: Arc<ToolRegistry>,
    connect: Connect,
    events: tokio::sync::mpsc::UnboundedSender<McpEvent>,
    /// Respawn backoff shape — crate-visible so tests collapse the clock.
    pub(crate) backoff_base: Duration,
    pub(crate) backoff_max: Duration,
    /// Per-server notice gates (F-10b): token-bucket rate limiting so a
    /// chatty server can never flood the UI. Keyed by id; cleared on
    /// remove.
    notice_gates: std::sync::Mutex<HashMap<String, NoticeGate>>,
}

/// Token-bucket for one server's logging notifications: a small burst
/// then a sustained 10/min, drops counted and folded into a synthetic
/// "suppressed" notice when the gate reopens. Shape constants live in
/// [`NOTICE_BURST`]/[`NOTICE_REFILL_PER_TOKEN`].
#[derive(Debug)]
struct NoticeGate {
    tokens: u32,
    last: Option<std::time::Instant>,
    suppressed: u32,
}

impl Default for NoticeGate {
    /// A fresh gate opens at FULL burst — the first notice from a server
    /// must never be the one that gets dropped.
    fn default() -> Self {
        Self {
            tokens: NOTICE_BURST,
            last: None,
            suppressed: 0,
        }
    }
}

const NOTICE_BURST: u32 = 5;
const NOTICE_REFILL_PER_TOKEN: Duration = Duration::from_secs(6);
const NOTICE_MESSAGE_MAX: usize = 500;

impl NoticeGate {
    /// Take one token if available; otherwise count the drop. Returns
    /// `(pass, suppressed_since_last_pass)`.
    fn admit(&mut self) -> (bool, u32) {
        let now = std::time::Instant::now();
        let elapsed = self.last.map(|l| now.duration_since(l)).unwrap_or_default();
        self.last = Some(now);
        let refill = (elapsed.as_secs_f64() / NOTICE_REFILL_PER_TOKEN.as_secs_f64()) as u32;
        self.tokens = NOTICE_BURST.min(self.tokens.saturating_add(refill));
        if self.tokens == 0 {
            self.suppressed = self.suppressed.saturating_add(1);
            return (false, 0);
        }
        self.tokens -= 1;
        (true, std::mem::take(&mut self.suppressed))
    }
}

impl McpManager {
    /// Returns the event receiver — main spawns the consumer that turns
    /// [`McpEvent`]s into broadcasts + engine rebuilds.
    pub fn new(
        registry: Arc<ToolRegistry>,
        connect: Connect,
    ) -> (Self, tokio::sync::mpsc::UnboundedReceiver<McpEvent>) {
        let (events, events_rx) = tokio::sync::mpsc::unbounded_channel();
        (
            Self {
                entries: RwLock::new(HashMap::new()),
                registry,
                connect,
                events,
                backoff_base: BACKOFF_BASE,
                backoff_max: BACKOFF_MAX,
                notice_gates: std::sync::Mutex::new(HashMap::new()),
            },
            events_rx,
        )
    }

    /// Startup restore: connect every launch-list row concurrently (one
    /// hung server costs only its own init timeout) and register the
    /// connected ones. A row that fails is SKIPPED with a warning — it
    /// stays fixable from the UI and retried at the next restart. Every
    /// connected entry gets a SUPERVISOR task (rmcp death signal →
    /// respawn with backoff).
    pub async fn restore(self: &Arc<Self>, store: &Arc<Store>) {
        let Ok(rows) = store.list_mcp_servers().await else {
            tracing::warn!("failed to load MCP servers; skipping");
            return;
        };
        let connects = rows.into_iter().map(|row| async move {
            tracing::info!(id = %row.id, command = %row.command, "connecting to MCP");
            let cfg = flux_mcp::McpServerConfig {
                command: row.command.clone(),
                args: row.args.clone(),
                env: row.env.clone(),
            };
            let result = (self.connect)(cfg, self.wire_notices(&row.id)).await;
            (row, result)
        });
        for (row, result) in futures_util::future::join_all(connects).await {
            match result {
                Ok(connected) => {
                    let id = row.id.clone();
                    let outcomes = register_tools(&self.registry, &id, &connected.tools);
                    let names = registered_names(&outcomes);
                    tracing::info!(id = %id, tools = names.len(), "MCP connected");
                    self.entries.write().unwrap().insert(
                        id.clone(),
                        McpEntry {
                            row,
                            tool_names: names,
                            state: McpLiveState::Running,
                            cancel: connected.cancel,
                            relist: connected.relist,
                        },
                    );
                    self.spawn_supervisor(id, connected.handle);
                }
                Err(e) => {
                    tracing::warn!(id = %row.id, error = %e, "MCP server failed to start; skipping");
                }
            }
        }
    }

    /// Apply one persisted add: connect + register into the global
    /// registry + record the entry. Failure = the row stays on disk (the
    /// next restart retries) and the error rides back to the UI inline.
    pub async fn apply_add(
        self: &Arc<Self>,
        id: &str,
        row: &McpServerRow,
    ) -> anyhow::Result<Vec<ToolRegistration>> {
        if self.entries.read().unwrap().contains_key(id) {
            anyhow::bail!("MCP server '{id}' is already running");
        }
        let cfg = flux_mcp::McpServerConfig {
            command: row.command.clone(),
            args: row.args.clone(),
            env: row.env.clone(),
        };
        let connected = (self.connect)(cfg, self.wire_notices(id)).await?;
        let outcomes = register_tools(&self.registry, id, &connected.tools);
        self.entries.write().unwrap().insert(
            id.to_string(),
            McpEntry {
                row: row.clone(),
                tool_names: registered_names(&outcomes),
                state: McpLiveState::Running,
                cancel: connected.cancel,
                relist: connected.relist,
            },
        );
        self.spawn_supervisor(id.to_string(), connected.handle);
        Ok(outcomes)
    }

    /// Apply one persisted remove: cancel the session (the supervisor's
    /// rmcp quit signal resolves; it unregisters its names and exits),
    /// drop the entry, and unregister exactly the registered names here —
    /// best-effort: an entry that never came up (a failed startup connect)
    /// is not an error — the row is gone either way. Returns whether a
    /// live tool set actually changed (the caller rebuilds the engines
    /// only then).
    pub fn apply_remove(&self, id: &str) -> anyhow::Result<bool> {
        let Some(entry) = self.entries.write().unwrap().remove(id) else {
            return Ok(false);
        };
        (entry.cancel)();
        for name in &entry.tool_names {
            self.registry.unregister(name);
        }
        self.notice_gates.lock().unwrap().remove(id);
        Ok(!entry.tool_names.is_empty())
    }

    /// Live states for the wire summaries (absent = OFFLINE).
    pub fn states(&self) -> HashMap<String, McpState> {
        self.entries
            .read()
            .unwrap()
            .iter()
            .map(|(id, e)| (id.clone(), McpState::from(e.state)))
            .collect()
    }

    /// Live tool-name sets for the wire summaries (absent = no live
    /// session — the summary distinguishes that via the state).
    pub fn tool_names(&self) -> HashMap<String, Vec<String>> {
        self.entries
            .read()
            .unwrap()
            .iter()
            .map(|(id, e)| (id.clone(), e.tool_names.clone()))
            .collect()
    }

    fn spawn_supervisor(self: &Arc<Self>, id: String, handle: Box<dyn McpSessionHandle>) {
        let mgr = Arc::clone(self);
        tokio::spawn(mgr.supervise(id, handle));
    }

    /// Wire the notice forwarder for one connection: the handler's sink
    /// is a fresh channel whose receiver maps notices onto the manager's
    /// event loop, tagged with the server id. Lives while the connection
    /// does — the handler drops the sender when the session dies.
    fn wire_notices(
        self: &Arc<Self>,
        id: &str,
    ) -> tokio::sync::mpsc::UnboundedSender<flux_mcp::ServerNotice> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mgr = Arc::clone(self);
        let id = id.to_string();
        tokio::spawn(async move {
            while let Some(notice) = rx.recv().await {
                match notice {
                    flux_mcp::ServerNotice::ToolsListChanged => {
                        mgr.handle_tools_list_changed(&id).await;
                    }
                    flux_mcp::ServerNotice::Log { level, message } => {
                        mgr.handle_log(&id, level, message);
                    }
                }
            }
        });
        tx
    }

    /// `tools/list_changed` (F-10a): re-list the live session's tool set
    /// and re-register it over the old one. The old wrappers are
    /// unregistered only AFTER a successful re-list (a failed re-list
    /// keeps the old, possibly-stale set — still callable). The chats
    /// ALWAYS rebuild: the registry now binds fresh wrappers and a name
    /// may have vanished entirely.
    async fn handle_tools_list_changed(self: &Arc<Self>, id: &str) {
        let relist = self
            .entries
            .read()
            .unwrap()
            .get(id)
            .and_then(|e| e.relist.clone());
        let Some(relist) = relist else {
            tracing::debug!(id = %id, "tools/list_changed for a fake entry; ignored");
            return;
        };
        tracing::info!(id = %id, "MCP server tool list changed; re-listing");
        let tools = match relist().await {
            Ok(tools) => tools,
            Err(e) => {
                tracing::warn!(id = %id, error = %e, "MCP tool re-list failed; keeping the old set");
                return;
            }
        };
        {
            let mut entries = self.entries.write().unwrap();
            let Some(entry) = entries.get_mut(id) else {
                return; // removed mid-re-list
            };
            for name in std::mem::take(&mut entry.tool_names) {
                self.registry.unregister(&name);
            }
            let outcomes = register_tools(&self.registry, id, &tools);
            entry.tool_names = registered_names(&outcomes);
        }
        self.emit(McpEvent::ToolsChanged(id.to_string()));
    }

    /// A logging notification (F-10b): cap the message, pass the
    /// per-server token bucket, and forward. Drops are folded into a
    /// synthetic "suppressed" notice when the gate reopens — the UI
    /// never sees a flood, but knows one happened.
    fn handle_log(self: &Arc<Self>, id: &str, level: String, message: String) {
        let mut message = message;
        if message.chars().count() > NOTICE_MESSAGE_MAX {
            message = message.chars().take(NOTICE_MESSAGE_MAX).collect::<String>() + "…";
        }
        let (pass, suppressed) = {
            let mut gates = self.notice_gates.lock().unwrap();
            gates.entry(id.to_string()).or_default().admit()
        };
        if !pass {
            return;
        }
        if suppressed > 0 {
            self.events
                .send(McpEvent::Notice {
                    id: id.to_string(),
                    level: "info".to_string(),
                    message: format!("…{suppressed} earlier notices suppressed"),
                })
                .ok();
        }
        self.events
            .send(McpEvent::Notice {
                id: id.to_string(),
                level,
                message,
            })
            .ok();
    }

    /// The self-healing loop: await rmcp's quit signal; a CLOSED/FAILED
    /// session respawns with backoff (the entry stays visible as Backoff
    /// and its tools are unregistered — the old wrappers bind to the dead
    /// peer); a CANCELLED session is a deliberate teardown (apply_remove
    /// already dropped the entry) and ends the task. Owns the handle so a
    /// successful respawn swaps in the NEW session and keeps supervising.
    async fn supervise(self: Arc<Self>, id: String, mut handle: Box<dyn McpSessionHandle>) {
        loop {
            let reason = handle.quit().await;
            {
                let mut entries = self.entries.write().unwrap();
                let Some(entry) = entries.get_mut(&id) else {
                    // apply_remove raced ahead of the signal — nothing left.
                    return;
                };
                for name in std::mem::take(&mut entry.tool_names) {
                    self.registry.unregister(&name);
                }
                if matches!(reason, flux_mcp::McpQuit::Cancelled) {
                    return; // deliberate teardown — the entry is already gone
                }
                entry.state = McpLiveState::Backoff;
            }
            tracing::warn!(id = %id, ?reason, "MCP session ended; respawning with backoff");
            self.emit(McpEvent::Changed(id.clone()));

            let mut attempt: u32 = 0;
            loop {
                attempt += 1;
                let exponent = attempt.saturating_sub(1).min(16);
                let delay = (self.backoff_base)
                    .saturating_mul(2u32.saturating_pow(exponent))
                    .min(self.backoff_max);
                tokio::time::sleep(delay).await;
                // The row may have been removed while we backed off.
                let Some(row) = self.entries.read().unwrap().get(&id).map(|e| e.row.clone()) else {
                    tracing::info!(id = %id, "MCP entry removed during backoff; supervisor exiting");
                    return;
                };
                let cfg = flux_mcp::McpServerConfig {
                    command: row.command.clone(),
                    args: row.args.clone(),
                    env: row.env.clone(),
                };
                match (self.connect)(cfg, self.wire_notices(&id)).await {
                    Ok(connected) => {
                        let names = registered_names(&register_tools(
                            &self.registry,
                            &id,
                            &connected.tools,
                        ));
                        {
                            let mut entries = self.entries.write().unwrap();
                            let Some(entry) = entries.get_mut(&id) else {
                                return; // removed mid-connect; the dropped handle kills the child
                            };
                            entry.tool_names = names;
                            entry.state = McpLiveState::Running;
                            entry.cancel = connected.cancel;
                            entry.relist = connected.relist;
                        }
                        tracing::info!(id = %id, attempt, "MCP respawned");
                        // ALWAYS a rebuild: the old tool wrappers bind to
                        // the dead peer, regardless of the name set being
                        // equal.
                        self.emit(McpEvent::ToolsChanged(id.clone()));
                        handle = connected.handle; // supervise the NEW session
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(
                            id = %id,
                            attempt,
                            error = %e,
                            "MCP respawn failed; backing off again"
                        );
                    }
                }
            }
        }
    }

    fn emit(&self, event: McpEvent) {
        let _ = self.events.send(event);
    }
}

/// Register the server's tools into the global registry under the
/// collision rules (reserved chat-owned names and existing tools are
/// skipped — an external tool can never shadow them). Returns the names
/// that REGISTERED (the removal set).
/// One tool's registration outcome — the add ack's per-tool detail: a
/// name collision is visible in the UI, never only in the server log.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolRegistration {
    pub name: String,
    pub registered: bool,
    /// Why a tool was skipped — absent when registered.
    pub reason: Option<String>,
}

/// The names that actually registered (the entry's truth set).
fn registered_names(outcomes: &[ToolRegistration]) -> Vec<String> {
    outcomes
        .iter()
        .filter(|r| r.registered)
        .map(|r| r.name.clone())
        .collect()
}

fn register_tools(
    registry: &ToolRegistry,
    id: &str,
    tools: &[Arc<dyn Tool>],
) -> Vec<ToolRegistration> {
    tools
        .iter()
        .map(|tool| {
            let name = tool.name().to_string();
            if flux_chat::reserved::is_reserved_tool_name(&name) {
                tracing::error!(
                    server = %id,
                    tool = %name,
                    "MCP tool name collides with a built-in/reserved tool; skipping",
                );
                ToolRegistration {
                    name,
                    registered: false,
                    reason: Some("reserved by flux (built-in/state/question tools)".to_string()),
                }
            } else if registry.register_if_absent(Arc::clone(tool)) {
                ToolRegistration {
                    name,
                    registered: true,
                    reason: None,
                }
            } else {
                tracing::error!(
                    server = %id,
                    tool = %name,
                    "MCP tool name already registered; skipping",
                );
                ToolRegistration {
                    name,
                    registered: false,
                    reason: Some("name already registered".to_string()),
                }
            }
        })
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
    Ok(summary(row, McpState::Unspecified, Vec::new()))
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
pub(crate) async fn summaries(
    store: &Arc<Store>,
    states: &HashMap<String, McpState>,
    tool_names: &HashMap<String, Vec<String>>,
) -> anyhow::Result<Vec<McpServerSummary>> {
    let mut out: Vec<McpServerSummary> = store
        .list_mcp_servers()
        .await?
        .into_iter()
        .map(|row| {
            // A row with no live entry never came up (failed startup
            // connect) — OFFLINE, per the persist-first contract.
            let state = states.get(&row.id).copied().unwrap_or(McpState::Offline);
            let names = tool_names.get(&row.id).cloned().unwrap_or_default();
            summary(row, state, names)
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

fn summary(row: McpServerRow, state: McpState, tool_names: Vec<String>) -> McpServerSummary {
    let mut env_keys: Vec<String> = row.env.into_keys().collect();
    env_keys.sort();
    McpServerSummary {
        id: row.id,
        command: row.command,
        args: row.args,
        env_keys,
        state: state as i32,
        tool_names,
    }
}

/// Consume supervisor events: a respawn (ToolsChanged) fans an engine
/// rebuild + broadcast — the same machinery `add_mcp_server` uses; a
/// state-only change (Changed) re-broadcasts so the UI's status stays
/// truthful. Runs for the process lifetime.
pub(crate) async fn consume_events(
    state: Arc<crate::ServerState>,
    mcp: Arc<McpManager>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<McpEvent>,
) {
    use crate::management::broadcast_mcp_servers;
    while let Some(event) = rx.recv().await {
        match event {
            McpEvent::ToolsChanged(id) => {
                tracing::info!(id = %id, "MCP tools changed; rebuilding chats");
                state.restart_all_chats().await;
                broadcast_mcp_servers(&state, &mcp).await;
            }
            McpEvent::Changed(id) => {
                tracing::info!(id = %id, "MCP state changed");
                broadcast_mcp_servers(&state, &mcp).await;
            }
            McpEvent::Notice { id, level, message } => {
                crate::management::broadcast_mcp_notice(&state, &id, &level, &message).await;
            }
        }
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

    /// A connect whose handles SHARE one quit script (the test pushes
    /// `McpQuit`s to kill whichever session is live) — the death→respawn
    /// path's driver.
    fn fake_connect_shared(
        quits: Arc<std::sync::Mutex<std::collections::VecDeque<flux_mcp::McpQuit>>>,
        ok_tools: Vec<(&'static str, Vec<Arc<dyn Tool>>)>,
    ) -> Connect {
        let map: HashMap<String, Vec<Arc<dyn Tool>>> = ok_tools
            .into_iter()
            .map(|(id, tools)| (id.to_string(), tools))
            .collect();
        Arc::new(
            move |cfg: flux_mcp::McpServerConfig,
                  _notices: tokio::sync::mpsc::UnboundedSender<flux_mcp::ServerNotice>| {
                let tools = map.get(&cfg.command).cloned();
                let quits = Arc::clone(&quits);
                Box::pin(async move {
                    let tools = tools.ok_or_else(|| anyhow::anyhow!("spawn failed"))?;
                    Ok(ConnectedServer {
                        handle: Box::new(FakeHandle { quits }),
                        cancel: Arc::new(|| {}),
                        tools,
                        relist: None,
                    })
                }) as ConnectFuture
            },
        )
    }

    /// A connect that succeeds for `ok` ids with the given tools and fails
    /// for everything else (the spawn-failure path). Every handle from one
    /// id SHARES a quit script (push `McpQuit`s to kill the session).
    fn fake_connect(ok_tools: Vec<(&'static str, Vec<Arc<dyn Tool>>)>) -> Connect {
        let map: HashMap<String, Vec<Arc<dyn Tool>>> = ok_tools
            .into_iter()
            .map(|(id, tools)| (id.to_string(), tools))
            .collect();
        Arc::new(
            move |cfg: flux_mcp::McpServerConfig,
                  _notices: tokio::sync::mpsc::UnboundedSender<flux_mcp::ServerNotice>| {
                let tools = map.get(&cfg.command).cloned();
                Box::pin(async move {
                    let tools = tools.ok_or_else(|| anyhow::anyhow!("spawn failed"))?;
                    Ok(ConnectedServer {
                        handle: Box::new(FakeHandle::default()),
                        cancel: Arc::new(|| {}),
                        tools,
                        relist: None,
                    })
                }) as ConnectFuture
            },
        )
    }

    /// Scripted session handle: `quits` holds queued outcomes (empty =
    /// the session stays alive); `cancelled` records cancel() calls.
    struct FakeHandle {
        quits: Arc<std::sync::Mutex<std::collections::VecDeque<flux_mcp::McpQuit>>>,
    }

    impl Default for FakeHandle {
        fn default() -> Self {
            Self {
                quits: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
            }
        }
    }

    impl McpSessionHandle for FakeHandle {
        fn quit(
            &mut self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = flux_mcp::McpQuit> + Send + '_>>
        {
            let quits = Arc::clone(&self.quits);
            Box::pin(async move {
                loop {
                    if let Some(q) = quits.lock().unwrap().pop_front() {
                        return q;
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
        }
    }
    // (no-op cancel: tests script removals through the entry's closure)

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
        assert_eq!(
            summaries(&store, &HashMap::new(), &HashMap::new())
                .await
                .unwrap()
                .len(),
            1
        );
        // Duplicate id rejects.
        let err = add(&store, row("fs", "npx")).await.unwrap_err();
        assert!(err.to_string().contains("duplicate MCP server id"));
    }

    #[tokio::test]
    async fn remove_round_trip() {
        let store = test_store().await;
        add(&store, row("fs", "npx")).await.unwrap();
        remove(&store, "fs").await.unwrap();
        assert!(
            summaries(&store, &HashMap::new(), &HashMap::new())
                .await
                .unwrap()
                .is_empty()
        );
        let err = remove(&store, "fs").await.unwrap_err();
        assert!(err.to_string().contains("unknown MCP server id"));
    }

    #[tokio::test]
    async fn apply_add_registers_tools_and_apply_remove_unregisters_exactly_them() {
        let store = test_store().await;
        let registry = Arc::new(ToolRegistry::new());
        registry.register(Arc::new(Builtin));
        let (manager, _rx) = McpManager::new(
            Arc::clone(&registry),
            fake_connect(vec![(
                "echo",
                fake_tools(&["mcp_echo", "bash", "state_get"]),
            )]),
        );
        let manager = Arc::new(manager);

        add(&store, row("echo", "echo")).await.unwrap();
        let outcomes = manager
            .apply_add("echo", &row("echo", "echo"))
            .await
            .unwrap();

        // mcp_echo registered; the built-in and the reserved name skipped —
        // each with its reason on the ack-visible outcome (the UI shows
        // what landed instead of grepping the log).
        assert!(registry.get("mcp_echo").is_some());
        assert!(registry.get("bash").is_some(), "built-in untouched");
        assert!(registry.get("state_get").is_none(), "reserved name skipped");
        assert_eq!(outcomes.len(), 3);
        let by_name: HashMap<&str, &ToolRegistration> =
            outcomes.iter().map(|r| (r.name.as_str(), r)).collect();
        assert!(by_name["mcp_echo"].registered);
        assert_eq!(by_name["mcp_echo"].reason, None);
        assert!(!by_name["bash"].registered);
        assert_eq!(
            by_name["bash"].reason.as_deref(),
            Some("name already registered")
        );
        assert!(!by_name["state_get"].registered);
        assert!(
            by_name["state_get"]
                .reason
                .as_deref()
                .unwrap()
                .contains("reserved")
        );

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
        let (manager, _rx) = McpManager::new(Arc::clone(&registry), fake_connect(vec![])); // everything fails
        let manager = Arc::new(manager);

        add(&store, row("bad", "bad")).await.unwrap();
        let err = manager
            .apply_add("bad", &row("bad", "bad"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("spawn failed"));
        // The row stays (the next restart retries); the registry is clean.
        assert_eq!(
            summaries(&store, &HashMap::new(), &HashMap::new())
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(registry.entries().is_empty());
    }

    #[tokio::test]
    async fn restore_skips_failed_rows_and_registers_connected_ones() {
        let store = test_store().await;
        add(&store, row("good", "good")).await.unwrap();
        add(&store, row("dead", "dead")).await.unwrap();
        let registry = Arc::new(ToolRegistry::new());
        let (manager, _rx) = McpManager::new(
            Arc::clone(&registry),
            fake_connect(vec![("good", fake_tools(&["mcp_a"]))]),
        );
        let manager = Arc::new(manager);
        manager.restore(&store).await;
        assert!(registry.get("mcp_a").is_some());
        // The failed row stays on the launch list (retry at next restart).
        assert_eq!(
            summaries(&store, &HashMap::new(), &HashMap::new())
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn death_respawns_with_backoff_and_fans_tools_changed() {
        let store = test_store().await;
        let registry = Arc::new(ToolRegistry::new());
        let quits = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        let (manager, mut rx) = McpManager::new(
            Arc::clone(&registry),
            fake_connect_shared(
                Arc::clone(&quits),
                vec![("echo", fake_tools(&["mcp_echo"]))],
            ),
        );
        let mut manager = manager;
        manager.backoff_base = Duration::from_millis(2);
        manager.backoff_max = Duration::from_millis(4);
        let manager = Arc::new(manager);

        add(&store, row("echo", "echo")).await.unwrap();
        manager
            .apply_add("echo", &row("echo", "echo"))
            .await
            .unwrap();
        assert!(registry.get("mcp_echo").is_some());

        // Kill the live session: the supervisor must unregister the dead
        // tools, mark Backoff, respawn, re-register, and fan a rebuild.
        quits.lock().unwrap().push_back(flux_mcp::McpQuit::Closed);

        let e1 = rx.recv().await.unwrap();
        assert!(
            matches!(e1, McpEvent::Changed(_)),
            "backoff announced: {e1:?}"
        );
        let e2 = rx.recv().await.unwrap();
        assert!(
            matches!(e2, McpEvent::ToolsChanged(_)),
            "rebuild fanned on respawn: {e2:?}"
        );
        assert!(registry.get("mcp_echo").is_some(), "tools re-registered");
        assert_eq!(
            manager.states().get("echo"),
            Some(&McpState::Running),
            "back to running after the respawn"
        );
    }

    /// A connect that succeeds exactly ONCE (the initial spawn); every
    /// later call — the respawns — fails.
    fn fake_connect_ok_then_fail(
        tools: Vec<Arc<dyn Tool>>,
    ) -> (
        Arc<std::sync::Mutex<std::collections::VecDeque<flux_mcp::McpQuit>>>,
        Connect,
    ) {
        let count = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let quits = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        let connect_quits = Arc::clone(&quits);
        let connect = Arc::new(
            move |_cfg: flux_mcp::McpServerConfig,
                  _notices: tokio::sync::mpsc::UnboundedSender<flux_mcp::ServerNotice>| {
                let n = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let tools = tools.clone();
                let quits = Arc::clone(&connect_quits);
                Box::pin(async move {
                    if n > 0 {
                        anyhow::bail!("respawn failed");
                    }
                    Ok(ConnectedServer {
                        handle: Box::new(FakeHandle { quits }),
                        cancel: Arc::new(|| {}),
                        tools,
                        relist: None,
                    })
                }) as ConnectFuture
            },
        );
        (quits, connect)
    }

    #[tokio::test]
    async fn tools_list_changed_relists_and_reregisters_exactly() {
        let store = test_store().await;
        let registry = Arc::new(ToolRegistry::new());
        let tools = fake_tools(&["old_tool"]);
        // The fake's relist returns the NEW server-side set (a rename).
        let relist_tools = fake_tools(&["new_tool"]);
        let relist: flux_mcp::RelistFn = {
            let relist_tools = std::sync::Mutex::new(relist_tools);
            Arc::new(move || {
                let tools = relist_tools.lock().unwrap().clone();
                Box::pin(async move { Ok(tools) })
            })
        };
        let map: HashMap<String, (Vec<Arc<dyn Tool>>, flux_mcp::RelistFn)> =
            HashMap::from([("echo".to_string(), (tools, Arc::clone(&relist)))]);
        let connect: Connect = Arc::new(
            move |cfg: flux_mcp::McpServerConfig,
                  _notices: tokio::sync::mpsc::UnboundedSender<flux_mcp::ServerNotice>| {
                let entry = map.get(&cfg.command).cloned();
                Box::pin(async move {
                    let (tools, relist) = entry.ok_or_else(|| anyhow::anyhow!("spawn failed"))?;
                    Ok(ConnectedServer {
                        handle: Box::new(FakeHandle::default()),
                        cancel: Arc::new(|| {}),
                        tools,
                        relist: Some(relist),
                    })
                }) as ConnectFuture
            },
        );
        let (manager, mut rx) = McpManager::new(Arc::clone(&registry), connect);
        let manager = Arc::new(manager);
        add(&store, row("echo", "echo")).await.unwrap();
        manager
            .apply_add("echo", &row("echo", "echo"))
            .await
            .unwrap();
        assert!(registry.get("old_tool").is_some());

        manager.handle_tools_list_changed("echo").await;

        // The old wrapper is unregistered, the fresh set registered, and
        // the chats rebuild (the registry re-bound).
        assert!(registry.get("old_tool").is_none());
        assert!(registry.get("new_tool").is_some());
        assert_eq!(
            manager.tool_names().get("echo").map(|v| v.as_slice()),
            Some(&["new_tool".to_string()][..])
        );
        assert!(matches!(rx.recv().await, Some(McpEvent::ToolsChanged(id)) if id == "echo"));
    }

    #[tokio::test]
    async fn notice_gate_admits_the_burst_then_folds_the_flood() {
        let registry = Arc::new(ToolRegistry::new());
        let (manager, _rx) = McpManager::new(
            Arc::clone(&registry),
            fake_connect(vec![("echo", fake_tools(&["mcp_a"]))]),
        );
        let manager = Arc::new(manager);
        manager
            .apply_add("echo", &row("echo", "echo"))
            .await
            .unwrap();

        // The burst passes; everything after is dropped (a fast test's
        // elapsed time never refills a token).
        for i in 0..NOTICE_BURST {
            manager.handle_log("echo", "info".into(), format!("m{i}"));
        }
        manager.handle_log("echo", "info".into(), "flood".into());
        assert_eq!(manager.notice_gates.lock().unwrap()["echo"].suppressed, 1);
        // Other servers have their own gate.
        manager.handle_log("other", "info".into(), "m".into());
        assert!(manager.notice_gates.lock().unwrap().contains_key("other"));
        // Removal clears the gate.
        manager.apply_remove("echo").unwrap();
        assert!(!manager.notice_gates.lock().unwrap().contains_key("echo"));
    }

    #[tokio::test]
    async fn respawn_failure_stays_in_backoff_and_retries() {
        let store = test_store().await;
        let registry = Arc::new(ToolRegistry::new());
        let (quits, connect) = fake_connect_ok_then_fail(fake_tools(&["mcp_bad"]));
        let (manager, mut rx) = McpManager::new(Arc::clone(&registry), connect);
        let mut manager = manager;
        manager.backoff_base = Duration::from_millis(1);
        manager.backoff_max = Duration::from_millis(2);
        let manager = Arc::new(manager);

        add(&store, row("bad", "bad")).await.unwrap();
        manager.apply_add("bad", &row("bad", "bad")).await.unwrap();
        assert!(registry.get("mcp_bad").is_some());

        // Kill the session; every respawn attempt fails → the entry stays
        // Backoff with no tools registered, retrying forever (capped).
        quits.lock().unwrap().push_back(flux_mcp::McpQuit::Closed);
        let e1 = rx.recv().await.unwrap();
        assert!(matches!(e1, McpEvent::Changed(_)));
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(registry.get("mcp_bad").is_none(), "no tools while down");
        assert_eq!(
            manager.states().get("bad"),
            Some(&McpState::Backoff),
            "stuck in backoff while respawns fail"
        );
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
