use crate::agent::build_server_state;
use crate::chat::handle_chat;
use crate::config::ServerConfig;
use crate::state::ServerState;
use crate::streaming::run_stream;
use crate::tools::handle_tools_list;
use crate::transport::Notifier;
use flux_mcp::McpServerConfig;
use rig_core::completion::Message;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

#[derive(Debug, Deserialize)]
pub(crate) struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcResponse {
    jsonrpc: String,
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct InitializeParams {
    #[serde(default)]
    pub provider: Option<ProviderParams>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "name")]
pub(crate) enum ProviderParams {
    #[serde(rename = "openai")]
    OpenAi {
        #[serde(default = "default_openai_model")]
        model: String,
        #[serde(default, rename = "baseUrl", alias = "base_url")]
        base_url: Option<String>,
        #[serde(default, rename = "apiKey", alias = "api_key")]
        api_key: Option<String>,
        /// Which OpenAI API family to use: "completions" (default, compatible with
        /// DeepSeek etc.) or "responses" (OpenAI native Responses API).
        #[serde(default = "default_openai_api")]
        api: String,
    },
    #[serde(rename = "anthropic")]
    Anthropic {
        #[serde(default = "default_anthropic_model")]
        model: String,
        #[serde(default, rename = "apiKey", alias = "api_key")]
        api_key: Option<String>,
    },
}

fn default_openai_model() -> String {
    "gpt-4o-mini".to_string()
}

fn default_openai_api() -> String {
    "completions".to_string()
}

fn default_anthropic_model() -> String {
    "claude-3-5-sonnet-20240620".to_string()
}

/// Convert a JSON-RPC history array into Rig `Message`s.
fn parse_history(value: &Value) -> Option<Vec<Message>> {
    let array = value.as_array()?;
    Some(
        array
            .iter()
            .filter_map(|entry| {
                let role = entry.get("role")?.as_str()?;
                let content = entry.get("content")?.as_str()?;
                match role {
                    "system" => Some(Message::system(content)),
                    "user" => Some(Message::user(content)),
                    "assistant" => Some(Message::assistant(content)),
                    _ => None,
                }
            })
            .collect(),
    )
}

fn provider_name(provider: &ProviderParams) -> &str {
    match provider {
        ProviderParams::OpenAi { .. } => "openai",
        ProviderParams::Anthropic { .. } => "anthropic",
    }
}

pub(crate) fn make_result(id: Option<Value>, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: Some(result),
        error: None,
    }
}

pub(crate) fn make_error(
    id: Option<Value>,
    code: i32,
    message: String,
    data: Option<Value>,
) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message,
            data,
        }),
    }
}

pub(crate) async fn handle_request<N: Notifier>(
    req: JsonRpcRequest,
    state: Arc<Mutex<ServerState>>,
    notifier: N,
    config: Arc<ServerConfig>,
) {
    let mut st = state.lock().await;

    match req.method.as_str() {
        "initialize" => {
            let mut params: InitializeParams = req
                .params
                .and_then(|p| serde_json::from_value(p).ok())
                .unwrap_or_default();

            // Server-side config takes precedence over client-provided values.
            let client_provider = params.provider.take();
            if config.provider.is_some() {
                if client_provider.is_some() {
                    info!("Provider configured in server config; ignoring provider from initialize request");
                }
                params.provider = config.provider.clone();
            } else {
                params.provider = client_provider;
            }
            if config.workdir.is_some() {
                params.workdir = config.workdir.clone();
            }
            if !config.mcp_servers.is_empty() {
                params.mcp_servers = config.mcp_servers.clone();
            }

            let provider_name = params
                .provider
                .as_ref()
                .map(provider_name)
                .unwrap_or("none");
            let mcp_count = params.mcp_servers.len();
            info!(
                provider = provider_name,
                workdir = params.workdir.as_deref().unwrap_or("default"),
                mcp_servers = mcp_count,
                "initialize request"
            );

            if params.provider.is_none() {
                warn!("No provider configured; initialize will fail unless config provides one");
            }

            match build_server_state(params, config.preamble.clone()).await {
                Ok(new_state) => {
                    let tools_count = new_state.tool_defs.len();
                    let mcp_count = new_state.mcp_sessions.len();
                    *st = new_state;
                    st.initialized = true;
                    info!(
                        tools = tools_count,
                        mcp_sessions = mcp_count,
                        "server initialized"
                    );
                    notifier
                        .send_json(make_result(
                            req.id,
                            json!({
                                "protocolVersion": "2024-11-05",
                                "serverInfo": { "name": "flux-server", "version": env!("CARGO_PKG_VERSION") },
                                "capabilities": { "tools": true, "chat": true }
                            }),
                        ))
                        .await;
                }
                Err(e) => {
                    warn!(error = %e, "initialize failed");
                    notifier
                        .send_json(make_error(req.id, -32603, e.to_string(), None))
                        .await;
                }
            }
        }
        "chat" => {
            if !st.initialized {
                warn!("chat request before initialization");
                notifier
                    .send_json(make_error(
                        req.id,
                        -32002,
                        "Server not initialized".to_string(),
                        None,
                    ))
                    .await;
                return;
            }
            let msg_len = req
                .params
                .as_ref()
                .and_then(|p| p.get("message"))
                .and_then(|v| v.as_str())
                .map(|s| s.len())
                .unwrap_or(0);
            info!(message_len = msg_len, "chat request");
            if let Some(history_value) = req.params.as_ref().and_then(|p| p.get("history")) {
                if let Some(history) = parse_history(history_value) {
                    st.history = history;
                }
            }
            let agent = st.agent.take().expect("agent missing");
            let mut history = std::mem::take(&mut st.history);
            let response = handle_chat(req.id, req.params, agent.as_ref(), &mut history).await;
            st.agent = Some(agent);
            st.history = history;
            notifier.send_json(response).await;
        }
        "chat/stream" => {
            if !st.initialized {
                warn!("chat/stream request before initialization");
                notifier
                    .send_json(make_error(
                        req.id,
                        -32002,
                        "Server not initialized".to_string(),
                        None,
                    ))
                    .await;
                return;
            }
            let agent = st.streaming_agent.clone().expect("streaming agent missing");
            let params = req.params.clone().unwrap_or_else(|| json!({}));
            let stream_id = params
                .get("stream_id")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| "default".to_string());
            let message = params
                .get("message")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_default();
            if message.is_empty() {
                warn!("chat/stream request missing message");
                notifier
                    .send_json(make_error(
                        req.id,
                        -32602,
                        "Missing message".to_string(),
                        None,
                    ))
                    .await;
                return;
            }
            info!(stream_id = %stream_id, message_len = message.len(), "chat/stream request");
            if let Some(history_value) = params.get("history") {
                if let Some(history) = parse_history(history_value) {
                    st.history = history;
                }
            }
            let stream_id_for_response = stream_id.clone();
            let stream_notifier = notifier.clone();
            let state_for_stream = state.clone();
            drop(st);
            tokio::spawn(async move {
                run_stream(agent, state_for_stream, stream_id, message, stream_notifier).await;
            });
            notifier
                .send_json(make_result(
                    req.id,
                    json!({ "accepted": true, "stream_id": stream_id_for_response }),
                ))
                .await;
        }
        "tools/list" => {
            if !st.initialized {
                warn!("tools/list request before initialization");
                notifier
                    .send_json(make_error(
                        req.id,
                        -32002,
                        "Server not initialized".to_string(),
                        None,
                    ))
                    .await;
                return;
            }
            debug!(tools = st.tool_defs.len(), "tools/list request");
            notifier
                .send_json(handle_tools_list(req.id, &st.tool_defs))
                .await;
        }
        _ => {
            notifier
                .send_json(make_error(
                    req.id,
                    -32601,
                    format!("Method not found: {}", req.method),
                    None,
                ))
                .await;
        }
    }
}
