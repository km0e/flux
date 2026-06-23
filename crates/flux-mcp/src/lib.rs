//! Flux MCP client: bridge external MCP servers into Rig agents.
//!
//! This crate spawns MCP servers as child processes (via `rmcp`'s stdio transport),
//! fetches their tool lists, and exposes those tools as Rig [`ToolDyn`] objects so
//! they can be handed directly to an `AgentBuilder`.

use std::collections::HashMap;
use std::process::Stdio;

use rig_core::tool::rmcp::McpTool;
use rig_core::tool::ToolDyn;
use rmcp::model::ClientInfo;
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::ClientHandler;
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

impl McpServerConfig {
    /// Create a configuration for `command`.
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            env: HashMap::new(),
        }
    }

    /// Add a command-line argument.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Add an environment variable.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }
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
}

/// A no-op MCP client handler.
///
/// We only need the client-side connection so we can call `tools/list` and
/// `tools/call`. Server-initiated requests are handled with default behavior.
#[derive(Debug, Clone, Default)]
struct NoopClientHandler;

impl ClientHandler for NoopClientHandler {
    fn get_info(&self) -> ClientInfo {
        ClientInfo::default()
    }
}

/// Keeps an MCP client connection alive.
///
/// Dropping this handle cancels the underlying service loop and closes the
/// connection to the child process.
pub struct McpSession {
    #[allow(dead_code)]
    service: RunningService<RoleClient, NoopClientHandler>,
}

impl McpSession {
    fn new(service: RunningService<RoleClient, NoopClientHandler>) -> Self {
        Self { service }
    }
}

/// High-level MCP manager.
///
/// Use this to connect to one or more external MCP servers and collect their
/// tools as Rig-compatible objects.
#[derive(Debug, Default)]
pub struct McpManager;

impl McpManager {
    /// Create a new, empty manager.
    pub fn new() -> Self {
        Self
    }

    /// Spawn and connect to a single MCP server.
    ///
    /// Returns a session handle (which must be kept alive while the tools are
    /// used) and a list of Rig tool objects ready to be passed to an agent.
    pub async fn connect(
        &self,
        config: &McpServerConfig,
    ) -> Result<(McpSession, Vec<Box<dyn ToolDyn>>), McpError> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        for (key, value) in &config.env {
            command.env(key, value);
        }

        let transport = TokioChildProcess::new(command)?;
        let handler = NoopClientHandler;
        let service = rmcp::ServiceExt::serve(handler, transport)
            .await
            .map_err(|e| McpError::Initialize(e.to_string()))?;

        let tools = service.peer().list_all_tools().await?;
        let sink = service.peer().clone();

        let rig_tools: Vec<Box<dyn ToolDyn>> = tools
            .into_iter()
            .map(|tool| {
                let name = tool.name.to_string();
                tracing::info!(tool = name, "registered MCP tool");
                Box::new(McpTool::from_mcp_server(tool, sink.clone())) as Box<dyn ToolDyn>
            })
            .collect();

        Ok((McpSession::new(service), rig_tools))
    }
}

/// Convenience function: connect to one MCP server and return its tools.
///
/// The caller is responsible for keeping the returned [`McpSession`] alive.
pub async fn connect_mcp_server(
    config: &McpServerConfig,
) -> Result<(McpSession, Vec<Box<dyn ToolDyn>>), McpError> {
    McpManager::new().connect(config).await
}
