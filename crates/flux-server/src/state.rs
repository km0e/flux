use crate::agent::{DynPrompt, DynStreamingPrompt};
use flux_mcp::McpSession;
use rig_core::completion::{Message, ToolDefinition};
use std::sync::Arc;

/// Shared server state for one client connection.
pub(crate) struct ServerState {
    pub initialized: bool,
    pub agent: Option<Box<dyn DynPrompt>>,
    pub streaming_agent: Option<Arc<dyn DynStreamingPrompt>>,
    pub tool_defs: Vec<ToolDefinition>,
    /// Conversation history for this session.
    pub history: Vec<Message>,
    /// Kept alive so the MCP connections stay open while the agent runs.
    #[allow(dead_code)]
    pub mcp_sessions: Vec<McpSession>,
}

impl ServerState {
    pub fn new() -> Self {
        Self {
            initialized: false,
            agent: None,
            streaming_agent: None,
            tool_defs: Vec::new(),
            history: Vec::new(),
            mcp_sessions: Vec::new(),
        }
    }
}

impl Default for ServerState {
    fn default() -> Self {
        Self::new()
    }
}
