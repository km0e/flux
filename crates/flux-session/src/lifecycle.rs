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
}
