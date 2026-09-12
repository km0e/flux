use crate::{Connection, CoreError, Message, ToolDefinition};
use async_trait::async_trait;

/// One model available from a provider — a [`Provider::list_models`] entry.
/// Only the id is guaranteed across OpenAI-compatible gateways. Context
/// length is DEFENSIVELY probed from the common non-standard extras some
/// gateways add (vLLM `max_model_len`, Groq `context_window`, OpenRouter
/// `context_length`/`top_provider.context_length`) — the standard OpenAI
/// payload carries none, so `None` is the norm, never a fallback value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub context_length: Option<u64>,
}

/// Stateless provider factory — cheap to clone, shared across connections.
///
/// Call [`begin`](Provider::begin) to create a stateful session (a
/// [`ProviderPort`] implementor) for a single conversation.
///
/// The factory is cheap to build per resolution (string assembly — network
/// state lives in the shared HTTP client it captures), which is what makes
/// per-chat provider selection and mid-conversation model overrides cheap:
/// the adapter builds a fresh factory instance whenever a conversation
/// pins or swaps its provider.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Create a new connection (session). `system_prompt` and `tools` are
    /// fixed for the connection's lifetime — they are pre-written to the
    /// cache together with `history` so that `open` only receives new
    /// messages.
    fn begin(
        &self,
        system_prompt: &str,
        tools: &[ToolDefinition],
        history: &[Message],
    ) -> Box<dyn Connection + Send>;

    /// Best-effort model catalog — the OpenAI-compatible `GET /models`
    /// probe. Also serves as a connection test: one request validates the
    /// base URL and the API key together. The default reports the
    /// capability as unsupported: listing is a convenience, never a gate —
    /// model strings stay free-form and chat creation never depends on it.
    async fn list_models(&self) -> Result<Vec<ModelInfo>, CoreError> {
        Err(CoreError::Provider(
            "provider does not support model listing".to_string(),
        ))
    }
}
