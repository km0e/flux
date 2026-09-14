//! The conversation loop as a pure reducer.
//!
//! [`Machine::step`] is total and pure: no IO, no await, no randomness. It
//! consumes [`LoopInput`] (the loop's input vocabulary, shared with the
//! provider connection and the tool executor as channel peers) and emits
//! [`LoopFact`]s — a semantic, past-tense trace of the conversation that
//! the chat layer folds independently (persistence, routing, provider
//! triggering, rebuild orchestration). Policy (when to interrupt, when a
//! round ends) lives here; every mechanism (tokens, drops, I/O) lives in
//! the peers.

use flux_core::LoopFact;
use flux_core::LoopInput;
use flux_core::RoundOutcome;
use flux_core::StreamEvent;
use flux_core::StreamHandle;
use flux_core::WireEvent;
use flux_core::{ChatStateKind, Message, Role, StreamChunk, ToolCall};
use std::collections::{HashMap, VecDeque};
use tracing::warn;

/// Tool call with its parsed arguments (the batch vocabulary).
pub type ToolCallWithArgs = (ToolCall, HashMap<String, serde_json::Value>);

/// Collected output of one provider stream.
#[derive(Debug, Default, PartialEq)]
pub struct StreamOutput {
    pub text: String,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

/// The machine's state.
#[derive(Debug, PartialEq)]
pub enum State {
    /// Await a user message — rounds only start from here.
    Idle,
    /// A provider stream is in flight.
    Streaming { output: StreamOutput },
    /// Tool batch processing: tools dispatch one at a time (supervised
    /// flights owned by the executor peer). No approval phase — tools
    /// execute directly; the chat boundary rides ToolCtx (filled
    /// adapter-side before tool.call).
    ProcessingTools {
        /// Tools still to execute, dispatched one at a time.
        exec_queue: VecDeque<ToolCallWithArgs>,
        /// Tools dispatched and in flight: call_id → tool name. One entry
        /// today (single flight); a map so parallel dispatch later needs no
        /// state reshape.
        inflight: HashMap<String, String>,
        /// A cancel was absorbed into this flag after interrupting the
        /// in-flight tool; wrap up with a Cancelled event once the
        /// flight's result lands.
        cancelled: bool,
    },
}

/// The outcome of one [`Machine::step`].
#[derive(Debug)]
pub struct Step {
    /// The semantic facts this step produced, in order.
    pub facts: Vec<LoopFact>,
}

/// The conversation loop state machine.
pub struct Machine {
    state: State,
    /// Provider-bound messages: the next stream request body.
    pending: Vec<Message>,
    /// Round transcript: committed at every batch boundary (the batch's
    /// assistant message plus its results, atomically) and at round end
    /// (the final segment).
    transcript: Vec<Message>,
    /// The active stream's cancellation handle (delivered by the chat
    /// layer via `LoopInput::StreamHandle` after it opened the
    /// connection). Dropped when the round wraps or the user cancels —
    /// the drop stops the connection's push. Pure cancellation capability;
    /// no I/O lives here.
    active_stream: Option<StreamHandle>,
    /// The control-plane gate. Arriving at Idle the gate is engaged AND
    /// drained in the same step (nothing is running, nothing can start —
    /// everything the shell sent before the decision precedes the Hold in
    /// the FIFO) — the step emits [`LoopFact::GateReleased`] directly.
    /// Arriving mid-round it arms: the live round (and any turns queued
    /// behind it — they belong to the pre-rebuild context) runs to its
    /// wrap-up, where the gate fires. Never sticky: the flag is consumed
    /// the moment the gate fires.
    hold: bool,
    /// User turns that arrived mid-round (streaming / tool flight).
    /// QUEUED, not dropped: each starts a fresh round in the same step that
    /// wraps the current one (FIFO — see [`Machine::start_queued_round`]).
    /// The kernel serializes user turns itself; the R1 interrupt-send
    /// delivers the interject pair (cancel + message) back-to-back on
    /// this FIFO, so the order holds by construction. A `Cancel` discards
    /// the queue (stop means stop — a turn sent after the cancel still
    /// runs). Invariant: the machine never rests Idle with a
    /// non-empty queue — an armed gate defers to queued rounds (they run
    /// pre-gate, inside the pre-rebuild context), so the gate fires only
    /// once the queue is drained.
    queued: VecDeque<String>,
}

impl Machine {
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            pending: Vec::new(),
            transcript: Vec::new(),
            active_stream: None,
            hold: false,
            queued: VecDeque::new(),
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// The wire-visible round state (the `RoundState` fact's source).
    pub fn state_kind(&self) -> ChatStateKind {
        match self.state {
            State::Idle => ChatStateKind::Idle,
            // Tool processing (a pending `question` tool included) is part
            // of a live round — the wire-visible state stays streaming.
            State::Streaming { .. } | State::ProcessingTools { .. } => ChatStateKind::Streaming,
        }
    }
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    /// Apply one event. Total: every (state, input) pair is defined.
    pub fn step(&mut self, event: LoopInput) -> Step {
        let state = std::mem::replace(&mut self.state, State::Idle);
        Step {
            facts: self.apply(state, event),
        }
    }

    /// The transition table — the reducer is the source of truth.
    fn apply(&mut self, state: State, event: LoopInput) -> Vec<LoopFact> {
        match state {
            // ── Idle ──
            State::Idle => match event {
                LoopInput::UserMessage(text) => self.start_round_with_user(Message::user(text)),
                // A stale handle after the round wrapped — drop (cancels a
                // push nobody listens to anymore).
                LoopInput::StreamHandle(_) => {
                    self.state = State::Idle;
                    Vec::new()
                }
                // Control-plane barrier: the chat layer wants to rebuild
                // the conversation engine. At Idle the gate engages AND
                // drains in this step — nothing is running, and everything
                // sent before the decision precedes the Hold in the FIFO,
                // so nothing can start a round behind it.
                LoopInput::Hold => {
                    self.hold = false;
                    vec![LoopFact::GateReleased]
                }
                // Stale chunk/feedback racing a round boundary — ignore.
                _ => {
                    self.state = State::Idle;
                    Vec::new()
                }
            },

            // ── Streaming ──
            State::Streaming { output } => match event {
                LoopInput::Stream(StreamEvent::Chunk(StreamChunk::Text(delta))) => {
                    let mut out = output;
                    out.text.push_str(&delta);
                    let facts = vec![LoopFact::Wire(WireEvent::TextDelta(delta))];
                    self.state = State::Streaming { output: out };
                    facts
                }
                LoopInput::Stream(StreamEvent::Chunk(StreamChunk::Reasoning(delta))) => {
                    let mut out = output;
                    out.reasoning
                        .get_or_insert_with(String::new)
                        .push_str(&delta);
                    let facts = vec![LoopFact::Wire(WireEvent::ReasoningDelta(delta))];
                    self.state = State::Streaming { output: out };
                    facts
                }
                LoopInput::Stream(StreamEvent::Chunk(StreamChunk::ToolCalls(tcs))) => {
                    let mut out = output;
                    out.tool_calls.extend(tcs);
                    self.state = State::Streaming { output: out };
                    Vec::new()
                }
                // Forward signaling while the model forms a tool call — pure
                // wire passthrough, NO state: the complete ToolCalls batch at
                // End remains the sole dispatch source.
                LoopInput::Stream(StreamEvent::Chunk(StreamChunk::ToolCallPreview {
                    id,
                    name,
                    args_delta,
                })) => {
                    self.state = State::Streaming { output };
                    vec![LoopFact::Wire(WireEvent::ToolCallPreview {
                        id,
                        name,
                        args_delta,
                    })]
                }
                LoopInput::Stream(StreamEvent::Chunk(StreamChunk::Usage {
                    prompt_tokens,
                    completion_tokens,
                    cached_tokens,
                })) => {
                    self.state = State::Streaming { output };
                    vec![LoopFact::Wire(WireEvent::Usage {
                        prompt_tokens,
                        completion_tokens,
                        cached_tokens,
                    })]
                }
                LoopInput::Stream(StreamEvent::Chunk(StreamChunk::End { finish_reason })) => {
                    self.finish_stream_ok(output, finish_reason)
                }
                LoopInput::Stream(StreamEvent::Failed { message, code }) => {
                    self.finish_stream_failed(output, RoundOutcome::Failed { message, code })
                }
                // Cancel is a user action, not an error — notify the client
                // with a distinct Cancelled event; the partial stays, the
                // round ends, Idle absorbs the residual chunks before any
                // follow-up round starts. The handle drops here: the
                // connection stops pushing immediately. Queued turns die
                // with the cancel (stop means stop — the R1 interrupt-send
                // pairs the cancel with the next message back-to-back on
                // this FIFO, so a turn sent after the cancel still runs).
                LoopInput::Cancel => {
                    self.active_stream = None;
                    self.queued.clear();
                    self.finish_stream_failed(output, RoundOutcome::Cancelled)
                }
                // The stream's cancel handle arrives while streaming —
                // register it (a cancel before this lands drops the slot's
                // later occupant via end_round; an Idle arrival drops it on
                // sight).
                LoopInput::StreamHandle(handle) => {
                    self.active_stream = Some(handle);
                    self.state = State::Streaming { output };
                    Vec::new()
                }
                // A turn sent mid-round: QUEUED, not dropped — it starts in
                // the same step that wraps the current round.
                LoopInput::UserMessage(text) => {
                    self.queued.push_back(text);
                    self.state = State::Streaming { output };
                    Vec::new()
                }
                // Control-plane barrier mid-round: arm the gate — it
                // engages at the next wrap-up (a live round is never
                // interrupted; it finishes first).
                LoopInput::Hold => {
                    self.hold = true;
                    self.state = State::Streaming { output };
                    Vec::new()
                }
                // An action racing a live stream — absorbed (total reducer).
                _ => {
                    self.state = State::Streaming { output };
                    Vec::new()
                }
            },

            // ── ProcessingTools ──
            State::ProcessingTools {
                mut exec_queue,
                mut inflight,
                cancelled,
            } => match event {
                LoopInput::ToolFinished { call, result } if inflight.contains_key(&call.id) => {
                    inflight.remove(&call.id);
                    self.dual_write(&call.id, result.clone());
                    let mut facts = vec![LoopFact::Wire(WireEvent::ToolResult {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        result: result.clone(),
                    })];
                    if cancelled {
                        // The absorbed cancel resolves once the in-flight
                        // tool reports back: keep its result, drop the
                        // rest, end the round with a Cancelled event
                        // (cancel is a user action, not an error).
                        facts.push(LoopFact::Wire(WireEvent::Cancelled));
                        facts.extend(self.end_round(RoundOutcome::Cancelled));
                    } else if let Some((call, args)) = exec_queue.pop_front() {
                        inflight.insert(call.id.clone(), call.name.clone());
                        self.state = State::ProcessingTools {
                            exec_queue,
                            inflight,
                            cancelled: false,
                        };
                        facts.push(LoopFact::ToolDispatched {
                            arguments: args.clone(),
                            call,
                        });
                    } else {
                        // Batch done: commit the transcript ATOMICALLY per
                        // batch — the assistant tool_calls message and every
                        // result of its batch land on disk together, BEFORE
                        // the continuation stream opens. This bounds the
                        // crash window (SIGKILL/OOM — no graceful path) to
                        // the live segment: a finished batch is never lost,
                        // and the persisted tail never carries a dangling
                        // tool_call (OpenAI-compatible APIs reject those
                        // with 400).
                        let pending = std::mem::take(&mut self.pending);
                        facts.push(LoopFact::TranscriptCommitted(std::mem::take(
                            &mut self.transcript,
                        )));
                        self.state = State::Streaming {
                            output: StreamOutput::default(),
                        };
                        facts.push(LoopFact::ModelInputRequested(pending));
                    }
                    facts
                }
                LoopInput::Cancel if !inflight.is_empty() => {
                    // Cancel while a tool is in flight: interrupt it. The
                    // executor peer cancels the tool's cooperative token; a
                    // tool that stops in time contributes its partial
                    // result, one that ignores the token is force-
                    // terminated — either way a ToolFinished lands and
                    // resolves the absorbed cancel. Drop the remaining
                    // batch — it is void now (the client receives
                    // Cancelled and never sees these tools). Void every
                    // committed-but-unexecuted tool first so the provider
                    // prefix never keeps a dangling tool_call after the
                    // round (the in-flight tool still reports a result on
                    // its own ToolFinished). Queued turns die with the
                    // cancel — stop means stop.
                    self.void_cancelled_tools(&exec_queue);
                    self.queued.clear();
                    // Kernel policy: no tool is exempt from user
                    // interruption — a cancel interrupts the in-flight
                    // flight. A second cancel is a no-op (the interrupt
                    // already went out).
                    let facts = if !cancelled {
                        vec![LoopFact::InterruptTools]
                    } else {
                        Vec::new()
                    };
                    self.state = State::ProcessingTools {
                        exec_queue: VecDeque::new(),
                        inflight,
                        cancelled: true,
                    };
                    facts
                }
                LoopInput::Cancel => {
                    // Defensive: a cancel observed with no tool in flight
                    // (only a logic bug or a race with the dispatch chain
                    // can produce this state) ends the round immediately.
                    // Void any committed-but-unexecuted tools before ending
                    // so their tool_calls don't dangle in the provider context.
                    self.void_cancelled_tools(&exec_queue);
                    self.queued.clear();
                    let mut facts = vec![LoopFact::Wire(WireEvent::Cancelled)];
                    facts.extend(self.end_round(RoundOutcome::Cancelled));
                    facts
                }
                // A turn sent during a tool flight: QUEUED — it starts when
                // the ROUND wraps (a tool batch may continue into a
                // follow-up stream first).
                LoopInput::UserMessage(text) => {
                    self.queued.push_back(text);
                    self.state = State::ProcessingTools {
                        exec_queue,
                        inflight,
                        cancelled,
                    };
                    Vec::new()
                }
                // Control-plane barrier mid-round: arm the gate — it
                // engages at the next wrap-up.
                LoopInput::Hold => {
                    self.hold = true;
                    self.state = State::ProcessingTools {
                        exec_queue,
                        inflight,
                        cancelled,
                    };
                    Vec::new()
                }
                _ => {
                    self.state = State::ProcessingTools {
                        exec_queue,
                        inflight,
                        cancelled,
                    };
                    Vec::new()
                }
            },
        }
    }

    // ── helpers ─────────────────────────────────────────────

    /// Round entry: persist the user message and open the first stream.
    fn start_round_with_user(&mut self, msg: Message) -> Vec<LoopFact> {
        self.pending.push(msg.clone());
        // Handoff: the request consumes the provider context; what remains
        // in `pending` (deferred tool-result voids) rides the next request.
        let pending = std::mem::take(&mut self.pending);
        self.state = State::Streaming {
            output: StreamOutput::default(),
        };
        vec![
            LoopFact::TranscriptCommitted(vec![msg]),
            LoopFact::ModelInputRequested(pending),
        ]
    }

    /// Round end: the transcript fact precedes the StreamEnd wire event so
    /// the chat layer's persistence fold awaits the append BEFORE the
    /// client sees the wrap-up (persist-before-announce), and the semantic
    /// [`RoundOutcome`] classification follows — the consumer folds it
    /// instead of scraping wire events (cancel handling). The transcript
    /// here is the FINAL segment only — earlier batches committed at their
    /// own boundaries.
    /// The active stream handle drops — the connection stops pushing
    /// (residual chunks, if any, are absorbed by Idle). An armed
    /// control-plane gate fires here — but a turn queued BEFORE the gate
    /// runs first (it belongs to the pre-rebuild context), so the gate
    /// defers to its wrap-up.
    fn end_round(&mut self, outcome: RoundOutcome) -> Vec<LoopFact> {
        let transcript = std::mem::take(&mut self.transcript);
        self.active_stream = None;
        let finish_reason = match &outcome {
            RoundOutcome::Completed { finish_reason } => finish_reason.clone(),
            _ => None,
        };
        let mut facts = vec![
            LoopFact::TranscriptCommitted(transcript),
            LoopFact::Wire(WireEvent::StreamEnd { finish_reason }),
            LoopFact::RoundEnded(outcome),
        ];
        if self.hold {
            if let Some(next) = self.queued.pop_front() {
                facts.extend(self.start_queued_round(next));
            } else {
                self.hold = false;
                self.state = State::Idle;
                facts.push(LoopFact::GateReleased);
            }
        } else if let Some(next) = self.queued.pop_front() {
            facts.extend(self.start_queued_round(next));
        } else {
            self.state = State::Idle;
        }
        facts
    }

    /// Start a turn that queued mid-round, in the same step that wrapped
    /// the previous round: the boundary is REPORTED (`RoundState(Idle)` —
    /// state snapshots read it; the loop's own transition check cannot
    /// emit it because the step re-enters Streaming) and the next round
    /// opens without the machine resting Idle.
    /// `RoundState(Streaming)` restores the snapshot truth.
    fn start_queued_round(&mut self, text: String) -> Vec<LoopFact> {
        let mut facts = vec![LoopFact::RoundState(ChatStateKind::Idle)];
        facts.extend(self.start_round_with_user(Message::user(text)));
        facts.push(LoopFact::RoundState(ChatStateKind::Streaming));
        facts
    }

    /// Stream ended normally: record the assistant message; dispatch the
    /// whole tool batch (one flight at a time), or end the round.
    fn finish_stream_ok(
        &mut self,
        output: StreamOutput,
        finish_reason: Option<String>,
    ) -> Vec<LoopFact> {
        let assistant = Message {
            role: Role::Assistant,
            content: output.text,
            reasoning_content: output.reasoning,
            tool_calls: output.tool_calls.clone(),
            tool_call_id: None,
        };
        self.transcript.push(assistant);
        if output.tool_calls.is_empty() {
            self.end_round(RoundOutcome::Completed { finish_reason })
        } else {
            let batch: Vec<ToolCallWithArgs> = output
                .tool_calls
                .iter()
                .map(|c| {
                    let args = match serde_json::from_str(&c.arguments) {
                        Ok(v) => v,
                        Err(e) => {
                            // Malformed tool arguments from the provider: keep
                            // executing with `{}` (matches historical behavior)
                            // but surface it — silent degradation hides why a
                            // tool fails/does the wrong thing.
                            warn!(
                                tool = %c.name,
                                error = %e,
                                raw = %c.arguments,
                                "provider returned unparseable tool arguments"
                            );
                            HashMap::new()
                        }
                    };
                    (c.clone(), args)
                })
                .collect();
            self.dispatch_batch(batch)
        }
    }

    /// Enter ProcessingTools with a freshly committed batch and dispatch
    /// its first tool immediately (single-flight discipline). No approval
    /// phase — boundary enrichment lives in the adapter's execute
    /// pipeline, so a committed batch goes straight to dispatch.
    fn dispatch_batch(&mut self, batch: Vec<ToolCallWithArgs>) -> Vec<LoopFact> {
        let mut exec_queue: VecDeque<ToolCallWithArgs> = batch.into();
        let Some((call, args)) = exec_queue.pop_front() else {
            // Unreachable: callers only enter with a non-empty batch — a
            // belt-and-braces guard instead of an invariant panic.
            return self.end_round(RoundOutcome::Completed {
                finish_reason: None,
            });
        };
        let mut inflight = HashMap::new();
        inflight.insert(call.id.clone(), call.name.clone());
        self.state = State::ProcessingTools {
            exec_queue,
            inflight,
            cancelled: false,
        };
        vec![LoopFact::ToolDispatched {
            arguments: args.clone(),
            call,
        }]
    }

    /// Stream failed / cancelled: preserve the partial reply, announce the
    /// terminal wire event (derived from the classification), end the
    /// round with it.
    fn finish_stream_failed(
        &mut self,
        output: StreamOutput,
        outcome: RoundOutcome,
    ) -> Vec<LoopFact> {
        let mut facts = Vec::new();
        match &outcome {
            RoundOutcome::Failed { message, code } => {
                facts.push(LoopFact::Wire(WireEvent::StreamError {
                    message: message.clone(),
                    code: code.clone(),
                }));
            }
            RoundOutcome::Cancelled => facts.push(LoopFact::Wire(WireEvent::Cancelled)),
            // Completed wraps via end_round directly — this path only
            // carries failure-shaped endings.
            _ => {}
        }
        self.push_partial(&output);
        facts.extend(self.end_round(outcome));
        facts
    }

    fn push_partial(&mut self, output: &StreamOutput) {
        if !output.text.is_empty() || output.reasoning.is_some() {
            self.transcript.push(Message {
                role: Role::Assistant,
                content: output.text.clone(),
                reasoning_content: output.reasoning.clone(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            });
        }
    }

    /// Write a tool result into BOTH buffers: provider context and the
    /// persisted transcript — one creation point, so the two copies can
    /// never disagree.
    fn dual_write(&mut self, call_id: &str, content: impl Into<String>) {
        let msg = Message::tool(call_id, content);
        self.pending.push(msg.clone());
        self.transcript.push(msg);
    }

    /// A cancel voids every committed-but-unexecuted tool of the batch. Each
    /// tool_call already committed to the provider prefix must still receive a
    /// matching tool message, or the next round's request carries dangling
    /// tool_calls (OpenAI-compatible APIs reject them with 400). The in-flight
    /// tool is exempt — it reports a real result through [`Self::dual_write`]
    /// on its own `ToolFinished`. The voids ride in `pending` and are delivered
    /// together with the next round's user message (deferred delivery), so no
    /// provider round-trip is spent on a user interrupt.
    fn void_cancelled_tools(&mut self, exec_queue: &VecDeque<ToolCallWithArgs>) {
        for (call, _) in exec_queue.iter() {
            self.dual_write(&call.id, "cancelled by user");
        }
    }
}

#[cfg(test)]
#[path = "machine_tests.rs"]
mod tests;
