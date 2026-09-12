//! The conversation driver — a pure pump over the two loop channels.
//!
//! The [`Loop`] owns the machine and nothing else: it consumes
//! [`LoopInput`]s from the input channel, steps the machine, and forwards
//! the produced [`LoopFact`]s onto the output channel (bounded — a slow
//! fact consumer backpressures the round instead of buffering it
//! unboundedly, the same discipline the router's bounded queue provided).
//! Round-state transitions ride the fact trace as `LoopFact::RoundState`
//! (emitted after a step's own facts — the state is the post-step truth).
//! Every collaborator (session layer, provider connection, tool executor,
//! persistence fold) is a channel peer wired by the adapter; the loop
//! holds no I/O and no trait objects.

use crate::machine::Machine;
use flux_core::{LoopFact, LoopInput};
use tokio::sync::mpsc;

/// Output channel capacity — the loop's backpressure surface. A consumer
/// slower than this stalls the round (facts stop flowing) instead of
/// buffering without bound.
pub const OUT_CAPACITY: usize = 1024;

/// The assembled loop: machine + two channels. `run` consumes the input
/// receiver and feeds the output sender until the input channel closes
/// (all peers gone) — then the loop exits and the task ends.
pub struct Loop {
    machine: Machine,
    input: mpsc::UnboundedReceiver<LoopInput>,
    output: mpsc::Sender<LoopFact>,
}

impl Loop {
    pub fn new(
        machine: Machine,
        input: mpsc::UnboundedReceiver<LoopInput>,
        output: mpsc::Sender<LoopFact>,
    ) -> Self {
        Self {
            machine,
            input,
            output,
        }
    }

    /// Run until the input channel closes. Fact order = step order;
    /// `RoundState` follows a step's own facts (post-step truth).
    pub async fn run(mut self) {
        let mut last_state = self.machine.state_kind();
        while let Some(input) = self.input.recv().await {
            let step = self.machine.step(input);
            for fact in step.facts {
                // A send failure means the fact consumer is gone (chat task
                // dead) — nothing left to serve; exit.
                if self.output.send(fact).await.is_err() {
                    return;
                }
            }
            let now = self.machine.state_kind();
            if now != last_state {
                last_state = now;
                if self.output.send(LoopFact::RoundState(now)).await.is_err() {
                    return;
                }
            }
            // (the machine never halts on its own — the loop exits only
            // when every input sender is gone)
        }
    }
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
