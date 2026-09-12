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
//!   `ToolFinished` back into the loop's FIFO (stamped with `ends_round`
//!   from the consumer's round-ending-name config);
//! - **engine rebuild**: the ONE control command. The session layer
//!   mutates the truth sources (provider pin, context base, the global
//!   tool registry) and sends `Rebuild`; the consumer arms the machine's
//!   gate (a live round — and any turns queued behind it, which belong
//!   to the pre-rebuild context — runs to its wrap-up first), and at the
//!   fired gate rebuilds the engine IN PLACE: the tool registry from the
//!   current global truth, the connection re-begin over the live history
//!   above the context base, the provider instance from the carried pin.
//!   The same deterministic assembly every spawn uses — the engine never
//!   dies for a rebuild, and a live round is never mutated;
//! - **feature orchestration**: a `feature_done` result + round end
//!   composes onto the same rebuild flow: the consumer arms the gate,
//!   and at the fired gate archives the context base (the whole live
//!   context), swaps the connection over the archived prefix, and
//!   injects the follow-up (the `feature_done` result — the next
//!   feature's opening turn) as the next round's user turn. A racing
//!   user message waits behind the gate and its round lands INSIDE the
//!   archive (it runs first, on the intact context).

use crate::chat::{CONTEXT_BASE_KEY, Chat, ResolvedPin};
use crate::question::QuestionTool;
use crate::spawn::{ChatKit, assemble_tools};
use crate::tool_exec::{FlightOutput, Flights};
use flux_core::{
    ChatKind, ChatStateKind, Connection, FEATURE_DONE_TOOL, LoopFact, LoopInput, OutputPort,
    Provider, RoundOutcome, StreamEvent, ToolDefinition, ToolRegistry, WireEvent,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::mpsc;

/// Commands ops sends to the round consumer.
pub(crate) enum RoundCmd {
    /// Rebuild the engine in place at the next machine gate: a live round
    /// — and any turns queued behind it — finishes first; at the fired
    /// gate the consumer re-assembles from the truth sources (the global
    /// tool registry, the store's live history above the context base)
    /// and re-begins its connection. `provider` replaces the chat's
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
    /// overflow-buffer round marking), the `ToolPort` the supervised
    /// flights call, and the store for the feature orchestration's
    /// truth-source writes. Its `id` doubles as the consumer's chat id
    /// (logging + the store calls).
    pub(crate) chat: Arc<Chat>,
    /// The wire sink (production: the router; tests: a collector).
    pub(crate) wire: Arc<dyn OutputPort>,
    /// The round-ending tool NAMES (adapter config — feature mode
    /// configures `feature_done`); the consumer stamps `ends_round` on
    /// each flight feedback. The machine knows only the bool.
    pub(crate) round_ending_tools: HashSet<String>,
    pub(crate) state_slot: Arc<StdMutex<ChatStateKind>>,
    /// The connection the loop's `ModelInputRequested` opens. Replaced in
    /// place at every fired gate — the in-place rebuild's whole point.
    pub(crate) connection: Box<dyn Connection>,
    /// Whether the feature orchestration hook is active (feature chats).
    pub(crate) feature_mode: bool,
    // ── in-place rebuild materials (read fresh at every gate) ──
    /// The agent preamble — every re-begin carries it.
    pub(crate) system_prompt: Arc<str>,
    /// The global tool registry (the shell's Arc — MCP mutations are
    /// visible through it at every gate).
    pub(crate) global_registry: Arc<ToolRegistry>,
    /// Initial state key → description (the state tools' schema).
    pub(crate) descriptions: Arc<HashMap<String, String>>,
    /// The chat's question board (re-assembles the question tool).
    pub(crate) questions: Arc<crate::question::QuestionBoard>,
    /// The conversation kind (feature_done registration).
    pub(crate) kind: ChatKind,
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
    // Feature-restart intent captured mid-round: (call id, result text).
    // Taken (consumed) when the armed gate fires — so it survives queued
    // rounds that run pre-gate — and cleared by a user cancel (the user
    // said stop: no rebuild, no injection).
    let mut feature_result: Option<(String, String)> = None;
    // The gate is in flight: the Hold went out, waiting for the machine's
    // `GateReleased`. Exactly two triggers arm it — a `Rebuild` command or
    // a feature_done result observed at a round end — and a second trigger
    // while this is set coalesces into the same gate. `GateReleased` always
    // discharges into `apply_rebuild` (the feature follow-up, if any, rides
    // it), so no separate intent flag exists: the gate IS the intent.
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
                // feedback. The consumer stamps `ends_round` here (adapter
                // config: the round-ending tool NAMES; the machine knows
                // only the bool).
                let (call, result) = flights.collect(done);
                let ends_round = deps.round_ending_tools.contains(&call.name);
                if loop_tx
                    .send(LoopInput::ToolFinished {
                        call,
                        result,
                        ends_round,
                    })
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
                        deps.chat.persist_messages(&messages).await;
                    }
                    LoopFact::Wire(event) => {
                        deps.wire.emit(event).await;
                    }
                    LoopFact::RoundEnded(outcome) => {
                        // The semantic round classification — the feature
                        // hook's input. The machine assigns it at the single
                        // wrap-up point; no wire-event scraping here:
                        // - `ToolEnded` carries the round-ending tool's call
                        //   + full result (the next feature's opening turn);
                        //   only `feature_done` composes onto the rebuild
                        //   flow — other round-ending names (if an adapter
                        //   ever configures more) just end their round.
                        // - `Cancelled` clears the intent: a user cancel
                        //   absorbed during the feature_done flight wrapped
                        //   the round with Cancelled (the machine exempts
                        //   the tool from interruption) — the user said
                        //   stop: no rebuild, no injection; the next user
                        //   message continues on the intact context.
                        if deps.feature_mode {
                            match outcome {
                                RoundOutcome::ToolEnded { call, result }
                                    if call.name == FEATURE_DONE_TOOL =>
                                {
                                    feature_result = Some((call.id, result));
                                }
                                RoundOutcome::Cancelled => feature_result = None,
                                _ => {}
                            }
                        }
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
                        // registry, live history above the context base —
                        // then inject the feature follow-up (if any) as
                        // the next round's user turn. The engine never
                        // dies for a rebuild; the consumer keeps folding.
                        apply_rebuild(&mut deps, feature_result.take(), &loop_tx).await;
                        gate_awaited = false;
                    }
                    LoopFact::RoundState(kind) => {
                        *deps.state_slot.lock().unwrap() = kind;
                        // Feature restart: the round ended of its own accord
                        // with a feature_done result — compose onto the same
                        // rebuild flow. If a rebuild is already holding or
                        // draining, the feature intent finalizes at ITS exit;
                        // if a user message already started the next round,
                        // the Hold arms and lands at THAT round's wrap-up.
                        // (`feature_result` is only ever Some in feature
                        // mode — its capture site is guarded by it.)
                        if kind == ChatStateKind::Idle && feature_result.is_some() && !gate_awaited
                        {
                            let _ = loop_tx.send(LoopInput::Hold);
                            gate_awaited = true;
                        }
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

/// Finalize the feature orchestration at the drained gate: persist the
/// context base (archive the WHOLE live context — a round that raced the
/// gate already reached the store and lands inside the archive) and hand
/// the follow-up to the session layer (the respawn injects it as the
/// fresh engine's first user turn). The wire notice rides the truth-source
/// write (the mutator announces). A persistence failure logs and skips the
/// injection — respawning on an un-archived context with a feature opening
/// turn would start the next feature on a stale context.
/// Rebuild the engine in place at the fired gate. Order matters:
///
/// 1. **feature archive** (if a feature_done result is pending) — the new
///    connection's prefix is the live history above the NEW base;
/// 2. **re-assembly** — the registry from the CURRENT global truth (the
///    same `assemble_tools` the initial spawn runs) and the connection
///    re-begin over the live history above the context base, on the
///    chat's current provider instance;
/// 3. **follow-up injection** — the archived feature's opening turn rides
///    the loop channel as a plain user turn; the machine is Idle here
///    (the gate fired only once the turn queue drained), so it starts
///    the next feature on the FRESH connection.
///
/// A store failure keeps the current engine materials (the rebuild
/// retries at the next gate); an archive failure skips only the
/// injection — the next feature would start on an un-archived context,
/// the same conservative call the respawn flow made.
async fn apply_rebuild(
    deps: &mut RoundDeps,
    feature: Option<(String, String)>,
    loop_tx: &mpsc::UnboundedSender<LoopInput>,
) {
    let follow_up = match feature {
        Some((call_id, text)) => match archive_feature(deps, &call_id, &text).await {
            Ok(text) => Some(text),
            Err(e) => {
                tracing::warn!(
                    chat_id = %deps.chat.id,
                    error = %e,
                    "feature context archive failed; skipping the follow-up injection"
                );
                None
            }
        },
        None => None,
    };
    let base = match deps.chat.store.load_state(&deps.chat.id).await {
        Ok(state) => state
            .get(CONTEXT_BASE_KEY)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0),
        Err(e) => {
            tracing::warn!(
                chat_id = %deps.chat.id,
                error = %e,
                "engine rebuild: failed to read the context base; keeping the current connection"
            );
            return;
        }
    };
    let history = match deps
        .chat
        .store
        .load_messages_after(&deps.chat.id, base)
        .await
    {
        Ok(history) => history,
        Err(e) => {
            tracing::warn!(
                chat_id = %deps.chat.id,
                error = %e,
                "engine rebuild: failed to load the live history; keeping the current connection"
            );
            return;
        }
    };
    // Re-assemble the per-chat registry from the current global truth —
    // the same assembly every spawn runs — and swap the chat's lookup
    // surface atomically (tool calls between rebuilds never observe a
    // half-swapped set).
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
        deps.kind,
    );
    let tool_defs: Arc<[ToolDefinition]> = Arc::from(fresh.definitions().into_boxed_slice());
    deps.chat.tools.replace_with(&fresh);
    deps.connection = deps
        .provider
        .begin(&deps.system_prompt, &tool_defs, &history);
    tracing::info!(
        chat_id = %deps.chat.id,
        base,
        history = history.len(),
        "engine rebuilt in place at the machine gate"
    );
    if let Some(text) = follow_up {
        let _ = loop_tx.send(LoopInput::UserMessage(text));
    }
}

/// Archive the feature context at the gate: resolve the follow-up's FULL
/// text first (a buffered feature_done result must not dangle behind the
/// GC below), persist the context base (the whole live context archives —
/// a round that raced the gate already reached the store and lands inside
/// the archive), GC the buffered outputs whose calls the archive removed
/// from the model's view, and announce the rebase. Returns the resolved
/// follow-up text (the injection's payload).
async fn archive_feature(
    deps: &mut RoundDeps,
    call_id: &str,
    text: &str,
) -> anyhow::Result<String> {
    let text = match deps
        .chat
        .store
        .load_buf_entry(&deps.chat.id, call_id)
        .await?
    {
        Some(full) => full,
        None => text.to_string(),
    };
    let base = deps.chat.store.max_message_id(&deps.chat.id).await?;
    deps.chat
        .store
        .save_state_entry(&deps.chat.id, CONTEXT_BASE_KEY, &base.to_string())
        .await?;
    deps.chat.store.gc_buf_entries(&deps.chat.id, base).await?;
    deps.wire
        .emit(WireEvent::ContextRebased {
            base_message_id: base,
        })
        .await;
    tracing::info!(
        chat_id = %deps.chat.id,
        base,
        "feature context archived at the machine gate"
    );
    Ok(text)
}

/// Sets the done flag on drop — every exit from the consumer task body,
/// including a panic unwinding through it.
struct DoneGuard(Arc<std::sync::atomic::AtomicBool>);

impl Drop for DoneGuard {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
