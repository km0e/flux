//! ChatManager operations — lease/subscription bookkeeping, broadcast, and
//! lease gating for every mutating chat operation.
//!
//! Lock order invariant: ALWAYS acquire `chats` before `identities` (router
//! fanout only takes `chats`). Violating this order can deadlock the
//! manager. Identity interior state (std mutexes) is never held across an
//! await.

use crate::identity::{Session, SessionRef, SharedSessionSink};
use crate::manager::{CachedChat, ChatInfoOwned, ServerState, new_chat_id};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    Ok,
    /// The request carried an idempotency key (client_msg_id) this chat
    /// already accepted — the turn was absorbed, NOT enqueued again.
    Duplicate,
    Busy,
    NotFound,
}

/// Result of a rebase request — carries the resolved base so the wire
/// response can echo the ACTUAL rebase point (the context_rebased stream
/// event carries the same value).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebaseOutcome {
    /// Rebased; the resolved base message id.
    Rebased(i64),
    /// Nothing to do (no lease holder, or the base failed to
    /// resolve/persist) — the context is unchanged and nothing echoed.
    Noop,
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

/// Holder check: the opaque identity comparison (same allocation). The
/// lease/viewer structures hold `SessionRef`; the token string never
/// participates in authorization.
impl ServerState {
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
    /// identity — and its leases — survive for the grace window waiting for
    /// a `session_resume`; only the live connection is dropped (viewer
    /// entries removed, so a resumed client's re-claim takes the
    /// fresh-subscribe path and receives history).
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
        for chat in chats.values_mut() {
            if chat.viewers.remove(&session.sn()).is_some() {
                chat.router.viewer_gone(session.sn());
            }
        }
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
        // window. An expired identity is invisible here — the reaper owns
        // it (the read lock suffices: adoption mutates the identity's
        // interior state and the viewer maps, never the identities map).
        let live = session.is_live();
        if !live {
            let detached_for = session.detached_for()?;
            if detached_for >= self.manager.grace() {
                return None;
            }
        }
        session.attach(sink);
        // Connection state resets on adoption: drop the superseded
        // connection's viewer entries (drop marks go with them) so the
        // rebuilt client's re-claim takes the fresh-subscribe path and
        // receives the history + state snapshots it needs to render. The
        // leases — the expensive, precious part — persist untouched.
        for chat in chats.values_mut() {
            if chat.viewers.remove(&session.sn()).is_some() {
                chat.router.viewer_gone(session.sn());
            }
        }
        let leases = chats
            .values()
            .filter(|c| c.lease_held_by(session))
            .map(|c| c.id.clone())
            .collect();
        Some((SessionRef::clone(session), leases))
    }

    /// Look up a live-or-grace-window identity by token WITHOUT attaching
    /// anything — the read-only identity resolution the Connect surface's
    /// lease-gated RPCs use (the session token rides request metadata) and
    /// `ResumeSession`'s no-sink variant. Returns the identity handle plus
    /// the chats whose lease it holds.
    pub async fn session_leases(&self, token: &str) -> Option<(SessionRef, Vec<String>)> {
        let chats = self.manager.chats.read().await;
        let identities = self.manager.identities.read().await;
        let session = identities.get(token)?;
        let live = session.is_live();
        if !live {
            let detached_for = session.detached_for()?;
            if detached_for >= self.manager.grace() {
                return None;
            }
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
            // Claim is the single open message — the operator path
            // no longer needs chat_open. Ordering invariant: the history
            // snapshot is enqueued FIRST (the sink's content queue is
            // FIFO), then ensure_viewer activates the subscription, so
            // any live frame after it lands behind the history. The store
            // read happens under the chats write lock (milliseconds, same
            // class as send_message's ensure_task — accepted, T-04).
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
                                        // messages by it (rebase base,
                                        // context_rebased correlation).
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
        identities.get(token).is_some_and(|s| {
            s.is_live() || s.detached_for().is_some_and(|d| d < self.manager.grace())
        })
    }

    /// The resume grace window — the terminal side channel reuses it:
    /// a PTY outlives its socket by exactly this long before the reaper
    /// kills it (a refresh re-attaches within the window).
    pub fn session_grace(&self) -> std::time::Duration {
        self.manager.grace()
    }

    /// Return the lease; the subscription (if any) is kept. The client-facing
    /// exit path was merged into `chat_close` (unsubscribe_chat releases the
    /// lease automatically) — this method stays as an internal primitive for
    /// tests and lifecycle logic that manipulate the lease directly.
    pub async fn release_chat(&self, session: &SessionRef, chat_id: &str) {
        let released = {
            let mut chats = self.manager.chats.write().await;
            if let Some(chat) = chats.get_mut(chat_id)
                && chat.lease_held_by(session)
            {
                chat.lease = None;
                true
            } else {
                false
            }
        };
        // Lease released → active flips false: broadcast the list (create/
        // delete/rename already broadcast; lease changes need it too, or other
        // windows' sidebars go stale).
        if released {
            self.broadcast_chats().await;
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
    /// the claim snapshot — accepted, T-04).
    ///
    /// A repeat open by an already-subscribed viewer skips the history
    /// snapshot (idempotent — history is delivered only with the first
    /// subscription; a `chat_close` + re-open restarts it) but re-sends
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
                    // missing frame (C5).
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

    /// Create a chat: persist to the store (unchanged S1/M4 guards), insert
    /// into the cache with a fresh router, and grant the creator the lease
    /// plus a subscription. The task is NOT spawned — first message spawns
    /// it lazily (spec §4.1). The provider instance arrives already
    /// resolved (the server registry selected it at the session's
    /// `chat_create` dispatch) and is kept in the cache so every later
    /// task spawn runs on the SAME provider; the id/model labels persist
    /// explicitly so respawns and the UI never re-derive them.
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
        let router = RouterHandle::spawn(id.clone(), Arc::clone(&self.manager));
        let info = ChatInfoOwned {
            chat_id: id.clone(),
            name: name.to_owned(),
            created_at: created_at.clone(),
            // A new chat's first activity is its creation.
            last_activity_at: created_at.clone(),
            active: true,
            workdir: workdir.clone(),
            provider: pin.id.clone(),
            model: pin.model.clone(),
        };
        {
            let mut chats = self.manager.chats.write().await;
            let mut entry = CachedChat {
                seq: std::sync::atomic::AtomicU64::new(0),
                id: id.clone(),
                name: name.to_owned(),
                created_at,
                last_activity_at: info.last_activity_at.clone(),
                workdir,
                provider_id: pin.id.clone(),
                model: pin.model.clone(),
                provider: Some(pin.provider),
                lease: Some(SessionRef::clone(session)),
                viewers: HashMap::new(),
                questions: Arc::new(flux_chat::question::QuestionBoard::new()),
                router,
                task: None,
                recent_msg_ids: std::collections::VecDeque::new(),
            };
            entry.ensure_viewer(session);
            chats.insert(id, entry);
        }
        self.broadcast_chats().await;
        Ok(info)
    }

    /// Gate on the lease: empty or self → proceed; another holder → Busy.
    /// Shuts down the router, aborts the running task, removes the record,
    /// and broadcasts the list to all sessions (M14 fix). The router has
    /// already exited (FIFO `Shutdown`), so no error frame is broadcast.
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

    /// Restart from a message (rebase): rebuild the conversation context
    /// to live only above `base`. Only the lease holder may rebase (same
    /// gate as cancel). The base is a truth source, persisted AT REQUEST
    /// TIME (a crash after the persist realizes the intent on resume —
    /// the request is durable). `None` (to-latest) resolves against the
    /// current max: messages a racing round appends AFTER the request
    /// stay live (they are newer than the request — an explicit semantic
    /// change from the old apply-time resolution, pinned by tests). The
    /// engine rebuilds at the next boundary — a live round finishes
    /// first; the respawn loads the live context above the base. No task
    /// yet (an empty chat that never sent a message) is a Noop — there is
    /// nothing to archive or reload. The resolved base rides
    /// [`RebaseOutcome::Rebased`] so the wire response can echo the
    /// ACTUAL rebase point.
    pub async fn rebase_chat(
        &self,
        session: &SessionRef,
        chat_id: &str,
        base: Option<i64>,
    ) -> RebaseOutcome {
        let resolved = {
            let chats = self.manager.chats.read().await;
            let Some(chat) = chats.get(chat_id) else {
                return RebaseOutcome::NotFound;
            };
            match &chat.lease {
                Some(owner) if !chat.lease_held_by(session) => return RebaseOutcome::Busy,
                // No lease holder: nobody is acting on this chat — rebasing
                // is a mutation, so a passive viewer must not trigger it.
                None => return RebaseOutcome::Noop,
                Some(_) => {}
            }
            let resolved = match base {
                Some(b) => b,
                None => match self.store.max_message_id(chat_id).await {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!(chat_id, error = %e, "failed to resolve the rebase base");
                        return RebaseOutcome::Noop;
                    }
                },
            };
            if let Err(e) = self
                .store
                .save_state_entry(chat_id, flux_chat::CONTEXT_BASE_KEY, &resolved.to_string())
                .await
            {
                tracing::warn!(chat_id, error = %e, "failed to persist the rebase base");
                return RebaseOutcome::Noop;
            }
            // The archive removed every tool call at/below the base from
            // the model's view — their buffered outputs die with them (the
            // live context above the base is the keep-set, one SQL).
            if let Err(e) = self.store.gc_buf_entries(chat_id, resolved).await {
                tracing::warn!(chat_id, error = %e, "failed to gc buf entries at the rebase");
            }
            // The mutator announces (the engine rebuild follows within the
            // quiesce window).
            chat.router.try_wire(WireEvent::ContextRebased {
                base_message_id: resolved,
            });
            resolved
        };
        self.request_restart(chat_id).await;
        RebaseOutcome::Rebased(resolved)
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
mod tests {
    use super::*;
    use crate::test_util::{
        DummyProvider, create_chat, find_chats, find_kind, register, sess, test_state, wait_for,
    };
    use flux_proto::flux::v1::subscribe_response::Kind;
    use flux_store::Store;

    #[tokio::test]
    async fn close_by_holder_releases_lease_and_broadcasts_the_list() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        // b registers BEFORE creation so it observes both broadcasts.
        let recorded_b = register(&state, "b").await;
        let (cid, _frames, _router) = create_chat(&state, "a", Arc::new(DummyProvider)).await;

        // Creation granted the lease → the broadcast shows active=true.
        wait_for(|| {
            find_chats(&recorded_b.lock().unwrap())
                .is_some_and(|c| c.chats.iter().any(|x| x.chat_id == cid && x.active))
        })
        .await;

        // a closes (full exit = unsubscribe + release): the lease release MUST
        // re-broadcast the list — otherwise other windows' In-use badges stay
        // stale until some unrelated event.
        state.unsubscribe_chat(&sess(&state, "a").await, &cid).await;
        wait_for(|| {
            find_chats(&recorded_b.lock().unwrap())
                .is_some_and(|c| c.chats.iter().any(|x| x.chat_id == cid && !x.active))
        })
        .await;

        // The lease really is free: b's send auto-claims and proceeds.
        assert!(matches!(
            state
                .send_message(&sess(&state, "b").await, &cid, "hi".into())
                .await
                .unwrap(),
            SendOutcome::Ok
        ));
    }

    #[tokio::test]
    async fn claim_grants_lease_and_viewer() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let recorded1 = register(&state, "s1").await;
        let info = state
            .create_chat(
                &sess(&state, "s1").await,
                "c",
                std::env::temp_dir().to_str().unwrap(),
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap();
        // create granted the lease to s1 → idempotent
        assert_eq!(
            state
                .claim_chat(&sess(&state, "s1").await, &info.chat_id)
                .await,
            ClaimOutcome::AlreadyOwned
        );
        // s1 releases → s2's claim grants the free lease
        state
            .release_chat(&sess(&state, "s1").await, &info.chat_id)
            .await;
        assert_eq!(
            state
                .claim_chat(&sess(&state, "s2").await, &info.chat_id)
                .await,
            ClaimOutcome::Granted
        );
        // s2 releases → s1's claim (steal path with no demotion — the lease
        // was already free) grants again
        state
            .release_chat(&sess(&state, "s2").await, &info.chat_id)
            .await;
        assert_eq!(
            state
                .claim_chat(&sess(&state, "s1").await, &info.chat_id)
                .await,
            ClaimOutcome::Granted
        );
        // No demotion frame ever fired — s1 never lost its lease to s2.
        assert!(find_kind(&recorded1.lock().unwrap(), "error").is_none());
    }

    #[tokio::test]
    async fn claim_steals_a_foreign_lease_and_demotes_the_holder_in_band() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let recorded1 = register(&state, "s1").await;
        let info = state
            .create_chat(
                &sess(&state, "s1").await,
                "c",
                std::env::temp_dir().to_str().unwrap(),
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap();
        // s2 claims a chat whose lease s1 holds → STEAL: the claim succeeds
        // and s1 is demoted in-band (the same chat_busy error a rejected
        // send produces — no new frame type).
        assert_eq!(
            state
                .claim_chat(&sess(&state, "s2").await, &info.chat_id)
                .await,
            ClaimOutcome::Granted
        );
        let demotion = {
            let guard = recorded1.lock().unwrap();
            find_kind(&guard, "error").unwrap().clone()
        };
        assert_eq!(demotion.chat_id, info.chat_id);
        match &demotion.kind {
            Some(Kind::Error(e)) => {
                assert_eq!(e.code, flux_proto::flux::v1::ErrorCode::ChatBusy as i32);
                // The demotion text NEVER carries the new holder's resume token —
                // the token is a bearer credential (the stream open adopts the
                // identity); leaking it into another client's stream would let
                // the reader take over the holder's session.
                assert_eq!(e.message, "Chat is in use by another session");
                assert!(!e.message.contains("s2"));
            }
            other => panic!("expected error element, got {other:?}"),
        }
        // The demoted holder's send is now rejected (it holds no lease).
        assert!(matches!(
            state
                .send_message(&sess(&state, "s1").await, &info.chat_id, "hi".into())
                .await
                .unwrap(),
            SendOutcome::Busy
        ));
        // The new holder's repeat claim is idempotent.
        assert_eq!(
            state
                .claim_chat(&sess(&state, "s2").await, &info.chat_id)
                .await,
            ClaimOutcome::AlreadyOwned
        );
    }

    #[tokio::test]
    async fn claim_delivers_history_snapshot_and_is_idempotent() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let (chat_id, recorded1, _router) =
            create_chat(&state, "s1", Arc::new(DummyProvider)).await;
        let recorded2 = register(&state, "s2").await;
        // Seed messages (written directly to the store — send_message would spawn the provider, so it is not used).
        state
            .store
            .append_messages(&chat_id, &[flux_core::Message::user("hi")])
            .await
            .unwrap();

        // Lease held by s1 → s2's claim STEALS it: Granted, the snapshot
        // (history + round state) arrives with the lease, and s1 is demoted
        // in-band (the same chat_busy error a rejected send produces).
        assert_eq!(
            state.claim_chat(&sess(&state, "s2").await, &chat_id).await,
            ClaimOutcome::Granted
        );
        let demoted = {
            let guard = recorded1.lock().unwrap();
            find_kind(&guard, "error").unwrap().clone()
        };
        assert!(matches!(
            &demoted.kind,
            Some(Kind::Error(e)) if e.code == flux_proto::flux::v1::ErrorCode::ChatBusy as i32
        ));

        // The steal delivered the history snapshot + round state with the
        // lease (single-message claim) — the new holder watches the chat
        // live from here on. Filter by type when asserting, never by
        // absolute index (the demotion and broadcasts interleave).
        let frames = recorded2.lock().unwrap().clone();
        let hist =
            find_kind(&frames, "chat_history").expect("claim must deliver a history snapshot");
        assert_eq!(hist.chat_id, chat_id);
        match &hist.kind {
            Some(Kind::ChatHistory(h)) => {
                assert_eq!(h.messages.len(), 1);
                assert_eq!(h.messages[0].role, flux_proto::flux::v1::Role::User as i32);
                assert_eq!(h.messages[0].content, "hi");
            }
            other => panic!("expected chat_history, got {other:?}"),
        }
        // Lazily-unspawned task → authoritative Idle snapshot.
        assert!(frames.iter().any(|el| matches!(
            &el.kind,
            Some(Kind::ChatState(s)) if s.state == flux_proto::flux::v1::ChatStateKind::Idle as i32
        )));

        // Idempotent: the new holder's repeated claim → AlreadyOwned, no
        // history/state resend.
        let before = recorded2.lock().unwrap().len();
        assert_eq!(
            state.claim_chat(&sess(&state, "s2").await, &chat_id).await,
            ClaimOutcome::AlreadyOwned
        );
        assert_eq!(recorded2.lock().unwrap().len(), before);

        // s2 releases → s1's claim grants the FREE lease (no demotion —
        // nobody held it). The release and the grant each broadcast the
        // list; the final one reflects s1 as the active holder.
        state
            .release_chat(&sess(&state, "s2").await, &chat_id)
            .await;
        assert_eq!(
            state.claim_chat(&sess(&state, "s1").await, &chat_id).await,
            ClaimOutcome::Granted
        );
        let last_list = {
            let guard = recorded2.lock().unwrap();
            find_chats(&guard)
                .expect("lease-release/claim must broadcast the list")
                .clone()
        };
        assert_eq!(last_list.chats[0].chat_id, chat_id);
        assert!(last_list.chats[0].active);
    }

    #[tokio::test]
    async fn create_chat_persists_current_dir_from_workdir() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let _recorded = register(&state, "s1").await;
        let workdir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let info = state
            .create_chat(
                &sess(&state, "s1").await,
                "c",
                &workdir,
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap();
        let persisted = state.store.load_state(&info.chat_id).await.unwrap();
        assert_eq!(
            persisted.get("workdir").map(String::as_str),
            Some(workdir.as_str())
        );
        assert_eq!(
            persisted.get("current_dir").map(String::as_str),
            Some(workdir.as_str())
        );
    }

    #[tokio::test]
    async fn claim_unknown_chat_returns_not_found() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let _recorded = register(&state, "s1").await;
        assert_eq!(
            state.claim_chat(&sess(&state, "s1").await, "no-such").await,
            ClaimOutcome::NotFound
        );
    }

    #[tokio::test]
    async fn release_keeps_viewer_membership() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let _recorded = register(&state, "s1").await;
        let info = state
            .create_chat(
                &sess(&state, "s1").await,
                "c",
                std::env::temp_dir().to_str().unwrap(),
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap();
        let s1 = sess(&state, "s1").await;
        state.release_chat(&s1, &info.chat_id).await;
        // Releasing does not kill the subscription: s1 is still among the viewers
        assert!(
            state
                .manager
                .chats
                .read()
                .await
                .get(&info.chat_id)
                .unwrap()
                .viewers
                .contains_key(&s1.sn()),
            "release must keep the viewer membership"
        );
        // and s1 can immediately re-claim (the lease is free)
        assert_eq!(
            state
                .claim_chat(&sess(&state, "s1").await, &info.chat_id)
                .await,
            ClaimOutcome::Granted
        );
    }

    #[tokio::test]
    async fn subscribe_is_idempotent_and_unsubscribe_removes_viewer() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let _recorded = register(&state, "s1").await;
        let info = state
            .create_chat(
                &sess(&state, "s1").await,
                "c",
                std::env::temp_dir().to_str().unwrap(),
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap();
        // Subscribing to a nonexistent chat → NotFound
        assert_eq!(
            state
                .subscribe_chat(&sess(&state, "s2").await, "no-such")
                .await,
            SubscribeOutcome::NotFound
        );
        assert_eq!(
            state
                .subscribe_chat(&sess(&state, "s2").await, &info.chat_id)
                .await,
            SubscribeOutcome::Subscribed
        );
        // Idempotent: a repeated subscription still reports Subscribed
        assert_eq!(
            state
                .subscribe_chat(&sess(&state, "s2").await, &info.chat_id)
                .await,
            SubscribeOutcome::Subscribed
        );
        state
            .unsubscribe_chat(&sess(&state, "s2").await, &info.chat_id)
            .await;
        assert!(
            !state
                .manager
                .chats
                .read()
                .await
                .get(&info.chat_id)
                .unwrap()
                .viewers
                .contains_key(&sess(&state, "s2").await.sn())
        );
    }

    #[tokio::test]
    async fn chat_exists_reflects_cache_membership_without_side_effect() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        // Nonexistent → false (no side effects — no viewer was subscribed).
        assert!(!state.chat_exists("no-such").await);
        let _recorded = register(&state, "s1").await;
        let info = state
            .create_chat(
                &sess(&state, "s1").await,
                "c",
                std::env::temp_dir().to_str().unwrap(),
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap();
        // After creation → true.
        assert!(state.chat_exists(&info.chat_id).await);
        // Read-only: joins no viewer (no subscription side effects).
        assert!(
            !state
                .manager
                .chats
                .read()
                .await
                .get(&info.chat_id)
                .unwrap()
                .viewers
                .contains_key(&sess(&state, "s2").await.sn()),
            "chat_exists must not subscribe the session"
        );
    }

    #[tokio::test]
    async fn create_grants_lease_to_creator_and_broadcasts_chats() {
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let state = test_state(store).await;
        let a = register(&state, "a").await;
        let b = register(&state, "b").await;
        let info = state
            .create_chat(
                &sess(&state, "a").await,
                "c",
                std::env::temp_dir().to_str().unwrap(),
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap();
        assert!(info.active, "creator starts with the lease");
        // Broadcast to every session (including the creator itself)
        for rec in [&a, &b] {
            wait_for(|| find_chats(&rec.lock().unwrap()).is_some()).await;
        }
        let chats_a = {
            let guard = a.lock().unwrap();
            find_chats(&guard).unwrap().clone()
        };
        assert_eq!(chats_a.chats[0].chat_id, info.chat_id);
        assert!(chats_a.chats[0].active);
    }

    #[tokio::test]
    async fn delete_refused_while_lease_held_by_other_session() {
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let state = test_state(store.clone()).await;
        let _a = register(&state, "a").await;
        let _b = register(&state, "b").await;
        let info = state
            .create_chat(
                &sess(&state, "a").await,
                "c",
                std::env::temp_dir().to_str().unwrap(),
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap();
        // b tries to delete a's active chat → rejected (the lease model)
        assert_eq!(
            state
                .delete_chat(&sess(&state, "b").await, &info.chat_id)
                .await,
            MutateOutcome::Busy
        );
        assert!(
            store
                .list_chats()
                .await
                .unwrap()
                .iter()
                .any(|s| s.chat_id == info.chat_id)
        );
        assert!(state.manager.chats.read().await.contains_key(&info.chat_id));
    }

    #[tokio::test]
    async fn delete_removes_record_and_broadcasts_when_lease_free() {
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let state = test_state(store.clone()).await;
        let a = register(&state, "a").await;
        let b = register(&state, "b").await;
        let info = state
            .create_chat(
                &sess(&state, "a").await,
                "c",
                std::env::temp_dir().to_str().unwrap(),
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap();
        // Lease released → free for any session to delete (the abort path for
        // a running task is covered by lifecycle's
        // delete_chat_aborts_task_without_crash_notice)
        state
            .release_chat(&sess(&state, "a").await, &info.chat_id)
            .await;
        assert_eq!(
            state
                .delete_chat(&sess(&state, "b").await, &info.chat_id)
                .await,
            MutateOutcome::Ok
        );
        // The cache entry was removed
        assert!(!state.manager.chats.read().await.contains_key(&info.chat_id));
        // The store row was removed
        assert!(
            !store
                .list_chats()
                .await
                .unwrap()
                .iter()
                .any(|s| s.chat_id == info.chat_id)
        );
        // Broadcast the chats list to every session (deleter included).
        // recorded_type returns the FIRST chats frame — the stale pre-delete
        // list; the post-delete broadcast is the last one and the list is
        // empty. Wait for the empty-list frame, then assert each session's
        // last frame reflects the deletion.
        wait_for(|| find_chats(&b.lock().unwrap()).is_some_and(|c| c.chats.is_empty())).await;
        for rec in [&a, &b] {
            let last = {
                let guard = rec.lock().unwrap();
                find_chats(&guard)
                    .expect("delete broadcast recorded")
                    .clone()
            };
            assert!(
                last.chats.is_empty(),
                "final chats broadcast must show the chat removed"
            );
        }
    }

    #[tokio::test]
    async fn rename_requires_lease_and_broadcasts() {
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let state = test_state(store).await;
        let _a = register(&state, "a").await;
        let b = register(&state, "b").await;
        let info = state
            .create_chat(
                &sess(&state, "a").await,
                "c",
                std::env::temp_dir().to_str().unwrap(),
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap();
        // A non-holder renaming → Busy
        assert_eq!(
            state
                .rename_chat(&sess(&state, "b").await, &info.chat_id, "x")
                .await,
            MutateOutcome::Busy
        );
        // The holder renames → Ok + broadcast. The creation-time broadcast with
        // the old name was recorded first — wait for the new-name frame
        // (recorded_type returns the first chats frame, which carries the old name).
        assert_eq!(
            state
                .rename_chat(&sess(&state, "a").await, &info.chat_id, "x")
                .await,
            MutateOutcome::Ok
        );
        wait_for(|| {
            find_chats(&b.lock().unwrap())
                .is_some_and(|c| c.chats.first().is_some_and(|x| x.name == "x"))
        })
        .await;
        let last = {
            let guard = b.lock().unwrap();
            find_chats(&guard)
                .expect("rename broadcast recorded")
                .clone()
        };
        assert_eq!(last.chats[0].name, "x");
    }

    #[tokio::test]
    async fn detach_keeps_lease_and_drops_viewers() {
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let state = test_state(store).await;
        let sa = sess(&state, "a").await;
        let sb = sess(&state, "b").await;
        let info = state
            .create_chat(
                &sa,
                "c",
                std::env::temp_dir().to_str().unwrap(),
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap();
        state.subscribe_chat(&sb, &info.chat_id).await;
        state.detach_session(&sa, &sa.sink().unwrap()).await;
        // Detach semantics: viewer entries dropped, but the LEASE (and
        // the detached marker) survive the grace window.
        {
            let chats = state.manager.chats.read().await;
            let chat = chats.get(&info.chat_id).unwrap();
            assert!(
                chat.lease_held_by(&sa),
                "detach must keep the lease for the grace window"
            );
            assert!(
                !chat.viewers.contains_key(&sa.sn()),
                "the dead connection's viewer entry is dropped"
            );
            assert!(chat.viewers.contains_key(&sb.sn()), "other viewers survive");
        }
        assert!(sa.detached_for().is_some());
        state
            .manager
            .grace_ms
            .store(0, std::sync::atomic::Ordering::Relaxed);
        // The reaper completes the deferred teardown: lease released...
        state.reap_detached().await;
        let chats = state.manager.chats.read().await;
        let chat = chats.get(&info.chat_id).unwrap();
        assert!(
            chat.lease.is_none(),
            "the reaper releases the expired lease"
        );
        // ...and the detached marker is consumed.
        assert!(sa.detached_for().is_none());
    }

    #[tokio::test]
    async fn chat_list_marks_lease_holder_active() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let _a = register(&state, "a").await;
        let info = state
            .create_chat(
                &sess(&state, "a").await,
                "c",
                std::env::temp_dir().to_str().unwrap(),
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap();
        {
            let guard = state.chat_info_guard().await;
            assert!(guard.info(&info.chat_id).unwrap().active);
        }
        state
            .release_chat(&sess(&state, "a").await, &info.chat_id)
            .await;
        {
            let guard = state.chat_info_guard().await;
            assert!(!guard.info(&info.chat_id).unwrap().active);
        }
    }

    #[tokio::test]
    async fn lease_transition_broadcasts_active_flag() {
        // Regression: broadcast_chats is only triggered on create/delete/
        // rename — lease claims/releases (including disconnect auto-release)
        // left other windows' sidebar `active` flag stale. Claim/release must
        // broadcast the fresh list too.
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let state = test_state(store).await;
        let _a = register(&state, "a").await;
        let b = register(&state, "b").await;
        let info = state
            .create_chat(
                &sess(&state, "a").await,
                "c",
                std::env::temp_dir().to_str().unwrap(),
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap();
        // The create broadcast active=true to b.
        wait_for(|| {
            find_chats(&b.lock().unwrap())
                .is_some_and(|c| c.chats.first().is_some_and(|x| x.active))
        })
        .await;

        // Releasing the lease → broadcast active=false.
        state
            .release_chat(&sess(&state, "a").await, &info.chat_id)
            .await;
        wait_for(|| {
            find_chats(&b.lock().unwrap())
                .is_some_and(|c| c.chats.first().is_some_and(|x| !x.active))
        })
        .await;

        // Another session claims → broadcast active=true.
        state
            .claim_chat(&sess(&state, "b").await, &info.chat_id)
            .await;
        let last = {
            let guard = b.lock().unwrap();
            find_chats(&guard)
                .expect("claim broadcast recorded")
                .clone()
        };
        assert!(
            last.chats.first().is_some_and(|x| x.active),
            "claim must broadcast the chat as active again"
        );
    }

    #[tokio::test]
    async fn create_chat_accepts_any_resolvable_workdir() {
        // No project-root allowlist — any resolvable
        // directory is a valid workdir (the boundary is the OS/container's
        // job, not this layer's).
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let state = test_state(store).await;
        let _a = register(&state, "a").await;
        let outside = std::env::temp_dir().join("flux-roots-outside");
        std::fs::create_dir_all(&outside).unwrap();
        let inside = outside.join("proj");
        std::fs::create_dir_all(&inside).unwrap();

        let info = state
            .create_chat(
                &sess(&state, "a").await,
                "c1",
                outside.to_str().unwrap(),
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await
            .expect("any resolvable directory is accepted");
        assert_eq!(
            info.workdir,
            std::fs::canonicalize(&outside).unwrap().to_str().unwrap(),
            "ChatInfo carries the canonicalized workdir"
        );
        let _ = inside; // keep the dir alive to the end of the test
    }

    #[tokio::test]
    async fn detach_defers_release_broadcast_to_the_reaper() {
        // a disconnects → DETACH: no broadcast yet, the lease is held
        // for the grace window ("in use" stays honest — the holder may come
        // back). The reaper completes the teardown: only THEN does b receive
        // a list frame where a is no longer active.
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let state = test_state(store).await;
        let sa = sess(&state, "a").await;
        let b = register(&state, "b").await;
        let info = state
            .create_chat(
                &sa,
                "c",
                std::env::temp_dir().to_str().unwrap(),
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap();
        state.detach_session(&sa, &sa.sink().unwrap()).await;
        state
            .manager
            .grace_ms
            .store(0, std::sync::atomic::Ordering::Relaxed);
        assert!(
            !find_chats(&b.lock().unwrap()).is_some_and(|c| {
                c.chats
                    .iter()
                    .any(|x| x.chat_id == info.chat_id && !x.active)
            }),
            "detach must not broadcast the release"
        );
        state.reap_detached().await;
        wait_for(|| {
            find_chats(&b.lock().unwrap()).is_some_and(|c| {
                c.chats
                    .iter()
                    .any(|x| x.chat_id == info.chat_id && !x.active)
            })
        })
        .await;
    }
}
