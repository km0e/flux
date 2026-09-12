//! Per-chat router — fans out WireEvents to the chat's viewers.
//!
//! One router task per chat, spawned at chat creation and alive for the
//! chat's lifetime. The task's `OutputPort` adapter (`ChannelOutput`)
//! forwards every event into the router's control channel; the router
//! serializes once and broadcasts to each viewer's session sink (single
//! drop surface — the transport's bounded content queue). A slow viewer's
//! sink reports `send_raw == false`, the router marks it dropped and sends
//! an `error{stream_gap}` notice on the sink's control channel instead of
//! stalling anyone (spec §4.2). The model's `question` prompts also go via
//! `send_control` straight to the lease holder so an answer request can
//! never be starved by a full content queue; it parks until answered and
//! is re-delivered to each new lease holder.

use crate::identity::SessionRef;
use crate::manager::ManagerState;
use async_trait::async_trait;
use flux_core::ErrorCode;
use flux_core::{OutputPort, WireEvent};
use flux_proto::flux::v1::subscribe_response::Kind;
use flux_proto::flux::v1::{
    ContextRebased, ErrorEvent, ProviderSwitched, QuestionPrompt, QuestionRequired, ReasoningDelta,
    StreamCancelled, StreamEnd, SubscribeResponse, TextDelta, ToolCallPreview, ToolResult,
    ToolStart, Usage,
};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::warn;

/// Router control channel depth — the task's output backpressure surface.
const CONTROL_QUEUE: usize = 1024;

/// P1 delta batching: flush accumulated stream deltas within this interval so
/// a slow/trickling provider still shows text promptly (bounds first-token
/// latency added by aggregation).
const BATCH_FLUSH: std::time::Duration = std::time::Duration::from_millis(25);
/// P1 delta batching: flush once this many deltas accumulate even if the
/// interval hasn't elapsed (frame-count bound under heavy input; also guards
/// against the select randomly starving the timer under a saturated channel).
const BATCH_MAX: usize = 32;
/// P1 delta batching: char-size bound per batched frame. BATCH_MAX counts
/// frames — an upstream that sends large SSE chunks would otherwise turn a
/// single WS frame into a multi-KB render spike on the client (the frontend
/// folds each frame into the DOM).
const BATCH_MAX_CHARS: usize = 2048;

/// A chat's router handle: the control-channel sender stored in the chat
/// entry, cloned into the task's output port. The channel is sealed — the
/// only way to enqueue is through the typed methods below.
#[derive(Clone)]
pub(crate) struct RouterHandle {
    chat_id: String,
    control_tx: mpsc::Sender<RouterControl>,
}

/// Router control messages. `Event` comes from the chat task's output port
/// (`ChannelOutput`); `Claimed`/`ViewerGone` from ops, `QuestionAnswered`
/// from the question response, `Shutdown` from delete. Module-private: only
/// this file constructs or matches it.
enum RouterControl {
    /// A WireEvent produced by the chat's task (via [`ChannelOutput`]).
    Event(WireEvent),
    /// A session claimed the chat's lease — re-deliver a parked question.
    Claimed(SessionRef),
    /// The round's pending question was answered — clear the parked copy.
    QuestionAnswered,
    /// A viewer unsubscribed (or its session died) — forget its dropped
    /// mark, so a fresh subscription starts clean (a resubscription IS the
    /// resync).
    ViewerGone(u64),
    /// The chat is being deleted — the router exits.
    Shutdown,
}

impl RouterHandle {
    /// Spawn the router task for a chat and return its handle.
    pub(crate) fn spawn(chat_id: String, manager: Arc<ManagerState>) -> Self {
        let (control_tx, control_rx) = mpsc::channel(CONTROL_QUEUE);
        let task_chat_id = chat_id.clone();
        tokio::spawn(async move {
            run_router(Router {
                chat_id: task_chat_id,
                manager,
                rx: control_rx,
                pending_question: None,
                dropped: HashSet::new(),
                pending_text: None,
                pending_reasoning: None,
                batch_started: None,
            })
            .await;
        });
        Self {
            chat_id,
            control_tx,
        }
    }

    /// Forward a WireEvent from the loop's round consumer or the
    /// adapter-side tooling. Awaits the bounded control channel — a full
    /// router stalls the sender instead of dropping stream content (the
    /// same backpressure discipline as ever).
    pub(crate) async fn event(&self, event: WireEvent) -> bool {
        self.control_tx
            .send(RouterControl::Event(event))
            .await
            .is_ok()
    }

    /// Forward a WireEvent from the OPS layer (outside the kernel task —
    /// e.g. the direct-apply provider-switch path). Non-blocking: a full
    /// queue drops the event (the chat list carries the truth; the
    /// kernel-apply path never goes through here).
    pub(crate) fn try_wire(&self, event: WireEvent) -> bool {
        self.control_tx
            .try_send(RouterControl::Event(event))
            .inspect_err(|_| {
                warn!(chat = %self.chat_id, "router control queue full: ops wire event dropped")
            })
            .is_ok()
    }

    /// Tell the router a session claimed the lease — re-deliver any parked
    /// question. Non-blocking.
    pub(crate) fn claim(&self, session: SessionRef) -> bool {
        self.control_tx
            .try_send(RouterControl::Claimed(session))
            .inspect_err(
                |_| warn!(chat = %self.chat_id, "router control queue full: claim control dropped"),
            )
            .is_ok()
    }

    /// The round's pending question was answered — clear the parked copy.
    /// Non-blocking.
    pub(crate) fn question_answered(&self) -> bool {
        self.control_tx
            .try_send(RouterControl::QuestionAnswered)
            .inspect_err(|_| {
                warn!(
                    chat = %self.chat_id,
                    "router control queue full: question_answered dropped — parked question may re-appear"
                )
            })
            .is_ok()
    }

    /// A viewer unsubscribed (or its session died) — forget its dropped
    /// mark, so a fresh subscription starts clean (a resubscription IS the
    /// resync). Non-blocking.
    pub(crate) fn viewer_gone(&self, sn: u64) -> bool {
        self.control_tx
            .try_send(RouterControl::ViewerGone(sn))
            .inspect_err(|_| {
                warn!(
                    chat = %self.chat_id,
                    "router control queue full: viewer_gone dropped — drop mark may persist"
                )
            })
            .is_ok()
    }

    /// The chat is being deleted — the router exits. Non-blocking.
    pub(crate) fn shutdown(&self) -> bool {
        self.control_tx
            .try_send(RouterControl::Shutdown)
            .inspect_err(
                |_| warn!(chat = %self.chat_id, "router control queue full: shutdown dropped"),
            )
            .is_ok()
    }
}

/// The router task's state.
pub(crate) struct Router {
    chat_id: String,
    manager: Arc<ManagerState>,
    rx: mpsc::Receiver<RouterControl>,
    /// Serialized question awaiting an answer; re-delivered to each new
    /// lease holder until answered or the round dies.
    pending_question: Option<SubscribeResponse>,
    /// Viewers currently dropping frames (queue full): non-boundary events
    /// are skipped silently until the next boundary event clears them.
    dropped: HashSet<u64>,
    /// Accumulated consecutive `TextDelta`s awaiting a batched flush (see
    /// P1 delta batching: fewer wire frames, same semantics). `(text, count)`.
    pending_text: Option<(String, usize)>,
    /// Accumulated consecutive `ReasoningDelta`s awaiting a batched flush.
    pending_reasoning: Option<(String, usize)>,
    /// When the pending batch started — drives the latency-bound flush timer.
    batch_started: Option<tokio::time::Instant>,
}

pub(crate) async fn run_router(mut router: Router) {
    loop {
        // Latency-bound flush: if a batch is pending, arm a timer that fires
        // `BATCH_FLUSH` after it started; otherwise a no-op (never-ready) timer.
        let flush = match router.batch_started {
            Some(started) => tokio::time::sleep_until(started + BATCH_FLUSH),
            None => tokio::time::sleep(std::time::Duration::MAX),
        };
        tokio::pin!(flush);

        tokio::select! {
            control = router.rx.recv() => {
                match control {
                    Some(RouterControl::Event(event)) => router.on_event(event).await,
                    Some(RouterControl::Claimed(session)) => router.on_claimed(&session).await,
                    Some(RouterControl::QuestionAnswered) => router.pending_question = None,
                    Some(RouterControl::ViewerGone(sn)) => {
                        router.dropped.remove(&sn);
                    }
                    Some(RouterControl::Shutdown) => {
                        // Flush trailing text so nothing accumulated is lost on delete.
                        router.flush_pending().await;
                        break;
                    }
                    None => break, // channel closed — task ends
                }
            }
            _ = &mut flush => {
                // Interval elapsed with a pending batch — send it now.
                router.flush_pending().await;
            }
        }
    }
}

impl Router {
    async fn on_event(&mut self, event: WireEvent) {
        // P1 delta batching: consecutive text/reasoning deltas merge into one
        // frame; a kind switch, a boundary event, BATCH_MAX accumulation, or
        // the interval timer flushes the accumulated batch.
        match event {
            WireEvent::TextDelta(delta) => {
                self.flush_reasoning().await; // kind switch: reasoning → text
                if self.pending_text.is_none() {
                    self.batch_started = Some(tokio::time::Instant::now());
                }
                let slot = self.pending_text.get_or_insert_with(|| (String::new(), 0));
                slot.0.push_str(&delta);
                slot.1 += 1;
                if slot.1 >= BATCH_MAX || slot.0.chars().count() >= BATCH_MAX_CHARS {
                    self.flush_pending().await;
                }
            }
            WireEvent::ReasoningDelta(delta) => {
                self.flush_text().await; // kind switch: text → reasoning
                if self.pending_reasoning.is_none() {
                    self.batch_started = Some(tokio::time::Instant::now());
                }
                let slot = self
                    .pending_reasoning
                    .get_or_insert_with(|| (String::new(), 0));
                slot.0.push_str(&delta);
                slot.1 += 1;
                if slot.1 >= BATCH_MAX || slot.0.chars().count() >= BATCH_MAX_CHARS {
                    self.flush_pending().await;
                }
            }
            // Tool-call preview: flushed pending deltas first (the preview
            // must land AFTER the prose that preceded it), then fanned out
            // per element — never batched (its ordering against the
            // ToolStart it precedes must hold), and no activity touch: a
            // preview can fire per SSE fragment (delta-frequency), and the
            // ToolStart it announces touches anyway.
            preview @ WireEvent::ToolCallPreview { .. } => {
                self.flush_pending().await;
                self.fanout_kind(kind_of(&preview), false).await;
            }
            other => {
                // Non-delta events (boundary / tool / usage / prompt) flush the
                // accumulated text FIRST so the frontend sees stream content
                // before the terminal/tool event.
                self.flush_pending().await;
                self.dispatch_event(other).await;
            }
        }
    }

    /// Serialize + fan out one non-delta event. Runs after pending batch flush.
    async fn dispatch_event(&mut self, event: WireEvent) {
        // Activity stamp first: every non-delta event implies round progress
        // (tool flights, usage, boundaries, notices). The in-memory cache's
        // recency key is hydrated once at startup and the store keeps its own
        // stamp on message appends — without this touch every `chats`
        // broadcast would carry a stale creation-time value until restart.
        // Deltas are excluded (they can be very frequent).
        self.touch_activity().await;
        // NOTE: the cache does NOT update provider labels here. Every
        // ProviderSwitched emitter (ops direct-apply, the round consumer's
        // apply point) has already synced the cache — labels AND the resolved
        // instance — before emitting; the event is a landing NOTICE, not the
        // update.
        let is_boundary = matches!(
            event,
            WireEvent::StreamEnd { .. } | WireEvent::Cancelled | WireEvent::StreamError { .. }
        );
        // A round boundary ends any parked question: the `question` tool is
        // round-blocking, so no legitimate pending question can outlive its
        // round (a cancelled/crashed round leaves a dead flight behind). Clear
        // the parked copy — it would otherwise be re-delivered to every new
        // claimant as a phantom card whose answer lands on a dead oneshot and
        // is dropped silently.
        if is_boundary {
            self.pending_question = None;
        }
        if matches!(event, WireEvent::QuestionRequired { .. }) {
            self.deliver_question(event).await;
        } else {
            self.fanout_kind(kind_of(&event), is_boundary).await;
        }
    }

    /// Flush pending text + reasoning batches, reset the batch timer.
    async fn flush_pending(&mut self) {
        self.flush_text().await;
        self.flush_reasoning().await;
        self.batch_started = None;
    }

    /// Flush the pending text batch, if any.
    async fn flush_text(&mut self) {
        if let Some((text, _)) = self.pending_text.take() {
            self.fanout_kind(Kind::TextDelta(TextDelta { delta: text }), false)
                .await;
        }
    }

    /// Flush the pending reasoning batch, if any.
    async fn flush_reasoning(&mut self) {
        if let Some((reasoning, _)) = self.pending_reasoning.take() {
            self.fanout_kind(
                Kind::ReasoningDelta(ReasoningDelta { delta: reasoning }),
                false,
            )
            .await;
        }
    }

    /// Refresh the chat's in-memory `last_activity_at` (the wire recency
    /// key). Called on every non-delta wire event — a few write-lock
    /// acquisitions per round, never per delta.
    async fn touch_activity(&mut self) {
        let mut chats = self.manager.chats.write().await;
        if let Some(chat) = chats.get_mut(&self.chat_id) {
            chat.last_activity_at = utc_now_stamp();
        }
    }

    /// A question goes straight to the lease holder on the control
    /// channel, so a slow holder cannot starve its own answer request.
    /// With no holder the question is parked and re-delivered on the next
    /// claim. The holder's identity carries its sink — no second lookup.
    async fn deliver_question(&mut self, event: WireEvent) {
        let (holder, el) = {
            let chats = self.manager.chats.read().await;
            let Some(chat) = chats.get(&self.chat_id) else {
                return;
            };
            // The question is a per-chat element — it takes the chat's next
            // sequence like any fanout. Its REDelivery (on_claimed) carries
            // the original stamp; the client's snapshot reconciliation is
            // type-scoped and never drops questions.
            let mut el = element(&self.chat_id, kind_of(&event));
            el.chat_seq = chat.next_seq(true);
            (chat.lease.clone(), el)
        };
        if let Some(holder) = holder {
            holder.send_control(el.clone());
        }
        self.pending_question = Some(el);
    }

    async fn on_claimed(&mut self, session: &SessionRef) {
        if let Some(el) = self.pending_question.clone() {
            session.send_control(el);
        }
    }

    /// Fan out one element to every viewer's sink (single drop surface —
    /// the sink's content queue). The envelope is built ONCE and stamped
    /// with the chat's next sequence (R2 — fetch_add's old value: every
    /// viewer sees the same number for the same element, and the claim/open
    /// snapshots, taken without consuming, slot clients into the same
    /// ordering). Boundary events go out as content and clear the viewer's
    /// dropped mark; other events whose send fails mark the viewer dropped
    /// (elements skipped silently until the mark clears) and send a gap
    /// notice via the sink's control channel — delivered at the first
    /// available network slot. The only await is the chats read-lock
    /// acquisition; guard-held sections never await anything else — every
    /// send is the non-blocking `send`/`send_control`.
    async fn fanout_kind(&mut self, kind: Kind, is_boundary: bool) {
        let chats = self.manager.chats.read().await;
        let Some(chat) = chats.get(&self.chat_id) else {
            return;
        };
        let mut el = element(&self.chat_id, kind);
        el.chat_seq = chat.next_seq(true);
        for (sn, viewer) in &chat.viewers {
            if is_boundary {
                viewer.send(el.clone());
                self.dropped.remove(sn);
                continue;
            }
            if self.dropped.contains(sn) {
                continue; // mid-gap: frames are dropped silently
            }
            if !viewer.send(el.clone()) {
                self.dropped.insert(*sn);
                viewer.send_control(gap_element(&self.chat_id));
            }
        }
    }
}

/// Current UTC time in the store's stamp format (`%Y-%m-%dT%H:%M:%SZ`),
/// so the cache and the DB carry comparable recency values. No time-crate
/// dependency: civil-from-days (Howard Hinnant's algorithm) over the Unix
/// epoch.
fn utc_now_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    utc_stamp(secs)
}

/// Format Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
fn utc_stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Map one WireEvent onto the stream element's oneof payload. Total over
/// the kernel vocabulary — there is no fallible serialization step anymore
/// (the old serde path could skip events on serialization failure).
/// ToolResult's `name` is dropped: the wire element pairs results to cards
/// by call id alone (unchanged from the JSON protocol).
fn kind_of(event: &WireEvent) -> Kind {
    match event {
        WireEvent::TextDelta(delta) => Kind::TextDelta(TextDelta {
            delta: delta.clone(),
        }),
        WireEvent::ReasoningDelta(delta) => Kind::ReasoningDelta(ReasoningDelta {
            delta: delta.clone(),
        }),
        WireEvent::Usage {
            prompt_tokens,
            completion_tokens,
            cached_tokens,
        } => Kind::Usage(Usage {
            prompt_tokens: *prompt_tokens,
            completion_tokens: *completion_tokens,
            cached_tokens: *cached_tokens,
        }),
        WireEvent::ToolStart {
            id,
            name,
            arguments,
        } => Kind::ToolStart(ToolStart {
            id: id.clone(),
            name: name.clone(),
            arguments: arguments.clone(),
        }),
        WireEvent::ToolCallPreview {
            id,
            name,
            args_delta,
        } => Kind::ToolCallPreview(ToolCallPreview {
            id: id.clone(),
            name: name.clone(),
            arguments_delta: args_delta.clone(),
        }),
        WireEvent::ToolResult { id, result, .. } => Kind::ToolResult(ToolResult {
            id: id.clone(),
            result: result.clone(),
        }),
        WireEvent::QuestionRequired { id, text, options } => {
            Kind::QuestionRequired(QuestionRequired {
                id: id.clone(),
                question: Some(QuestionPrompt {
                    text: text.clone(),
                    options: options.clone().unwrap_or_default(),
                }),
            })
        }
        WireEvent::StreamError { message, code } => Kind::Error(ErrorEvent {
            code: flux_proto::flux::v1::ErrorCode::from(
                code.clone().unwrap_or(ErrorCode::StreamCrashed),
            ) as i32,
            message: message.clone(),
        }),
        WireEvent::Cancelled => Kind::StreamCancelled(StreamCancelled {}),
        WireEvent::StreamEnd { finish_reason } => Kind::StreamEnd(StreamEnd {
            finish_reason: finish_reason.clone(),
        }),
        WireEvent::ContextRebased { base_message_id } => Kind::ContextRebased(ContextRebased {
            base_message_id: *base_message_id,
        }),
        WireEvent::ProviderSwitched { provider, model } => {
            Kind::ProviderSwitched(ProviderSwitched {
                provider: provider.clone(),
                model: model.clone(),
            })
        }
    }
}

/// Wrap a oneof payload into the stream element envelope. `chat_seq` is
/// set by the caller — the fanout and snapshot paths own the sequencing.
pub(crate) fn element(chat_id: &str, kind: Kind) -> SubscribeResponse {
    SubscribeResponse {
        chat_seq: 0,
        chat_id: chat_id.to_owned(),
        kind: Some(kind),
    }
}

/// The slow-viewer gap notice (control channel, never stamped — the
/// client's snapshot reconciliation is type-scoped and never drops
/// errors).
fn gap_element(chat_id: &str) -> SubscribeResponse {
    element(
        chat_id,
        Kind::Error(ErrorEvent {
            code: flux_proto::flux::v1::ErrorCode::StreamGap as i32,
            message: "Stream paused: viewer fell behind — reload to resync".into(),
        }),
    )
}

/// The task's OutputPort adapter: forwards WireEvents into the chat's
/// router. `send().await` on the bounded control channel provides the same
/// backpressure the old direct-to-WS path had — a full router stalls the
/// machine loop instead of dropping stream content.
pub(crate) struct ChannelOutput {
    pub(crate) router: RouterHandle,
}

#[async_trait]
impl OutputPort for ChannelOutput {
    async fn emit(&self, event: WireEvent) {
        if !self.router.event(event).await {
            tracing::debug!("router gone (chat deleted?); dropping event");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::ServerState;
    use crate::ops::SubscribeOutcome;
    use crate::test_util::{
        DummyProvider, FailingSink, create_chat, find_kind, kinds, register, sess, test_state,
        wait_for, wait_for_kind,
    };
    use flux_core::WireEvent;
    use flux_proto::flux::v1::subscribe_response::Kind;
    use flux_store::Store;
    use std::sync::{Arc, Mutex as StdMutex};

    type Rec = Arc<StdMutex<Vec<SubscribeResponse>>>;

    /// Replace the viewer's sink with a failing one — `send` always
    /// reports a full content queue (models a stalled network) while
    /// `send_control` records. The router must mark the viewer, skip
    /// content, and still get gap notices through. Returns the
    /// control-channel record.
    async fn stall_viewer(state: &ServerState, _chat_id: &str, sid: &str) -> Rec {
        let (sink, recorded) = FailingSink::new();
        // Swap the viewer identity's connection sink for a failing one —
        // the fanout path reads the sink the identity carries.
        let stalled_session = sess(state, sid).await;
        stalled_session.swap_sink(sink);
        recorded
    }

    #[tokio::test]
    async fn utc_stamp_matches_the_store_format() {
        assert_eq!(utc_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_stamp(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[tokio::test]
    async fn fanout_delivers_events_to_all_viewers() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let (chat_id, a, router) = create_chat(&state, "a", Arc::new(DummyProvider)).await;
        let b = register(&state, "b").await;
        assert_eq!(
            state
                .subscribe_chat(&sess(&state, "b").await, &chat_id)
                .await,
            SubscribeOutcome::Subscribed
        );
        let _ = router
            .control_tx
            .try_send(RouterControl::Event(WireEvent::TextDelta("hi".to_string())));
        for rec in [&a, &b] {
            wait_for_kind(rec, "text_delta").await;
            let guard = rec.lock().unwrap();
            let el = find_kind(&guard, "text_delta").unwrap();
            assert_eq!(el.chat_id, chat_id);
            match &el.kind {
                Some(Kind::TextDelta(t)) => assert_eq!(t.delta, "hi"),
                other => panic!("expected text_delta, got {other:?}"),
            }
        }
    }

    /// R2: the fanout stamps every element with the chat's next sequence —
    /// all viewers see the same number for the same element, counting up
    /// from 0 (fetch_add's old value).
    #[tokio::test]
    async fn fanout_stamps_monotonic_seq() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let (_chat_id, a, router) = create_chat(&state, "a", Arc::new(DummyProvider)).await;
        // ToolStarts (non-delta events are never batched — each fanned out
        // individually).
        for i in 0..3 {
            let _ = router
                .control_tx
                .try_send(RouterControl::Event(WireEvent::ToolStart {
                    id: format!("c{i}"),
                    name: "t".into(),
                    arguments: "{}".into(),
                }));
        }
        wait_for(|| kinds(&a.lock().unwrap()).len() >= 3).await;
        let rec = a.lock().unwrap();
        let seqs: Vec<u64> = rec
            .iter()
            .filter(|el| matches!(&el.kind, Some(Kind::ToolStart(_))))
            .map(|el| el.chat_seq)
            .collect();
        assert_eq!(seqs, vec![0, 1, 2]);
    }

    /// Tool-call previews fan out one element each, AFTER any pending text
    /// batch (kind-switch flush), with no activity touch. Ordering against
    /// the ToolStart they precede must hold per element.
    #[tokio::test]
    async fn tool_call_preview_flushes_text_then_fans_out_individually() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let (_chat_id, a, router) = create_chat(&state, "a", Arc::new(DummyProvider)).await;
        // Two text deltas accumulate into a pending batch...
        let _ = router
            .control_tx
            .try_send(RouterControl::Event(WireEvent::TextDelta("he".into())));
        let _ = router
            .control_tx
            .try_send(RouterControl::Event(WireEvent::TextDelta("llo".into())));
        // ...and two previews must land as two separate elements, after the
        // flushed text batch.
        for i in 0..2 {
            let _ = router
                .control_tx
                .try_send(RouterControl::Event(WireEvent::ToolCallPreview {
                    id: format!("c{i}"),
                    name: Some("bash".into()),
                    args_delta: None,
                }));
        }
        wait_for(|| {
            kinds(&a.lock().unwrap())
                .iter()
                .filter(|k| k == &&"tool_call_preview")
                .count()
                >= 2
        })
        .await;
        let rec = a.lock().unwrap();
        let kinds_seq: Vec<&str> = rec
            .iter()
            .filter_map(|el| match &el.kind {
                Some(Kind::TextDelta(_)) => Some("text_delta"),
                Some(Kind::ToolCallPreview(_)) => Some("tool_call_preview"),
                // Session-level broadcasts (chats/ready/…) ride the same
                // sink — not part of the per-chat ordering under test.
                _ => None,
            })
            .collect();
        // The pending text batch flushed BEFORE the first preview.
        assert_eq!(
            kinds_seq,
            vec!["text_delta", "tool_call_preview", "tool_call_preview"]
        );
        // Never batched: each preview is its own element with its own id.
        let ids: Vec<String> = rec
            .iter()
            .filter_map(|el| match &el.kind {
                Some(Kind::ToolCallPreview(p)) => Some(p.id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["c0".to_string(), "c1".to_string()]);
    }

    #[tokio::test]
    async fn slow_viewer_drops_frames_and_gets_gap_notice_until_boundary() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let (chat_id, _a, router) = create_chat(&state, "a", Arc::new(DummyProvider)).await;
        let stalled = stall_viewer(&state, &chat_id, "a").await;
        for i in 0..80 {
            let _ = router
                .control_tx
                .try_send(RouterControl::Event(WireEvent::TextDelta(format!("{i}"))));
        }
        // The slow reader gets a gap notice (control channel, bypassing the full content queue); incremental frames are silently dropped.
        wait_for_kind(&stalled, "error").await;
        let gap = {
            let guard = stalled.lock().unwrap();
            find_kind(&guard, "error").unwrap().clone()
        };
        match &gap.kind {
            Some(Kind::Error(e)) => {
                assert_eq!(e.code, flux_proto::flux::v1::ErrorCode::StreamGap as i32);
                assert_eq!(gap.chat_id, chat_id);
            }
            other => panic!("expected error element, got {other:?}"),
        }
        assert!(
            !kinds(&stalled.lock().unwrap()).contains(&"text_delta"),
            "dropped frames must not reach the stalled viewer"
        );
        // Boundary events clear the dropped-frame mark (content path; on a
        // full sink the frame itself is dropped, but the mark MUST be
        // cleared) — dropping again after the boundary re-marks and re-gaps.
        let _ = router
            .control_tx
            .try_send(RouterControl::Event(WireEvent::StreamEnd {
                finish_reason: None,
            }));
        let _ = router
            .control_tx
            .try_send(RouterControl::Event(WireEvent::TextDelta("x".to_string())));
        wait_for(|| {
            stalled
                .lock()
                .unwrap()
                .iter()
                .filter(|el| {
                    matches!(
                        &el.kind,
                        Some(Kind::Error(e)) if e.code == flux_proto::flux::v1::ErrorCode::StreamGap as i32
                    )
                })
                .count()
                >= 2
        })
        .await;
    }

    #[tokio::test]
    async fn dropped_viewer_recovers_on_resubscribe() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let (chat_id, a, router) = create_chat(&state, "a", Arc::new(DummyProvider)).await;
        let real_sink = sess(&state, "a").await.sink().unwrap();
        let stalled = stall_viewer(&state, &chat_id, "a").await;
        // 64 = two full BATCH_MAX(32) batches so no pending residue remains
        // (batches stay whole; otherwise a tail fragment would merge with the
        // following frames and break the single-delta assertions below).
        for i in 0..64 {
            let _ = router
                .control_tx
                .try_send(RouterControl::Event(WireEvent::TextDelta(format!("{i}"))));
        }
        // The slow reader drops frames and receives a gap notice.
        wait_for_kind(&stalled, "error").await;
        // Unsubscribe then resubscribe: the dropped-frame mark must clear with
        // them (a resubscription IS the resync). The real connection sink is
        // restored with the resubscription (a resumed/recovering viewer).
        state
            .unsubscribe_chat(&sess(&state, "a").await, &chat_id)
            .await;
        sess(&state, "a").await.swap_sink(real_sink);
        assert_eq!(
            state
                .subscribe_chat(&sess(&state, "a").await, &chat_id)
                .await,
            SubscribeOutcome::Subscribed
        );
        // No boundary event needed — the next ordinary frame arrives directly (real sink + cleared mark).
        let _ = router
            .control_tx
            .try_send(RouterControl::Event(WireEvent::TextDelta(
                "fresh".to_string(),
            )));
        wait_for(|| {
            a.lock()
                .unwrap()
                .iter()
                .any(|el| matches!(&el.kind, Some(Kind::TextDelta(t)) if t.delta == "fresh"))
        })
        .await;
    }

    #[tokio::test]
    async fn question_required_bypasses_full_queue_to_lease_holder() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let (chat_id, _a, router) = create_chat(&state, "a", Arc::new(DummyProvider)).await;
        // The holder's queue is clogged too — the question must still get through (an answer request never starves behind a slow reader).
        // The question rides the holder identity's control channel, which the
        // swap redirected to the FailingSink's record.
        let stalled = stall_viewer(&state, &chat_id, "a").await;
        let _ = router
            .control_tx
            .try_send(RouterControl::Event(WireEvent::QuestionRequired {
                id: "q1".into(),
                text: "pick one".into(),
                options: Some(vec!["a".into(), "b".into()]),
            }));
        wait_for_kind(&stalled, "question_required").await;
        let el = {
            let guard = stalled.lock().unwrap();
            find_kind(&guard, "question_required").unwrap().clone()
        };
        match &el.kind {
            Some(Kind::QuestionRequired(q)) => {
                assert_eq!(q.id, "q1");
                let q = q.question.as_ref().unwrap();
                assert_eq!(q.text, "pick one");
                assert_eq!(q.options, vec!["a".to_string(), "b".to_string()]);
            }
            other => panic!("expected question_required, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn question_parked_and_redelivered_to_new_claimant() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let (chat_id, a, router) = create_chat(&state, "a", Arc::new(DummyProvider)).await;
        // No holder: the question parks, delivered to nobody.
        state.release_chat(&sess(&state, "a").await, &chat_id).await;
        let _ = router
            .control_tx
            .try_send(RouterControl::Event(WireEvent::QuestionRequired {
                id: "q1".into(),
                text: "pick one".into(),
                options: None,
            }));
        // b claims → redelivery. Events keep their order on the same channel; by the time b receives it, a's verdict has long settled.
        let b = register(&state, "b").await;
        assert_eq!(
            state.claim_chat(&sess(&state, "b").await, &chat_id).await,
            crate::ops::ClaimOutcome::Granted
        );
        wait_for_kind(&b, "question_required").await;
        assert!(
            kinds(&a.lock().unwrap())
                .iter()
                .all(|t| *t != "question_required"),
            "parked question must not reach the old holder"
        );
        // Answering clears the parked copy: c's claim does not redeliver. No
        // timed sleep — send a follow-up frame on the same control channel
        // that c can observe as the sync point: the control channel is FIFO,
        // so that TextDelta sits behind QuestionAnswered and claim(c); its
        // arrival at c proves the parked-clear and claim were processed in
        // order before it.
        let _ = router.control_tx.try_send(RouterControl::QuestionAnswered);
        state.release_chat(&sess(&state, "b").await, &chat_id).await;
        let c = register(&state, "c").await;
        assert_eq!(
            state.claim_chat(&sess(&state, "c").await, &chat_id).await,
            crate::ops::ClaimOutcome::Granted
        );
        let _ = router
            .control_tx
            .try_send(RouterControl::Event(WireEvent::TextDelta("sync".into())));
        wait_for_kind(&c, "text_delta").await;
        assert!(
            kinds(&c.lock().unwrap())
                .iter()
                .all(|t| *t != "question_required"),
            "answered question must not be re-delivered"
        );
    }

    #[tokio::test]
    async fn batches_consecutive_text_deltas_into_single_frame() {
        // P1: consecutive TextDeltas merge into ONE wire element whose
        // `delta` is the concatenation — fewer frames, same semantics.
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let (_chat_id, a, router) = create_chat(&state, "a", Arc::new(DummyProvider)).await;
        for s in ["hello ", "world", "!"] {
            let _ = router
                .control_tx
                .try_send(RouterControl::Event(WireEvent::TextDelta(s.to_string())));
        }
        wait_for_kind(&a, "text_delta").await;
        let texts: Vec<String> = a
            .lock()
            .unwrap()
            .iter()
            .filter_map(|el| match &el.kind {
                Some(Kind::TextDelta(t)) => Some(t.delta.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["hello world!".to_string()]);
    }

    #[tokio::test]
    async fn char_cap_flushes_a_batch_before_the_frame_count_cap() {
        // BATCH_MAX counts frames — a batch of large deltas must flush on
        // the char bound instead of piling ~6KB into one frame (the
        // client folds each element into the DOM; a huge element is a
        // render spike).
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let (_chat_id, a, router) = create_chat(&state, "a", Arc::new(DummyProvider)).await;
        let delta = "x".repeat(200); // 11 deltas = 2200 chars > BATCH_MAX_CHARS
        for _ in 0..11 {
            let _ = router
                .control_tx
                .try_send(RouterControl::Event(WireEvent::TextDelta(delta.clone())));
        }
        wait_for_kind(&a, "text_delta").await;
        // 11 × 200 = 2200 chars; the first flush fires at ≥2048 — the final
        // ~152-char remainder stays pending until the next boundary/frame.
        let frames = a
            .lock()
            .unwrap()
            .iter()
            .filter(|el| matches!(&el.kind, Some(Kind::TextDelta(_))))
            .count();
        assert!(frames >= 1, "char cap must flush mid-batch");
        let delivered: usize = a
            .lock()
            .unwrap()
            .iter()
            .filter_map(|el| match &el.kind {
                Some(Kind::TextDelta(t)) => Some(t.delta.chars().count()),
                _ => None,
            })
            .sum();
        assert!(
            (2000..=2200).contains(&delivered),
            "flushed chars must be the cap-bounded prefix, got {delivered}"
        );
    }

    #[tokio::test]
    async fn reasoning_kind_switch_flushes_pending_text() {
        // P1: text → reasoning transition flushes the accumulated text so
        // it is not held while thinking begins.
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let (_chat_id, a, router) = create_chat(&state, "a", Arc::new(DummyProvider)).await;
        let _ = router
            .control_tx
            .try_send(RouterControl::Event(WireEvent::TextDelta(
                "answer ".to_string(),
            )));
        let _ = router
            .control_tx
            .try_send(RouterControl::Event(WireEvent::TextDelta(
                "part".to_string(),
            )));
        let _ = router
            .control_tx
            .try_send(RouterControl::Event(WireEvent::ReasoningDelta(
                "think".to_string(),
            )));
        wait_for(|| {
            let ks = kinds(&a.lock().unwrap());
            ks.contains(&"text_delta") && ks.contains(&"reasoning_delta")
        })
        .await;
        let rec = a.lock().unwrap();
        let text = rec
            .iter()
            .find_map(|el| match &el.kind {
                Some(Kind::TextDelta(t)) => Some(t.delta.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(text, "answer part");
        let reason = rec
            .iter()
            .find_map(|el| match &el.kind {
                Some(Kind::ReasoningDelta(t)) => Some(t.delta.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(reason, "think");
    }

    #[tokio::test]
    async fn boundary_flushes_pending_text_before_stream_end() {
        // P1: a boundary (StreamEnd) must flush accumulated text BEFORE the
        // terminal event, preserving content-then-end ordering.
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let (_chat_id, a, router) = create_chat(&state, "a", Arc::new(DummyProvider)).await;
        let _ = router
            .control_tx
            .try_send(RouterControl::Event(WireEvent::TextDelta(
                "final words".to_string(),
            )));
        let _ = router
            .control_tx
            .try_send(RouterControl::Event(WireEvent::StreamEnd {
                finish_reason: None,
            }));
        wait_for_kind(&a, "stream_end").await;
        let rec = a.lock().unwrap();
        let idx_text = rec
            .iter()
            .position(|el| matches!(&el.kind, Some(Kind::TextDelta(_))))
            .expect("text must be flushed");
        let idx_end = rec
            .iter()
            .position(|el| matches!(&el.kind, Some(Kind::StreamEnd(_))))
            .expect("stream_end must arrive");
        assert!(idx_text < idx_end, "text must precede stream_end");
        match &rec[idx_text].kind {
            Some(Kind::TextDelta(t)) => assert_eq!(t.delta, "final words"),
            other => panic!("expected text_delta, got {other:?}"),
        }
    }
}
