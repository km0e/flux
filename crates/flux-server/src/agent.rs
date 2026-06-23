use crate::rpc::ProviderParams;
use crate::state::ServerState;
use crate::streaming::StreamEvent;
use anyhow::{Context, Result};
use async_trait::async_trait;
use flux_mcp::McpManager;
use flux_tools::{FileRead, FileWrite, Grep, ListDir, Shell};
use futures::{Stream, StreamExt};
use rig_core::agent::MultiTurnStreamItem;
use rig_core::agent::{Agent, PromptHook};
use rig_core::client::CompletionClient;
use rig_core::completion::{Chat, CompletionModel, GetTokenUsage, Message, PromptError};
use rig_core::streaming::{StreamedAssistantContent, StreamingChat};
use rig_core::tool::ToolDyn;
use rig_core::wasm_compat::WasmCompatSend;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use tracing::info;

/// Object-safe wrapper around Rig's `Chat` trait.
#[async_trait]
pub(crate) trait DynPrompt: Send + Sync {
    async fn chat(&self, message: &str, history: &mut Vec<Message>) -> Result<String, PromptError>;
}

#[async_trait]
impl<T: Chat + Send + Sync> DynPrompt for T {
    async fn chat(&self, message: &str, history: &mut Vec<Message>) -> Result<String, PromptError> {
        <Self as Chat>::chat(self, message, history).await
    }
}

/// Object-safe wrapper around Rig's streaming chat interface.
#[async_trait]
pub(crate) trait DynStreamingPrompt: Send + Sync {
    async fn stream_chat(
        &self,
        message: &str,
        history: Vec<Message>,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>, PromptError>;
}

#[async_trait]
impl<M, P> DynStreamingPrompt for Agent<M, P>
where
    M: CompletionModel + 'static,
    <M as CompletionModel>::StreamingResponse: WasmCompatSend + GetTokenUsage + 'static,
    P: PromptHook<M> + Send + Sync + 'static,
{
    async fn stream_chat(
        &self,
        message: &str,
        history: Vec<Message>,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>, PromptError> {
        let stream =
            <Self as StreamingChat<M, M::StreamingResponse>>::stream_chat(self, message, history)
                .await;
        let mapped = stream.filter_map(|item| async move {
            match item.ok()? {
                MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(t)) => {
                    Some(StreamEvent::Chunk(t.text().to_string()))
                }
                MultiTurnStreamItem::FinalResponse(fr) => {
                    let new_history = fr.history().map(|h| h.to_vec()).unwrap_or_default();
                    Some(StreamEvent::Done(fr.response().to_string(), new_history))
                }
                _ => None,
            }
        });
        Ok(Box::pin(mapped))
    }
}

pub(crate) async fn build_server_state(
    params: crate::rpc::InitializeParams,
    preamble: Option<String>,
) -> Result<ServerState> {
    let workdir = params
        .workdir
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // Concrete tool instances.
    let file_read = FileRead::new(&workdir);
    let file_write = FileWrite::new(&workdir);
    let list_dir = ListDir::new(&workdir);
    let grep = Grep::new(&workdir);
    let shell = Shell::new(&workdir);

    let mut tools: Vec<Box<dyn ToolDyn>> = vec![
        Box::new(file_read),
        Box::new(file_write),
        Box::new(list_dir),
        Box::new(grep),
        Box::new(shell),
    ];

    let mut mcp_sessions = Vec::new();
    if !params.mcp_servers.is_empty() {
        let manager = McpManager::new();
        for config in &params.mcp_servers {
            info!(command = %config.command, "connecting to MCP server");
            let (session, mut mcp_tools) = manager
                .connect(config)
                .await
                .context("Failed to connect to MCP server")?;
            info!(tools = mcp_tools.len(), "MCP server connected");
            mcp_sessions.push(session);
            tools.append(&mut mcp_tools);
        }
    }

    info!(workdir = %workdir.display(), built_in_tools = 5, "building server state");

    let mut tool_defs = Vec::new();
    for tool in &tools {
        tool_defs.push(tool.definition(String::new()).await);
    }

    let provider = params.provider.context("No provider configured")?;
    let preamble = preamble.unwrap_or_else(|| {
        "You are a helpful coding assistant. Use the provided tools when needed.".to_string()
    });
    info!("building agent");
    let (agent, streaming_agent) = build_agent(provider, tools, &preamble).await?;
    info!("agent built");

    Ok(ServerState {
        initialized: false,
        agent: Some(agent),
        streaming_agent: Some(streaming_agent),
        tool_defs,
        history: Vec::new(),
        mcp_sessions,
    })
}

pub(crate) async fn build_agent(
    provider: ProviderParams,
    tools: Vec<Box<dyn ToolDyn>>,
    preamble: &str,
) -> Result<(Box<dyn DynPrompt>, Arc<dyn DynStreamingPrompt>)> {
    match provider {
        ProviderParams::OpenAi {
            model,
            base_url,
            api_key,
            api,
        } => {
            let api_key = if let Some(key) = api_key {
                key
            } else {
                std::env::var("OPENAI_API_KEY")
                    .context("OPENAI_API_KEY not set and no api_key provided")?
            };

            let (prompt_agent, streaming_agent): (Box<dyn DynPrompt>, Arc<dyn DynStreamingPrompt>) =
                match api.as_str() {
                    "responses" => {
                        info!(provider = "openai", %model, api = "responses", base_url = base_url.as_deref().unwrap_or("default"), "creating OpenAI Responses API client");
                        let mut builder =
                            rig_core::providers::openai::Client::builder().api_key(api_key);
                        if let Some(url) = base_url {
                            builder = builder.base_url(&url);
                        }
                        let client = builder.build().context("Failed to create OpenAI client")?;
                        let agent = client.agent(&model).preamble(preamble).tools(tools).build();
                        (Box::new(agent.clone()), Arc::new(agent))
                    }
                    _ => {
                        info!(provider = "openai", %model, api = "completions", base_url = base_url.as_deref().unwrap_or("default"), "creating OpenAI Completions API client");
                        let mut builder = rig_core::providers::openai::CompletionsClient::builder()
                            .api_key(api_key);
                        if let Some(url) = base_url {
                            builder = builder.base_url(&url);
                        }
                        let client = builder.build().context("Failed to create OpenAI client")?;
                        let agent = client.agent(&model).preamble(preamble).tools(tools).build();
                        (Box::new(agent.clone()), Arc::new(agent))
                    }
                };
            Ok((prompt_agent, streaming_agent))
        }
        ProviderParams::Anthropic { model, api_key } => {
            info!(provider = "anthropic", %model, "creating Anthropic client");
            let key = api_key.context("Anthropic provider requires api_key")?;
            let client = rig_core::providers::anthropic::Client::builder()
                .api_key(key)
                .build()
                .context("Failed to create Anthropic client")?;
            let agent = client.agent(&model).preamble(preamble).tools(tools).build();
            let prompt_agent: Box<dyn DynPrompt> = Box::new(agent.clone());
            let streaming_agent: Arc<dyn DynStreamingPrompt> = Arc::new(agent);
            Ok((prompt_agent, streaming_agent))
        }
    }
}
