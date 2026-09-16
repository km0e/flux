//! OpenAI-compatible provider using the chat/completions streaming API.

use anyhow::Context;
use async_trait::async_trait;
use flux_core::Connection;
use flux_core::CoreError;
use flux_core::ModelInfo;
use flux_core::Provider;
use flux_core::StreamChunk;
use flux_core::StreamEvent;
use flux_core::StreamHandle;
use flux_core::StreamSink;
use flux_core::{Message, ToolCall, ToolDefinition};
use futures::Stream;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::trace;

// ── Config ───────────────────────────────────────────────────────────────────

pub fn default_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}

/// Endpoint-level configuration only — the MODEL is not part of it: every
/// provider instance is pinned to an explicit model at construction
/// (chat creation and SwitchProvider both require one; there is no
/// default anywhere in the system).
#[derive(Debug, Clone, Deserialize)]
pub struct OpenAiConfig {
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
}

/// Request-level generation parameters — the model-registry's editable
/// `params` (all optional; an unset field is OMITTED from the request so
/// the upstream default applies). These are the standard OpenAI
/// chat-completions knobs; `context_length` is informational only and
/// never reaches a request (it lives in the registry, not here).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OpenAiParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
}

impl OpenAiParams {
    /// Whether any field is set — an empty params contributes nothing to
    /// the request body (byte-identical suffix to the params-less form).
    pub fn is_empty(&self) -> bool {
        self.max_tokens.is_none() && self.temperature.is_none() && self.top_p.is_none()
    }

    /// The JSON fragment spliced into the request suffix (without the
    /// surrounding braces; empty string when nothing is set).
    fn body_fragment(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let json = json_string(self);
        // `{"k":v,...}` → `"k":v,...` — the suffix builder owns the braces.
        json[1..json.len() - 1].to_string()
    }
}

// ── Provider factory ─────────────────────────────────────────────────────────

pub struct OpenAiProvider {
    model: String,
    /// Bare base URL (no `/chat/completions` suffix) — the `/models`
    /// probe anchors here.
    base_url: String,
    endpoint: String,
    api_key: String,
    /// Request-level generation params (from the model registry), baked at
    /// construction — per-connection constant, part of the suffix.
    params: OpenAiParams,
    client: reqwest::Client,
}

impl OpenAiProvider {
    /// `model` is the per-instance pin (a server registry entry carries no
    /// model of its own — selection is always an explicit per-chat
    /// decision). The `/models` probe is model-agnostic, so the registry
    /// builds probe instances with the empty string. `params` carries the
    /// model-registry's generation knobs (default = none — the request is
    /// byte-identical to the params-less form).
    pub fn new(
        config: OpenAiConfig,
        model: String,
        params: OpenAiParams,
        client: reqwest::Client,
    ) -> anyhow::Result<Self> {
        let api_key = config.api_key.filter(|k| !k.is_empty()).context(
            "no API key configured — set one on the provider (the web UI's Providers dialog)",
        )?;
        let base_url = config.base_url.trim_end_matches('/').to_string();
        let endpoint = base_url.clone() + "/chat/completions";
        Ok(Self {
            model,
            base_url,
            endpoint,
            api_key,
            params,
            client,
        })
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn begin(
        &self,
        system_prompt: &str,
        tools: &[ToolDefinition],
        history: &[Message],
    ) -> Box<dyn Connection + Send> {
        let tool_defs: Vec<ApiTool> = tools.iter().map(ApiTool::from).collect();
        let tools_json = json_string(&tool_defs);

        // prefix = model + messages-array-open + system message
        let mut prefix = format!(
            r#"{{"model":{},"messages":[{{"role":"system","content":{}}}"#,
            json_string(&self.model),
            json_string(system_prompt),
        );
        // Write history messages into prefix
        write_pending(&mut prefix, history);

        // suffix = messages-array-close + stream opts + generation params
        // + tools. Params ride between the stream options and the tools —
        // object key order is irrelevant to JSON, and the fragment is
        // per-connection constant (a params change rebuilds the engine),
        // so the prefix cache is unaffected.
        let suffix = build_suffix(&self.params.body_fragment(), &tools_json);

        Box::new(OpenAiSession {
            sse: crate::sse::SseClient::new(
                self.client.clone(),
                self.endpoint.clone(),
                self.api_key.clone(),
            ),
            prefix: Arc::new(Mutex::new(prefix)),
            suffix,
        })
    }

    /// Best-effort model catalog: `GET {base_url}/models` (the standard
    /// OpenAI-compatible probe; also validates base URL + API key in one
    /// request). Ids are sorted — the UI presents a stable list.
    async fn list_models(&self) -> Result<Vec<ModelInfo>, CoreError> {
        let url = format!("{}/models", self.base_url);
        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(|e| CoreError::Provider(format!("model list request failed: {e}")))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| CoreError::Provider(format!("model list read failed: {e}")))?;
        if !status.is_success() {
            // Size-cap the body excerpt — an upstream error page must not
            // flood the message (same discipline as the SSE error path).
            let excerpt: String = body.chars().take(512).collect();
            return Err(CoreError::Provider(format!("HTTP {status}: {excerpt}")));
        }
        let parsed: ModelsResponse = serde_json::from_str(&body)
            .map_err(|e| CoreError::Provider(format!("model list parse: {e}")))?;
        let mut models: Vec<ModelInfo> = parsed
            .data
            .into_iter()
            .map(ModelsEntry::into_info)
            .collect();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }
}

/// Build the request suffix: messages-array-close + stream options +
/// generation params (when any) + tools. A free function so the params
/// embedding is directly testable without a live POST.
fn build_suffix(params_fragment: &str, tools_json: &str) -> String {
    if params_fragment.is_empty() {
        format!(
            r#"],"stream":true,"stream_options":{{"include_usage":true}},"tools":{}}}"#,
            tools_json,
        )
    } else {
        format!(
            r#"],"stream":true,"stream_options":{{"include_usage":true}},{}, "tools":{}}}"#,
            params_fragment, tools_json,
        )
    }
}

// ── Session ──────────────────────────────────────────────────────────────────

struct OpenAiSession {
    sse: crate::sse::SseClient,
    /// Shared with the returned stream so it can stay `'static` while still
    /// writing the completed exchange back into the prefix cache. The
    /// `PrefixGuard` inside `run_stream` is the single write point: it fires
    /// on every exit path, including mid-stream failure and consumer cancel.
    prefix: Arc<Mutex<String>>,
    suffix: String,
}

/// Provider stream stall deadline: no data within this window (measured
/// from the pump start / the last chunk) marks the stream dead (a hung
/// gateway) — the connection stops pushing and surfaces a `Failed` event
/// (fail-fast). A live long stream is NOT killed by a total timeout.
pub const STREAM_STALL_TIMEOUT: Duration = Duration::from_secs(120);

#[async_trait]
impl Connection for OpenAiSession {
    /// Open one model round: append `pending` to the prefix, POST the
    /// request, and spawn the push pump — parsed stream chunks flow into
    /// `sink`; the returned handle is loop-owned (dropping it cancels the
    /// pump, the HTTP response drops, the prefix guard writes the partial).
    async fn open(
        &mut self,
        pending: &[Message],
        sink: StreamSink,
    ) -> Result<StreamHandle, CoreError> {
        let prefix = self.prefix.clone();
        {
            let mut p = lock_prefix(&prefix);
            write_pending(&mut p, pending);
        }

        let mut body = String::new();
        {
            let p = lock_prefix(&prefix);
            body.reserve(p.len() + self.suffix.len());
            body.push_str(&p);
        }
        body.push_str(&self.suffix);

        // The POST happens here (awaited) — an open failure surfaces as a
        // returned Err before any handle exists.
        //
        // Transient-failure retry: ONE re-POST on a retryable HTTP status
        // (429/5xx/408 — see `is_transient_status`). The pending messages
        // are already in the prefix cache; the retry deliberately re-sends
        // the SAME body without re-writing them (a second write_pending
        // would duplicate the round's messages). Nothing has been pushed
        // to the sink at this point, so the retry is invisible to the
        // kernel: either a healthy stream opens, or the error surfaces
        // exactly as it did before (machine/consumer unchanged).
        let source = match self.sse.stream(body.clone()).await {
            Ok(source) => source,
            Err(CoreError::ProviderStatus {
                status,
                retry_after_secs,
                ..
            }) if crate::sse::is_transient_status(status) => {
                let delay = Duration::from_secs(retry_after_secs.unwrap_or(1));
                tracing::warn!(
                    status,
                    backoff = ?delay,
                    "transient provider failure; re-POSTing once"
                );
                tokio::time::sleep(delay).await;
                self.sse.stream(body).await?
            }
            Err(e) => return Err(e),
        };

        let (handle, token) = StreamHandle::new();
        tokio::spawn(pump_stream(source, sink, prefix, token));
        Ok(handle)
    }
}

/// The push pump: one streamed round, parsed chunks flowed into `sink`.
/// The `PrefixGuard` inside is the single writer of the prefix cache: it
/// fires on every exit path — clean completion, provider error, JSON
/// parse error, stall, and cancellation (loop-dropped handle) all land
/// here, preserving partial content; tool calls are only preserved when
/// the round completed cleanly.
async fn pump_stream(
    mut source: Pin<Box<dyn Stream<Item = Result<String, CoreError>> + Send>>,
    sink: StreamSink,
    prefix: Arc<Mutex<String>>,
    token: CancellationToken,
) {
    let emit = |event: StreamEvent| sink(event);
    let mut guard = PrefixGuard::new(prefix, SseParser::new());
    let mut seen_done = false;
    let mut stall_deadline = tokio::time::Instant::now() + STREAM_STALL_TIMEOUT;
    loop {
        let item = tokio::select! {
            _ = token.cancelled() => {
                // Cancelled by the loop (handle dropped): the guard still
                // writes the partial; nothing is pushed after the cancel.
                trace!("stream pump cancelled");
                return;
            }
            _ = tokio::time::sleep_until(stall_deadline) => {
                emit(StreamEvent::Failed {
                    message: format!("provider stream stalled (no data for {STREAM_STALL_TIMEOUT:?})"),
                    code: Some(flux_core::ErrorCode::ProviderConnection),
                });
                return;
            }
            item = source.next() => item,
        };
        match item {
            Some(Ok(data)) => {
                stall_deadline = tokio::time::Instant::now() + STREAM_STALL_TIMEOUT;
                // `[DONE]` is the normal end-of-stream sentinel for OpenAI
                // flows — sse.rs does not intercept it; the semantic call
                // lives here: EOF without it and without a finish_reason
                // means the upstream truncated (see the EOF branch below).
                if data == "[DONE]" {
                    seen_done = true;
                    continue;
                }
                let chunk: StreamingChunk = match serde_json::from_str(&data) {
                    Ok(c) => c,
                    Err(e) => {
                        emit(StreamEvent::Failed {
                            message: format!("SSE JSON: {e}"),
                            code: None,
                        });
                        return;
                    }
                };
                let (usage, finish_reason, events) = parse_chunk(chunk);
                if let Some(u) = usage {
                    emit(StreamEvent::Chunk(u));
                }
                if let Some(fr) = finish_reason {
                    guard.parser.note_finish_reason(fr);
                }
                for c in guard.parser.feed(events) {
                    emit(StreamEvent::Chunk(c));
                }
            }
            Some(Err(e)) => {
                emit(StreamEvent::Failed {
                    message: e.to_string(),
                    code: Some(e.error_code()),
                });
                return;
            }
            None => {
                // EOF truncation detection: a clean EOF with neither
                // `[DONE]` nor a finish_reason (the final chunk of a healthy
                // stream always carries one) — the upstream cut the response
                // mid-content. Surface as Failed: the machine keeps and
                // persists what already arrived, the client gets an error
                // notice, and a partial tool_call never enters the prefix
                // cache (committed stays unset — no dirty data).
                if !seen_done && !guard.parser.has_finish_reason() {
                    emit(StreamEvent::Failed {
                        message: "SSE stream ended without [DONE] or finish_reason — upstream truncated the response".to_string(),
                        code: None,
                    });
                    return;
                }
                for c in guard.parser.flush() {
                    if let StreamChunk::ToolCalls(tcs) = &c {
                        guard.tool_calls.extend(tcs.clone());
                    }
                    emit(StreamEvent::Chunk(c));
                }
                guard.committed = true;
                emit(StreamEvent::Chunk(StreamChunk::End {
                    finish_reason: guard.parser.take_finish_reason(),
                }));
                trace!("stream ended");
                return;
            }
        }
    }
}

// ── Serialization helpers ────────────────────────────────────────────────────

/// Serialize a value to a JSON string, logging a warning on failure.
/// Returns `"{}"` as the safe fallback for malformed input.
fn json_string<T: serde::Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to serialize JSON");
        "{}".into()
    })
}

// ── OpenAI wire-format message (for request body serialization) ───────────────

#[derive(Serialize)]
struct ApiToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: &'static str,
    function: ApiToolCallFunction,
}

#[derive(Serialize)]
struct ApiToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct ApiAssistantMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<ApiToolCall>,
}

fn api_tool_calls(tool_calls: &[ToolCall]) -> Vec<ApiToolCall> {
    tool_calls
        .iter()
        .map(|tc| ApiToolCall {
            id: tc.id.clone(),
            call_type: "function",
            function: ApiToolCallFunction {
                name: tc.name.clone(),
                arguments: tc.arguments.clone(),
            },
        })
        .collect()
}

// ── Cache helpers ────────────────────────────────────────────────────────────

fn write_pending(prefix: &mut String, pending: &[Message]) {
    for msg in pending {
        let json = if msg.tool_calls.is_empty() {
            serde_json::to_string(msg)
        } else {
            let api_msg = ApiAssistantMessage {
                role: msg.role.to_string(),
                content: msg.content.clone(),
                reasoning_content: None,
                tool_calls: api_tool_calls(&msg.tool_calls),
            };
            serde_json::to_string(&api_msg)
        };
        match json {
            // The comma joins entries — only emitted together with a
            // successfully serialized message, so a failure can never leave
            // a dangling separator in the request body.
            Ok(s) => {
                prefix.push(',');
                prefix.push_str(&s);
            }
            Err(e) => tracing::warn!(error = %e, "serialize message"),
        }
    }
}

fn write_assistant(
    prefix: &mut String,
    content: &str,
    reasoning: Option<&str>,
    tool_calls: &[ToolCall],
) {
    let msg = ApiAssistantMessage {
        role: "assistant".into(),
        content: content.to_string(),
        reasoning_content: reasoning.filter(|r| !r.is_empty()).map(|r| r.to_string()),
        tool_calls: api_tool_calls(tool_calls),
    };
    if let Ok(json) = serde_json::to_string(&msg) {
        prefix.push(',');
        prefix.push_str(&json);
    }
}

// ── Prefix cache guard ───────────────────────────────────────────────────────

/// Writes the round's assistant message into the prefix cache when dropped.
///
/// Single write point for every stream exit: clean completion (`committed`),
/// provider error chunk, JSON parse error, and consumer cancellation all drop
/// this guard and land here. Content/reasoning are preserved whenever
/// non-empty (mirroring `push_partial` in flux-loop); tool calls are only
/// preserved when the round completed cleanly — a partial tool call has no
/// tool result and could be rejected by the API on the next request.
struct PrefixGuard {
    prefix: Arc<Mutex<String>>,
    parser: SseParser,
    tool_calls: Vec<ToolCall>,
    committed: bool,
}

impl PrefixGuard {
    fn new(prefix: Arc<Mutex<String>>, parser: SseParser) -> Self {
        Self {
            prefix,
            parser,
            tool_calls: Vec::new(),
            committed: false,
        }
    }
}

impl Drop for PrefixGuard {
    fn drop(&mut self) {
        let content = self.parser.take_content();
        let reasoning = self.parser.take_reasoning();
        let has_content = !content.is_empty() || !reasoning.is_empty();
        let has_tool_calls = self.committed && !self.tool_calls.is_empty();
        if !has_content && !has_tool_calls {
            return;
        }
        let reasoning_opt = if reasoning.is_empty() {
            None
        } else {
            Some(reasoning.as_str())
        };
        let tool_calls: &[ToolCall] = if self.committed {
            &self.tool_calls
        } else {
            &[]
        };
        write_assistant(
            &mut lock_prefix(&self.prefix),
            &content,
            reasoning_opt,
            tool_calls,
        );
    }
}

/// Lock the prefix cache, recovering from poisoning. `write_assistant` is
/// infallible so a poisoned lock cannot happen — recovery is a safe no-op.
fn lock_prefix(prefix: &Arc<Mutex<String>>) -> MutexGuard<'_, String> {
    prefix.lock().unwrap_or_else(|e| e.into_inner())
}

/// Extract the usage chunk and delta events from one stream chunk.
///
/// Usage may ride on a dedicated empty-choices chunk (strict OpenAI) or on a
/// final non-empty chunk (some compatible gateways) — both are honored.
fn parse_chunk(chunk: StreamingChunk) -> (Option<StreamChunk>, Option<String>, Vec<SseEvent>) {
    let usage = chunk.usage.map(|u| StreamChunk::Usage {
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        cached_tokens: u
            .prompt_tokens_details
            .map(|d| d.cached_tokens)
            .unwrap_or(0),
    });
    // finish_reason rides on the last choice; take it before consuming.
    let finish_reason = chunk.choices.first().and_then(|c| c.finish_reason.clone());
    let mut choices = chunk.choices;
    if choices.is_empty() {
        return (usage, finish_reason, Vec::new());
    }
    let events = parse_sse_events(choices.remove(0).delta);
    (usage, finish_reason, events)
}

// ── OpenAI wire-format types ────────────────────────────────────────────────

#[derive(Serialize)]
struct ApiTool<'a> {
    #[serde(rename = "type")]
    tool_type: &'static str,
    function: ApiToolDefinition<'a>,
}

#[derive(Serialize)]
struct ApiToolDefinition<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a Value,
}

impl<'a> From<&'a ToolDefinition> for ApiTool<'a> {
    fn from(tool: &'a ToolDefinition) -> Self {
        Self {
            tool_type: "function",
            function: ApiToolDefinition {
                name: &tool.name,
                description: &tool.description,
                parameters: &tool.parameters,
            },
        }
    }
}
// ── Model catalog (GET /models) ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelsEntry>,
}

/// One catalog entry. The standard payload carries only `id`; the context
/// keys below are NON-STANDARD extras (each from a different gateway
/// flavor) — unknown fields are ignored, and the first present key wins.
#[derive(Debug, Deserialize)]
struct ModelsEntry {
    id: String,
    /// OpenRouter-style.
    context_length: Option<u64>,
    /// vLLM-style.
    max_model_len: Option<u64>,
    /// Groq-style.
    context_window: Option<u64>,
    /// OpenRouter's nested `top_provider.context_length`.
    top_provider: Option<TopProvider>,
}

#[derive(Debug, Deserialize)]
struct TopProvider {
    context_length: Option<u64>,
}

impl ModelsEntry {
    fn into_info(self) -> ModelInfo {
        let context_length = self
            .context_length
            .or(self.max_model_len)
            .or(self.context_window)
            .or_else(|| self.top_provider.and_then(|t| t.context_length));
        ModelInfo {
            id: self.id,
            context_length,
        }
    }
}

// ── SSE deserialization ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct StreamingChunk {
    choices: Vec<StreamingChoice>,
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Usage {
    prompt_tokens: u32,
    completion_tokens: u32,
    prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct StreamingChoice {
    /// Compatible gateways may send terminal chunks (finish_reason/usage)
    /// without a `delta` object.
    #[serde(default)]
    delta: StreamingDelta,
    /// Why the model stopped ("stop", "length", "content_filter",
    /// "tool_calls", ...). Surfaces on the last chunk and rides along to
    /// the `StreamChunk::End` so truncated answers are distinguishable.
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct StreamingDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<StreamingToolCall>,
}

#[derive(Debug, Deserialize)]
struct StreamingToolCall {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    function: StreamingFunction,
}

#[derive(Debug, Deserialize)]
struct StreamingFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_arguments")]
    arguments: Option<String>,
}

fn deserialize_arguments<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Null => Ok(None),
        Value::String(s) if s.is_empty() => Ok(None),
        Value::String(s) => Ok(Some(s)),
        Value::Object(_) => {
            let s = serde_json::to_string(&value).map_err(de::Error::custom)?;
            Ok(Some(s))
        }
        _ => Ok(None),
    }
}

// ── SSE event type ──────────────────────────────────────────────────────────

enum SseEvent {
    Text(String),
    Reasoning(String),
    ToolFragment {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
}

/// Consume one delta by value — content/reasoning/tool-call strings move
/// straight into the events, no per-chunk clones on the hot path. The delta
/// is single-consumer (one chunk's delta feeds exactly this call).
fn parse_sse_events(delta: StreamingDelta) -> Vec<SseEvent> {
    let mut events = Vec::new();
    if let Some(c) = delta.content
        && !c.is_empty()
    {
        events.push(SseEvent::Text(c));
    }
    if let Some(r) = delta.reasoning_content
        && !r.is_empty()
    {
        events.push(SseEvent::Reasoning(r));
    }
    for tc in delta.tool_calls {
        events.push(SseEvent::ToolFragment {
            index: tc.index,
            id: tc.id,
            name: tc.function.name,
            arguments: tc.function.arguments.unwrap_or_default(),
        });
    }
    events
}

// ── SseParser — stateful SSE → StreamChunk ──────────────────────────────────

struct ToolCallAccum {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

struct SseParser {
    pending: Vec<ToolCallAccum>,
    /// Which indices already emitted a [`StreamChunk::ToolCallPreview`]
    /// identity event (id + name both parsed). Preview chunks are forward
    /// signaling — the complete `ToolCalls` batch at flush stays the sole
    /// dispatch source.
    announced: Vec<bool>,
    content: String,
    reasoning: String,
    /// The last finish_reason seen on a terminal chunk; surfaced on
    /// [`StreamChunk::End`].
    finish_reason: Option<String>,
}

impl SseParser {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            announced: Vec::new(),
            content: String::new(),
            reasoning: String::new(),
            finish_reason: None,
        }
    }

    fn note_finish_reason(&mut self, reason: String) {
        self.finish_reason = Some(reason);
    }

    /// Whether the provider reported a terminal reason (normal streams always
    /// do on their last chunk — used for EOF-truncation detection).
    fn has_finish_reason(&self) -> bool {
        self.finish_reason.is_some()
    }

    fn take_finish_reason(&mut self) -> Option<String> {
        self.finish_reason.take()
    }

    fn feed(&mut self, events: Vec<SseEvent>) -> Vec<StreamChunk> {
        let mut chunks = Vec::new();
        for event in events {
            match event {
                SseEvent::Text(t) => {
                    self.content.push_str(&t);
                    chunks.push(StreamChunk::Text(t));
                }
                SseEvent::Reasoning(t) => {
                    self.reasoning.push_str(&t);
                    chunks.push(StreamChunk::Reasoning(t));
                }
                SseEvent::ToolFragment {
                    index,
                    id,
                    name,
                    arguments,
                } => {
                    while self.pending.len() <= index {
                        self.pending.push(ToolCallAccum {
                            id: None,
                            name: None,
                            arguments: String::new(),
                        });
                        self.announced.push(false);
                    }
                    let tc = &mut self.pending[index];
                    if let Some(i) = id {
                        tc.id = Some(i);
                    }
                    if let Some(n) = name {
                        tc.name = Some(n);
                    }
                    if !arguments.is_empty() {
                        tc.arguments.push_str(&arguments);
                    }
                    // Forward signaling while the model is still forming the
                    // call: the identity event fires the moment id + name are
                    // BOTH parsed (most gateways put them on the first
                    // fragment); every argument fragment follows as a delta.
                    // Fragments arriving before the identity (rare) just
                    // accumulate — the flush batch carries them regardless.
                    //
                    // The accumulated-args clone lives INSIDE the identity
                    // branch: it fires once per call, never per fragment (a
                    // per-fragment clone of everything accumulated so far is
                    // quadratic in the call's argument size — large tool
                    // payloads stream in hundreds of fragments).
                    if !self.announced[index] {
                        if let (Some(cid), Some(cname)) = (tc.id.clone(), tc.name.clone()) {
                            self.announced[index] = true;
                            chunks.push(StreamChunk::ToolCallPreview {
                                id: cid,
                                name: Some(cname),
                                args_delta: (!tc.arguments.is_empty())
                                    .then(|| tc.arguments.clone()),
                            });
                        }
                    } else if !arguments.is_empty()
                        && let Some(cid) = tc.id.clone()
                    {
                        chunks.push(StreamChunk::ToolCallPreview {
                            id: cid,
                            name: None,
                            args_delta: Some(arguments),
                        });
                    }
                }
            }
        }
        chunks
    }

    fn flush(&mut self) -> Vec<StreamChunk> {
        self.announced.clear();
        let tcs: Vec<ToolCall> = std::mem::take(&mut self.pending)
            .into_iter()
            .filter_map(|tc| {
                let (Some(id), Some(name)) = (tc.id, tc.name) else {
                    return None;
                };
                Some(ToolCall {
                    id,
                    name,
                    arguments: tc.arguments,
                })
            })
            .collect();
        if tcs.is_empty() {
            Vec::new()
        } else {
            vec![StreamChunk::ToolCalls(tcs)]
        }
    }

    fn take_content(&mut self) -> String {
        std::mem::take(&mut self.content)
    }
    fn take_reasoning(&mut self) -> String {
        std::mem::take(&mut self.reasoning)
    }
}

#[cfg(test)]
mod sse_parser_tests {
    use super::*;

    fn text(s: &str) -> SseEvent {
        SseEvent::Text(s.to_string())
    }
    fn reasoning(s: &str) -> SseEvent {
        SseEvent::Reasoning(s.to_string())
    }
    fn tf(index: usize, id: Option<&str>, name: Option<&str>, args: &str) -> SseEvent {
        SseEvent::ToolFragment {
            index,
            id: id.map(|s| s.to_string()),
            name: name.map(|s| s.to_string()),
            arguments: args.to_string(),
        }
    }

    fn is_text(chunk: &StreamChunk) -> bool {
        matches!(chunk, StreamChunk::Text(_))
    }
    fn is_reasoning(chunk: &StreamChunk) -> bool {
        matches!(chunk, StreamChunk::Reasoning(_))
    }

    #[test]
    fn single_tool_call_one_chunk() {
        let mut p = SseParser::new();
        let events = vec![tf(0, Some("call_1"), Some("read_file"), r#"{"path":"/x"}"#)];
        let chunks = p.feed(events);
        // The identity preview fires immediately (id + name both parsed);
        // the fragment's args ride along as the first delta.
        assert_eq!(chunks.len(), 1);
        match &chunks[0] {
            StreamChunk::ToolCallPreview {
                id,
                name,
                args_delta,
            } => {
                assert_eq!(id, "call_1");
                assert_eq!(name.as_deref(), Some("read_file"));
                assert_eq!(args_delta.as_deref(), Some(r#"{"path":"/x"}"#));
            }
            other => panic!("expected ToolCallPreview, got {other:?}"),
        }
        let flushed = p.flush();
        assert_eq!(flushed.len(), 1);
        match &flushed[0] {
            StreamChunk::ToolCalls(tcs) => {
                assert_eq!(tcs.len(), 1);
                assert_eq!(tcs[0].id, "call_1");
                assert_eq!(tcs[0].name, "read_file");
                assert_eq!(tcs[0].arguments, r#"{"path":"/x"}"#);
            }
            _ => panic!("expected ToolCalls"),
        }
    }

    #[test]
    fn preview_identity_fires_once_then_deltas() {
        let mut p = SseParser::new();
        // Fragment 1: id + name + first args slice → identity preview (with args).
        let chunks = p.feed(vec![tf(0, Some("c"), Some("bash"), r#"{"cmd":"ech"#)]);
        assert_eq!(chunks.len(), 1);
        let StreamChunk::ToolCallPreview { name, .. } = &chunks[0] else {
            panic!("expected preview");
        };
        assert_eq!(name.as_deref(), Some("bash"));
        // Fragment 2+: args only → delta previews.
        let chunks = p.feed(vec![tf(0, None, None, r#"o hi"}"#)]);
        assert_eq!(chunks.len(), 1);
        let StreamChunk::ToolCallPreview {
            id,
            name,
            args_delta,
        } = &chunks[0]
        else {
            panic!("expected preview");
        };
        assert_eq!(id, "c");
        assert!(name.is_none(), "identity fires exactly once");
        assert_eq!(args_delta.as_deref(), Some(r#"o hi"}"#));
        // flush still carries the complete call as the dispatch source.
        let flushed = p.flush();
        let StreamChunk::ToolCalls(tcs) = &flushed[0] else {
            panic!("expected ToolCalls");
        };
        assert_eq!(tcs[0].arguments, r#"{"cmd":"echo hi"}"#);
    }

    #[test]
    fn preview_waits_for_id_and_name() {
        let mut p = SseParser::new();
        // Args arrive before identity — no preview (identity is the gate).
        let chunks = p.feed(vec![tf(0, None, None, r#"{"a""#)]);
        assert!(chunks.is_empty());
        // id only — still no preview.
        let chunks = p.feed(vec![tf(0, Some("x"), None, "")]);
        assert!(chunks.is_empty());
        // name completes the identity — preview fires, accumulated args ride.
        let chunks = p.feed(vec![tf(0, None, Some("grep"), "")]);
        assert_eq!(chunks.len(), 1);
        let StreamChunk::ToolCallPreview { args_delta, .. } = &chunks[0] else {
            panic!("expected preview");
        };
        assert_eq!(args_delta.as_deref(), Some(r#"{"a""#));
        // An index with no complete identity never previews.
        let mut p2 = SseParser::new();
        p2.feed(vec![tf(0, None, Some("bash"), "{}")]);
        assert!(
            p2.flush().is_empty(),
            "incomplete identity is dropped at flush"
        );
    }

    #[test]
    fn fragmented_tool_call() {
        let mut p = SseParser::new();
        // id arrives first (no preview yet — name missing)
        p.feed(vec![tf(0, Some("call_2"), None, "")]);
        // name arrives next → identity preview fires
        let chunks = p.feed(vec![tf(0, None, Some("bash"), "")]);
        assert_eq!(chunks.len(), 1);
        assert!(matches!(chunks[0], StreamChunk::ToolCallPreview { .. }));
        // arguments arrive in fragments → deltas
        assert_eq!(p.feed(vec![tf(0, None, None, r#"{"cmd":"echo "#)]).len(), 1);
        assert_eq!(p.feed(vec![tf(0, None, None, r#"hello"}"#)]).len(), 1);
        let flushed = p.flush();
        assert_eq!(flushed.len(), 1);
        match &flushed[0] {
            StreamChunk::ToolCalls(tcs) => {
                assert_eq!(tcs.len(), 1);
                assert_eq!(tcs[0].id, "call_2");
                assert_eq!(tcs[0].name, "bash");
                assert_eq!(tcs[0].arguments, r#"{"cmd":"echo hello"}"#);
            }
            _ => panic!("expected ToolCalls"),
        }
    }

    #[test]
    fn multi_tool_call_interleaved() {
        let mut p = SseParser::new();
        p.feed(vec![tf(0, Some("id0"), Some("tool_a"), r#"{}"#)]);
        p.feed(vec![tf(1, Some("id1"), Some("tool_b"), r#"{}"#)]);
        let flushed = p.flush();
        assert_eq!(flushed.len(), 1);
        let StreamChunk::ToolCalls(tcs) = &flushed[0] else {
            panic!("expected ToolCalls");
        };
        let ids: Vec<&str> = tcs.iter().map(|tc| tc.id.as_str()).collect();
        assert_eq!(ids, vec!["id0", "id1"]);
    }

    #[test]
    fn reasoning_and_text_mixed() {
        let mut p = SseParser::new();
        let events = vec![text("Hello"), reasoning("Let me think..."), text(" world")];
        let chunks = p.feed(events);
        assert_eq!(chunks.len(), 3);
        assert!(is_text(&chunks[0]));
        assert!(is_reasoning(&chunks[1]));
        assert!(is_text(&chunks[2]));
        assert_eq!(p.take_content(), "Hello world");
        assert_eq!(p.take_reasoning(), "Let me think...");
    }

    #[test]
    fn flush_incomplete_discarded() {
        let mut p = SseParser::new();
        // Tool fragment with name but no id
        p.feed(vec![tf(0, None, Some("bash"), r#"{}"#)]);
        let flushed = p.flush();
        assert!(
            flushed.is_empty(),
            "incomplete tool call should not be emitted"
        );
    }

    #[test]
    fn text_accumulation() {
        let mut p = SseParser::new();
        p.feed(vec![text("one")]);
        p.feed(vec![text("two")]);
        p.feed(vec![text("three")]);
        assert_eq!(p.take_content(), "onetwothree");
    }

    #[test]
    fn reasoning_accumulation() {
        let mut p = SseParser::new();
        p.feed(vec![reasoning("step 1. ")]);
        p.feed(vec![reasoning("step 2.")]);
        assert_eq!(p.take_reasoning(), "step 1. step 2.");
    }

    #[test]
    fn flush_clears_pending() {
        let mut p = SseParser::new();
        p.feed(vec![tf(0, Some("id"), Some("name"), r#"{}"#)]);
        let first = p.flush();
        assert_eq!(first.len(), 1);
        let second = p.flush();
        assert!(second.is_empty(), "second flush should be empty");
    }
}

#[cfg(test)]
mod serialization_tests {
    use super::*;
    use flux_core::{Message, ToolCall};

    #[test]
    fn api_tool_calls_format() {
        let tcs = vec![ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"/tmp/test"}"#.into(),
        }];
        let api = api_tool_calls(&tcs);
        let json = serde_json::to_string(&api).unwrap();
        assert!(json.contains(r#""id":"call_1""#));
        assert!(json.contains(r#""type":"function""#));
        assert!(json.contains(r#""name":"read_file""#));
        assert!(json.contains(r#""arguments":"{\"path\":\"/tmp/test\"}"#));
    }

    #[test]
    fn write_assistant_text_only() {
        let mut prefix = String::new();
        write_assistant(&mut prefix, "Hello world", None, &[]);
        // Should start with ',' and be valid JSON
        assert!(prefix.starts_with(','));
        let json = &prefix[1..]; // skip leading ','
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["content"], "Hello world");
        assert!(v.get("tool_calls").is_none());
        assert!(v.get("reasoning_content").is_none());
    }

    #[test]
    fn write_assistant_with_reasoning() {
        let mut prefix = String::new();
        write_assistant(&mut prefix, "answer", Some("thinking..."), &[]);
        let json = &prefix[1..];
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["content"], "answer");
        assert_eq!(v["reasoning_content"], "thinking...");
    }

    #[test]
    fn write_assistant_with_tool_calls() {
        let mut prefix = String::new();
        let tcs = vec![ToolCall {
            id: "call_x".into(),
            name: "bash".into(),
            arguments: r#"{"cmd":"ls"}"#.into(),
        }];
        write_assistant(&mut prefix, "", None, &tcs);
        let json = &prefix[1..];
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(v["role"], "assistant");
        let api_tcs = v["tool_calls"].as_array().unwrap();
        assert_eq!(api_tcs.len(), 1);
        assert_eq!(api_tcs[0]["id"], "call_x");
        assert_eq!(api_tcs[0]["type"], "function");
        assert_eq!(api_tcs[0]["function"]["name"], "bash");
    }

    #[test]
    fn write_pending_user_message() {
        let mut prefix = String::new();
        let msgs = vec![Message::user("hello")];
        write_pending(&mut prefix, &msgs);
        assert!(prefix.starts_with(','));
        let json = &prefix[1..];
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(v["role"], "user");
        assert_eq!(v["content"], "hello");
    }

    #[test]
    fn write_pending_tool_message() {
        let mut prefix = String::new();
        let msgs = vec![Message::tool("call_99", "file content")];
        write_pending(&mut prefix, &msgs);
        let json = &prefix[1..];
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(v["role"], "tool");
        assert_eq!(v["tool_call_id"], "call_99");
        assert_eq!(v["content"], "file content");
    }

    #[test]
    fn terminal_chunk_without_delta_deserializes() {
        // Compatible gateways may attach usage/finish_reason to a final
        // chunk without a `delta` object — it must not kill the stream.
        let chunk: StreamingChunk = serde_json::from_str(
            r#"{"choices":[{"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
        )
        .expect("terminal chunk without delta parses");
        assert_eq!(chunk.choices.len(), 1);
        assert!(chunk.usage.is_some());
    }

    #[test]
    fn finish_reason_is_deserialized_from_terminal_chunk() {
        let chunk: StreamingChunk =
            serde_json::from_str(r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#)
                .expect("terminal chunk with finish_reason parses");
        assert_eq!(chunk.choices[0].finish_reason.as_deref(), Some("length"));
    }

    #[test]
    fn usage_on_non_empty_choices_chunk_is_emitted() {
        let chunk: StreamingChunk = serde_json::from_str(
            r#"{"choices":[{"delta":{"content":"final"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
        )
        .unwrap();
        let (usage, _, events) = parse_chunk(chunk);
        let usage = usage.expect("usage riding on a final content chunk is emitted");
        assert!(matches!(
            usage,
            StreamChunk::Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                ..
            }
        ));
        assert!(!events.is_empty());
    }
}

#[cfg(test)]
mod prefix_guard_tests {
    use super::*;

    // ── PrefixGuard unit tests ────────────────────────────────────────────────

    fn dropped_prefix(events: Vec<SseEvent>, tool_calls: Vec<ToolCall>, committed: bool) -> String {
        let prefix = Arc::new(std::sync::Mutex::new(String::new()));
        let mut parser = SseParser::new();
        parser.feed(events);
        let mut guard = PrefixGuard::new(prefix.clone(), parser);
        guard.tool_calls = tool_calls;
        guard.committed = committed;
        drop(guard);
        lock_prefix(&prefix).clone()
    }

    #[test]
    fn drop_writes_partial_content_and_reasoning() {
        let prefix = dropped_prefix(
            vec![
                SseEvent::Text("Hello".into()),
                SseEvent::Reasoning("think".into()),
            ],
            Vec::new(),
            false,
        );
        assert!(prefix.contains(r#""role":"assistant""#));
        assert!(prefix.contains(r#""content":"Hello""#));
        assert!(prefix.contains(r#""reasoning_content":"think""#));
    }

    #[test]
    fn drop_uncommitted_keeps_content_omits_tool_calls() {
        let prefix = dropped_prefix(
            vec![SseEvent::Text("answer".into())],
            vec![ToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"/x"}"#.into(),
            }],
            false,
        );
        assert!(prefix.contains(r#""content":"answer""#));
        assert!(
            !prefix.contains("tool_calls"),
            "uncommitted partial tool calls must not be cached"
        );
    }

    #[test]
    fn drop_committed_writes_tool_calls() {
        let prefix = dropped_prefix(
            Vec::new(),
            vec![ToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"/x"}"#.into(),
            }],
            true,
        );
        assert!(prefix.contains("tool_calls"));
        assert!(prefix.contains(r#""id":"call_1""#));
    }

    #[test]
    fn drop_empty_writes_nothing() {
        let prefix = dropped_prefix(Vec::new(), Vec::new(), false);
        assert!(prefix.is_empty());
    }

    #[test]
    fn drop_committed_empty_writes_nothing() {
        let prefix = dropped_prefix(Vec::new(), Vec::new(), true);
        assert!(
            prefix.is_empty(),
            "empty assistant message must not be cached"
        );
    }

    // ── pump_stream stream-level tests (teardown paths) ────────────────────

    fn text_chunk(s: &str) -> String {
        format!(r#"{{"choices":[{{"delta":{{"content":"{s}"}}}}]}}"#)
    }

    fn tool_fragment_chunk() -> String {
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"/x\"}"}}]}}]}"#
            .to_string()
    }

    /// Drive `pump_stream` over a scripted SSE source, collect the pushed
    /// stream events, and return (prefix cache, events). The pump task is
    /// joined so all exits (end/error/stall) are observed synchronously.
    async fn collect(source_items: Vec<Result<String, CoreError>>) -> (String, Vec<StreamEvent>) {
        let prefix = Arc::new(std::sync::Mutex::new(String::new()));
        let source: Pin<Box<dyn Stream<Item = Result<String, CoreError>> + Send>> =
            Box::pin(futures::stream::iter(source_items));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
        let sink: StreamSink = Arc::new(move |e| {
            let _ = tx.send(e);
        });
        let (handle, token) = StreamHandle::new();
        let pump = tokio::spawn(pump_stream(source, sink, prefix.clone(), token.clone()));
        // The handle stays alive (loop-owned in production) — dropping it
        // here would cancel the pump before the script ran out.
        let mut events = Vec::new();
        while let Some(e) = rx.recv().await {
            events.push(e);
        }
        pump.await.unwrap();
        drop(handle);
        (lock_prefix(&prefix).clone(), events)
    }

    fn failed(events: &[StreamEvent]) -> Option<(&str, Option<flux_core::ErrorCode>)> {
        events.iter().find_map(|e| match e {
            StreamEvent::Failed { message, code } => Some((message.as_str(), code.clone())),
            _ => None,
        })
    }

    #[tokio::test]
    async fn clean_end_writes_content_and_tool_calls() {
        // Normal end = the [DONE] sentinel (or a final chunk carrying finish_reason).
        let (prefix, events) = collect(vec![
            Ok(text_chunk("Hello")),
            Ok(tool_fragment_chunk()),
            Ok("[DONE]".to_string()),
        ])
        .await;
        assert!(prefix.contains(r#""content":"Hello""#));
        assert!(prefix.contains("tool_calls"));
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Chunk(StreamChunk::End {
                finish_reason: None
            }))
        ));
    }

    #[tokio::test]
    async fn eof_without_done_or_finish_reason_is_truncation_error() {
        // Upstream cut mid-content (clean EOF, no [DONE], no finish_reason):
        // judged truncated — the Failed path leaves committed unset, so the
        // partial tool_call never enters the prefix cache.
        let (prefix, events) =
            collect(vec![Ok(text_chunk("Hello")), Ok(tool_fragment_chunk())]).await;
        let (message, _) = failed(&events).expect("truncation must surface as Failed");
        assert!(message.contains("truncated"), "{message}");
        assert!(
            !prefix.contains("tool_calls"),
            "truncated tool calls must not enter the prefix cache: {prefix}"
        );
        // Content already streamed is preserved (the machine persists the partial).
        assert!(prefix.contains(r#""content":"Hello""#));
    }

    #[tokio::test]
    async fn eof_without_done_but_with_finish_reason_succeeds() {
        // Complete content but a gateway that never sends [DONE] (final chunk
        // carries finish_reason): succeeds as usual — truncation requires
        // BOTH signals to be missing, never just one.
        let (_, events) = collect(vec![
            Ok(text_chunk("Hello")),
            Ok(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#.to_string()),
        ])
        .await;
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Chunk(StreamChunk::End {
                finish_reason: Some(reason)
            })) if reason == "stop"
        ));
    }

    #[tokio::test]
    async fn end_chunk_carries_finish_reason() {
        let (_, events) = collect(vec![
            Ok(text_chunk("Hello")),
            Ok(r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#.to_string()),
        ])
        .await;
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Chunk(StreamChunk::End {
                finish_reason: Some(reason)
            })) if reason == "length"
        ));
    }

    #[tokio::test]
    async fn error_chunk_preserves_partial() {
        let (prefix, events) = collect(vec![
            Ok(text_chunk("Hello")),
            Ok(text_chunk(" world")),
            Err(CoreError::Provider("boom".into())),
        ])
        .await;
        assert!(prefix.contains(r#""content":"Hello world""#));
        assert!(!prefix.contains("tool_calls"));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, StreamEvent::Failed { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn parse_error_preserves_partial() {
        let (prefix, events) =
            collect(vec![Ok(text_chunk("Hello")), Ok("not json".to_string())]).await;
        assert!(prefix.contains(r#""content":"Hello""#));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, StreamEvent::Failed { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn cancelled_handle_stops_the_pump_and_preserves_partial() {
        // The loop drops the handle on cancel: the token fires, the pump
        // exits (guard writes the partial, nothing further is pushed).
        let prefix = Arc::new(std::sync::Mutex::new(String::new()));
        // A stream that would keep producing forever if not cancelled.
        let big: Vec<Result<String, CoreError>> =
            (0..10_000).map(|_| Ok(text_chunk("tick"))).collect();
        let source: Pin<Box<dyn Stream<Item = Result<String, CoreError>> + Send>> =
            Box::pin(futures::stream::iter(big));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
        let sink: StreamSink = Arc::new(move |e| {
            let _ = tx.send(e);
        });
        let (handle, token) = StreamHandle::new();
        let pump = tokio::spawn(pump_stream(source, sink, prefix.clone(), token.clone()));
        // A few chunks arrive, then the loop drops the handle.
        let mut seen = 0;
        while seen < 3 {
            if rx.recv().await.is_some() {
                seen += 1;
            }
        }
        drop(handle);
        assert!(token.is_cancelled());
        pump.await.unwrap();
        assert!(lock_prefix(&prefix).contains(r#""content":"tick"#));
    }
}

#[cfg(test)]
mod params_tests {
    use super::*;

    #[test]
    fn empty_params_produce_no_fragment() {
        let p = OpenAiParams::default();
        assert!(p.is_empty());
        assert_eq!(p.body_fragment(), "");
        // Deserialization round-trips from a registry JSON blob.
        let parsed: OpenAiParams = serde_json::from_str("{}").unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn fragment_splices_only_set_fields() {
        let p = OpenAiParams {
            max_tokens: Some(8192),
            temperature: Some(0.7),
            top_p: None,
        };
        assert_eq!(p.body_fragment(), r#""max_tokens":8192,"temperature":0.7"#);
    }

    #[test]
    fn suffix_embeds_params_between_stream_opts_and_tools() {
        let suffix = build_suffix(r#""max_tokens":16"#, "[]");
        // The suffix closes a valid JSON object — verify with a real parse.
        // It starts with `]` (the messages-array close), so prepend the open.
        let full = format!(r#"{{"model":"m","messages":[{}"#, suffix);
        let v: serde_json::Value = serde_json::from_str(&full).unwrap();
        assert_eq!(v["max_tokens"], 16);
        assert_eq!(v["tools"], serde_json::json!([]));
    }

    #[test]
    fn suffix_without_params_is_byte_stable() {
        let no_params = build_suffix("", "[]");
        assert!(!no_params.contains("max_tokens"));
        assert!(no_params.contains(r#""stream":true"#));
        assert!(no_params.contains(r#""tools":[]"#));
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// Drain a request's headers (read to the blank line) so the canned
    /// response can be written safely.
    async fn read_request_head(stream: &mut TcpStream) {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = stream.read(&mut byte).await.unwrap();
            assert!(n > 0, "peer closed before headers ended");
            buf.extend_from_slice(&byte);
            if buf.ends_with(b"\r\n\r\n") {
                return;
            }
        }
    }

    /// A canned upstream: each entry answers one connection with the given
    /// status line + body. `requests` counts accepted connections.
    async fn serve_script(
        listener: TcpListener,
        script: Vec<(&'static str, String)>,
        requests: Arc<AtomicU64>,
    ) {
        tokio::spawn(async move {
            for (status_line, body) in script {
                let (mut stream, _) = listener.accept().await.unwrap();
                requests.fetch_add(1, Ordering::SeqCst);
                read_request_head(&mut stream).await;
                let retry = if status_line.contains("429") {
                    "Retry-After: 0\r\n"
                } else {
                    ""
                };
                let head = format!(
                    "{status_line}\r\nContent-Type: text/event-stream\r\n{retry}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).await.unwrap();
                stream.write_all(body.as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            }
        });
    }

    /// A provider session pinned to the mock endpoint — the retry lives
    /// in `Connection::open`, not on SseClient::stream, so the tests drive
    /// the real open path. The client is proxy-proof: a host shell's
    /// `http_proxy` must never detour the loopback POSTs (phantom-502).
    fn session_for(addr: std::net::SocketAddr) -> Box<dyn flux_core::Connection> {
        let provider = OpenAiProvider::new(
            OpenAiConfig {
                base_url: format!("http://{addr}"),
                api_key: Some("test-key".into()),
            },
            "test-model".into(),
            OpenAiParams::default(),
            flux_test_support::loopback_client(),
        )
        .unwrap();
        provider.begin("sys", &[], &[])
    }

    fn recording_sink(events: Arc<std::sync::Mutex<Vec<StreamEvent>>>) -> StreamSink {
        Arc::new(move |event| events.lock().unwrap().push(event))
    }

    fn minimal_sse() -> String {
        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
         data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
         data: [DONE]\n\n"
            .into()
    }

    #[tokio::test]
    async fn transient_429_is_retried_once_and_succeeds() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicU64::new(0));
        serve_script(
            listener,
            vec![
                ("HTTP/1.1 429 Too Many Requests", "slow down".into()),
                ("HTTP/1.1 200 OK", minimal_sse()),
            ],
            Arc::clone(&requests),
        )
        .await;
        let mut client = session_for(addr);

        // Retry-After: 0 keeps the backoff at zero — the test asserts the
        // re-POST happened, not the wait.
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handle = client
            .open(&[], recording_sink(Arc::clone(&events)))
            .await
            .expect("retry must succeed");
        // The pump runs on its own task — give it a beat to push the
        // deltas before the handle drop cancels it.
        for _ in 0..100 {
            if !events.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop(handle); // settle the pump
        let events = events.lock().unwrap();
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Chunk(StreamChunk::Text(t)) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "hi", "the retried stream carries content");
        assert_eq!(requests.load(Ordering::SeqCst), 2, "exactly two POSTs");
    }

    #[tokio::test]
    async fn consecutive_429s_exhaust_the_single_retry() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicU64::new(0));
        serve_script(
            listener,
            vec![
                ("HTTP/1.1 429 Too Many Requests", "a".into()),
                ("HTTP/1.1 429 Too Many Requests", "b".into()),
            ],
            Arc::clone(&requests),
        )
        .await;
        let mut client = session_for(addr);

        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let err = client
            .open(&[], recording_sink(Arc::clone(&events)))
            .await
            .expect_err("retry exhausted");
        assert!(
            matches!(err, CoreError::ProviderStatus { status: 429, .. }),
            "the second failure surfaces as-is: {err}"
        );
        assert_eq!(requests.load(Ordering::SeqCst), 2, "exactly two POSTs");
    }

    #[tokio::test]
    async fn non_transient_400_is_not_retried() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicU64::new(0));
        // A second canned response would answer any retry — the request
        // count proves none happened.
        serve_script(
            listener,
            vec![("HTTP/1.1 400 Bad Request", "bad".into())],
            Arc::clone(&requests),
        )
        .await;
        let mut client = session_for(addr);

        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let err = client
            .open(&[], recording_sink(Arc::clone(&events)))
            .await
            .expect_err("400 is deterministic");
        assert!(matches!(err, CoreError::ProviderStatus { status: 400, .. }));
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "no retry on a client error"
        );
    }

    #[test]
    fn transient_classifier() {
        for status in [408, 429, 500, 502, 503, 504] {
            assert!(
                crate::sse::is_transient_status(status),
                "{status} is transient"
            );
        }
        for status in [400, 401, 403, 404, 422] {
            assert!(
                !crate::sse::is_transient_status(status),
                "{status} is deterministic"
            );
        }
    }
}
