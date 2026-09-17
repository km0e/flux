//! Task lifecycle — lazy spawn, stale replacement, engine rebuild.
//!
//! A chat's runtime task is spawned on first message (lazy creation) and
//! lives for the chat's lifetime: the conversation engine (machine +
//! consumer + executor + connection) never dies for a truth-source
//! change. There is no crash observer: the done flag lets the next
//! `ensure_task` replace a terminated (crashed/exited) task — a
//! terminated task is exactly as stale as an exited one.
//!
//! The ENGINE REBUILD is the third lifecycle move, and it is a MESSAGE,
//! not a lifecycle event: a truth-source change (provider pin, the
//! global tool registry) persists the change, then `request_restart`
//! sends `Rebuild` on the live engine's control channel — the consumer
//! arms the machine's gate (a live round, and any turns queued behind
//! it, finishes first) and rebuilds the engine IN PLACE at the fired
//! gate, re-assembling from the truth sources with the same
//! deterministic assembly every spawn uses. No live engine: nothing to
//! quiesce — the next lazy spawn reads the fresh truth.

use crate::manager::{CachedChat, ServerState};
use crate::router::ChannelOutput;
use flux_core::OutputPort;
use std::sync::Arc;

/// A chat's runtime task handle, stored in the chat entry.
pub(crate) struct ChatTask {
    pub(crate) handle: flux_chat::handle::ChatHandle,
}

impl ServerState {
    /// Lazily spawn the chat's runtime task. Caller must hold the chat
    /// entry via `&mut` from the `manager.chats` write lock (send_message
    /// does). No-op while a live task exists; a terminated task (done
    /// flag — clean halt or child-task exit) is replaced.
    pub(crate) async fn ensure_task(self: &Arc<Self>, chat: &mut CachedChat) -> anyhow::Result<()> {
        if chat.live_task().is_some() {
            return Ok(());
        }
        self.spawn_task(chat).await.map(|_| ())
    }

    /// Spawn the engine from the truth sources. Caller holds the chat
    /// entry via `&mut` from the `manager.chats` write lock (the spawn's
    /// store reads ride the lock — milliseconds, same class as the claim
    /// snapshot; splitting them out two-phase would break the
    /// assemble-under-lock invariant for a single-user local server).
    async fn spawn_task(
        self: &Arc<Self>,
        chat: &mut CachedChat,
    ) -> anyhow::Result<flux_chat::handle::ChatHandle> {
        let output: Arc<dyn OutputPort> = Arc::new(ChannelOutput {
            router: chat.router.clone(),
        });
        // Rebirth with context: the FULL persisted transcript is the live
        // context (a chat's transcript only grows; forking, not archiving,
        // is how a conversation restarts from a message). A load failure
        // propagates — an answer with zero context is worse than a visible
        // error. The read-side invariant guard keeps the re-begin history
        // provider-valid (see flux_chat::history).
        let history = flux_chat::validate_history(self.store.load_messages(&chat.id).await?);
        // The chat's provider instance: the hydrated/resolved pin. An
        // unresolvable pin (registry changed across restarts) is an
        // EXPLICIT error naming the dead pin — never a silent fallback to
        // some other provider. Recovery: a SwitchProvider swap, then send.
        let provider = chat.provider.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "pinned provider '{}' is no longer registered — switch providers to continue",
                chat.provider_id
            )
        })?;
        let handle = flux_chat::spawn::spawn(
            flux_chat::chat::ChatInit {
                id: chat.id.clone(),
                history,
                questions: Arc::clone(&chat.questions),
                provider,
            },
            Arc::clone(&self.system_prompt),
            Arc::clone(&self.tool_registry),
            Arc::clone(&self.store),
            Arc::clone(&self.initial_state),
            output,
        )
        .await;
        chat.task = Some(ChatTask {
            handle: handle.clone(),
        });
        // Round boundaries → sidebar truth: the engine's round-state watch
        // fires exactly on the Idle↔Streaming transitions (send mid-round
        // wraps report both), and each flip re-broadcasts the chat list so
        // EVERY window's sidebar `running` flag stays current — not just
        // this chat's viewers. One spawn per task (lazy, on first message
        // or stale replacement); the watcher exits when the task dies
        // (clean halt, abort, chat deletion) — its sender drops with the
        // consumer's deps.
        let mut state_rx = handle.subscribe_state();
        let state = Arc::clone(self);
        tokio::spawn(async move {
            let mut saw_running = false;
            while state_rx.changed().await.is_ok() {
                saw_running = *state_rx.borrow() == flux_core::ChatStateKind::Streaming;
                state.broadcast_chats().await;
            }
            // The task died: the slot never writes Idle on the way out, so
            // a sidebar last shown a running chat would spin forever. One
            // final broadcast — `info()` reads the live-task truth (a dead
            // task is filtered), so it clears the flag (or drops the chat).
            if saw_running {
                state.broadcast_chats().await;
            }
        });
        Ok(handle)
    }

    /// Request an engine rebuild for one chat. The CALLER mutates the
    /// truth sources first (persist the pin; the global registry is
    /// already current), then calls this: the `Rebuild` command rides the
    /// live engine's control channel, the machine's gate arms (a live
    /// round — and queued turns — finish first), and the consumer
    /// rebuilds the engine IN PLACE at the fired gate. No live task:
    /// nothing to quiesce — the next lazy spawn reads the fresh truth.
    pub(crate) async fn request_restart(&self, chat_id: &str) {
        let handle = {
            let chats = self.manager.chats.read().await;
            chats.get(chat_id).and_then(|c| c.live_task()).cloned()
        };
        if let Some(handle) = handle {
            handle.rebuild(None);
        }
    }

    /// Rebuild every chat pinned to `(provider, model)`. Model-registry
    /// changes (params saved) are truth the request builder bakes into the
    /// connection at begin — exactly the matching chats rebuild, at the
    /// machine gate as always. Chats without a live task skip the quiesce;
    /// their next lazy spawn reads the fresh params anyway.
    pub async fn restart_chats_matching(&self, provider: &str, model: &str) {
        let ids: Vec<String> = self
            .manager
            .chats
            .read()
            .await
            .iter()
            .filter(|(_, c)| c.provider_id == provider && c.model == model)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.request_restart(&id).await;
        }
    }

    /// Re-resolve every chat pinned to `provider_id` against the CURRENT
    /// registry and push the fresh instance to the chat cache AND its live
    /// engines — the endpoint-edit path (UpdateProvider): url / api_key
    /// changed BEHIND the pin, so the pin (id + model) stays while the
    /// instance must not. The resolver arrives from the server registry
    /// (the session layer owns no provider registry). The mechanics are
    /// the switch path's: cache sync (`chat.provider`), then a
    /// carried-pin `Rebuild` — a live round and queued turns finish
    /// first, the fresh endpoint rides the re-begin at the fired gate; a
    /// chat without a live task just re-reads its cache at the next lazy
    /// spawn. A pin that no longer resolves (a racing remove) keeps its
    /// stale instance — the next round fails naming the pin (the remove
    /// semantics). No `provider_switched` wire notice: the pin didn't
    /// move, only the endpoint behind it did.
    pub async fn refresh_chats_pinned_to(
        &self,
        provider_id: &str,
        resolve: impl Fn(&str, &str) -> Option<Arc<dyn flux_core::Provider>>,
    ) {
        // Snapshot the matching chats' (id, model) under the read lock;
        // the per-chat work takes the write lock one entry at a time.
        let matching: Vec<(String, String)> = {
            let chats = self.manager.chats.read().await;
            chats
                .iter()
                .filter(|(_, c)| c.provider_id == provider_id)
                .map(|(id, c)| (id.clone(), c.model.clone()))
                .collect()
        };
        for (chat_id, model) in matching {
            let Some(provider) = resolve(provider_id, &model) else {
                continue; // unresolvable (racing remove): keep the stale pin
            };
            let mut chats = self.manager.chats.write().await;
            let Some(chat) = chats.get_mut(&chat_id) else {
                continue; // reaped between the snapshot and the lock
            };
            if chat.provider_id != provider_id {
                continue; // re-pinned meanwhile — never clobber a newer pin
            }
            chat.provider = Some(provider.clone());
            if let Some(handle) = chat.live_task() {
                handle.rebuild(Some(flux_chat::ResolvedPin {
                    provider,
                    id: chat.provider_id.clone(),
                    model: chat.model.clone(),
                }));
            }
        }
    }

    /// Rebuild EVERY chat's engine. Tool-registry changes are global
    /// truth: each respawn re-assembles from the CURRENT global registry.
    /// Chats without a live task skip the quiesce — their next lazy spawn
    /// reads the fresh registry anyway.
    pub async fn restart_all_chats(&self) {
        let ids: Vec<String> = self.manager.chats.read().await.keys().cloned().collect();
        for id in ids {
            self.request_restart(&id).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{CancelOutcome, MutateOutcome, QuestionOutcome, SendOutcome};
    use crate::test_util::{
        DummyProvider, ScriptItem, create_chat, find_kind, register, sess, test_state, wait_for,
        wait_for_kind,
    };
    use flux_core::StreamChunk;
    use flux_proto::flux::v1::subscribe_response::Kind;
    use flux_store::Store;
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;

    async fn task_exists(state: &ServerState, chat_id: &str) -> bool {
        state
            .manager
            .chats
            .read()
            .await
            .get(chat_id)
            .map(|c| c.task.is_some())
            .unwrap_or(false)
    }

    fn stream_end_count(rec: &[flux_proto::flux::v1::SubscribeResponse]) -> usize {
        rec.iter()
            .filter(|el| matches!(&el.kind, Some(Kind::StreamEnd(_))))
            .count()
    }

    /// Provider handing out a fresh scripted connection per spawn; the
    /// first begin consumes the whole script (later begins — a respawn —
    /// get empty connections, exactly the old replay-factory semantics).
    fn replay_provider(script: crate::test_util::Script) -> Arc<dyn flux_core::Provider> {
        Arc::new(crate::test_util::ScriptedProvider::replay(script))
    }

    /// Provider whose connection yields one delta then never ends — the
    /// delete-abort path's victim.
    fn hang_provider() -> Arc<dyn flux_core::Provider> {
        Arc::new(crate::test_util::ScriptedProvider::once(
            crate::test_util::hang_script(),
        ))
    }

    #[tokio::test]
    async fn first_message_spawns_task_and_streams_round_to_creator() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let provider = replay_provider(vec![vec![
            ScriptItem::Chunk(Ok(StreamChunk::Text("hi".into()))),
            ScriptItem::Chunk(Ok(StreamChunk::End {
                finish_reason: None,
            })),
        ]]);
        let (cid, frames, _router) = create_chat(&state, "a", provider).await;

        // Creation does NOT spawn the task — it starts lazily on the first message.
        assert!(!task_exists(&state, &cid).await);

        let out = state
            .send_message(&sess(&state, "a").await, &cid, "hello".into())
            .await
            .unwrap();
        assert_eq!(out, SendOutcome::Ok);

        // Task spawned; streaming content reaches the creator (lease holder = subscriber).
        assert!(task_exists(&state, &cid).await);
        wait_for_kind(&frames, "text_delta").await;
        wait_for_kind(&frames, "stream_end").await;
    }

    #[tokio::test]
    async fn round_completion_keeps_task_and_release_keeps_task_alive() {
        // Scripts for two rounds; the take-once provider proves the task is
        // not re-spawned between rounds (a re-spawn would trigger a second
        // begin, which the take-once session rejects outright).
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let provider = replay_provider(vec![
            vec![
                ScriptItem::Chunk(Ok(StreamChunk::Text("one".into()))),
                ScriptItem::Chunk(Ok(StreamChunk::End {
                    finish_reason: None,
                })),
            ],
            vec![
                ScriptItem::Chunk(Ok(StreamChunk::Text("two".into()))),
                ScriptItem::Chunk(Ok(StreamChunk::End {
                    finish_reason: None,
                })),
            ],
        ]);
        let (cid, frames, _router) = create_chat(&state, "a", provider).await;

        state
            .send_message(&sess(&state, "a").await, &cid, "m1".into())
            .await
            .unwrap();
        wait_for_kind(&frames, "text_delta").await;

        // Release the lease: the task keeps running, the subscription stays (release mid-stream).
        state.release_chat(&sess(&state, "a").await, &cid).await;
        assert!(task_exists(&state, &cid).await);

        // Send another round: the lease is free → auto-claim + the SAME task is
        // reused (the second round's text comes from the same session's second
        // script segment; no crash frames).
        state
            .send_message(&sess(&state, "a").await, &cid, "m2".into())
            .await
            .unwrap();
        wait_for(|| stream_end_count(&frames.lock().unwrap()) >= 2).await;

        {
            let rec = frames.lock().unwrap();
            let texts: Vec<String> = rec
                .iter()
                .filter_map(|el| match &el.kind {
                    Some(Kind::TextDelta(t)) => Some(t.delta.clone()),
                    _ => None,
                })
                .collect();
            assert_eq!(texts, vec!["one", "two"]);
            assert!(find_kind(&rec, "error").is_none());
        }
        // Auto-claim: after release, the lease returns to the sender.
        let lease = state
            .manager
            .chats
            .read()
            .await
            .get(&cid)
            .unwrap()
            .lease
            .clone();
        assert!(lease.as_ref().is_some_and(|h| h.token() == "a"));
    }

    #[tokio::test]
    async fn question_response_gated_to_lease_holder() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        register(&state, "b").await;
        let (cid, _frames, _router) = create_chat(&state, "a", Arc::new(DummyProvider)).await;

        // a is the creator (holds the lease) → Ok; b holds no lease → NotOwner
        // (the answer is dropped, not an error).
        assert_eq!(
            state
                .question_response(&sess(&state, "a").await, &cid, "q1", "yes".into())
                .await,
            QuestionOutcome::UnknownQuestion,
            "no pending question: answered-and-dropped, not Ok"
        );
        assert_eq!(
            state
                .question_response(&sess(&state, "b").await, &cid, "q1", "yes".into())
                .await,
            QuestionOutcome::NotOwner
        );
        // Unknown chat → NotFound.
        assert_eq!(
            state
                .question_response(&sess(&state, "a").await, "no-such", "q1", "yes".into())
                .await,
            QuestionOutcome::NotFound
        );
    }

    #[tokio::test]
    async fn cancel_gated_to_lease_holder_and_idle_is_noop() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        register(&state, "b").await;
        let (cid, _frames, _router) = create_chat(&state, "a", Arc::new(DummyProvider)).await;

        // a holds the lease → Ok; b → Busy.
        assert_eq!(
            state.cancel_chat(&sess(&state, "a").await, &cid).await,
            CancelOutcome::Ok
        );
        assert_eq!(
            state.cancel_chat(&sess(&state, "b").await, &cid).await,
            CancelOutcome::Busy
        );

        // Lease free after release: nobody can cancel → no-op Ok (a read-only client sends no cancels).
        state.release_chat(&sess(&state, "a").await, &cid).await;
        assert_eq!(
            state.cancel_chat(&sess(&state, "b").await, &cid).await,
            CancelOutcome::Ok
        );

        // Unknown chat → NotFound.
        assert_eq!(
            state.cancel_chat(&sess(&state, "a").await, "no-such").await,
            CancelOutcome::NotFound
        );
    }

    #[tokio::test]
    async fn delete_chat_aborts_task_and_removes_record() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let (cid, frames, _router) = create_chat(&state, "a", hang_provider()).await;

        // The task hangs on a stream that never ends.
        state
            .send_message(&sess(&state, "a").await, &cid, "go".into())
            .await
            .unwrap();
        wait_for_kind(&frames, "text_delta").await;
        assert!(task_exists(&state, &cid).await);

        // Delete: abort the task, remove the record, broadcast the list.
        assert_eq!(
            state.delete_chat(&sess(&state, "a").await, &cid).await,
            MutateOutcome::Ok
        );
        assert!(state.manager.chats.read().await.get(&cid).is_none());
    }

    // ── refresh_chats_pinned_to (the provider ENDPOINT-edit apply) ────

    #[tokio::test]
    async fn refresh_swaps_the_cache_only_for_the_matching_pin() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let old = Arc::new(DummyProvider) as Arc<dyn flux_core::Provider>;
        let fresh = Arc::new(DummyProvider) as Arc<dyn flux_core::Provider>;
        // Two chats: `c1` pinned to "test" (the create_chat helper's pin
        // id), `c2` pinned to "other" — both on the SAME old instance.
        let (c1, _frames, _r1) = create_chat(&state, "a", Arc::clone(&old)).await;
        register(&state, "b").await;
        let c2 = state
            .create_chat(
                &sess(&state, "b").await,
                "c2",
                "/tmp",
                flux_chat::ResolvedPin {
                    provider: Arc::clone(&old),
                    id: "other".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap()
            .chat_id;

        state
            .refresh_chats_pinned_to("test", |id, _model| {
                (id == "test").then(|| Arc::clone(&fresh))
            })
            .await;

        let chats = state.manager.chats.read().await;
        assert!(
            Arc::ptr_eq(chats.get(&c1).unwrap().provider.as_ref().unwrap(), &fresh),
            "the matching pin's cache instance must be the fresh one"
        );
        assert!(
            Arc::ptr_eq(chats.get(&c2).unwrap().provider.as_ref().unwrap(), &old),
            "a pin to a DIFFERENT provider is never touched"
        );
    }

    #[tokio::test]
    async fn refresh_keeps_the_stale_instance_when_the_pin_no_longer_resolves() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        let old = Arc::new(DummyProvider) as Arc<dyn flux_core::Provider>;
        let (cid, _frames, _r) = create_chat(&state, "a", Arc::clone(&old)).await;

        // A racing remove: the resolver misses. The stale instance stays —
        // the next round fails naming the pin (the remove semantics), the
        // cache never grows a phantom.
        state.refresh_chats_pinned_to("test", |_, _| None).await;

        let chats = state.manager.chats.read().await;
        assert!(Arc::ptr_eq(
            chats.get(&cid).unwrap().provider.as_ref().unwrap(),
            &old
        ));
    }

    #[tokio::test]
    async fn refresh_carries_the_fresh_pin_to_a_live_engine() {
        let state = test_state(Arc::new(Store::open_in_memory().await.unwrap())).await;
        // Round 1 runs on the scripted provider; the FRESH instance is a
        // recording one — its begin count is the rebuild's landing proof.
        let provider = replay_provider(vec![vec![
            ScriptItem::Chunk(Ok(StreamChunk::Text("one".into()))),
            ScriptItem::Chunk(Ok(StreamChunk::End {
                finish_reason: None,
            })),
        ]]);
        let (cid, frames, _router) = create_chat(&state, "a", provider).await;
        state
            .send_message(&sess(&state, "a").await, &cid, "hi".into())
            .await
            .unwrap();
        wait_for_kind(&frames, "stream_end").await;

        let begins = Arc::new(StdMutex::new(Vec::new()));
        let fresh: Arc<dyn flux_core::Provider> = Arc::new(crate::test_util::RecordingProvider {
            opens: Arc::clone(&begins),
        });
        state
            .refresh_chats_pinned_to("test", |_, _| Some(Arc::clone(&fresh)))
            .await;

        // The gate fired after the finished round: the fresh instance
        // re-began over the full persisted history (user "hi" + assistant
        // "one") — the carried-pin rebuild's landing proof.
        wait_for(|| begins.try_lock().map(|b| !b.is_empty()).unwrap_or(false)).await;
        {
            let b = begins.lock().unwrap();
            assert_eq!(b.len(), 1, "the fresh instance began exactly once");
            assert_eq!(b[0].len(), 2);
            assert_eq!(b[0][0].role, flux_core::Role::User);
            assert_eq!(b[0][1].role, flux_core::Role::Assistant);
        }
        // The cache carries the fresh instance too (the lazy-spawn path).
        let chats = state.manager.chats.read().await;
        assert!(Arc::ptr_eq(
            chats.get(&cid).unwrap().provider.as_ref().unwrap(),
            &fresh
        ));
        assert!(task_exists(&state, &cid).await);
    }
}
