//! ChatHandle — the external control surface of a spawned conversation.

use crate::ResolvedPin;
use crate::round::RoundControl;
use flux_core::{ChatStateKind, LoopInput};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

/// Control handle for a spawned conversation task (the session layer's
/// `ChatTask` wraps one; ops drives the conversation through it).
#[derive(Clone)]
pub struct ChatHandle {
    /// The loop's input channel — every user action (messages, cancels,
    /// rebases) rides the same FIFO, so a cancel always lands in order
    /// with the messages around it (Model E: the kernel treats a cancel
    /// as an ordinary queue event). The provider connection and the
    /// supervised tool flights push into the same channel.
    loop_tx: mpsc::UnboundedSender<LoopInput>,
    /// The round consumer's control channel (provider swaps).
    ctrl: RoundControl,
    /// Abort handles for the two peers (loop, consumer — the consumer
    /// carries the supervised flights). Delete/shutdown tears the whole
    /// flight down.
    aborts: Vec<AbortHandle>,
    done: Arc<AtomicBool>,
    /// Shared round-state slot — written by the round consumer from the
    /// fact trace, read for authoritative subscription snapshots.
    state: Arc<StdMutex<ChatStateKind>>,
}

impl ChatHandle {
    pub(crate) fn new(
        loop_tx: mpsc::UnboundedSender<LoopInput>,
        ctrl: RoundControl,
        aborts: Vec<AbortHandle>,
        done: Arc<AtomicBool>,
        state: Arc<StdMutex<ChatStateKind>>,
    ) -> Self {
        Self {
            loop_tx,
            ctrl,
            aborts,
            done,
            state,
        }
    }

    /// Queue a user turn. Returns `false` when the engine is gone (the
    /// input channel closed) — the caller decides recovery (e.g. re-parking
    /// a flush residue). The kernel QUEUES mid-round turns itself (machine
    /// turn queue), so sends never need to wait for a round boundary:
    /// in-order sends serialize naturally.
    pub fn send_user(&self, message: String) -> bool {
        match self.loop_tx.send(LoopInput::UserMessage(message)) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "failed to send user message to chat loop");
                false
            }
        }
    }

    /// Whether the conversation task has terminated. The flag is set only
    /// when the loop returns on its own (clean halt or child-task exit);
    /// aborted tasks never land here. A stale task is replaced lazily on
    /// the next send (ensure_task).
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    /// Current round state — authoritative snapshot for subscription
    /// replies (the session layer's `current_state` folds it into
    /// `chat_state` frames).
    pub fn state(&self) -> ChatStateKind {
        *self.state.lock().expect("chat state slot lock")
    }

    /// The round state for a snapshot reply, but only while the task is
    /// alive: a terminated (done) task has no live round — its slot may be
    /// frozen at whatever it last wrote (e.g. Streaming if it died mid-
    /// stream), so report `Idle` rather than a spinner that will never
    /// resolve (the next send lazily replaces the task anyway).
    pub fn active_state(&self) -> ChatStateKind {
        if self.is_done() {
            ChatStateKind::Idle
        } else {
            self.state()
        }
    }

    /// Signal cancellation of the current round. Non-blocking — the
    /// cancel rides the input queue and the loop reads it on its next
    /// select, absorbing an in-flight tool if one is running.
    pub fn send_cancel(&self) {
        if let Err(e) = self.loop_tx.send(LoopInput::Cancel) {
            tracing::warn!(error = %e, "failed to send cancel to chat loop");
        }
    }

    /// Request an engine rebuild. The request rides the consumer's
    /// control channel: the machine's gate arms (a live round — and any
    /// turns queued behind it — finish first), and at the fired gate the
    /// consumer rebuilds the engine IN PLACE from the truth sources
    /// (provider instance, tool registry, live history above the context
    /// base). The
    /// carried `provider` replaces the chat's provider instance for this
    /// and every later rebuild (the hot-swap path); `None` keeps the
    /// current one. Non-blocking; false = the consumer is gone (stale
    /// task; the caller's lazy replacement covers it).
    pub fn rebuild(&self, provider: Option<ResolvedPin>) -> bool {
        self.ctrl.rebuild(provider)
    }

    /// Abort every peer task (loop, consumer — the flights drop with the
    /// consumer). The abort is
    /// immediate — queued inputs are never read, so a cancel signal here
    /// would be dead weight; the cleanup that matters still runs as the
    /// task futures drop (StreamHandle → connection token, flight
    /// process groups, the flights' JoinSet).
    pub fn shutdown(&self) {
        for abort in &self.aborts {
            abort.abort();
        }
    }
}
