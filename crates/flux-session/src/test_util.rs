//! Test-only fixtures for the control plane — session sinks, identity
//! registration, and `ServerState` builders. The provider/connection
//! fakes live in `flux_core::test_util` (the `test-util` feature); they
//! are re-exported here so the moved test modules keep one import path.

use crate::manager::ServerState;
use flux_chat::ResolvedPin;
use flux_core::ToolRegistry;
pub(crate) use flux_core::test_util::{
    DummyProvider, RecordingProvider, Script, ScriptItem, ScriptedProvider, hang_script, wait_for,
};
use flux_proto::flux::v1::SubscribeResponse;
use flux_proto::flux::v1::subscribe_response::Kind;
use flux_store::Store;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

pub(crate) async fn test_state(store: Arc<Store>) -> Arc<ServerState> {
    ServerState::new(
        Arc::from(""),
        Arc::new(ToolRegistry::default()),
        store,
        HashMap::new(),
        &|_, _| None,
    )
    .await
    .map(Arc::new)
    .expect("test state: in-memory store must load chats")
}

// ── Session/sink helpers ────────────────────────────────────────────────────

/// In-memory session sink: records every delivered element.
pub(crate) struct RecordingSink(Arc<StdMutex<Vec<SubscribeResponse>>>);

impl RecordingSink {
    pub(crate) fn new() -> (Arc<Self>, Arc<StdMutex<Vec<SubscribeResponse>>>) {
        let recorded = Arc::new(StdMutex::new(Vec::new()));
        (Arc::new(Self(Arc::clone(&recorded))), recorded)
    }
}

impl crate::identity::SessionSink for RecordingSink {
    fn send(&self, event: SubscribeResponse) -> bool {
        self.0.lock().expect("recording lock").push(event);
        true
    }

    fn send_control(&self, event: SubscribeResponse) -> bool {
        self.0.lock().expect("recording lock").push(event);
        true
    }
}

/// Sink whose content channel is always full (models a stalled network) but
/// whose control channel works: `send` reports failure, `send_control`
/// records.
pub(crate) struct FailingSink(Arc<StdMutex<Vec<SubscribeResponse>>>);

impl FailingSink {
    pub(crate) fn new() -> (Arc<Self>, Arc<StdMutex<Vec<SubscribeResponse>>>) {
        let recorded = Arc::new(StdMutex::new(Vec::new()));
        (Arc::new(Self(Arc::clone(&recorded))), recorded)
    }
}

impl crate::identity::SessionSink for FailingSink {
    fn send(&self, _event: SubscribeResponse) -> bool {
        false
    }

    fn send_control(&self, event: SubscribeResponse) -> bool {
        self.0.lock().expect("recording lock").push(event);
        true
    }
}

// ── Element inspection helpers (the JSON-frame helpers' typed heirs) ───────

/// The recorded elements' oneof case names (`"text_delta"`, `"chats"`, …) —
/// the shape the old `types(&frames)` helper produced, over typed elements.
pub(crate) fn kind_name(k: &Kind) -> &'static str {
    match k {
        Kind::Ready(_) => "ready",
        Kind::Keepalive(_) => "keepalive",
        Kind::TextDelta(_) => "text_delta",
        Kind::Error(_) => "error",
        Kind::ReasoningDelta(_) => "reasoning_delta",
        Kind::Usage(_) => "usage",
        Kind::ToolStart(_) => "tool_start",
        Kind::ToolCallPreview(_) => "tool_call_preview",
        Kind::ToolResult(_) => "tool_result",
        Kind::QuestionRequired(_) => "question_required",
        Kind::StreamEnd(_) => "stream_end",
        Kind::StreamCancelled(_) => "stream_cancelled",
        Kind::ChatState(_) => "chat_state",
        Kind::ChatHistory(_) => "chat_history",
        Kind::ContextRebased(_) => "context_rebased",
        Kind::ProviderSwitched(_) => "provider_switched",
        Kind::Chats(_) => "chats",
        Kind::ChatCreated(_) => "chat_created",
        Kind::Providers(_) => "providers",
        Kind::Models(_) => "models",
        Kind::McpServers(_) => "mcp_servers",
        Kind::Skills(_) => "skills",
    }
}

/// The oneof case names of every recorded element, in arrival order.
pub(crate) fn kinds(recorded: &[SubscribeResponse]) -> Vec<&'static str> {
    recorded
        .iter()
        .filter_map(|el| el.kind.as_ref().map(kind_name))
        .collect()
}

/// First recorded element whose kind matches `name`.
pub(crate) fn find_kind<'a>(
    recorded: &'a [SubscribeResponse],
    name: &str,
) -> Option<&'a SubscribeResponse> {
    recorded
        .iter()
        .find(|el| el.kind.as_ref().is_some_and(|k| kind_name(k) == name))
}

/// Spin until `name` appears in the record (the wait_for analog over
/// typed elements).
pub(crate) async fn wait_for_kind(recorded: &Arc<StdMutex<Vec<SubscribeResponse>>>, name: &str) {
    wait_for(|| find_kind(&recorded.lock().unwrap(), name).is_some()).await
}

/// The LAST `chats` broadcast recorded (the authoritative list state).
pub(crate) fn find_chats(
    recorded: &[SubscribeResponse],
) -> Option<&flux_proto::flux::v1::ChatsBroadcast> {
    recorded.iter().rev().find_map(|el| match &el.kind {
        Some(Kind::Chats(c)) => Some(c),
        _ => None,
    })
}

/// Register an identity with an explicit token and a recording sink;
/// returns the shared record.
pub(crate) async fn register(
    state: &ServerState,
    sid: &str,
) -> Arc<StdMutex<Vec<SubscribeResponse>>> {
    let (sink, recorded) = RecordingSink::new();
    let session = crate::identity::Session::with_token(sid);
    session.attach(sink);
    state.manager.identities.write().await.insert(
        sid.to_string(),
        crate::identity::SessionRef::clone(&session),
    );
    recorded
}

/// Look up a registered identity by its token. Get-or-create: a token
/// never seen before is registered on the fly with a recording sink.
pub(crate) async fn sess(state: &ServerState, sid: &str) -> crate::identity::SessionRef {
    {
        let identities = state.manager.identities.read().await;
        if let Some(s) = identities.get(sid) {
            return crate::identity::SessionRef::clone(s);
        }
    }
    let (sink, _recorded) = RecordingSink::new();
    let session = crate::identity::Session::with_token(sid);
    session.attach(sink);
    state.manager.identities.write().await.insert(
        sid.to_string(),
        crate::identity::SessionRef::clone(&session),
    );
    session
}

/// Register the creator and create a chat; returns (chat_id, the creator's
/// recorded elements, the chat's router handle).
pub(crate) async fn create_chat(
    state: &ServerState,
    sid: &str,
    provider: Arc<dyn flux_core::Provider>,
) -> (
    String,
    Arc<StdMutex<Vec<SubscribeResponse>>>,
    crate::router::RouterHandle,
) {
    let recorded = register(state, sid).await;
    let info = state
        .create_chat(
            &sess(state, sid).await,
            "c",
            "/tmp",
            flux_core::ChatKind::Classic,
            ResolvedPin {
                provider,
                id: "test".into(),
                model: String::new(),
            },
        )
        .await
        .unwrap();
    let router = state
        .manager
        .chats
        .read()
        .await
        .get(&info.chat_id)
        .unwrap()
        .router
        .clone();
    (info.chat_id, recorded, router)
}
