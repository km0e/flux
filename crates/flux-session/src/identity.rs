//! Session identity and outbound sink — the transport side of the
//! ChatManager. A [`Session`] is one CLIENT IDENTITY: it outlives
//! connections (a page refresh adopts the same identity via its token) and
//! carries the current connection's outbound sink inside itself, so every
//! lease/viewer/routing structure holds the identity handle directly —
//! no string ids, no second lookup at fanout time.
//!
//! Two identifiers with strictly separated roles:
//! - `token` (UUID): the client-facing resume credential. The ONLY string
//!   surface — minted here, carried by the Subscribe stream's `ready`
//!   frame and the token→identity lookup. Never handed to
//!   lease/viewer/routing logic.
//! - `sn` (monotonic serial): the server-internal routing key. Map keys,
//!   router drop marks. Never on the wire.

use flux_proto::flux::v1::SubscribeResponse;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The outbound half of a session, as seen by the manager: an opaque,
/// non-blocking typed-element sink. Both methods must never await the
/// network — implementations use try_send semantics and drop on a full
/// queue. The elements are the proto wire vocabulary
/// ([`SubscribeResponse`]) — the event plane IS the wire, and the fanout
/// hands it pre-typed stream elements.
///
/// Two channels per connection (see the stream sink in flux-server's
/// grpc::events):
/// - `send` carries stream content; `false` = dropped (queue full).
///   The router marks the viewer dropped and notifies it via `send_control`.
/// - `send_control` carries must-deliver notices (gap, permission prompts)
///   on a small priority queue drained before content — effectively never
///   full, so a notice always reaches the client even under saturation.
pub trait SessionSink: Send + Sync {
    /// Non-blocking content send. `false` = element dropped (queue full) —
    /// the caller decides the recovery (gap notice + drop mark).
    fn send(&self, event: SubscribeResponse) -> bool;
    /// Non-blocking control send on the priority channel. `false` only when
    /// the control queue is full (rare: small channel, drained first).
    fn send_control(&self, event: SubscribeResponse) -> bool;
}

pub(crate) type SharedSessionSink = Arc<dyn SessionSink>;

/// Monotonic serial for the server-internal routing key.
static SN: AtomicU64 = AtomicU64::new(1);

/// One client identity. Created once (at first connection), adopted by
/// every later connection that replays its token within the grace window,
/// and destroyed by the reaper after the window lapses with no adoption.
///
/// Interior state uses `std::sync::Mutex` — sends are non-blocking
/// try_send calls, so the lock is never held across an await.
pub struct Session {
    token: String,
    sn: u64,
    /// The current connection's outbound; `None` = detached (no live
    /// connection). Swapped on adoption.
    conn: std::sync::Mutex<Option<SharedSessionSink>>,
    /// When the identity was detached — the grace window's clock. `None`
    /// while live (or never attached).
    detached_at: std::sync::Mutex<Option<Instant>>,
}

pub type SessionRef = Arc<Session>;

impl Session {
    /// Mint a fresh identity with a random resume token.
    pub fn create() -> SessionRef {
        Self::with_token(uuid::Uuid::new_v4().to_string())
    }

    /// Mint an identity with an explicit token (tests seed stable tokens).
    pub(crate) fn with_token(token: impl Into<String>) -> SessionRef {
        Arc::new(Self {
            token: token.into(),
            sn: SN.fetch_add(1, Ordering::Relaxed),
            conn: std::sync::Mutex::new(None),
            detached_at: std::sync::Mutex::new(None),
        })
    }

    /// The client-facing resume token (handshake frames only).
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The server-internal routing key.
    pub(crate) fn sn(&self) -> u64 {
        self.sn
    }

    /// Attach a connection: swaps the outbound sink in and clears the
    /// detached marker (adoption). Any previous connection is superseded —
    /// its teardown will no-op (see [`Session::detach`]).
    pub(crate) fn attach(&self, sink: SharedSessionSink) {
        *self.conn.lock().expect("conn lock") = Some(sink);
        *self.detached_at.lock().expect("detached lock") = None;
    }

    /// Detach a connection. Succeeds only when `sink` IS the current
    /// connection — a resumed connection's adoption swapped the sink, so a
    /// superseded teardown no-ops (the old ptr_eq guard, moved inside the
    /// identity where the swap is atomic). Marks the grace-window clock.
    /// Returns `false` when this connection no longer owns the identity.
    pub(crate) fn detach(&self, sink: &SharedSessionSink) -> bool {
        let mut conn = self.conn.lock().expect("conn lock");
        let is_current = conn.as_ref().is_some_and(|cur| Arc::ptr_eq(cur, sink));
        if is_current {
            *conn = None;
            *self.detached_at.lock().expect("detached lock") = Some(Instant::now());
        }
        is_current
    }

    /// Live = a connection is attached. Public: the Connect surface's
    /// stream tests (and the session_leases adoptability rule) read it.
    pub fn is_live(&self) -> bool {
        self.conn.lock().expect("conn lock").is_some()
    }

    /// The current connection's sink, when attached (tests use it to pass
    /// the stale-teardown guard and to restore a swapped sink).
    #[cfg(test)]
    pub(crate) fn sink(&self) -> Option<SharedSessionSink> {
        self.conn.lock().expect("conn lock").clone()
    }

    /// Replace the connection sink in place (tests: swap in a FailingSink to
    /// model a stalled viewer without re-registering).
    #[cfg(test)]
    pub(crate) fn swap_sink(&self, sink: SharedSessionSink) {
        *self.conn.lock().expect("conn lock") = Some(sink);
    }

    /// Clear the detached marker (reaper teardown: the identity is being
    /// discarded, and the observable "marker consumed" contract follows).
    pub(crate) fn clear_detached(&self) {
        *self.detached_at.lock().expect("detached lock") = None;
    }

    /// How long the identity has been detached, when detached.
    pub(crate) fn detached_for(&self) -> Option<Duration> {
        self.detached_at
            .lock()
            .expect("detached lock")
            .map(|t| t.elapsed())
    }

    /// Non-blocking content send to the current connection. `false` when
    /// detached or the queue is full — the caller decides the recovery.
    pub(crate) fn send(&self, event: SubscribeResponse) -> bool {
        let guard = self.conn.lock().expect("conn lock");
        guard.as_ref().is_some_and(|s| s.send(event))
    }

    /// Non-blocking control send (priority channel). `false` when detached
    /// or the control queue is full.
    pub(crate) fn send_control(&self, event: SubscribeResponse) -> bool {
        let guard = self.conn.lock().expect("conn lock");
        guard.as_ref().is_some_and(|s| s.send_control(event))
    }
}
