//! ChatManager operations — lease/subscription bookkeeping, broadcast, and
//! lease gating for every mutating chat operation.
//!
//! Lock order invariant: ALWAYS acquire `chats` before `identities` (router
//! fanout only takes `chats`). Violating this order can deadlock the
//! manager. Identity interior state (std mutexes) is never held across an
//! await.

use crate::identity::{Session, SessionRef, SharedSessionSink};
use crate::manager::{CachedChat, ChatId, ChatInfoOwned, ServerState, new_chat_id};
use crate::router::RouterHandle;
use anyhow::{Context, anyhow};
use flux_core::WireEvent;
use std::collections::HashMap;
use std::sync::Arc;

/// Send idempotency window per chat — how many recent client_msg_id keys
/// are remembered for duplicate absorption. The window only needs to
/// outlive a client retry burst, not the chat's lifetime.
const MAX_MSG_ID_WINDOW: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    Granted,
    AlreadyOwned,
    NotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutateOutcome {
    Ok,
    Busy,
    NotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscribeOutcome {
    Subscribed,
    NotFound,
}

/// Result of a viewer-grade chat open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenOutcome {
    /// Snapshot(s) delivered and the subscription activated.
    Subscribed,
    /// Unknown chat — no snapshots, no subscription.
    NotFound,
}

/// Failure of a fork request — the handler maps NotFound onto a transport
/// status; everything else rides the response's inline `error` (D4').
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkFailure {
    /// The source conversation does not exist.
    NotFound,
    /// The fork point is not a USER message row of the source chat.
    BadPoint,
    /// The store rejected the fork (an internal failure surfaced inline —
    /// the source is untouched either way).
    Internal(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    Ok,
    /// The request carried an idempotency key (client_msg_id) this chat
    /// already accepted — the turn was absorbed, NOT enqueued again.
    Duplicate,
    Busy,
    NotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionOutcome {
    /// Answer delivered to the pending question.
    Ok,
    /// The chat exists but holds no such pending question (stale, already
    /// answered, or the round died) — the answer is dropped.
    UnknownQuestion,
    NotOwner,
    NotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    Ok,
    Busy,
    NotFound,
}

/// Result of a provider hot-swap request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchOutcome {
    /// The swap is scheduled (applies at the current or next round
    /// boundary; `provider_switched` announces the actual application).
    Ok,
    Busy,
    NotFound,
}

/// Drop the session's viewer entries across every chat (the connection's
/// drop marks go with them) and let each router forget its dropped mark,
/// so the session's next subscription takes the fresh-subscribe path and
/// receives the history + state snapshots it needs to render. Shared by
/// detach (its own teardown) and resume (the superseded connection's
/// teardown) — the leases persist untouched either way.
fn remove_viewer_everywhere(chats: &mut HashMap<ChatId, CachedChat>, session: &SessionRef) {
    for chat in chats.values_mut() {
        if chat.viewers.remove(&session.sn()).is_some() {
            chat.router.viewer_gone(session.sn());
        }
    }
}

/// Holder check: the opaque identity comparison (same allocation). The
/// lease/viewer structures hold `SessionRef`; the token string never
/// participates in authorization.
impl ServerState {
    /// The adoptability rule shared by every token→identity surface
    /// (resume, lease-gated RPCs, terminal auth): an identity resolves
    /// while LIVE (a duplicate tab sharing the token) or while detached
    /// within the grace window. An expired identity is invisible here —
    /// the reaper owns it.
    fn identity_adoptable(&self, session: &SessionRef) -> bool {
        session.is_live()
            || session
                .detached_for()
                .is_some_and(|d| d < self.manager.grace())
    }

    /// Attach a fresh identity (transport accept): mints the token,
    /// registers, and binds the connection's sink. Returns the handle the
    /// dispatch adopts for every later frame.
    pub async fn attach_session(&self, sink: SharedSessionSink) -> SessionRef {
        let session = Session::create();
        session.attach(sink);
        self.manager
            .identities
            .write()
            .await
            .insert(session.token().to_string(), SessionRef::clone(&session));
        session
    }

    /// Session disconnect: DETACH instead of tearing down. The
    /// identity — and its leases — survive for the grace window waiting
    /// for the stream to re-open and adopt the token; only the live
    /// connection is dropped (viewer entries removed, so a resumed
    /// client's re-claim takes the fresh-subscribe path and receives
    /// history).
    ///
    /// Stale-teardown guard: the detach succeeds only when `sink` is still
    /// the identity's current connection — a resume superseded it and owns
    /// the identity now (the old connection's teardown must not release the
    /// new connection's leases).
    pub async fn detach_session(&self, session: &SessionRef, sink: &SharedSessionSink) {
        if !session.detach(sink) {
            return; // superseded by a resumed connection — it owns the identity now
        }
        let mut chats = self.manager.chats.write().await;
        remove_viewer_everywhere(&mut chats, session);
    }

    /// Adopt a previous identity by resume token. Resumable when the
    /// identity is currently live (a duplicate tab sharing the token) or
    /// detached within the grace window. On adoption the new sink replaces
    /// the old registration and the held leases are returned. Unknown or
    /// expired tokens return None — the caller keeps its freshly attached
    /// identity.
    pub async fn resume_session(
        &self,
        token: &str,
        sink: SharedSessionSink,
    ) -> Option<(SessionRef, Vec<String>)> {
        let mut chats = self.manager.chats.write().await;
        let identities = self.manager.identities.read().await;
        let session = identities.get(token)?;
        // Adoptable: live (duplicate tab) or detached within the grace
        // window (the read lock suffices: adoption mutates the identity's
        // interior state and the viewer maps, never the identities map).
        if !self.identity_adoptable(session) {
            return None;
        }
        session.attach(sink);
        // Connection state resets on adoption: drop the superseded
        // connection's viewer entries so the rebuilt client's re-claim
        // takes the fresh-subscribe path. The leases — the expensive,
        // precious part — persist untouched.
        remove_viewer_everywhere(&mut chats, session);
        let leases = chats
            .values()
            .filter(|c| c.lease_held_by(session))
            .map(|c| c.id.clone())
            .collect();
        Some((SessionRef::clone(session), leases))
    }

    /// Look up a live-or-grace-window identity by token WITHOUT attaching
    /// anything — the read-only identity resolution the Connect surface's
    /// lease-gated RPCs use (the session token rides request metadata)
    /// and the terminal side channel's auth applies the same rule. Returns
    /// the identity handle plus the chats whose lease it holds.
    pub async fn session_leases(&self, token: &str) -> Option<(SessionRef, Vec<String>)> {
        let chats = self.manager.chats.read().await;
        let identities = self.manager.identities.read().await;
        let session = identities.get(token)?;
        if !self.identity_adoptable(session) {
            return None;
        }
        let leases = chats
            .values()
            .filter(|c| c.lease_held_by(session))
            .map(|c| c.id.clone())
            .collect();
        Some((SessionRef::clone(session), leases))
    }

    /// Release the leases of detached identities whose grace window has
    /// elapsed — the deferred half of the teardown. A resume that adopted
    /// the identity meanwhile makes it a no-op (the identity is live again;
    /// only detached-expired ones are reaped and their registry entries
    /// removed, dropping the identity for good).
    pub async fn reap_detached(&self) {
        let expired: Vec<SessionRef> = {
            let identities = self.manager.identities.read().await;
            identities
                .values()
                .filter(|s| s.detached_for().is_some_and(|d| d >= self.manager.grace()))
                .cloned()
                .collect()
        };
        for session in expired {
            let released = {
                // Double-check under the lock: a resume between the sweep
                // and here re-attached the identity (detached_at cleared).
                if session.is_live() || session.detached_for().is_none() {
                    continue;
                }
                let mut chats = self.manager.chats.write().await;
                let mut released = false;
                for chat in chats.values_mut() {
                    if chat.lease_held_by(&session) {
                        chat.lease = None;
                        released = true;
                    }
                }
                released
            };
            if released {
                self.broadcast_chats().await;
            }
            // The detached marker is consumed with the registry entry — the
            // identity is unreferenced now (leases released, viewers removed
            // at detach) and can be freed.
            session.clear_detached();
            self.manager
                .identities
                .write()
                .await
                .remove(session.token());
        }
    }

    pub async fn claim_chat(&self, session: &SessionRef, chat_id: &str) -> ClaimOutcome {
        // `granted` = the active FLAG flipped (lease None → held; needs the
        // active-broadcast); `resumed` = re-claim by a rebuilt client
        // (snapshot without broadcast).
        let (granted, resumed);
        let mut demoted: Option<SessionRef> = None;
        {
            let mut chats = self.manager.chats.write().await;
            let Some(chat) = chats.get_mut(chat_id) else {
                return ClaimOutcome::NotFound;
            };
            let holder = chat.lease.clone();
            // Lease STEAL: claiming a chat whose lease is held by a DIFFERENT
            // session transfers the operator role. The previous holder is
            // demoted in-band below (it receives the same chat_busy error a
            // rejected send produces, so its client degrades itself to a
            // read-only viewer). The chat-level round keeps running; the new
            // holder watches it live and owns cancel. This is what makes the
            // viewer pane's Take over button real — without the steal, the
            // button could only ever click into a rejection.
            if let Some(owner) = &holder
                && !chat.lease_held_by(session)
            {
                demoted = Some(owner.clone());
            }
            let already_owner = holder.is_some() && demoted.is_none();
            // Fully-opened fast path: holder AND viewer → idempotent no-op
            //. A holder WITHOUT a viewer entry is a resumed connection
            // (detach removed its viewer registration): the rebuilt client
            // still needs the history + state snapshot — fall through to the
            // same delivery below, keeping the lease and skipping the
            // active-broadcast (nothing changed for other windows).
            if already_owner && chat.viewers.contains_key(&session.sn()) {
                return ClaimOutcome::AlreadyOwned;
            }
            resumed = already_owner;
            // Broadcast ONLY when the `active` flag flipped (the chat went
            // from unleased to leased). A steal keeps the flag true — the
            // HOLDER changed, not the occupancy — and ChatInfo carries no
            // holder identity, so other sessions would learn nothing new;
            // the demoted holder is told in-band instead.
            granted = holder.is_none();
            // Claim is the single open message — the operator path never
            // needs a separate OpenChat. Ordering invariant: the history
            // snapshot is enqueued FIRST (the sink's content queue is
            // FIFO), then ensure_viewer activates the subscription, so
            // any live frame after it lands behind the history. The store
            // read happens under the chats write lock (milliseconds, same
            // class as send_message's ensure_task — a two-phase split would
            // have to re-validate the whole claim transition; not worth it
            // for a single-user local server).
            match self.store.load_stored_messages(chat_id).await {
                Ok(stored) => {
                    // R2: the snapshot carries the CURRENT seq (peeked) —
                    // elements strictly BELOW it predate the snapshot and
                    // are dropped by the client; the equal one is the first
                    // live element after it.
                    let mut el = crate::router::element(
                        chat_id,
                        flux_proto::flux::v1::subscribe_response::Kind::ChatHistory(
                            flux_proto::flux::v1::ChatHistory {
                                messages: stored
                                    .into_iter()
                                    .map(|s| {
                                        let mut m: flux_proto::flux::v1::Message = s.message.into();
                                        // The store row id — the client names
                                        // messages by it (the fork point).
                                        m.id = s.id;
                                        m
                                    })
                                    .collect(),
                            },
                        ),
                    );
                    el.chat_seq = chat.next_seq(false);
                    session.send(el);
                }
                Err(e) => {
                    tracing::warn!(
                        chat_id,
                        error = %e,
                        "failed to load history for claim"
                    );
                }
            }
            // Send the round state with the snapshot (machine-
            // authoritative; a lazily-unspawned task = Idle). Order:
            // history → state → subscription activates.
            let state = chat
                .live_task()
                .map(|h| h.active_state())
                .unwrap_or(flux_core::ChatStateKind::Idle);
            let mut el = crate::router::element(
                chat_id,
                flux_proto::flux::v1::subscribe_response::Kind::ChatState(
                    flux_proto::flux::v1::ChatState {
                        state: flux_proto::flux::v1::ChatStateKind::from(state) as i32,
                    },
                ),
            );
            el.chat_seq = chat.next_seq(false);
            session.send(el);
            chat.ensure_viewer(session);
            chat.lease = Some(SessionRef::clone(session));
            // Tell the router a fresh holder is in place — it may
            // have a parked question waiting to be re-delivered.
            // (Also correct on resume: the rebuilt client never saw
            // the parked prompt, so redelivery is the point.)
            chat.router.claim(SessionRef::clone(session));
        }
        // A lease TRANSITION changed the chat's `active` flag — broadcast the
        // fresh list to every session. A resumed claim keeps the lease (no
        // transition, no broadcast); the snapshot delivery above still ran.
        if granted {
            self.broadcast_chats().await;
        }
        // In-band demotion of the previous holder (sent AFTER the lock is
        // dropped): the same chat_busy error a rejected send produces, so
        // the client's existing handler degrades it to a read-only viewer
        // without any new frame type.
        if let Some(old) = demoted {
            // The message NEVER carries the new holder's resume token: the
            // token is a bearer credential (the stream open adopts the whole
            // identity, leases included), so leaking it into the demoted
            // client's stream would let the reader take over the holder's
            // session. Same text as a rejected send's chat_busy — the demoted
            // client's existing handler degrades it to a read-only viewer.
            let el = crate::router::element(
                chat_id,
                flux_proto::flux::v1::subscribe_response::Kind::Error(
                    flux_proto::flux::v1::ErrorEvent {
                        code: flux_proto::flux::v1::ErrorCode::ChatBusy as i32,
                        message: "Chat is in use by another session".to_string(),
                    },
                ),
            );
            old.send(el);
        }
        if resumed {
            ClaimOutcome::AlreadyOwned
        } else {
            ClaimOutcome::Granted
        }
    }

    /// Terminal side-channel auth: the token must name a known identity
    /// that is live or detached within the grace window — the same
    /// adoptability rule `resume_session` applies. Read-only: a terminal
    /// never adopts or mutates the identity.
    pub async fn identity_resolvable(&self, token: &str) -> bool {
        let identities = self.manager.identities.read().await;
        identities
            .get(token)
            .is_some_and(|s| self.identity_adoptable(s))
    }

    /// The resume grace window — the terminal side channel reuses it:
    /// a PTY outlives its socket by exactly this long before the reaper
    /// kills it (a refresh re-attaches within the window).
    pub fn session_grace(&self) -> std::time::Duration {
        self.manager.grace()
    }

    /// Return the lease; the subscription (if any) is kept. The
    /// client-facing exit path is CloseChat (unsubscribe releases the
    /// lease automatically) — this method stays as an internal primitive
    /// for tests and lifecycle logic that manipulate the lease directly.
    pub async fn release_chat(&self, session: &SessionRef, chat_id: &str) {
        // Lease released → active flips false: broadcast the list (create/
        // delete/rename already broadcast; lease changes need it too, or other
        // windows' sidebars go stale).
        if self.release_lease_quiet(session, chat_id).await {
            self.broadcast_chats().await;
        }
    }

    /// The lease-release half of [`Self::release_chat`] WITHOUT the
    /// broadcast — for callers that fold the release into a larger
    /// transition carrying its own `chats` broadcast (the fork: the source
    /// lease hands over with the navigation, and attach_new_chat's
    /// broadcast is the one truthful frame). Guards against everything
    /// but self: a viewer-forker holds no lease; a foreign holder is
    /// untouched.
    async fn release_lease_quiet(&self, session: &SessionRef, chat_id: &str) -> bool {
        let mut chats = self.manager.chats.write().await;
        match chats.get_mut(chat_id) {
            Some(chat) if chat.lease_held_by(session) => {
                chat.lease = None;
                true
            }
            _ => false,
        }
    }

    /// Read-only existence check — whether a chat is cached. No
    /// subscription side effect. Used by the transport's ChatOpen to pre-check
    /// existence before sending history, so the history frame is not emitted
    /// for a chat that does not exist.
    pub async fn chat_exists(&self, chat_id: &str) -> bool {
        self.manager.chats.read().await.contains_key(chat_id)
    }

    /// Open a chat as a read-only viewer: the single-message subscribe
    /// path (the viewer-grade twin of [`Self::claim_chat`], without the
    /// lease). Ordering invariant — the history snapshot is enqueued
    /// FIRST, the authoritative round state second, and the subscription
    /// activates LAST (the sink queue is FIFO), so every live frame after
    /// the open lands behind the snapshots the client needs to render.
    /// ALL of it happens under ONE chats write-lock acquisition: a
    /// snapshot taken outside the lock would race the fanout — events
    /// between load and subscribe would land in neither the snapshot nor
    /// the viewer's queue (the claim path has always been atomic; the
    /// open path was split across the transport and could lose frames).
    /// The store read rides the write lock (milliseconds, same class as
    /// the claim snapshot).
    ///
    /// A repeat open by an already-subscribed viewer skips the history
    /// snapshot (idempotent — history is delivered only with the first
    /// subscription; a CloseChat + re-open restarts it) but re-sends
    /// the round-state snapshot: it is cheap, and the client converges
    /// its streaming state from authority (reconnects / gap reloads /
    /// multi-window no longer drift).
    pub async fn open_chat(&self, session: &SessionRef, chat_id: &str) -> OpenOutcome {
        let mut chats = self.manager.chats.write().await;
        let Some(chat) = chats.get_mut(chat_id) else {
            return OpenOutcome::NotFound;
        };
        if !chat.viewers.contains_key(&session.sn()) {
            match self.store.load_stored_messages(chat_id).await {
                Ok(stored) => {
                    // R2: the snapshot carries the CURRENT seq (peeked).
                    let mut el = crate::router::element(
                        chat_id,
                        flux_proto::flux::v1::subscribe_response::Kind::ChatHistory(
                            flux_proto::flux::v1::ChatHistory {
                                messages: stored
                                    .into_iter()
                                    .map(|s| {
                                        let mut m: flux_proto::flux::v1::Message = s.message.into();
                                        m.id = s.id;
                                        m
                                    })
                                    .collect(),
                            },
                        ),
                    );
                    el.chat_seq = chat.next_seq(false);
                    session.send(el);
                }
                Err(e) => {
                    // The open cannot fail here — the client gets the chat
                    // and the history is what failed; log it and deliver an
                    // empty snapshot so the client's render never hangs on a
                    // missing frame.
                    tracing::warn!(chat_id, error = %e, "failed to load chat history for open");
                    let mut el = crate::router::element(
                        chat_id,
                        flux_proto::flux::v1::subscribe_response::Kind::ChatHistory(
                            flux_proto::flux::v1::ChatHistory {
                                messages: Vec::new(),
                            },
                        ),
                    );
                    el.chat_seq = chat.next_seq(false);
                    session.send(el);
                }
            }
        }
        // The authoritative round-state snapshot — whether or not already
        // subscribed (idempotent, fresh). Order: history → state →
        // subscription activates.
        let state = chat
            .live_task()
            .map(|h| h.active_state())
            .unwrap_or(flux_core::ChatStateKind::Idle);
        let mut el = crate::router::element(
            chat_id,
            flux_proto::flux::v1::subscribe_response::Kind::ChatState(
                flux_proto::flux::v1::ChatState {
                    state: flux_proto::flux::v1::ChatStateKind::from(state) as i32,
                },
            ),
        );
        el.chat_seq = chat.next_seq(false);
        session.send(el);
        chat.ensure_viewer(session);
        OpenOutcome::Subscribed
    }

    /// Join the chat's viewers without any snapshot delivery — the raw
    /// subscription primitive `open_chat` builds on (inlined there under
    /// the same lock). Stays as the fixture/test-facing operation for
    /// tests that need a viewer without the snapshot protocol.
    pub async fn subscribe_chat(&self, session: &SessionRef, chat_id: &str) -> SubscribeOutcome {
        let mut chats = self.manager.chats.write().await;
        let Some(chat) = chats.get_mut(chat_id) else {
            return SubscribeOutcome::NotFound;
        };
        chat.ensure_viewer(session);
        SubscribeOutcome::Subscribed
    }

    pub async fn unsubscribe_chat(&self, session: &SessionRef, chat_id: &str) {
        // Whether the closing session held the lease (the RELEASE half of
        // "close = full exit").
        let released = {
            let mut chats = self.manager.chats.write().await;
            let Some(chat) = chats.get_mut(chat_id) else {
                return;
            };
            // close = full exit: unsubscribe AND release the lease if held. The
            // standalone release message was deleted (the client only ever used
            // release+close as a pair; the protocol surface merged into close).
            let released = chat.lease_held_by(session);
            if released {
                chat.lease = None;
            }
            chat.viewers.remove(&session.sn());
            // The dropped-frame mark belongs to the subscription itself: clear
            // it on unsubscribe so a resubscription starts clean (otherwise the
            // silence from a gap would persist until the next boundary event,
            // even though the gap notice already told the user "reload = resync").
            chat.router.viewer_gone(session.sn());
            released
        };
        // Lease released → active flips false: broadcast the list. The close
        // path carries the release's broadcast duty — without it other windows'
        // In-use badges stay stale until some unrelated event re-broadcasts.
        if released {
            self.broadcast_chats().await;
        }
    }

    /// Create a chat: persist to the store (the atomicity guards below),
    /// insert into the cache with a fresh router, and grant the creator
    /// the lease plus a subscription. The task is NOT spawned — first
    /// message spawns it lazily. The provider instance arrives already
    /// resolved (the server registry selected it at the CreateChat
    /// dispatch) and is kept in the cache so every later task spawn runs
    /// on the SAME provider; the id/model labels persist explicitly so
    /// respawns and the UI never re-derive them.
    pub async fn create_chat(
        &self,
        session: &SessionRef,
        name: &str,
        workdir: &str,
        pin: flux_chat::ResolvedPin,
    ) -> anyhow::Result<ChatInfoOwned> {
        let id = new_chat_id();
        if workdir.trim().is_empty() {
            return Err(anyhow!("workdir cannot be empty"));
        }
        // Validate the argument: a canonicalize failure (path does not exist /
        // does not resolve) rejects creation outright — no fallback to the
        // server directory (default_workdir was removed). No project-root
        // allowlist — the server runs with the starting
        // user's permissions, so any readable directory is a valid workdir
        // (real isolation is the OS/container boundary's job).
        let workdir = std::fs::canonicalize(workdir)
            .map_err(|e| anyhow!("workdir does not resolve: {workdir}: {e}"))?
            .to_string_lossy()
            .into_owned();
        let created_at = self
            .store
            .insert_chat(&id, name)
            .await
            .with_context(|| format!("failed to insert chat {id} into DB"))?;
        // The workdir pair is ONE semantic unit — the chat boundary and
        // its transient shell cwd — persisted atomically: either both
        // land (creation proceeds) or neither does (one rollback).
        if let Err(e) = self
            .store
            .save_state_entries(&id, &[("workdir", &workdir), ("current_dir", &workdir)])
            .await
        {
            tracing::warn!(chat_id = %id, error = %e, "failed to persist initial workdir pair; aborting chat creation");
            if let Err(cleanup_err) = self.store.delete_chat(&id).await {
                tracing::warn!(chat_id = %id, error = %cleanup_err, "rollback of failed chat creation failed");
            }
            return Err(anyhow!("chat workdir could not be persisted"));
        }
        // The provider pin persists best-effort: a failure leaves the state
        // absent → spawns fall back to the server default (never a broken
        // chat over a display label).
        let pin_pair: &[(&str, &str)] = match pin.model.is_empty() {
            true => &[("provider", &pin.id)],
            false => &[("provider", &pin.id), ("model", &pin.model)],
        };
        if let Err(e) = self.store.save_state_entries(&id, pin_pair).await {
            tracing::warn!(chat_id = %id, error = %e, "failed to persist provider pin");
        }
        Ok(self
            .attach_new_chat(session, id, name.to_owned(), created_at, workdir, pin, None)
            .await)
    }

    /// Assemble + register a freshly created chat — the create/fork shared
    /// tail: router spawn, the cache entry carrying the caller's LEASE,
    /// and the authoritative chats broadcast. The viewer slot is
    /// deliberately NOT taken here: the caller's immediate claim then
    /// rides the resumed path (lease kept) and DELIVERS the history
    /// snapshot — for a fork that snapshot is the whole point (the copied
    /// transcript); for a create it is an honest empty frame.
    /// `created_at` (a fork's creation stamp comes from its own row)
    /// starts the chat's activity clock.
    #[allow(clippy::too_many_arguments)]
    async fn attach_new_chat(
        &self,
        session: &SessionRef,
        id: String,
        name: String,
        created_at: String,
        workdir: String,
        pin: flux_chat::ResolvedPin,
        forked_from: Option<String>,
    ) -> ChatInfoOwned {
        let router = RouterHandle::spawn(id.clone(), Arc::clone(&self.manager));
        // ONE construction: the wire snapshot is DERIVED from the cache
        // entry (`CachedChat::info`) instead of both being hand-built from
        // the same fields — the two shapes cannot drift apart.
        let entry = CachedChat {
            seq: std::sync::atomic::AtomicU64::new(0),
            id,
            // A new chat's first activity is its creation.
            last_activity_at: created_at.clone(),
            workdir,
            provider_id: pin.id.clone(),
            model: pin.model.clone(),
            provider: Some(pin.provider),
            forked_from_chat: forked_from,
            lease: Some(SessionRef::clone(session)),
            viewers: HashMap::new(),
            questions: Arc::new(flux_chat::question::QuestionBoard::new()),
            router,
            task: None,
            recent_msg_ids: std::collections::VecDeque::new(),
            // `created_at` sits AFTER `last_activity_at` on purpose —
            // struct fields evaluate in literal order, and the activity
            // stamp clones the creation stamp before it moves.
            name,
            created_at,
        };
        let info = entry.info();
        {
            let mut chats = self.manager.chats.write().await;
            chats.insert(info.chat_id.clone(), entry);
        }
        self.broadcast_chats().await;
        info
    }

    /// Gate on the lease: empty or self → proceed; another holder → Busy.
    /// Shuts down the router, aborts the running task, removes the record,
    /// and broadcasts the list to all sessions. The router has already
    /// exited (FIFO `Shutdown`), so no error frame is broadcast.
    pub async fn delete_chat(&self, session: &SessionRef, chat_id: &str) -> MutateOutcome {
        let task = {
            let mut chats = self.manager.chats.write().await;
            let Some(chat) = chats.get_mut(chat_id) else {
                return MutateOutcome::NotFound;
            };
            match &chat.lease {
                Some(owner) if !chat.lease_held_by(session) => {
                    return MutateOutcome::Busy;
                }
                _ => {}
            }
            // The router must observe Shutdown before its sender is dropped.
            chat.router.shutdown();
            let task = chat.task.take();
            chats.remove(chat_id);
            task
        };
        if let Some(task) = task {
            // Abort the task: its ChannelOutput drops with the task; the router has already exited.
            task.handle.shutdown();
        }
        if let Err(e) = self.store.delete_chat(chat_id).await {
            tracing::warn!(chat_id = %chat_id, error = %e, "failed to delete chat");
        }
        self.broadcast_chats().await;
        MutateOutcome::Ok
    }

    pub async fn rename_chat(
        &self,
        session: &SessionRef,
        chat_id: &str,
        name: &str,
    ) -> MutateOutcome {
        {
            let mut chats = self.manager.chats.write().await;
            let Some(chat) = chats.get_mut(chat_id) else {
                return MutateOutcome::NotFound;
            };
            match &chat.lease {
                Some(owner) if !chat.lease_held_by(session) => {
                    return MutateOutcome::Busy;
                }
                _ => {}
            }
            chat.name = name.to_owned();
        }
        if let Err(e) = self.store.rename_chat(chat_id, name).await {
            tracing::warn!(chat_id = %chat_id, error = %e, "failed to rename chat");
        }
        self.broadcast_chats().await;
        MutateOutcome::Ok
    }

    /// Send a user message: an empty lease is auto-claimed (joining the
    /// subscription and notifying the router to redeliver parked prompts);
    /// the task is lazily spawned (created when missing or terminated); the
    /// message routes to the task. The kernel QUEUES mid-round turns
    /// (machine turn queue), so sends never wait for a round boundary —
    /// including while a rebuild gate is armed (the turn runs pre-gate,
    /// inside the pre-rebuild context). No error is thrown for identity
    /// failures or unknown chats — outcomes carry the result.
    /// Awaits the spawn inside the chats write lock (store I/O +
    /// provider.begin, milliseconds; acceptable for a single user with a few
    /// windows). Lock order chats→sessions is preserved.
    pub async fn send_message(
        self: &Arc<Self>,
        session: &SessionRef,
        chat_id: &str,
        message: String,
    ) -> anyhow::Result<SendOutcome> {
        self.send_message_inner(session, chat_id, message, false, None)
            .await
    }

    /// R1 interrupt-send: fuse "cancel the live round" and "submit the
    /// user's message" into ONE operation. Both inputs ride the kernel's
    /// FIFO back-to-back under the chats write lock, so the interject order
    /// (cancel-first — the machine's documented assumption) holds by
    /// construction and the ordering of independent HTTP requests stops
    /// mattering. The cancel keeps the kernel's "stop means stop"
    /// discipline: a newer interrupt-send supersedes an older one (its
    /// cancel kills the older queued turn), and an explicit cancel after
    /// an interrupt-send kills the queued turn. On an Idle engine the
    /// cancel is absorbed and the message starts immediately (equals a
    /// plain send). Same lease gate, auto-claim, and lazy spawn as
    /// [`Self::send_message`].
    pub async fn send_message_interrupting(
        self: &Arc<Self>,
        session: &SessionRef,
        chat_id: &str,
        message: String,
    ) -> anyhow::Result<SendOutcome> {
        self.send_message_inner(session, chat_id, message, true, None)
            .await
    }

    /// Send with an idempotency key: a resend carrying an already-accepted
    /// `client_msg_id` is absorbed as a duplicate (no second turn), so a
    /// client retry after an ambiguous timeout can never double-submit.
    /// `interrupt = true` is the R1 fused cancel+send.
    pub async fn send_message_idempotent(
        self: &Arc<Self>,
        session: &SessionRef,
        chat_id: &str,
        message: String,
        interrupt: bool,
        client_msg_id: Option<String>,
    ) -> anyhow::Result<SendOutcome> {
        self.send_message_inner(session, chat_id, message, interrupt, client_msg_id)
            .await
    }

    async fn send_message_inner(
        self: &Arc<Self>,
        session: &SessionRef,
        chat_id: &str,
        message: String,
        interrupt: bool,
        client_msg_id: Option<String>,
    ) -> anyhow::Result<SendOutcome> {
        let auto_claimed = {
            let mut chats = self.manager.chats.write().await;
            let Some(chat) = chats.get_mut(chat_id) else {
                return Ok(SendOutcome::NotFound);
            };
            match &chat.lease {
                Some(owner) if !chat.lease_held_by(session) => {
                    return Ok(SendOutcome::Busy);
                }
                _ => {}
            }
            // Idempotency: a key the chat already accepted absorbs the
            // resend. The check runs before the enqueue and the record
            // after it — all inside the chats write lock, so two racing
            // resends can never both pass, and a failed ensure_task leaves
            // the key unrecorded (a retry still works).
            if let Some(key) = &client_msg_id
                && chat.recent_msg_ids.contains(key)
            {
                return Ok(SendOutcome::Duplicate);
            }
            let mut auto_claimed = false;
            if chat.lease.is_none() {
                chat.ensure_viewer(session);
                chat.lease = Some(SessionRef::clone(session));
                chat.router.claim(SessionRef::clone(session));
                auto_claimed = true;
            }
            self.ensure_task(chat).await?;
            if let Some(task) = &chat.task {
                // The fused interrupt pair: cancel first, message second —
                // two sends on one FIFO inside one critical section, so
                // the machine reads them in this exact order.
                if interrupt {
                    task.handle.send_cancel();
                }
                task.handle.send_user(message);
            }
            // The turn is enqueued — record the idempotency key (FIFO,
            // capped; the window only needs to outlive a client retry).
            if let Some(key) = client_msg_id {
                chat.recent_msg_ids.push_back(key);
                while chat.recent_msg_ids.len() > MAX_MSG_ID_WINDOW {
                    chat.recent_msg_ids.pop_front();
                }
            }
            auto_claimed
        };
        // The auto-claim granted the lease → active flips true: broadcast the
        // list (lease changes need it too, or other windows' active flags go
        // stale).
        if auto_claimed {
            self.broadcast_chats().await;
        }
        Ok(SendOutcome::Ok)
    }

    /// Answer to the model's question: only the lease holder may answer
    /// (non-holders' answers are dropped without error — the semantics the
    /// old approval answers had). The answer first notifies the router to
    /// clear its parked question copy (so a stale question is not redelivered
    /// on the next claim), then goes straight to the waiting tool flight via
    /// the board's oneshot.
    pub async fn question_response(
        &self,
        session: &SessionRef,
        chat_id: &str,
        question_id: &str,
        answer: String,
    ) -> QuestionOutcome {
        let chats = self.manager.chats.read().await;
        let Some(chat) = chats.get(chat_id) else {
            return QuestionOutcome::NotFound;
        };
        if !chat.lease_held_by(session) {
            return QuestionOutcome::NotOwner;
        }
        chat.router.question_answered();
        let delivered = chat.questions.respond(question_id, &answer);
        if delivered {
            QuestionOutcome::Ok
        } else {
            QuestionOutcome::UnknownQuestion
        }
    }

    /// Cancel the current round: lease holder only; an empty lease = no-op
    /// Ok (nobody can cancel; a read-only client sends no cancels). Busy =
    /// someone else holds it.
    pub async fn cancel_chat(&self, session: &SessionRef, chat_id: &str) -> CancelOutcome {
        let chats = self.manager.chats.read().await;
        let Some(chat) = chats.get(chat_id) else {
            return CancelOutcome::NotFound;
        };
        match &chat.lease {
            Some(owner) if !chat.lease_held_by(session) => CancelOutcome::Busy,
            Some(_) => {
                if let Some(handle) = chat.live_task() {
                    handle.send_cancel();
                }
                CancelOutcome::Ok
            }
            None => CancelOutcome::Ok,
        }
    }

    /// Hot-swap the conversation's provider (and optionally the model).
    /// Lease holder only (the swap shapes every subsequent round). The
    /// resolved instance arrives from the session's dispatch (the server
    /// registry owns selection; an unknown id is rejected there).
    ///
    /// The pin is a truth source, mutated AT REQUEST TIME: it persists,
    /// the cache syncs (labels AND instance), `provider_switched`
    /// announces the change, and the `Rebuild` command carries the fresh
    /// instance to the engine — the machine's gate arms (a live round and
    /// queued turns finish first), and the consumer re-begins its
    /// connection on the swapped provider over the live context, in
    /// place, at the fired gate.
    pub async fn switch_provider(
        &self,
        session: &SessionRef,
        chat_id: &str,
        pin: flux_chat::ResolvedPin,
    ) -> anyhow::Result<SwitchOutcome> {
        {
            let mut chats = self.manager.chats.write().await;
            let Some(chat) = chats.get_mut(chat_id) else {
                return Ok(SwitchOutcome::NotFound);
            };
            match &chat.lease {
                Some(owner) if !chat.lease_held_by(session) => {
                    return Ok(SwitchOutcome::Busy);
                }
                _ => {}
            }
            self.persist_provider_pin(chat_id, &pin.id, &pin.model)
                .await;
            chat.provider_id = pin.id.clone();
            chat.model = pin.model.clone();
            // The instance syncs with the pin: the next re-begin lands on
            // the swapped provider (a stale instance would silently revert
            // the model override).
            chat.provider = Some(pin.provider.clone());
            // Non-blocking: a full router queue drops the notice (the chat
            // list carries the truth).
            chat.router.try_wire(WireEvent::ProviderSwitched {
                provider: pin.id.clone(),
                model: pin.model.clone(),
            });
            // The rebuild rides the consumer's control channel with the
            // fresh instance; no live engine — nothing to rebuild (the
            // next lazy spawn reads the fresh pin).
            if let Some(handle) = chat.live_task() {
                handle.rebuild(Some(pin));
            }
        }
        Ok(SwitchOutcome::Ok)
    }

    /// Persist the provider pin directly (the no-live-task path).
    async fn persist_provider_pin(&self, chat_id: &str, provider_id: &str, model: &str) {
        // One semantic unit: the pin pair lands together or not at all
        // (a torn pair would respawn on a new provider with a stale model).
        let pair: &[(&str, &str)] = match model.is_empty() {
            true => &[("provider", provider_id)],
            false => &[("provider", provider_id), ("model", model)],
        };
        if let Err(e) = self.store.save_state_entries(chat_id, pair).await {
            tracing::warn!(chat_id, error = %e, "failed to persist provider pin");
        }
    }

    /// The source chat's persisted pin (registry id + model) — the fork
    /// handler resolves the provider INSTANCE from it BEFORE forking, so
    /// a dead pin refuses the fork with an inline error (mirroring
    /// create_chat's pin gate) instead of forking into a chat that cannot
    /// spawn.
    pub async fn chat_pin(&self, chat_id: &str) -> Option<(String, String)> {
        let chats = self.manager.chats.read().await;
        chats
            .get(chat_id)
            .map(|c| (c.provider_id.clone(), c.model.clone()))
    }

    /// Fork a conversation from a message: a NEW chat holding a copy of
    /// the source transcript up to but EXCLUDING `fork_point` (a USER
    /// message row of that chat — the turn being redone; it re-enters the
    /// fork only when the user re-sends it, whose content the client
    /// prefills into the fork's composer), inheriting the source's workdir
    /// and provider pin. The SOURCE chat is untouched — a fork is a
    /// non-destructive read + create, so ANY viewer may trigger it (no
    /// lease gate; the source needs no quiescing either, a live round
    /// keeps running). The new chat's lease goes to the caller; the fresh
    /// `chats` broadcast carries it to every session, and the engine
    /// spawns lazily on the fork's first message — over the copied
    /// transcript (a fork IS the restart-from-a-message mechanism).
    pub async fn fork_chat(
        &self,
        session: &SessionRef,
        source_chat_id: &str,
        fork_point: i64,
        pin: flux_chat::ResolvedPin,
    ) -> Result<ChatInfoOwned, ForkFailure> {
        // Source existence (fast path, cache) + the name the fork carries.
        // The store's FK on forked_from_chat is the concurrent-deletion
        // backstop behind this read.
        let source_name = {
            let chats = self.manager.chats.read().await;
            let Some(chat) = chats.get(source_chat_id) else {
                return Err(ForkFailure::NotFound);
            };
            chat.name.clone()
        };
        let new_id = new_chat_id();
        let data = self
            .store
            .fork_chat(
                source_chat_id,
                fork_point,
                &new_id,
                &format!("{source_name} (fork)"),
            )
            .await
            .map_err(
                |e| match e.downcast::<flux_store::messages::BadForkPoint>() {
                    Ok(_) => ForkFailure::BadPoint,
                    Err(e) => {
                        tracing::warn!(
                            chat_id = %source_chat_id,
                            fork_point,
                            error = %e,
                            "fork failed"
                        );
                        ForkFailure::Internal(e.to_string())
                    }
                },
            )?;
        // The forker navigates to the fork: the source lease hands over with
        // the navigation — released HERE, before attach_new_chat's broadcast,
        // so that frame is already truthful (source free) and no window
        // renders the source In-use until the client's own CloseChat lands
        // (its release broadcast would otherwise race the client's
        // badge-suppression window with a stale still-leased frame). A
        // viewer-forker holds no lease and a foreign holder is untouched
        // (the quiet release's guard); the released subscription stays —
        // the client's CloseChat still unsubscribes it.
        self.release_lease_quiet(session, source_chat_id).await;
        Ok(self
            .attach_new_chat(
                session,
                new_id,
                format!("{source_name} (fork)"),
                data.created_at,
                data.workdir,
                pin,
                Some(source_chat_id.to_owned()),
            )
            .await)
    }

    /// Broadcast the chat list to every registered session (create/delete/
    /// rename/claim/release — the lease-transition signal). One element,
    /// fanned out over the identity registry.
    pub(crate) async fn broadcast_chats(&self) {
        let owned: Vec<ChatInfoOwned> = {
            let chats = self.manager.chats.read().await;
            chats.values().map(|c| c.info()).collect()
        };
        let el = crate::router::element(
            "",
            flux_proto::flux::v1::subscribe_response::Kind::Chats(
                flux_proto::flux::v1::ChatsBroadcast {
                    chats: owned.iter().map(Into::into).collect(),
                },
            ),
        );
        self.broadcast_element(el).await;
    }

    /// Broadcast ONE stream element to every live session. The manager is
    /// vocabulary-agnostic at this point — callers (the management plane's
    /// registry broadcasts) build the typed element; this is the single
    /// fan-out point over the identity registry.
    pub async fn broadcast_element(&self, el: flux_proto::flux::v1::SubscribeResponse) {
        let identities = self.manager.identities.read().await;
        for session in identities.values().filter(|s| s.is_live()) {
            session.send(el.clone());
        }
    }
}

/// How often the session reaper sweeps the detached table.
const REAP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Spawn the periodic session reaper driving the grace window.
/// Call once at transport startup. Without it, a genuinely closed tab
/// would hold its leases forever.
pub fn spawn_session_reaper(state: Arc<ServerState>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(REAP_INTERVAL);
        loop {
            interval.tick().await;
            state.reap_detached().await;
        }
    });
}

#[cfg(test)]
#[path = "ops_tests.rs"]
mod tests;
