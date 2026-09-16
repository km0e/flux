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
    let (chat_id, recorded1, _router) = create_chat(&state, "s1", Arc::new(DummyProvider)).await;
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
    let hist = find_kind(&frames, "chat_history").expect("claim must deliver a history snapshot");
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
    // The claim (the client's first act after create) registers the
    // viewer slot this test exercises.
    let _ = state.claim_chat(&s1, &info.chat_id).await;
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
        find_chats(&b.lock().unwrap()).is_some_and(|c| c.chats.first().is_some_and(|x| x.active))
    })
    .await;

    // Releasing the lease → broadcast active=false.
    state
        .release_chat(&sess(&state, "a").await, &info.chat_id)
        .await;
    wait_for(|| {
        find_chats(&b.lock().unwrap()).is_some_and(|c| c.chats.first().is_some_and(|x| !x.active))
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
