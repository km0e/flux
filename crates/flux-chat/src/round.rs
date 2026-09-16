//! The round consumer — the chat layer's fold over the loop's fact trace.
//!
//! ONE task per chat consumes the loop's output channel (its commands from
//! ops ride a separate control channel, and the tool-flight completions a
//! third arm — selected together, the consumer is the single ordered
//! interpreter of everything the loop produces AND of the tool flights it
//! supervises). The task lives for the chat's lifetime; it exits only when
//! the fact channel closes (the loop died) or the control channel closes
//! (the handle dropped) — NEVER for a rebuild. Its folds:
//!
//! - **persistence**: `TranscriptCommitted` → store append, awaited
//!   inline — the fact order (transcript before `StreamEnd`) preserves
//!   the persist-before-announce guarantee;
//! - **routing**: `Wire` facts → the router (fan-out to viewers);
//! - **provider triggering**: `ModelInputRequested` → open the current
//!   connection, push its handle back into the loop;
//! - **tool dispatch & flight supervision**: `ToolDispatched` → the
//!   `tool_start` wire + a supervised flight ([`Flights`], two-tier
//!   interrupt, panic capture); `InterruptTools` → cancel the in-flight
//!   token inline; the flight completion arm pushes exactly one
//!   `ToolFinished` back into the loop's FIFO;
//! - **engine rebuild**: the ONE control command. The session layer
//!   mutates the truth sources (provider pin, the global tool registry)
//!   and sends `Rebuild`; the consumer arms the machine's gate (a live
//!   round — and any turns queued behind it, which belong to the
//!   pre-rebuild context — runs to its wrap-up first), and at the fired
//!   gate rebuilds the engine IN PLACE: the tool registry from the
//!   current global truth, the connection re-begin over the FULL
//!   persisted transcript, the provider instance from the carried pin.
//!   The same deterministic assembly every spawn uses — the engine never
//!   dies for a rebuild, and a live round is never mutated.

use crate::chat::{Chat, ResolvedPin};
use crate::question::QuestionTool;
use crate::spawn::{ChatKit, assemble_tools};
use crate::tool_exec::{FlightOutput, Flights};
use flux_core::{
    ChatStateKind, Connection, LoopFact, LoopInput, OutputPort, Provider, Role, StreamEvent,
    ToolDefinition, ToolRegistry, WireEvent,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::mpsc;

/// Commands ops sends to the round consumer.
pub(crate) enum RoundCmd {
    /// Rebuild the engine in place at the next machine gate: a live round
    /// — and any turns queued behind it — finishes first; at the fired
    /// gate the consumer re-assembles from the truth sources (the global
    /// tool registry, the store's full persisted transcript) and
    /// re-begins its connection. `provider` replaces the chat's
    /// provider instance for this and every later rebuild (the hot-swap
    /// path); `None` keeps the current one.
    Rebuild { provider: Option<ResolvedPin> },
}

/// The consumer's control handle (held by the ChatHandle for ops).
#[derive(Clone)]
pub(crate) struct RoundControl {
    tx: mpsc::UnboundedSender<RoundCmd>,
}

impl RoundControl {
    pub(crate) fn new(tx: mpsc::UnboundedSender<RoundCmd>) -> Self {
        Self { tx }
    }

    pub(crate) fn rebuild(&self, provider: Option<ResolvedPin>) -> bool {
        self.tx.send(RoundCmd::Rebuild { provider }).is_ok()
    }
}

/// Everything the consumer folds over (wired once at spawn). The
/// connection lives for the engine's lifetime — a truth-source change
/// rebuilds it IN PLACE at the machine gate (never mid-round).
pub(crate) struct RoundDeps {
    /// The chat entity: persistence fold (transcript appends + the
    /// overflow-buffer round marking), and the `ToolPort` the supervised
    /// flights call. Its `id` doubles as the consumer's chat id
    /// (logging + the store calls).
    pub(crate) chat: Arc<Chat>,
    /// The wire sink (production: the router; tests: a collector).
    pub(crate) wire: Arc<dyn OutputPort>,
    pub(crate) state_slot: Arc<StdMutex<ChatStateKind>>,
    /// The connection the loop's `ModelInputRequested` opens. Replaced in
    /// place at every fired gate — the in-place rebuild's whole point.
    pub(crate) connection: Box<dyn Connection>,
    // ── in-place rebuild materials (read fresh at every gate) ──
    /// The agent preamble (base, WITHOUT the skill catalog) — every
    /// re-begin recomposes the prompt over it (skills.rs).
    pub(crate) system_prompt: Arc<str>,
    /// The global tool registry (the shell's Arc — MCP mutations are
    /// visible through it at every gate).
    pub(crate) global_registry: Arc<ToolRegistry>,
    /// Initial state key → description (the state tools' schema).
    pub(crate) descriptions: Arc<HashMap<String, String>>,
    /// The chat's question board (re-assembles the question tool).
    pub(crate) questions: Arc<crate::question::QuestionBoard>,
    /// The chat's provider instance (model-pinned) — replaced by a
    /// `Rebuild` carrying a fresh pin, used by every re-begin.
    pub(crate) provider: Arc<dyn Provider>,
}

/// Run the consumer for the chat's lifetime: until the fact channel closes
/// (the loop died) or the control channel closes (the handle dropped).
/// Rebuilds happen IN PLACE at the machine's gate — the task never exits
/// for one.
/// `done_flag` fires on every exit (DoneGuard semantics live with the
/// caller — this function takes the flag and sets it on drop).
pub(crate) async fn run_round(
    mut facts: mpsc::Receiver<LoopFact>,
    mut ctrl: mpsc::UnboundedReceiver<RoundCmd>,
    loop_tx: mpsc::UnboundedSender<LoopInput>,
    mut deps: RoundDeps,
    done_flag: Arc<std::sync::atomic::AtomicBool>,
) {
    let _done = DoneGuard(done_flag);
    // Supervised tool flights, driven directly by this fold loop (the
    // tool_exec module owns the mechanics): ToolDispatched dispatches,
    // InterruptTools cancels the in-flight token, the completion arm
    // pushes exactly one ToolFinished per dispatch into the loop's FIFO.
    let mut flights = Flights::new();
    // The gate is in flight: the Hold went out, waiting for the machine's
    // `GateReleased`. The one trigger is a `Rebuild` command; a second
    // trigger while this is set coalesces into the same gate.
    // `GateReleased` always discharges into `apply_rebuild`.
    let mut gate_awaited = false;
    loop {
        // Deterministic order: facts first (the loop's trace is the
        // timeline), then control commands. Ctrl-closed (the handle
        // dropped: chat deleted or task replaced) = immediate exit —
        // waiting for the loop to close the facts channel would deadlock:
        // the loop waits for every loop_tx clone to drop, and the consumer
        // holds one until it exits.
        let item = tokio::select! {
            biased;
            fact = facts.recv() => match fact {
                Some(f) => Item::Fact(f),
                None => Item::Closed,
            },
            cmd = ctrl.recv() => match cmd {
                Some(c) => Item::Cmd(c),
                None => Item::Closed,
            },
            done = flights.join_next(), if flights.active() => Item::Flight(done),
        };
        match item {
            Item::Closed => break,
            Item::Flight(done) => {
                // The flight's outcome — exactly one feedback per dispatch,
                // pushed into the loop's FIFO like every other peer's
                // feedback.
                let (call, result) = flights.collect(done);
                if loop_tx
                    .send(LoopInput::ToolFinished { call, result })
                    .is_err()
                {
                    break; // loop gone — nothing left to feed
                }
            }
            Item::Cmd(RoundCmd::Rebuild { provider }) => {
                // A carried pin replaces the chat's provider instance now;
                // the swap itself happens at the fired gate (never
                // mid-round).
                if let Some(pin) = provider {
                    deps.provider = pin.provider;
                }
                if !gate_awaited {
                    // Arm the gate: it engages at the next boundary
                    // (immediately if Idle — the machine declares the
                    // boundary, no state probing). A live round finishes
                    // first; a second intent while the gate is in flight
                    // coalesces.
                    let _ = loop_tx.send(LoopInput::Hold);
                    gate_awaited = true;
                }
            }
            Item::Fact(fact) => {
                match fact {
                    LoopFact::TranscriptCommitted(messages) => {
                        let ids = deps.chat.persist_messages(&messages).await;
                        // The turn-acceptance commit is exactly one user
                        // message — announce its row id so the sender's
                        // client can name its own live bubble (the fork
                        // affordance). Announced AFTER the persist lands
                        // (the same discipline as every wire fact here).
                        if let ([msg], [id]) = (&messages[..], &ids[..])
                            && msg.role == Role::User
                        {
                            deps.wire
                                .emit(WireEvent::MessagePersisted {
                                    id: *id,
                                    content: msg.content.clone(),
                                })
                                .await;
                        }
                    }
                    LoopFact::Wire(event) => {
                        deps.wire.emit(event).await;
                    }
                    LoopFact::RoundEnded(_) => {
                        // The semantic round classification — no consumer
                        // fold today; the fact remains the machine's
                        // authoritative terminal record.
                    }
                    LoopFact::ModelInputRequested(pending) => {
                        // The connection is never replaced — reads here are
                        // sequential.
                        let sink: flux_core::StreamSink = {
                            let loop_tx = loop_tx.clone();
                            Arc::new(move |event: StreamEvent| {
                                let _ = loop_tx.send(LoopInput::Stream(event));
                            })
                        };
                        match deps.connection.open(&pending, sink).await {
                            Ok(handle) => {
                                let _ = loop_tx.send(LoopInput::StreamHandle(handle));
                            }
                            Err(e) => {
                                tracing::warn!(chat_id = %deps.chat.id, error = %e, "failed to open provider stream");
                                let _ = loop_tx.send(LoopInput::Stream(StreamEvent::Failed {
                                    message: e.to_string(),
                                    code: Some(e.error_code()),
                                }));
                            }
                        }
                    }
                    LoopFact::ToolDispatched { call, arguments } => {
                        deps.wire
                            .emit(WireEvent::ToolStart {
                                id: call.id.clone(),
                                name: call.name.clone(),
                                arguments: call.arguments.clone(),
                            })
                            .await;
                        if flights.active() {
                            // Unreachable by machine construction (the
                            // machine dispatches one at a time and waits for
                            // the feedback); a defensive drop keeps a stuck
                            // consumer from double-dispatching.
                            tracing::warn!(tool = %call.name, "flight already active: dispatch dropped");
                        } else {
                            flights.dispatch(call, arguments, deps.chat.clone());
                        }
                    }
                    LoopFact::InterruptTools => flights.interrupt_all(),
                    LoopFact::GateReleased => {
                        // The gate fired: rebuild the engine IN PLACE from
                        // the truth sources — provider instance, tool
                        // registry, the full persisted transcript.
                        // The engine never dies for a rebuild; the consumer
                        // keeps folding.
                        apply_rebuild(&mut deps).await;
                        gate_awaited = false;
                    }
                    LoopFact::RoundState(kind) => {
                        *deps.state_slot.lock().unwrap() = kind;
                    }
                }
            }
        }
    }
}

enum Item {
    Fact(LoopFact),
    Cmd(RoundCmd),
    Flight(Option<Result<FlightOutput, tokio::task::JoinError>>),
    Closed,
}

/// Rebuild the engine in place at the fired gate. Order matters:
///
/// 1. **history read** — the new connection begins over the FULL persisted
///    transcript (a chat's context is its history; forking, not archiving,
///    is how a conversation restarts from a message);
/// 2. **re-assembly** — the registry from the CURRENT global truth (the
///    same `assemble_tools` the initial spawn runs) and the connection
///    re-begin over that history, on the chat's current provider instance.
///
/// A store failure keeps the current engine materials (the rebuild
/// retries at the next gate).
async fn apply_rebuild(deps: &mut RoundDeps) {
    let history = match deps.chat.store.load_messages(&deps.chat.id).await {
        // Read-side invariant guard: the re-begin history must be
        // provider-valid (see crate::history).
        Ok(history) => crate::validate_history(history),
        Err(e) => {
            tracing::warn!(
                chat_id = %deps.chat.id,
                error = %e,
                "engine rebuild: failed to load the history; keeping the current connection"
            );
            return;
        }
    };
    // Re-assemble the per-chat registry from the current global truth —
    // the same assembly every spawn runs — and swap the chat's lookup
    // surface atomically (tool calls between rebuilds never observe a
    // half-swapped set). The activation index re-derives from the freshly
    // loaded history: the gate sees only round-atomic transcripts, so the
    // index matches exactly what the model's context carries.
    let skill_index =
        crate::skills::derive_activation_index(&deps.chat.store, &deps.chat.id, &history).await;
    let question = QuestionTool::new(Arc::clone(&deps.questions), Arc::clone(&deps.wire));
    let kit = ChatKit {
        descriptions: &deps.descriptions,
        question,
    };
    let fresh = assemble_tools(
        &deps.global_registry,
        deps.chat.state_manager.clone(),
        &kit,
        &deps.chat.id,
        &deps.chat.store,
        skill_index,
    );
    let tool_defs: Arc<[ToolDefinition]> = Arc::from(fresh.definitions().into_boxed_slice());
    deps.chat.tools.replace_with(&fresh);
    // The catalog recomposes at every gate — a rebuild is the one point a
    // begin happens anyway, so the snapshot refreshes for free (the base
    // preamble in `system_prompt` stays untouched; see skills.rs).
    let composed_prompt = crate::skills::compose_system_prompt(
        &deps.system_prompt,
        deps.chat.state_manager.workdir(),
    );
    deps.connection = deps.provider.begin(&composed_prompt, &tool_defs, &history);
    tracing::info!(
        chat_id = %deps.chat.id,
        history = history.len(),
        "engine rebuilt in place at the machine gate"
    );
}

/// Sets the done flag on drop — every exit from the consumer task body,
/// including a panic unwinding through it.
struct DoneGuard(Arc<std::sync::atomic::AtomicBool>);

impl Drop for DoneGuard {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
