//! Flux MCP client: bridge external MCP servers into Flux agents.
//!
//! This crate connects external MCP servers — either spawned as child
//! processes (rmcp's stdio transport) or reached as remote Streamable HTTP
//! endpoints — fetches their tool lists, and exposes those tools as
//! [`flux_core::Tool`] objects. Everything downstream of the connect
//! (handshake, tool wrapping, notifications, session lifetime) is
//! transport-agnostic.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

use async_trait::async_trait;
use flux_core::{CoreError, Tool, ToolCtx};
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientInfo, ContentBlock};
use rmcp::service::{MaybeSendFuture, NotificationContext, RoleClient};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{
    IntoTransport, StreamableHttpClientTransport, TokioChildProcess, TransportAdapterIdentity,
};
use rmcp::{ClientHandler, Peer};
use tokio::process::Command;

/// How flux connects to one external MCP server — the per-server connect
/// config, carried from the store row (mapped in flux-server) into the
/// transport build here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerConfig {
    /// A child process flux spawns and talks to over stdio.
    Stdio {
        /// Executable to run.
        command: String,
        /// Arguments passed to the executable.
        args: Vec<String>,
        /// Extra environment variables (the child inherits NOTHING else).
        env: HashMap<String, String>,
    },
    /// A remote Streamable HTTP endpoint.
    Http {
        /// The endpoint URL (e.g. `https://example.com/mcp`).
        url: String,
        /// Headers sent with every request — auth rides here as an
        /// ordinary header (e.g. `Authorization: Bearer …`).
        headers: HashMap<String, String>,
    },
}

/// Errors that can occur when connecting to or calling an MCP server.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// Failed to spawn the child process.
    #[error("failed to spawn MCP server: {0}")]
    Spawn(#[from] std::io::Error),

    /// A header name/value failed to parse (HTTP transport config).
    #[error("invalid MCP HTTP header: {0}")]
    Header(#[from] http::header::InvalidHeaderName),

    /// A header value failed to parse (HTTP transport config).
    #[error("invalid MCP HTTP header value: {0}")]
    HeaderValue(#[from] http::header::InvalidHeaderValue),

    /// Failed to initialize the MCP session.
    #[error("MCP initialization failed: {0}")]
    Initialize(String),

    /// Failed to list tools from the MCP server.
    #[error("failed to list MCP tools: {0}")]
    ListTools(#[from] rmcp::ServiceError),

    /// Operation timed out.
    #[error("MCP operation timed out after {0}s")]
    Timeout(u64),
}

/// A server→client notification that concerns flux. The handler forwards
/// these to the manager's event loop; progress is deliberately NOT
/// forwarded — it is request-scoped, the tool wrapper's own 60s timeout
/// governs in-flight calls, and mapping progress tokens onto tool calls
/// would be guesswork (see the module doc of the server's McpManager).
#[derive(Debug, Clone)]
pub enum ServerNotice {
    /// `notifications/tools/list_changed` — the server's tool set changed;
    /// the manager re-lists and re-registers.
    ToolsListChanged,
    /// `notifications/message` — a log record with its level. The level is
    /// the MCP spec's lowercase string ("debug"/"info"/"warning"/…).
    Log { level: String, message: String },
}

/// The notifying handler: forwards the notifications flux cares about to
/// the manager's sink (one channel per connection — it dies with the
/// session, so the forwarder never outlives a dead child).
#[derive(Clone)]
struct NoticeClientHandler {
    sink: Option<tokio::sync::mpsc::UnboundedSender<ServerNotice>>,
}

impl ClientHandler for NoticeClientHandler {
    fn get_info(&self) -> ClientInfo {
        ClientInfo::default()
    }

    fn on_tool_list_changed(
        &self,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        let sink = self.sink.clone();
        async move {
            if let Some(sink) = sink {
                let _ = sink.send(ServerNotice::ToolsListChanged);
            }
        }
    }

    // SEP-2577 deprecated the logging notification protocol-side, but it
    // remains what every deployed server emits — handling it is the
    // pragmatic default until a replacement exists.
    #[allow(deprecated)]
    fn on_logging_message(
        &self,
        params: rmcp::model::LoggingMessageNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        let sink = self.sink.clone();
        async move {
            if let Some(sink) = sink {
                let level = logging_level_str(params.level);
                let message = notice_text(params.data);
                let _ = sink.send(ServerNotice::Log { level, message });
            }
        }
    }
}

/// The MCP spec's lowercase level spelling (serde lowercase — no Display).
#[allow(deprecated)]
fn logging_level_str(level: rmcp::model::LoggingLevel) -> String {
    use rmcp::model::LoggingLevel::*;
    match level {
        Debug => "debug",
        Info => "info",
        Notice => "notice",
        Warning => "warning",
        Error => "error",
        Critical => "critical",
        Alert => "alert",
        Emergency => "emergency",
    }
    .to_string()
}

/// The log data is arbitrary JSON: a string passes as-is, anything else
/// serializes (bounded by the manager's message cap downstream).
fn notice_text(data: serde_json::Value) -> String {
    match data {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    }
}

/// RAII session handle: Drop cancels the rmcp service token, which ends
/// the serve loop and drops the transport (killing the child). The death
/// signal is rmcp's own `RunningService::waiting()` — spawned at connect
/// and awaited by the self-healing supervisor; `QuitReason::Closed` =
/// the child died, `Cancelled` = our cancel.
pub struct McpSession {
    /// Resolves when the serve loop ends (child death, cancel, or task
    /// failure). Spawned in [`connect_with_peer`] from
    /// `RunningService::waiting()` — the one rmcp-native death signal
    /// (the cancellation token does NOT fire on transport closure).
    quit:
        Option<tokio::task::JoinHandle<Result<rmcp::service::QuitReason, tokio::task::JoinError>>>,
    /// Cancels the serve loop on explicit `cancel()`.
    token: Option<rmcp::service::RunningServiceCancellationToken>,
    /// Second token clone dedicated to Drop — `cancel(self)` consumes, so
    /// explicit cancel and Drop each get their own wrapper.
    drop_token: Option<rmcp::service::RunningServiceCancellationToken>,
}

/// Why a session's serve loop ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpQuit {
    /// The transport closed — for stdio, the child process died.
    Closed,
    /// Our `cancel()` (removal / deliberate teardown).
    Cancelled,
    /// Task-level failure (join error).
    Failed,
}

impl McpSession {
    /// Await the end of the session's serve loop. A second call (the
    /// watcher was already taken) reports `Failed` — the supervisor
    /// awaits exactly once per handle.
    pub async fn quit(&mut self) -> McpQuit {
        use rmcp::service::QuitReason;
        let Some(quit) = self.quit.take() else {
            return McpQuit::Failed;
        };
        match quit.await {
            Ok(Ok(QuitReason::Closed)) => McpQuit::Closed,
            Ok(Ok(QuitReason::Cancelled)) => McpQuit::Cancelled,
            Ok(Ok(QuitReason::JoinError(_))) | Ok(Ok(_)) | Ok(Err(_)) | Err(_) => {
                // `QuitReason` is non_exhaustive; anything new is treated
                // as a failure (respawn-worthy) rather than a clean cancel.
                McpQuit::Failed
            }
        }
    }

    /// End the session: cancel the service token — the serve loop exits
    /// and the transport drops, killing the child.
    pub fn cancel(&mut self) {
        if let Some(token) = self.token.take() {
            token.cancel();
        }
    }
}

impl Drop for McpSession {
    fn drop(&mut self) {
        // RAII: a dropped session kills its child (cancel → serve loop
        // exits → transport dropped → ChildWithCleanup).
        if let Some(token) = self.drop_token.take() {
            token.cancel();
        }
    }
}

/// An MCP tool wrapped as a [`flux_core::Tool`].
pub struct McpToolWrapper {
    name: String,
    description: String,
    schema: serde_json::Value,
    peer: Peer<RoleClient>,
}

impl McpToolWrapper {
    /// Create a wrapper from an rmcp tool descriptor and peer.
    pub fn from_rmcp_tool(tool: rmcp::model::Tool, peer: Peer<RoleClient>) -> Self {
        let schema: serde_json::Value = serde_json::Value::Object((*tool.input_schema).clone());
        Self {
            name: tool.name.to_string(),
            description: tool
                .description
                .map(|s| s.to_string())
                .unwrap_or_else(|| "(no description)".to_string()),
            schema,
            peer,
        }
    }
}

#[async_trait]
impl Tool for McpToolWrapper {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn schema(&self) -> serde_json::Value {
        self.schema.clone()
    }

    async fn call(
        &self,
        arguments: HashMap<String, serde_json::Value>,
        ctx: ToolCtx,
    ) -> Result<String, CoreError> {
        // The args map is inherently an object — convert to a JSON Map for
        // the MCP call. Nested objects pass through unchanged.
        let args_map: serde_json::Map<String, serde_json::Value> = arguments.into_iter().collect();

        let params = CallToolRequestParams::new(self.name.clone()).with_arguments(args_map);

        // Cooperative cancel: on interrupt, stop waiting (dropping the
        // request future releases the in-flight call) and return an empty
        // result — the kernel marks the interruption on the transcript.
        let request = self.peer.call_tool(params);
        tokio::select! {
            r = timeout(Duration::from_secs(60), request) => {
                let result: CallToolResult = r
                    .map_err(|_| CoreError::Tool(format!("MCP tool {} timed out after 60s", self.name)))?
                    .map_err(|e| CoreError::Tool(format!("MCP call failed: {e}")))?;
                Ok(extract_text_from_result(result))
            }
            _ = ctx.cancel.cancelled() => Ok(String::new()),
        }
    }
}

/// A capability to re-list a live session's tools and wrap them fresh —
/// the `tools/list_changed` path (the manager re-registers the returned
/// set over the old one). Built per connection in
/// [`production_connect`]; the wrapper's call bindings ride the same
/// live peer.
pub type RelistFn = Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn Future<Output = Result<Vec<Arc<dyn Tool>>, McpError>> + Send>>
        + Send
        + Sync,
>;

/// Build the relist capability for a live peer: list the current tool set
/// (30s timeout, same as connect) and wrap each entry against that peer.
pub fn relist_for_peer(peer: &Peer<RoleClient>) -> RelistFn {
    let peer = peer.clone();
    Arc::new(move || {
        let peer = peer.clone();
        Box::pin(async move {
            let metas = timeout(Duration::from_secs(30), peer.list_all_tools())
                .await
                .map_err(|_| McpError::Timeout(30))??;
            Ok(metas
                .into_iter()
                .map(|meta| {
                    Arc::new(McpToolWrapper::from_rmcp_tool(meta, peer.clone())) as Arc<dyn Tool>
                })
                .collect())
        })
    })
}

/// Extract plain text from a `CallToolResult`.
fn extract_text_from_result(result: CallToolResult) -> String {
    result
        .content
        .into_iter()
        .fold(String::new(), |mut acc, item| {
            if let ContentBlock::Text(tc) = item {
                if !acc.is_empty() {
                    acc.push('\n');
                }
                acc.push_str(&tc.text);
            }
            acc
        })
}

/// Configure a child command's environment for an MCP server.
///
/// The MCP child is arbitrary `npx -y`-downloaded code, so it must not
/// inherit the parent environment — which carries OPENAI_API_KEY,
/// credentials, and whatever else is in the shell. Clear everything and
/// restore only the per-server vars the user explicitly configured. A child
/// that needs PATH/HOME/proxy sets them here, in `[mcp_servers.env]`.
fn apply_mcp_env(cmd: &mut Command, env: &HashMap<String, String>) {
    cmd.env_clear();
    for (key, value) in env {
        cmd.env(key, value);
    }
}

/// Spawn and connect to a single MCP server (stdio), or open a Streamable
/// HTTP session (HTTP).
///
/// Returns the session (its `quit()` is the death signal; Drop cancels),
/// an external cancel capability (a closure over the same inner token —
/// usable while the session handle lives elsewhere, e.g. owned by a
/// supervisor), the peer, and raw tool metadata (without wrapping).
/// `notices` receives the server notifications flux forwards
/// (tools/list_changed, logging); `None` disables forwarding.
///
/// Tool-list changes reach this client over TWO spec-dependent paths
/// (verified against the official specs):
/// - spec ≤ 2025-06-18 (the deployed stdio majority): the server pushes
///   `notifications/tools/list_changed` unsolicited — it arrives at the
///   handler hook;
/// - spec 2026-07-28: the server only notifies clients that opened a
///   `subscriptions/listen` stream — so we open one for
///   `toolsListChanged` and pump it into the same sink. rmcp routes
///   subscription-delivered notifications EXCLUSIVELY to the
///   subscription channel (never the hook), so the two paths do not
///   double-fire; a legacy server that rejects the listen request is
///   logged and left on the hook path alone.
pub async fn connect_with_peer(
    config: &McpServerConfig,
    notices: Option<tokio::sync::mpsc::UnboundedSender<ServerNotice>>,
) -> Result<
    (
        McpSession,
        Arc<dyn Fn() + Send + Sync>,
        Peer<RoleClient>,
        Vec<rmcp::model::Tool>,
    ),
    McpError,
> {
    // The two branches differ ONLY in transport construction — the
    // handshake, tool listing, notification wiring and session plumbing
    // below are transport-agnostic (shared via `finish_connect`).
    match config {
        McpServerConfig::Stdio { command, args, env } => {
            let mut command = Command::new(command);
            command
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            apply_mcp_env(&mut command, env);
            let transport = TokioChildProcess::new(command)?;
            finish_connect(transport, notices).await
        }
        McpServerConfig::Http { url, headers } => {
            // Header names/values are validated here so a malformed user
            // config is a clean config error, not a mid-handshake surprise.
            let custom: HashMap<http::header::HeaderName, http::header::HeaderValue> = headers
                .iter()
                .map(|(k, v)| {
                    Ok((
                        http::header::HeaderName::from_bytes(k.as_bytes())?,
                        http::header::HeaderValue::from_str(v)?,
                    ))
                })
                .collect::<Result<_, McpError>>()?;
            let mut cfg = StreamableHttpClientTransportConfig::with_uri(url.clone())
                .custom_headers(custom)
                // A 404 session-expired reply triggers the transport's own
                // re-initialize + retry — the inner self-healing ring; the
                // supervisor stays the outer one (real outages).
                .reinit_on_expired_session(true);
            // Accept servers that never assign an MCP-Session-Id (stateless
            // deployments) — field-set, not a builder method, in rmcp 3.3.
            cfg.allow_stateless = true;
            // `from_config` builds rmcp's tuned default client: no idle
            // pooling (Linux Delayed-ACK stalls) and no redirects (so
            // custom headers can never leak to a redirect target).
            let transport = StreamableHttpClientTransport::from_config(cfg);
            finish_connect(transport, notices).await
        }
    }
}

/// The transport-agnostic tail of [`connect_with_peer`]: serve the
/// client session over any transport, list the tools, wire the
/// notification paths, and assemble the session handle.
async fn finish_connect<T, E>(
    transport: T,
    notices: Option<tokio::sync::mpsc::UnboundedSender<ServerNotice>>,
) -> Result<
    (
        McpSession,
        Arc<dyn Fn() + Send + Sync>,
        Peer<RoleClient>,
        Vec<rmcp::model::Tool>,
    ),
    McpError,
>
where
    T: IntoTransport<RoleClient, E, TransportAdapterIdentity>,
    E: std::error::Error + Send + Sync + 'static,
{
    let handler = NoticeClientHandler {
        sink: notices.clone(),
    };
    let service = timeout(
        Duration::from_secs(30),
        rmcp::ServiceExt::serve(handler, transport),
    )
    .await
    .map_err(|_| McpError::Timeout(30))?
    .map_err(|e| McpError::Initialize(e.to_string()))?;

    let tools = timeout(Duration::from_secs(30), service.peer().list_all_tools())
        .await
        .map_err(|_| McpError::Timeout(30))??;
    let peer = service.peer().clone();

    // Spec 2026-07-28 path: opt in to tools/list_changed over a
    // subscriptions/listen stream (see the doc above for why BOTH paths
    // exist). A legacy server rejects the method — that is fine, its
    // notifications ride the handler hook instead. The Subscription's
    // Drop unregisters; the pump ends when the session does (next →
    // None) or the transport dies (next → Err).
    if notices.is_some() {
        let sub_peer = peer.clone();
        let sub_sink = notices.clone();
        tokio::spawn(async move {
            let mut subscription = match sub_peer
                .listen(
                    rmcp::model::SubscriptionFilter::builder()
                        .tools_list_changed()
                        .build(),
                )
                .await
            {
                Ok(sub) => sub,
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        "subscriptions/listen rejected (legacy server); \
                         tools/list_changed rides the notification hook"
                    );
                    return;
                }
            };
            loop {
                match subscription.next().await {
                    Ok(Some(rmcp::model::ServerNotification::ToolListChangedNotification(_))) => {
                        if let Some(sink) = sub_sink.as_ref() {
                            let _ = sink.send(ServerNotice::ToolsListChanged);
                        }
                    }
                    Ok(Some(_)) => continue, // other subscribed categories: none requested
                    Ok(None) => return,      // subscription ended (session over)
                    Err(e) => {
                        tracing::debug!(error = %e, "tools subscription stream ended");
                        return;
                    }
                }
            }
        });
    }

    for tool in &tools {
        tracing::info!(tool = %tool.name, "registered MCP tool");
    }

    // rmcp's own death signal: `waiting()` consumes the service and
    // resolves when the serve loop ends (transport closed = child died,
    // cancellation = our teardown). Spawned so the caller stays free to
    // await it whenever the supervisor is ready.
    let token = Some(service.cancellation_token());
    let drop_token = Some(service.cancellation_token());
    // A third token clone for an EXTERNAL cancel capability (the removal
    // path) — `cancel(self)` consumes, so each capability owns its own
    // wrapper over the same inner token.
    let external = Arc::new(std::sync::Mutex::new(Some(service.cancellation_token())));
    let external_cancel: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        if let Some(token) = external.lock().unwrap().take() {
            token.cancel();
        }
    });
    let quit = tokio::spawn(async move { service.waiting().await });

    Ok((
        McpSession {
            quit: Some(quit),
            token,
            drop_token,
        },
        external_cancel,
        peer,
        tools,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── McpServerConfig ──

    #[test]
    fn stdio_config_fields_preserve_values() {
        let cfg = McpServerConfig::Stdio {
            command: "npx".into(),
            args: vec!["-y".into(), "@scope/server".into()],
            env: HashMap::from([("KEY".into(), "val".into())]),
        };
        let McpServerConfig::Stdio { command, args, env } = cfg else {
            panic!("expected the Stdio variant");
        };
        assert_eq!(command, "npx");
        assert_eq!(args, vec!["-y", "@scope/server"]);
        assert_eq!(env.get("KEY"), Some(&"val".to_string()));
    }

    #[test]
    fn http_config_fields_preserve_values() {
        let cfg = McpServerConfig::Http {
            url: "https://example.com/mcp".into(),
            headers: HashMap::from([("Authorization".into(), "Bearer tok".into())]),
        };
        let McpServerConfig::Http { url, headers } = cfg else {
            panic!("expected the Http variant");
        };
        assert_eq!(url, "https://example.com/mcp");
        assert_eq!(
            headers.get("Authorization"),
            Some(&"Bearer tok".to_string())
        );
    }

    // ── McpError ──

    #[test]
    fn mcp_error_spawn_display() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let err = McpError::Spawn(io);
        assert!(err.to_string().contains("no such file"));
    }

    #[test]
    fn mcp_error_initialize_display() {
        let err = McpError::Initialize("bad handshake".into());
        assert_eq!(err.to_string(), "MCP initialization failed: bad handshake");
    }

    #[test]
    fn mcp_error_from_io() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err: McpError = io.into();
        assert!(matches!(err, McpError::Spawn(_)));
    }

    #[test]
    fn mcp_error_debug() {
        let err = McpError::Initialize("fail".into());
        assert!(format!("{:?}", err).contains("Initialize"));
    }

    #[test]
    fn mcp_error_timeout_display() {
        let err = McpError::Timeout(30);
        assert!(err.to_string().contains("30s"));
    }

    #[test]
    fn mcp_error_header_display() {
        // A header NAME with illegal characters is a clean config error.
        let err: McpError = http::header::HeaderName::from_bytes(b"bad header\n")
            .unwrap_err()
            .into();
        assert!(err.to_string().contains("invalid MCP HTTP header"));
    }

    // ── extract_text_from_result ──

    #[test]
    fn extract_text_from_single_text_block() {
        let content = vec![ContentBlock::text("hello world")];
        let result = CallToolResult::success(content);
        let text = extract_text_from_result(result);
        assert_eq!(text, "hello world");
    }

    #[test]
    fn extract_text_joins_multiple_blocks() {
        let content = vec![ContentBlock::text("first"), ContentBlock::text("second")];
        let result = CallToolResult::success(content);
        let text = extract_text_from_result(result);
        assert_eq!(text, "first\nsecond");
    }

    #[test]
    fn extract_text_empty_result() {
        let result = CallToolResult::success(vec![]);
        let text = extract_text_from_result(result);
        assert!(text.is_empty());
    }

    // ── Arguments passthrough ──

    #[test]
    fn nested_object_arguments_pass_through_unchanged() {
        // The kernel passes a HashMap<String, Value>; nested objects
        // must reach the MCP server untouched (no string conversion).
        let args: HashMap<String, serde_json::Value> = HashMap::from([
            (
                "config".into(),
                serde_json::json!({"nested": {"deep": 1}, "level": "two"}),
            ),
            ("plain".into(), serde_json::json!("value")),
        ]);
        let args_map: serde_json::Map<String, serde_json::Value> = args.into_iter().collect();
        let params = CallToolRequestParams::new("test_tool").with_arguments(args_map);
        let serialized = serde_json::to_value(&params).unwrap();
        let arguments = &serialized["arguments"];
        assert_eq!(arguments["config"]["nested"]["deep"], 1);
        assert_eq!(arguments["config"]["level"], "two");
        assert_eq!(arguments["plain"], "value");
    }

    // ── env isolation for child processes ──

    #[cfg(unix)]
    #[tokio::test]
    async fn child_process_env_is_isolated_to_config() {
        // The MCP child must NOT inherit the parent environment — the
        // parent (flux-server) carries OPENAI_API_KEY and whatever else is
        // in the shell, and the child is arbitrary `npx -y`-downloaded
        // code. The child's env must be exactly the config.env keys,
        // nothing from the parent.
        let cfg = McpServerConfig::Stdio {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), "env".into()],
            env: HashMap::from([("ALLOWED_BY_USER".into(), "yes".into())]),
        };
        let (command, args, env) = match cfg {
            McpServerConfig::Stdio { command, args, env } => (command, args, env),
            _ => unreachable!(),
        };
        let mut cmd = tokio::process::Command::new(&command);
        cmd.args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        apply_mcp_env(&mut cmd, &env);
        let out = cmd.output().await.unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("ALLOWED_BY_USER=yes"),
            "config.env must reach the child"
        );
        // The child's env must contain NO key from the parent environment —
        // the whole point of env_clear. (The shell sets PWD/SHLVL/_ itself;
        // everything else present would have to have come from the parent.)
        let child_keys: std::collections::HashSet<&str> = text
            .lines()
            .filter_map(|l| l.split_once('=').map(|(k, _)| k))
            .collect();
        let parent_keys: std::collections::HashSet<String> =
            std::env::vars().map(|(k, _)| k).collect();
        for key in &child_keys {
            assert!(
                !parent_keys.contains(*key) || matches!(*key, "PWD" | "SHLVL" | "_"),
                "child env leaked parent variable {key}: {text}"
            );
        }
    }
}
