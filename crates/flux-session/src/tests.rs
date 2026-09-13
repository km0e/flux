//! Control-plane integration tests: the provider pin lifecycle, the
//! engine-rebuild flows (hot-swap / parked sends), and the fork flow,
//! driven end-to-end through `ServerState`.

use crate::manager::ServerState;
use crate::ops::ClaimOutcome;
use crate::ops::ForkFailure;
use crate::ops::SwitchOutcome;
use crate::test_util::{
    DummyProvider, RecordingProvider, ScriptItem, ScriptedProvider, hang_script, kinds, register,
    sess, wait_for, wait_for_kind,
};
use flux_chat::ResolvedPin;
use flux_core::{ChatStateKind, CoreError, Provider, Role, StreamChunk, ToolRegistry};
use flux_proto::flux::v1::subscribe_response::Kind;
use flux_store::Store;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

// ── provider pin + hot swap (instances, no registry) ────────────────────

/// Sync condition helper: the chat has a live task whose round state is Idle.
fn task_idle(state: &ServerState, cid: &str) -> bool {
    state
        .manager
        .chats
        .try_read()
        .ok()
        .and_then(|chats| {
            chats.get(cid).map(|c| {
                c.task
                    .as_ref()
                    .is_some_and(|t| t.handle.active_state() == ChatStateKind::Idle)
            })
        })
        .unwrap_or(false)
}

/// Sync condition helper: the chat's live round is in Streaming.
fn task_streaming(state: &ServerState, cid: &str) -> bool {
    state
        .manager
        .chats
        .try_read()
        .ok()
        .and_then(|chats| {
            chats.get(cid).map(|c| {
                c.task
                    .as_ref()
                    .is_some_and(|t| t.handle.active_state() == ChatStateKind::Streaming)
            })
        })
        .unwrap_or(false)
}

async fn instance_state() -> (Arc<ServerState>, Arc<Store>) {
    let store = Arc::new(Store::open_in_memory().await.unwrap());
    let state = Arc::new(
        ServerState::new(
            Arc::from(""),
            Arc::new(ToolRegistry::default()),
            store.clone(),
            HashMap::new(),
            &|_, _| None,
        )
        .await
        .unwrap(),
    );
    (state, store)
}

#[tokio::test]
async fn provider_swap_applies_at_boundary_and_persists_pin() {
    let begins = Arc::new(StdMutex::new(Vec::new()));
    let pinned_provider: Arc<dyn Provider> = Arc::new(ScriptedProvider::once(vec![vec![
        ScriptItem::Chunk(Ok(StreamChunk::Text("one".into()))),
        ScriptItem::Chunk(Ok(StreamChunk::End {
            finish_reason: None,
        })),
    ]]));
    let swap_provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        opens: Arc::clone(&begins),
    });
    let (state, store) = instance_state().await;
    register(&state, "a").await;
    let info = state
        .create_chat(
            &sess(&state, "a").await,
            "c",
            "/tmp",
            ResolvedPin {
                provider: Arc::clone(&pinned_provider),
                id: "pinned".into(),
                model: "pinned-model".into(),
            },
        )
        .await
        .unwrap();
    let cid = info.chat_id.clone();
    // The real client claims right after creating — the claim registers
    // the viewer slot so the router's fanout reaches `recorded`.
    let _ = state.subscribe_chat(&sess(&state, "a").await, &cid).await;
    assert_eq!(info.provider, "pinned");
    assert_eq!(info.model, "pinned-model");

    // Round 1 on the pinned provider.
    state
        .send_message(&sess(&state, "a").await, &cid, "hi".into())
        .await
        .unwrap();
    wait_for(|| task_idle(&state, &cid)).await;

    // Hot-swap to the recording provider with a model override (round over
    // → applies now).
    assert_eq!(
        state
            .switch_provider(
                &sess(&state, "a").await,
                &cid,
                ResolvedPin {
                    provider: swap_provider.clone(),
                    id: "swap".into(),
                    model: "custom".into(),
                },
            )
            .await
            .unwrap(),
        SwitchOutcome::Ok
    );
    // The respawn began the swap provider over the live context (user
    // "hi" + assistant "one"). The pin persists at REQUEST time (a truth
    // source) — the begin is the respawn's landing proof, so wait for IT.
    wait_for(|| begins.try_lock().map(|b| b.len() == 1).unwrap_or(false)).await;
    {
        let begins = begins.lock().unwrap();
        assert_eq!(begins.len(), 1, "the swap provider began exactly once");
        assert_eq!(begins[0].len(), 2);
        assert_eq!(begins[0][0].role, Role::User);
        assert_eq!(begins[0][1].role, Role::Assistant);
    }
    // The cache carries the swapped INSTANCE too: a respawn must land on
    // the same provider the chat last ran on.
    {
        let chats = state.manager.chats.read().await;
        let c = chats.get(&cid).unwrap();
        assert!(c.provider.is_some(), "cache instance synced on swap");
    }

    // The pin persisted at request time (the truth source — durable even
    // if the process died before the respawn).
    assert_eq!(
        store
            .load_state(&cid)
            .await
            .unwrap()
            .get("provider")
            .map(String::as_str),
        Some("swap")
    );

    // Round 2 streams through the swapped provider.
    state
        .send_message(&sess(&state, "a").await, &cid, "next".into())
        .await
        .unwrap();
    // (the recording connection answers "on swap")

    // Cache reflects the swap (ops syncs the cache labels AND instance at the request point).
    let (cached_provider, cached_model) = {
        let chats = state.manager.chats.read().await;
        let c = chats.get(&cid).unwrap();
        (c.provider_id.clone(), c.model.clone())
    };
    assert_eq!(cached_provider, "swap");
    assert_eq!(cached_model, "custom");
}

#[tokio::test]
async fn provider_swap_during_live_round_applies_at_wrap_up() {
    let begins = Arc::new(StdMutex::new(Vec::new()));
    let pinned_provider: Arc<dyn Provider> = Arc::new(ScriptedProvider::once(vec![vec![
        ScriptItem::Chunk(Ok(StreamChunk::Text("started".into()))),
        ScriptItem::Chunk(Ok(StreamChunk::End {
            finish_reason: None,
        })),
    ]]));
    let swap_provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        opens: Arc::clone(&begins),
    });
    let (state, store) = instance_state().await;
    register(&state, "a").await;
    let info = state
        .create_chat(
            &sess(&state, "a").await,
            "c",
            "/tmp",
            ResolvedPin {
                provider: Arc::clone(&pinned_provider),
                id: "pinned".into(),
                model: String::new(),
            },
        )
        .await
        .unwrap();
    let cid = info.chat_id;
    state
        .send_message(&sess(&state, "a").await, &cid, "hi".into())
        .await
        .unwrap();
    wait_for(|| task_idle(&state, &cid)).await;

    // Swap while Idle: parks + applies before the next round. Round flow is
    // never disturbed; the pin persists at the apply point.
    assert_eq!(
        state
            .switch_provider(
                &sess(&state, "a").await,
                &cid,
                ResolvedPin {
                    provider: swap_provider,
                    id: "swap".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap(),
        SwitchOutcome::Ok
    );
    wait_for(|| begins.try_lock().map(|b| !b.is_empty()).unwrap_or(false)).await;
    assert_eq!(
        store
            .load_state(&cid)
            .await
            .unwrap()
            .get("provider")
            .map(String::as_str),
        Some("swap")
    );

    // Lease gating: a non-holder cannot swap.
    register(&state, "b").await;
    assert_eq!(
        state
            .switch_provider(
                &sess(&state, "b").await,
                &cid,
                ResolvedPin {
                    provider: Arc::new(DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap(),
        SwitchOutcome::Busy
    );
    // Unknown chat.
    assert_eq!(
        state
            .switch_provider(
                &sess(&state, "a").await,
                "no-such",
                ResolvedPin {
                    provider: Arc::new(DummyProvider),
                    id: "swap".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap(),
        SwitchOutcome::NotFound
    );
}

// ── engine rebuild (the generic restart primitive) ──────────────────────

fn staged_provider(scripts: Vec<crate::test_util::Script>) -> Arc<dyn Provider> {
    Arc::new(ScriptedProvider::staged(scripts))
}

/// A full fork cycle: the fork copies the transcript up to but EXCLUDING
/// the USER message into a NEW chat (provenance + name), the source is
/// untouched, and the fork's first round runs over the COPIED context on
/// the fork's own provider — the redo turn re-enters only as the user's
/// re-sent message.
#[tokio::test]
async fn fork_copies_the_transcript_and_the_fork_runs_over_it() {
    let begins = Arc::new(StdMutex::new(Vec::new()));
    let (state, store) = instance_state().await;
    let _recorded = register(&state, "a").await;
    let info = state
        .create_chat(
            &sess(&state, "a").await,
            "c",
            "/tmp",
            ResolvedPin {
                provider: staged_provider(vec![vec![vec![
                    ScriptItem::Chunk(Ok(StreamChunk::Text("one".into()))),
                    ScriptItem::Chunk(Ok(StreamChunk::End {
                        finish_reason: None,
                    })),
                ]]]),
                id: "pinned".into(),
                model: String::new(),
            },
        )
        .await
        .unwrap();
    let cid = info.chat_id.clone();
    // The real client claims right after creating — the claim registers
    // the viewer slot so the router's fanout reaches `recorded`.
    let _ = state.subscribe_chat(&sess(&state, "a").await, &cid).await;

    // Round 1 on the staged provider (stage 0); the transcript persists
    // before the wrap-up announces (ids 1, 2: user + assistant).
    state
        .send_message(&sess(&state, "a").await, &cid, "hi".into())
        .await
        .unwrap();
    loop {
        if store.load_stored_messages(&cid).await.unwrap().len() >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // Fork at the user message (id 1): the copy stops BEFORE it — the
    // fork point is the turn being redone, not part of the fork's
    // history; it re-enters only when the user re-sends it.
    let fork = state
        .fork_chat(
            &sess(&state, "a").await,
            &cid,
            1,
            ResolvedPin {
                provider: Arc::new(RecordingProvider {
                    opens: Arc::clone(&begins),
                }),
                id: "pinned".into(),
                model: String::new(),
            },
        )
        .await
        .unwrap();
    let fid = fork.chat_id.clone();
    assert_ne!(fid, cid);
    assert_eq!(fork.name, "c (fork)");

    // The copy: NOTHING — the fork point was the first turn, so the
    // fork is a branch paused before its first message (fresh row ids
    // would exist only for copied rows); the SOURCE transcript is
    // untouched.
    let copied = store.load_stored_messages(&fid).await.unwrap();
    assert!(copied.is_empty());
    assert_eq!(
        store
            .load_state(&fid)
            .await
            .unwrap()
            .get("workdir")
            .cloned(),
        Some("/tmp".into()),
        "the fork inherits the source's workdir pair"
    );
    let (src_count, forked_from, forked_at): (i64, Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM messages WHERE chat_id = ?1), \
              (SELECT forked_from_chat FROM chats WHERE id = ?2), \
              (SELECT forked_from_message FROM chats WHERE id = ?2)",
    )
    .bind(&cid)
    .bind(&fid)
    .fetch_one(&store.pool)
    .await
    .unwrap();
    assert_eq!(src_count, 2, "the source transcript is untouched");
    assert_eq!(forked_from.as_deref(), Some(cid.as_str()));
    assert_eq!(forked_at, Some(1));

    // The fork's FIRST round runs over the copied context: the connection
    // begins over it — empty here (nothing preceded the fork point) — and
    // the re-sent turn rides as the pending input.
    state
        .send_message(&sess(&state, "a").await, &fid, "next".into())
        .await
        .unwrap();
    wait_for(|| begins.try_lock().map(|b| !b.is_empty()).unwrap_or(false)).await;
    let pending = begins.lock().unwrap()[0].clone();
    assert_eq!(
        pending,
        Vec::<flux_core::Message>::new(),
        "the fork's connection begins over the COPIED (empty) transcript"
    );

    // A fork point that is not a USER message of the source is refused.
    assert!(matches!(
        state
            .fork_chat(
                &sess(&state, "a").await,
                &cid,
                2,
                ResolvedPin {
                    provider: Arc::new(DummyProvider),
                    id: "pinned".into(),
                    model: String::new(),
                },
            )
            .await,
        Err(ForkFailure::BadPoint)
    ));
    // An unknown source chat is refused.
    assert!(matches!(
        state
            .fork_chat(
                &sess(&state, "a").await,
                "nope",
                1,
                ResolvedPin {
                    provider: Arc::new(DummyProvider),
                    id: "pinned".into(),
                    model: String::new(),
                },
            )
            .await,
        Err(ForkFailure::NotFound)
    ));
}

/// Forking hands the source lease over with the navigation: the forker
/// held the source's lease → the fork releases it (the attach broadcast
/// is the first truthful frame — no window renders the source In-use);
/// a FOREIGN holder's lease is untouched; a viewer-forker (no lease) is
/// a no-op.
#[tokio::test]
async fn fork_releases_the_callers_source_lease_and_leaves_foreign_ones() {
    let (state, store) = instance_state().await;
    let _recorded = register(&state, "a").await;
    let _recorded_b = register(&state, "b").await;
    let pin = || ResolvedPin {
        provider: Arc::new(DummyProvider),
        id: "pinned".into(),
        model: String::new(),
    };
    let info = state
        .create_chat(&sess(&state, "a").await, "c", "/tmp", pin())
        .await
        .unwrap();
    let cid = info.chat_id.clone();
    store
        .append_messages(&cid, &[flux_core::Message::user("hi")])
        .await
        .unwrap();

    // The holder forks: the source lease is released — a fresh claim by
    // the same session GRANTS again (not AlreadyOwned).
    let _fork = state
        .fork_chat(&sess(&state, "a").await, &cid, 1, pin())
        .await
        .unwrap();
    assert_eq!(
        state.claim_chat(&sess(&state, "a").await, &cid).await,
        ClaimOutcome::Granted,
        "the fork released the caller's source lease"
    );
    // Re-establish a's lease, then let b STEAL it and fork from there:
    // the fork must not touch the foreign holder's lease (b keeps it).
    state.release_chat(&sess(&state, "a").await, &cid).await;
    assert_eq!(
        state.claim_chat(&sess(&state, "b").await, &cid).await,
        ClaimOutcome::Granted
    );
    let _fork2 = state
        .fork_chat(&sess(&state, "a").await, &cid, 1, pin())
        .await
        .unwrap();
    assert_eq!(
        state.claim_chat(&sess(&state, "b").await, &cid).await,
        ClaimOutcome::AlreadyOwned,
        "a foreign holder's lease survives another session's fork"
    );
}

/// A provider hot-swap rebuilds the engine IN PLACE: the swap re-begins
/// the connection on the new provider over the live context (begin #2 on
/// a live engine — no respawn, the task never dies), and the next round
/// streams through the fresh connection.
#[tokio::test]
async fn provider_swap_rebegins_in_place_and_the_engine_stays_alive() {
    let (state, _store) = instance_state().await;
    let recorded = register(&state, "a").await;
    let old_provider = Arc::new(ScriptedProvider::staged(vec![vec![vec![
        ScriptItem::Chunk(Ok(StreamChunk::Text("one".into()))),
        ScriptItem::Chunk(Ok(StreamChunk::End {
            finish_reason: None,
        })),
    ]]]));
    let new_provider = Arc::new(ScriptedProvider::staged(vec![vec![vec![
        ScriptItem::Chunk(Ok(StreamChunk::Text("two".into()))),
        ScriptItem::Chunk(Ok(StreamChunk::End {
            finish_reason: None,
        })),
    ]]]));
    let info = state
        .create_chat(
            &sess(&state, "a").await,
            "c",
            "/tmp",
            ResolvedPin {
                provider: old_provider.clone(),
                id: "old".into(),
                model: String::new(),
            },
        )
        .await
        .unwrap();
    let cid = info.chat_id.clone();
    // The real client claims right after creating — the claim registers
    // the viewer slot so the router's fanout reaches `recorded`.
    let _ = state.subscribe_chat(&sess(&state, "a").await, &cid).await;

    // Round 1 runs on the OLD provider.
    state
        .send_message(&sess(&state, "a").await, &cid, "m1".into())
        .await
        .unwrap();
    wait_for(|| {
        recorded
            .lock()
            .unwrap()
            .iter()
            .any(|el| matches!(&el.kind, Some(Kind::TextDelta(t)) if t.delta == "one"))
    })
    .await;

    // Hot-swap: the pin persists, the Rebuild carries the fresh instance,
    // the gate fires at Idle, and the consumer re-begins IN PLACE.
    state
        .switch_provider(
            &sess(&state, "a").await,
            &cid,
            ResolvedPin {
                provider: new_provider.clone(),
                id: "new".into(),
                model: "m2".into(),
            },
        )
        .await
        .unwrap();
    wait_for(|| new_provider.begin_count() == 1).await;

    // The engine is ALIVE (no respawn) and the next round streams through
    // the re-begun connection.
    let chats = state.manager.chats.read().await;
    let task = chats.get(&cid).unwrap().task.as_ref().unwrap();
    assert!(!task.handle.is_done(), "the engine never dies for a swap");
    drop(chats);
    state
        .send_message(&sess(&state, "a").await, &cid, "m2".into())
        .await
        .unwrap();
    wait_for(|| {
        recorded
            .lock()
            .unwrap()
            .iter()
            .any(|el| matches!(&el.kind, Some(Kind::TextDelta(t)) if t.delta == "two"))
    })
    .await;
    let messages = store_messages(&state, &cid).await;
    let user_texts: Vec<&str> = messages
        .iter()
        .filter(|m| m.role == Role::User)
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(user_texts, vec!["m1", "m2"]);
    assert_eq!(
        old_provider.begin_count(),
        1,
        "the old engine was never respawned"
    );
}

/// A round that produces NOTHING (the stream fails before any chunk)
/// does not wedge the engine: the next send starts a fresh round (the
/// kernel's turn queue runs turns in order regardless of how the previous
/// one wrapped), and both user turns persist. Regression: under the old
/// respawn+flush regime this shape hung the flush (and the session's
/// frame pump with it) forever.
#[tokio::test]
async fn a_zero_commit_round_does_not_wedge_the_engine() {
    let (state, _store) = instance_state().await;
    let _recorded = register(&state, "a").await;
    let info = state
        .create_chat(
            &sess(&state, "a").await,
            "c",
            "/tmp",
            ResolvedPin {
                // Every round fails at the stream open: the user message
                // commits (round start), then StreamEvent::Failed wraps the
                // round with zero further commits.
                provider: Arc::new(ScriptedProvider::replay(vec![vec![ScriptItem::Chunk(
                    Err(CoreError::Provider("down".into())),
                )]])),
                id: "default".into(),
                model: String::new(),
            },
        )
        .await
        .unwrap();
    let cid = info.chat_id.clone();
    // The real client claims right after creating — the claim registers
    // the viewer slot so the router's fanout reaches `recorded`.
    let _ = state.subscribe_chat(&sess(&state, "a").await, &cid).await;

    state
        .send_message(&sess(&state, "a").await, &cid, "m1".into())
        .await
        .unwrap();
    state
        .send_message(&sess(&state, "a").await, &cid, "m2".into())
        .await
        .unwrap();

    // Both turns persist — the second round started even though the first
    // wrapped with nothing committed.
    let started = std::time::Instant::now();
    loop {
        let texts: Vec<String> = store_messages(&state, &cid)
            .await
            .into_iter()
            .filter(|m| m.role == Role::User)
            .map(|m| m.content)
            .collect();
        if texts == vec!["m1".to_string(), "m2".to_string()] {
            break;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "turns never persisted: {texts:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

async fn store_messages(state: &ServerState, cid: &str) -> Vec<flux_core::Message> {
    state.store.load_messages(cid).await.unwrap()
}

/// R1 interrupt-send against a LIVE round: the fused pair (cancel + user
/// turn) rides one FIFO, so the cancel wraps the hung round and the queued
/// message starts the next round — stream_cancelled precedes the second
/// stream_end, and both turns persist. The old two-request interject (a
/// cancel RPC + a chat RPC whose order HTTP does not guarantee) is
/// replaced by one ordered operation.
#[tokio::test]
async fn interrupt_send_cancels_the_live_round_and_runs_the_next() {
    let (state, _store) = instance_state().await;
    let recorded = register(&state, "a").await;
    let info = state
        .create_chat(
            &sess(&state, "a").await,
            "c",
            "/tmp",
            ResolvedPin {
                // ONE engine, TWO streams on its connection: stream 0 =
                // round 1 (hangs — the cancel's victim), stream 1 = the
                // interrupt-send's replacement round (the next open on the
                // SAME engine; a second staged stage would only be
                // consumed by a respawn).
                provider: Arc::new(ScriptedProvider::once(vec![
                    vec![
                        ScriptItem::Chunk(Ok(StreamChunk::Text("started".into()))),
                        ScriptItem::Hang,
                    ],
                    vec![
                        ScriptItem::Chunk(Ok(StreamChunk::Text("replied".into()))),
                        ScriptItem::Chunk(Ok(StreamChunk::End {
                            finish_reason: None,
                        })),
                    ],
                ])),
                id: "pinned".into(),
                model: String::new(),
            },
        )
        .await
        .unwrap();
    let cid = info.chat_id.clone();
    // The real client claims right after creating — the claim registers
    // the viewer slot so the router's fanout reaches `recorded`.
    let _ = state.subscribe_chat(&sess(&state, "a").await, &cid).await;

    // Round 1 goes live (its first delta reached the sink).
    state
        .send_message(&sess(&state, "a").await, &cid, "hi".into())
        .await
        .unwrap();
    wait_for_kind(&recorded, "text_delta").await;

    // ONE operation: cancel the live round + submit the message.
    state
        .send_message_interrupting(&sess(&state, "a").await, &cid, "next".into())
        .await
        .unwrap();

    // Two stream ends: the cancelled round's wrap-up, then the replacement
    // round's completion — with the cancellation notice between them.
    wait_for(|| {
        recorded
            .lock()
            .unwrap()
            .iter()
            .filter(|el| matches!(&el.kind, Some(Kind::StreamEnd(_))))
            .count()
            >= 2
    })
    .await;
    let kinds = kinds(&recorded.lock().unwrap());
    let cancel_pos = kinds
        .iter()
        .position(|k| *k == "stream_cancelled")
        .expect("the fused cancel must announce stream_cancelled");
    let first_end_pos = kinds
        .iter()
        .position(|k| *k == "stream_end")
        .expect("the cancelled round still wraps with stream_end");
    assert!(
        first_end_pos > cancel_pos,
        "stream_cancelled must precede the wrap-up, got {kinds:?}"
    );

    // Both turns persisted: round 1's user message and the replacement
    // round (its reply streamed from the connection's second stream).
    let started = std::time::Instant::now();
    loop {
        let texts: Vec<String> = store_messages(&state, &cid)
            .await
            .into_iter()
            .map(|m| (m.role, m.content))
            .filter(|(r, _)| *r == Role::User)
            .map(|(_, c)| c)
            .collect();
        if texts == vec!["hi".to_string(), "next".to_string()] {
            break;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "turns never persisted: {texts:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        store_messages(&state, &cid)
            .await
            .iter()
            .any(|m| m.role == Role::Assistant && m.content == "replied"),
        "the replacement round ran to completion"
    );
}

/// R1 interrupt-send against an IDLE engine: the cancel is absorbed (stale)
/// and the message starts immediately — no stream_cancelled may reach the
/// wire, and the round completes like a plain send.
#[tokio::test]
async fn interrupt_send_on_an_idle_engine_equals_a_plain_send() {
    let (state, _store) = instance_state().await;
    let recorded = register(&state, "a").await;
    let info = state
        .create_chat(
            &sess(&state, "a").await,
            "c",
            "/tmp",
            ResolvedPin {
                provider: Arc::new(ScriptedProvider::once(vec![vec![
                    ScriptItem::Chunk(Ok(StreamChunk::Text("replied".into()))),
                    ScriptItem::Chunk(Ok(StreamChunk::End {
                        finish_reason: None,
                    })),
                ]])),
                id: "pinned".into(),
                model: String::new(),
            },
        )
        .await
        .unwrap();
    let cid = info.chat_id.clone();
    // The real client claims right after creating — the claim registers
    // the viewer slot so the router's fanout reaches `recorded`.
    let _ = state.subscribe_chat(&sess(&state, "a").await, &cid).await;

    state
        .send_message_interrupting(&sess(&state, "a").await, &cid, "hi".into())
        .await
        .unwrap();
    wait_for_kind(&recorded, "stream_end").await;

    assert!(
        !kinds(&recorded.lock().unwrap()).contains(&"stream_cancelled"),
        "the stale cancel must be absorbed silently"
    );
    let msgs = store_messages(&state, &cid).await;
    assert!(
        msgs.iter()
            .any(|m| m.role == Role::User && m.content == "hi")
            && msgs
                .iter()
                .any(|m| m.role == Role::Assistant && m.content == "replied"),
        "the round ran like a plain send"
    );
}

#[tokio::test]
async fn fork_copies_buf_entries_of_copied_calls() {
    let (state, store) = instance_state().await;
    let _recorded = register(&state, "a").await;
    let info = state
        .create_chat(
            &sess(&state, "a").await,
            "c",
            "/tmp",
            ResolvedPin {
                provider: Arc::new(DummyProvider),
                id: "default".into(),
                model: String::new(),
            },
        )
        .await
        .unwrap();
    let cid = info.chat_id.clone();
    // The real client claims right after creating — the claim registers
    // the viewer slot so the router's fanout reaches `recorded`.
    let _ = state.subscribe_chat(&sess(&state, "a").await, &cid).await;

    // Seed the transcript: user (id 1) + a tool exchange (call_old, ids
    // 2-3) + a second user turn (id 4), with a buffered output for the
    // call and one orphan (no call anywhere).
    store
        .append_messages(
            &cid,
            &[
                flux_core::Message::user("hi"),
                flux_core::Message {
                    role: Role::Assistant,
                    content: String::new(),
                    reasoning_content: None,
                    tool_calls: vec![flux_core::ToolCall {
                        id: "call_old".into(),
                        name: "bash".into(),
                        arguments: "{}".into(),
                    }],
                    tool_call_id: None,
                },
                flux_core::Message::tool("call_old", "old result"),
                flux_core::Message::user("more"),
            ],
        )
        .await
        .unwrap();
    store
        .save_buf_entry(&cid, "call_old", "OLD OUTPUT")
        .await
        .unwrap();
    store.save_buf_entry(&cid, "orphan", "junk").await.unwrap();

    // Fork at the LAST user turn (id 4): the copy carries the whole tool
    // exchange, so the call's buffered output rides along; the orphan has
    // no copied call and is not copied. The source keeps both.
    let fork = state
        .fork_chat(
            &sess(&state, "a").await,
            &cid,
            4,
            ResolvedPin {
                provider: Arc::new(DummyProvider),
                id: "default".into(),
                model: String::new(),
            },
        )
        .await
        .unwrap();
    let fid = fork.chat_id.clone();
    assert_eq!(
        store
            .load_buf_entry(&fid, "call_old")
            .await
            .unwrap()
            .as_deref(),
        Some("OLD OUTPUT"),
        "the copied call's buffered output is resolvable in the fork"
    );
    assert!(
        store
            .load_buf_entry(&fid, "orphan")
            .await
            .unwrap()
            .is_none(),
        "an uncopied call's entry does not ride along"
    );
    // Source untouched: both entries remain.
    assert_eq!(
        store
            .load_buf_entry(&cid, "call_old")
            .await
            .unwrap()
            .as_deref(),
        Some("OLD OUTPUT")
    );
    assert_eq!(
        store
            .load_buf_entry(&cid, "orphan")
            .await
            .unwrap()
            .as_deref(),
        Some("junk")
    );

    // The copied tool_calls rows keep the call id — history rendering and
    // buf references agree on it.
    let copied = store.load_stored_messages(&fid).await.unwrap();
    let assistant = copied
        .iter()
        .find(|s| s.message.role == Role::Assistant)
        .expect("the copied assistant tool-call turn");
    assert_eq!(assistant.message.tool_calls[0].id, "call_old");
    assert!(assistant.id > 0, "fresh row ids in the fork");
}

// ── send idempotency (client_msg_id dedup) ──────────────────────────────

/// A resend carrying an already-accepted client_msg_id is absorbed as a
/// Duplicate (no second turn enqueued); a fresh id or a different chat is
/// unaffected. The window is per chat and survives engine rebuilds (it
/// lives on the shell).
#[tokio::test]
async fn send_with_a_client_msg_id_dedups_resends() {
    use crate::ops::SendOutcome;
    let (state, _store) = instance_state().await;
    register(&state, "a").await;
    let provider: Arc<dyn Provider> = Arc::new(DummyProvider);
    let info = state
        .create_chat(
            &sess(&state, "a").await,
            "c",
            "/tmp",
            ResolvedPin {
                provider,
                id: "pinned".into(),
                model: "m".into(),
            },
        )
        .await
        .unwrap();
    let cid = info.chat_id.to_owned();

    // First send with the key: accepted.
    assert_eq!(
        state
            .send_message_idempotent(
                &sess(&state, "a").await,
                &cid,
                "hi".into(),
                false,
                Some("key-1".into()),
            )
            .await
            .unwrap(),
        SendOutcome::Ok
    );
    // A resend with the SAME key: absorbed as a duplicate, never enqueued.
    assert_eq!(
        state
            .send_message_idempotent(
                &sess(&state, "a").await,
                &cid,
                "hi".into(),
                false,
                Some("key-1".into()),
            )
            .await
            .unwrap(),
        SendOutcome::Duplicate
    );
    // A fresh key is a new turn.
    assert_eq!(
        state
            .send_message_idempotent(
                &sess(&state, "a").await,
                &cid,
                "hi".into(),
                false,
                Some("key-2".into()),
            )
            .await
            .unwrap(),
        SendOutcome::Ok
    );
    // No key = no dedup (fire-once callers).
    assert_eq!(
        state
            .send_message(&sess(&state, "a").await, &cid, "hi".into())
            .await
            .unwrap(),
        SendOutcome::Ok
    );

    // The window is PER CHAT: the same key on another chat is a new turn.
    let provider: Arc<dyn Provider> = Arc::new(DummyProvider);
    let info2 = state
        .create_chat(
            &sess(&state, "a").await,
            "c2",
            "/tmp",
            ResolvedPin {
                provider,
                id: "pinned".into(),
                model: "m".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        state
            .send_message_idempotent(
                &sess(&state, "a").await,
                &info2.chat_id,
                "hi".into(),
                false,
                Some("key-1".into()),
            )
            .await
            .unwrap(),
        SendOutcome::Ok
    );
}

// ── graceful-shutdown drain ─────────────────────────────────────────────

#[tokio::test]
async fn drain_returns_immediately_without_live_tasks() {
    let (state, _store) = instance_state().await;
    let start = std::time::Instant::now();
    state.drain(std::time::Duration::from_secs(5)).await;
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
}

#[tokio::test]
async fn drain_cancels_a_live_round_and_lands_it_on_the_boundary() {
    let (state, store) = instance_state().await;
    register(&state, "a").await;
    let provider: Arc<dyn Provider> = Arc::new(ScriptedProvider::once(hang_script()));
    let info = state
        .create_chat(
            &sess(&state, "a").await,
            "c",
            "/tmp",
            ResolvedPin {
                provider,
                id: "scripted".into(),
                model: "m".into(),
            },
        )
        .await
        .unwrap();
    let cid = info.chat_id.clone();
    let _ = state.subscribe_chat(&sess(&state, "a").await, &cid).await;
    state
        .send_message(&sess(&state, "a").await, &cid, "hi".into())
        .await
        .unwrap();
    // The hang script pushed one delta then stalls — the round is live.
    wait_for(|| task_streaming(&state, &cid)).await;

    state.drain(std::time::Duration::from_secs(5)).await;

    // The round landed on the machine's boundary (cancel → commit → Idle).
    assert!(task_idle(&state, &cid));
    // The cancelled round's partial assistant text persisted — the
    // transcript commit is round-atomic, so the rebirth history is
    // provider-valid (user + assistant, no dangling tool_calls).
    let messages = store.load_messages(&cid).await.unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, Role::User);
    assert_eq!(messages[1].role, Role::Assistant);
    assert_eq!(messages[1].content, "started");
}
