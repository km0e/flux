//! Wire events emitted by the machine — the loop's output vocabulary.
//!
//! The adapter layer prefixes each event with the chat id and maps it
//! onto the transport's message type. One `WireEvent` per client-visible
//! output the loop produces.

use crate::ErrorCode;

/// One output event the machine produces for the client.
#[derive(Debug, Clone, PartialEq)]
pub enum WireEvent {
    /// A streamed text delta.
    TextDelta(String),
    /// A streamed reasoning delta.
    ReasoningDelta(String),
    /// Token usage for the completed exchange.
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        cached_tokens: u32,
    },
    /// A tool started executing.
    ToolStart {
        id: String,
        name: String,
        arguments: String,
    },
    /// A tool call the model is still forming — sent BEFORE [`WireEvent::ToolStart`]
    /// so the client can show the card while the arguments stream. The
    /// identity event (name set) fires when the call's id + name are parsed;
    /// argument-fragment events (args_delta set) follow. Pure signaling:
    /// never persisted, superseded by the ToolStart/ToolResult pair, voided
    /// client-side at round end when no ToolStart ever lands.
    ToolCallPreview {
        id: String,
        name: Option<String>,
        args_delta: Option<String>,
    },
    /// A tool finished executing.
    ToolResult {
        id: String,
        name: String,
        result: String,
    },
    /// The model's `question` tool awaits the user's answer. The question
    /// text and options are entirely agent-produced; the adapter maps this
    /// onto the transport's question_required message (chat id prefixed
    /// there). Delivered to the lease holder on the priority control
    /// channel and parked until answered.
    QuestionRequired {
        id: String,
        text: String,
        options: Option<Vec<String>>,
    },
    /// The stream failed — sent to the client.
    StreamError {
        message: String,
        code: Option<ErrorCode>,
    },
    /// The round was cancelled by the user. Distinct from the [`WireEvent::StreamError`]
    /// variant: cancellation is a user action, not a failure. Always followed by
    /// `StreamEnd` as the round wraps up.
    Cancelled,
    /// The round's reply is fully streamed and persisted. `finish_reason`
    /// carries the provider's end signal when the stream reported one —
    /// `"length"` / `"content_filter"` mean the answer was cut short.
    StreamEnd { finish_reason: Option<String> },
    /// The conversation context was rebuilt at `base_message_id` — a
    /// restart-from-a-message (rebase). Messages at/below the base are
    /// archived: the provider prefix was reset to only the live context
    /// above it, and the new base is persisted. The client inserts a
    /// neutral notice (the archive is still viewable/browsable).
    ContextRebased { base_message_id: i64 },
    /// The conversation's provider was hot-swapped — every subsequent round
    /// runs on the new provider/model. The new session was rebuilt over the
    /// live context (the persisted history above the context base), so the
    /// conversation continues seamlessly. Broadcast to all viewers.
    ProviderSwitched { provider: String, model: String },
}
