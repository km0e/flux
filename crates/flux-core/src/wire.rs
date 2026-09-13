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
    /// A USER message was just persisted with the store row `id`. Emitted by
    /// the round consumer at turn acceptance (the only commit whose batch is
    /// exactly one user message), so the sender's client can name its OWN
    /// live bubble without waiting for the next history snapshot — the fork
    /// affordance needs exactly this id (the row id is the fork point).
    /// `content` rides along for the client's bubble match (its un-id'd live
    /// user bubbles are matched by exact text; a cancelled turn never
    /// persisted, so its bubble correctly never gains an id).
    MessagePersisted { id: i64, content: String },
    /// The conversation's provider was hot-swapped — every subsequent round
    /// runs on the new provider/model. The new session was rebuilt over the
    /// persisted history (the full transcript — there is no archive), so the
    /// conversation continues seamlessly. Broadcast to all viewers.
    ProviderSwitched { provider: String, model: String },
}
