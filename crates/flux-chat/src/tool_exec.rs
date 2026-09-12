//! Tool-flight supervision — the round consumer's flight library.
//!
//! The loop emits [`LoopFact::ToolDispatched`] / [`LoopFact::InterruptTools`]
//! facts; the round consumer folds them by driving [`Flights`] directly in
//! its own select loop — no separate task, no command channel (the
//! consumer is the single ordered interpreter, so flight feedback rides
//! the same fold; an `InterruptTools` fact cancels the in-flight token
//! inline). This module owns the WHOLE mechanism: supervised flights
//! (JoinSet), the two-tier interrupt (cooperative token → grace →
//! force-drop with Drop-based cleanup), panic capture, and the "exactly
//! one feedback per dispatch" contract — every dispatched call yields
//! exactly one collectable outcome, however the flight ends (the consumer
//! stamps `ends_round` and pushes `LoopInput::ToolFinished` back into the
//! loop's FIFO).
//!
//! Policy stays in the machine; no adapter-provided knowledge lives here —
//! the round-ending tool NAMES are consumer config (feature mode
//! configures `feature_done` — the NAME never enters the kernel).

use flux_core::{ToolCall, ToolPort};
use futures::FutureExt;
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Grace period after a cooperative cancel before a tool flight is
/// force-terminated (the tool ignored its token). Generous enough for a
/// subprocess kill + partial-output drain, short enough that a stuck tool
/// never blocks the round wrap-up noticeably.
pub const INTERRUPT_GRACE: Duration = Duration::from_secs(5);

/// Uniform transcript marker for an interrupted tool result. The contract:
/// tools never mention cancellation themselves — they return whatever
/// partial output they have (possibly empty); the supervisor prepends the
/// marker, so the transcript marks interruption in exactly one place.
pub const INTERRUPTED_MARK: &str = "[interrupted by user]";

/// One in-flight tool: its call and its cooperative cancellation token
/// (an interrupt cancels it; the grace clock inside the flight handles
/// the rest). Single-flight: at most one entry.
pub(crate) struct Flight {
    call: ToolCall,
    token: CancellationToken,
    interrupted: bool,
}

/// Outcome of one tool flight, yielded by the JoinSet — exactly one per
/// dispatched call, however it ends.
pub(crate) enum FlightOutput {
    /// The tool returned — real result, possibly partial, already marked.
    Done { result: String },
    /// The tool ignored its token past the grace; its future was dropped
    /// (abort-equivalent: Drop-based cleanup runs).
    ForceInterrupted,
}

/// The supervised-flight registry. Single-flight: the machine dispatches
/// one tool at a time and waits for the feedback, so at most one entry is
/// live; the consumer drives it from its select loop.
pub(crate) struct Flights {
    set: tokio::task::JoinSet<FlightOutput>,
    active: Option<Flight>,
}

impl Flights {
    pub(crate) fn new() -> Self {
        Self {
            set: tokio::task::JoinSet::new(),
            active: None,
        }
    }

    pub(crate) fn active(&self) -> bool {
        self.active.is_some()
    }

    /// Spawn one tool flight (returns immediately — the consumer's select
    /// collects the outcome).
    pub(crate) fn dispatch(
        &mut self,
        call: ToolCall,
        args: HashMap<String, serde_json::Value>,
        tools: Arc<dyn ToolPort + Send + Sync>,
    ) {
        let token = CancellationToken::new();
        self.set.spawn(run_flight(
            tools,
            call.clone(),
            args,
            flux_core::ToolCtx {
                cancel: token.clone(),
                call_id: call.id.clone(),
                ..Default::default()
            },
        ));
        self.active = Some(Flight {
            call,
            token,
            interrupted: false,
        });
    }

    /// The flight completion arm — joins the JoinSet (never empty when
    /// `active()` is true).
    pub(crate) async fn join_next(
        &mut self,
    ) -> Option<Result<FlightOutput, tokio::task::JoinError>> {
        self.set.join_next().await
    }

    /// First-tier interrupt: cancel the in-flight token. A tool that stops
    /// in time contributes its partial result through the normal
    /// completion path; the grace clock force-terminates the rest.
    pub(crate) fn interrupt_all(&mut self) {
        if let Some(flight) = &mut self.active {
            flight.interrupted = true;
            flight.token.cancel();
        }
    }

    /// Collect one flight outcome, normalizing every ending into the
    /// `(call, final result)` pair. A task that died without yielding
    /// (panic outside the flight's catch_unwind — defensive) is reported
    /// as an error result.
    pub(crate) fn collect(
        &mut self,
        done: Option<Result<FlightOutput, tokio::task::JoinError>>,
    ) -> (ToolCall, String) {
        let Some(flight) = self.active.take() else {
            return (
                ToolCall {
                    id: "unknown".into(),
                    name: String::new(),
                    arguments: String::new(),
                },
                "Error: no tool in flight".into(),
            );
        };
        let result = match done {
            Some(Ok(FlightOutput::Done { result })) => result,
            Some(Ok(FlightOutput::ForceInterrupted)) => format!(
                "{INTERRUPTED_MARK} (force-terminated: the tool ignored cancellation for {INTERRUPT_GRACE:?})"
            ),
            Some(Err(e)) => format!("Error: tool task crashed: {e}"),
            // The JoinSet yielded nothing (a panic escaped before any
            // output — defensive): report an error result.
            None => "Error: tool task crashed (no output)".into(),
        };
        (flight.call, result)
    }
}

/// One tool flight: race the tool's execution against its own two-tier
/// interrupt clock (token fires → grace → force). The task always yields
/// exactly one [`FlightOutput`] — `biased` prefers the execution arm, so a
/// tool finishing during the grace window still contributes its real
/// result instead of losing it to a concurrently-elapsed timer.
async fn run_flight(
    tools: Arc<dyn ToolPort + Send + Sync>,
    call: ToolCall,
    args: HashMap<String, serde_json::Value>,
    ctx: flux_core::ToolCtx,
) -> FlightOutput {
    let token = ctx.cancel.clone();
    let grace = async {
        token.cancelled().await;
        tokio::time::sleep(INTERRUPT_GRACE).await;
    };
    tokio::pin!(grace);
    // AssertUnwindSafe: a panicking tool must become a transcript-visible
    // error result, not a lost in-flight entry (the machine would wait
    // forever for a ToolFinished that never comes).
    let exec = AssertUnwindSafe(tools.execute(&call, args, ctx));
    let outcome = tokio::select! {
        biased;
        res = exec.catch_unwind() => {
            let raw = match res {
                Ok(result) => result,
                Err(payload) => {
                    format!("Error: tool task panicked: {}", panic_message(&payload))
                }
            };
            // The single marking site: an interrupt that landed marks the
            // result (the tool itself never mentions cancellation).
            let result = if token.is_cancelled() && !raw.starts_with(INTERRUPTED_MARK) {
                format!("{INTERRUPTED_MARK}\n{raw}")
            } else {
                raw
            };
            FlightOutput::Done { result }
        }
        _ = &mut grace => FlightOutput::ForceInterrupted,
    };
    outcome
}

/// Best-effort panic payload rendering (downcast to the common string
/// payloads; anything else is reported opaquely).
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.to_string()
    } else {
        "(non-string panic payload)".into()
    }
}
