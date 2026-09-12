//! Flux MCP client: bridge external MCP servers into Flux agents.
//!
//! This crate spawns MCP servers as child processes (via `rmcp`'s stdio transport),
//! fetches their tool lists, and exposes those tools as [`flux_core::Tool`] objects.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;
use tokio::time::timeout;

use async_trait::async_trait;
use flux_core::{CoreError, Tool, ToolCtx};
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientInfo, ContentBlock};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::{ClientHandler, Peer};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// Configuration for an MCP server that should be launched as a child process.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Executable to run.
    pub command: String,
    /// Arguments passed to the executable.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables.
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Errors that can occur when connecting to or calling an MCP server.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// Failed to spawn the child process.
    #[error("failed to spawn MCP server: {0}")]
    Spawn(#[from] std::io::Error),

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

/// A no-op MCP client handler.
#[derive(Debug, Clone, Default)]
struct NoopClientHandler;

impl ClientHandler for NoopClientHandler {
    fn get_info(&self) -> ClientInfo {
        ClientInfo::default()
    }
}

/// RAII guard: holds the MCP connection alive until dropped.
/// The `service` field is never accessed directly — its `Drop` impl
/// shuts down the connection.
pub struct McpSession {
    // Drop guard: holds the MCP connection alive until McpSession is dropped.
    // The field is never read directly — its Drop impl shuts down the connection.
    #[allow(dead_code)]
    service: RunningService<RoleClient, NoopClientHandler>,
}

impl McpSession {
    pub(crate) fn new(service: RunningService<RoleClient, NoopClientHandler>) -> Self {
        Self { service }
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

/// Spawn and connect to a single MCP server.
///
/// Returns the session, peer, and raw tool metadata (without wrapping).
/// The caller can clone the peer to create per-session tool wrappers.
pub async fn connect_with_peer(
    config: &McpServerConfig,
) -> Result<(McpSession, Peer<RoleClient>, Vec<rmcp::model::Tool>), McpError> {
    let mut command = Command::new(&config.command);
    command
        .args(&config.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    apply_mcp_env(&mut command, &config.env);

    let transport = TokioChildProcess::new(command)?;
    let handler = NoopClientHandler;
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

    for tool in &tools {
        tracing::info!(tool = %tool.name, "registered MCP tool");
    }

    Ok((McpSession::new(service), peer, tools))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── McpServerConfig ──

    #[test]
    fn config_fields_preserve_values() {
        let cfg = McpServerConfig {
            command: "npx".into(),
            args: vec!["-y".into(), "@scope/server".into()],
            env: HashMap::from([("KEY".into(), "val".into())]),
        };
        assert_eq!(cfg.command, "npx");
        assert_eq!(cfg.args, vec!["-y", "@scope/server"]);
        assert_eq!(cfg.env.get("KEY"), Some(&"val".to_string()));
    }

    #[test]
    fn config_serialize_roundtrip() {
        let cfg = McpServerConfig {
            command: "npx".into(),
            args: vec!["-y".into(), "@scope/server".into()],
            env: HashMap::from([("KEY".into(), "val".into())]),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: McpServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.command, "npx");
        assert_eq!(back.args, vec!["-y", "@scope/server"]);
        assert_eq!(back.env.get("KEY"), Some(&"val".to_string()));
    }

    #[test]
    fn config_default_is_empty() {
        let cfg = McpServerConfig::default();
        assert!(cfg.command.is_empty());
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
        let cfg = McpServerConfig {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), "env".into()],
            env: HashMap::from([("ALLOWED_BY_USER".into(), "yes".into())]),
        };
        let mut cmd = tokio::process::Command::new(&cfg.command);
        cmd.args(&cfg.args)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        apply_mcp_env(&mut cmd, &cfg.env);
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
