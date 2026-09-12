//! The conversation loop's I/O vocabulary — the contract every channel
//! peer speaks.
//!
//! The loop is a pure pump between two channels: it consumes
//! [`LoopInput`] from one, steps the machine, and emits [`LoopFact`] on
//! the other. Everything else is a channel peer:
//!
//! - the **session layer** pushes `UserMessage` / `Cancel`;
//! - the **provider connection** pushes `Stream` events and a
//!   [`StreamHandle`] (dropped by the loop to stop the push — the handle
//!   is a pure cancellation capability, not I/O ownership);
//! - the **tool executor** consumes facts (`ToolDispatched`,
//!   `InterruptTools`) and pushes `ToolFinished` back;
//! - the **chat layer** folds the fact trace (persistence, routing,
//!   provider triggering, connection replacement, rebuild orchestration).
//!
//! Living in flux-core (not flux-loop) is what lets the provider and the
//! executor stay loop-agnostic: they depend only on this vocabulary.

use crate::{ChatStateKind, CoreError, ErrorCode, Message, StreamChunk, ToolCall, WireEvent};
use std::collections::HashMap;
use tokio_util::sync::CancellationToken;

// ── Input (→ loop) ──────────────────────────────────────────────────────────

/// One event driving the machine. Total over every (state, input) pair.
#[derive(Debug)]
pub enum LoopInput {
    /// A user turn. Turns arriving mid-round (streaming / tool flight) are
    /// QUEUED, not dropped: each starts a fresh round in the same step
    /// that wraps the current one (FIFO). A `Cancel` ends the current
    /// round AND discards the queued turns — stop means stop; a turn sent
    /// after the cancel still runs (the usual interject order is
    /// cancel-first).
    UserMessage(String),
    /// Cancel the active phase (stream or tool flight). Queue event like
    /// any other — always lands in FIFO order.
    Cancel,
    /// Register the active stream's cancellation handle. The loop drops it
    /// when the round wraps or the user cancels — the drop stops the
    /// connection's push immediately. A handle arriving outside a live
    /// stream is stale and dropped on arrival.
    StreamHandle(StreamHandle),
    /// Provider stream events (pushed by the connection).
    Stream(StreamEvent),
    /// Tool executor feedback — exactly one per dispatched call.
    ToolFinished { call: ToolCall, result: String },
    /// Control-plane barrier request: the chat layer is rebuilding the
    /// conversation engine (a connection-relevant truth source changed —
    /// provider pin, context base, tool registry; the machine knows
    /// neither). At Idle the gate engages AND drains in the same step —
    /// the step emits [`LoopFact::GateReleased`] directly (nothing is
    /// running, and everything sent before the decision precedes the Hold
    /// in the FIFO). Mid-round the gate arms: the live round — and any
    /// turns queued behind it, which belong to the pre-rebuild context —
    /// runs to its wrap-up, where the gate fires with the same fact. The
    /// flag is never sticky: consumed the moment the gate fires.
    Hold,
}

/// Events one provider stream pushes into the loop.
#[derive(Debug)]
pub enum StreamEvent {
    Chunk(StreamChunk),
    /// The stream failed before or while streaming (open error, parse
    /// error, stall, upstream truncation — the connection owns those
    /// semantics).
    Failed {
        message: String,
        code: Option<ErrorCode>,
    },
}

/// Cancellation capability for the active provider stream. Dropping it
/// cancels the connection's push (token fires, the pump task exits, the
/// HTTP response drops). Carries no I/O — the loop holding it stays
/// I/O-free.
#[derive(Debug, Clone)]
pub struct StreamHandle {
    token: CancellationToken,
}

impl StreamHandle {
    /// Create a handle paired with the token the connection's pump selects
    /// on.
    pub fn new() -> (Self, CancellationToken) {
        let token = CancellationToken::new();
        (
            Self {
                token: token.clone(),
            },
            token,
        )
    }

    /// The cancellation token (the connection's handle on the same
    /// capability).
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

// ── Output (loop →) ─────────────────────────────────────────────────────────

/// One semantic fact in the conversation's trace — past tense, ordered.
/// The loop knows no consumers; each consumer folds what it cares about.
#[derive(Debug, Clone, PartialEq)]
pub enum LoopFact {
    /// A client-visible wire event (deltas, usage, tool wire events,
    /// stream end, cancellation, errors) — the router's translation input.
    Wire(WireEvent),
    /// Messages committed to the conversation transcript (the round's user
    /// message at round start; the whole transcript at round end). The
    /// persistence fold appends these to the store — awaiting it before
    /// forwarding the round's `Wire(StreamEnd)` preserves the
    /// persist-before-announce guarantee.
    TranscriptCommitted(Vec<Message>),
    /// The round needs model input over `pending` — the chat layer opens
    /// its provider connection, which pushes `Stream` inputs and a
    /// `StreamHandle` back into the loop.
    ModelInputRequested(Vec<Message>),
    /// A tool was dispatched (the executor peer acts on this; the router
    /// fold maps it onto the `tool_start` wire event). `arguments` is the
    /// PARSED argument map the executor feeds the tool.
    ToolDispatched {
        call: ToolCall,
        arguments: HashMap<String, serde_json::Value>,
    },
    /// Interrupt every in-flight tool (machine policy — the executor owns
    /// the mechanism: cooperative tokens, grace, force-drop).
    InterruptTools,
    /// The wire-visible round state changed (idle/streaming) — the chat
    /// layer tracks this for authoritative subscription snapshots. The
    /// loop emits it on KIND transitions; a step that wraps a round AND
    /// starts the next queued turn re-enters Streaming within one step,
    /// so the machine reports that boundary step-locally (see the turn
    /// queue).
    RoundState(ChatStateKind),
    /// The control-plane gate fired: either the Hold found the machine
    /// Idle (engaged and drained in one step) or an armed gate reached
    /// its wrap-up with the turn queue drained. The rebuild flow acts on
    /// exactly this fact.
    GateReleased,
    /// How the round ENDED — the machine's semantic classification of the
    /// terminal transition, emitted once per round right after the
    /// wrap-up facts (transcript, StreamEnd). Consumers fold THIS instead
    /// of scraping `Wire` events for special cases (cancel handling) —
    /// no consumer ever re-derives semantics from a transport-shaped
    /// event.
    RoundEnded(RoundOutcome),
}

/// The terminal classification of one round. The machine assigns it at
/// the single wrap-up point ([`Machine`]'s `end_round`); every caller
/// path — normal completion, stream failure, user cancel — maps onto
/// exactly one variant.
#[derive(Debug, Clone, PartialEq)]
pub enum RoundOutcome {
    /// The final stream completed normally (a tool-less stream, or the
    /// follow-up stream after a tool batch).
    Completed { finish_reason: Option<String> },
    /// The user cancelled the round (stream cancel or tool interrupt).
    /// Whatever partial work landed stays; the queue was discarded.
    Cancelled,
    /// The stream failed before or while streaming (open error, parse
    /// error, stall, upstream truncation — the connection owns those
    /// semantics). The partial reply, if any, was preserved.
    Failed {
        message: String,
        code: Option<ErrorCode>,
    },
}

/// Where a provider connection pushes its stream events. A plain closure
/// keeps flux-core free of channel types and the connection free of the
/// loop — the adapter wraps whatever transport it likes. The sink never
/// blocks (the loop's input channel is unbounded).
pub type StreamSink = std::sync::Arc<dyn Fn(StreamEvent) + Send + Sync>;

/// A provider connection — one conversation's stateful session (the
/// prefix cache). Produced by a provider's `begin`; the chat layer drives
/// it at round boundaries. The connection lives for the engine's lifetime:
/// a truth-source change (provider pin, tool set, context base) rebuilds
/// the WHOLE engine (the session layer respawns it), so a connection is
/// never mutated in place.
#[async_trait::async_trait]
pub trait Connection: Send {
    async fn open(
        &mut self,
        pending: &[Message],
        sink: StreamSink,
    ) -> Result<StreamHandle, CoreError>;
}
