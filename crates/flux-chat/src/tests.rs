use super::spawn::assemble_tools;
use crate::chat::{Chat, ChatInit};
use crate::domain::StateManager;
use crate::handle::ChatHandle;
use crate::spawn::spawn;
use async_trait::async_trait;
use flux_core::test_util::{ScriptItem, ScriptedProvider, wait_for};
use flux_core::{CoreError, ErrorCode, Message, Role, StreamChunk, ToolCtx, WireEvent};
use flux_core::{OutputPort, ToolPort};
use flux_core::{Tool, ToolCall, ToolRegistry};
use flux_store::Store;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

/// A mock OutputPort that records wire events into per-kind fields for
/// assertions (the historical ChatSink test surface).
struct MockSink {
    events: StdMutex<Vec<&'static str>>,
    text_deltas: StdMutex<Vec<String>>,
    reasoning_deltas: StdMutex<Vec<String>>,
    stream_errors: StdMutex<Vec<String>>,
    stream_error_codes: StdMutex<Vec<Option<ErrorCode>>>,
    stream_ends: StdMutex<u32>,
    cancellations: StdMutex<u32>,
    tool_starts: StdMutex<Vec<(String, String, String)>>,
    tool_results: StdMutex<Vec<(String, String, String)>>,
    rebase_bases: StdMutex<Vec<i64>>,
}

impl MockSink {
    fn new() -> Self {
        Self {
            events: StdMutex::new(Vec::new()),
            text_deltas: StdMutex::new(Vec::new()),
            reasoning_deltas: StdMutex::new(Vec::new()),
            stream_errors: StdMutex::new(Vec::new()),
            stream_error_codes: StdMutex::new(Vec::new()),
            stream_ends: StdMutex::new(0),
            cancellations: StdMutex::new(0),
            tool_starts: StdMutex::new(Vec::new()),
            tool_results: StdMutex::new(Vec::new()),
            rebase_bases: StdMutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl OutputPort for MockSink {
    async fn emit(&self, event: WireEvent) {
        match event {
            WireEvent::TextDelta(delta) => {
                self.events.lock().unwrap().push("text_delta");
                self.text_deltas.lock().unwrap().push(delta);
            }
            WireEvent::ReasoningDelta(delta) => {
                self.events.lock().unwrap().push("reasoning_delta");
                self.reasoning_deltas.lock().unwrap().push(delta);
            }
            WireEvent::StreamError { message, code } => {
                self.events.lock().unwrap().push("stream_error");
                self.stream_errors.lock().unwrap().push(message);
                self.stream_error_codes.lock().unwrap().push(code);
            }
            WireEvent::Usage { .. } => {
                self.events.lock().unwrap().push("usage");
            }
            WireEvent::StreamEnd { .. } => {
                self.events.lock().unwrap().push("stream_end");
                *self.stream_ends.lock().unwrap() += 1;
            }
            WireEvent::ProviderSwitched { .. } => {
                self.events.lock().unwrap().push("provider_switched");
            }
            WireEvent::Cancelled => {
                self.events.lock().unwrap().push("cancelled");
                *self.cancellations.lock().unwrap() += 1;
            }
            WireEvent::ToolStart {
                id,
                name,
                arguments,
            } => {
                self.events.lock().unwrap().push("tool_start");
                self.tool_starts.lock().unwrap().push((id, name, arguments));
            }
            WireEvent::ToolResult { id, name, result } => {
                self.events.lock().unwrap().push("tool_result");
                self.tool_results.lock().unwrap().push((id, name, result));
            }
            WireEvent::QuestionRequired { .. } => {
                self.events.lock().unwrap().push("question_required");
            }
            WireEvent::ContextRebased { base_message_id } => {
                self.events.lock().unwrap().push("context_rebased");
                self.rebase_bases.lock().unwrap().push(base_message_id);
            }
            // Preview chunks never reach the sink in these scenarios (the
            // scripted providers don't emit them); kept for exhaustiveness.
            WireEvent::ToolCallPreview { .. } => {
                self.events.lock().unwrap().push("tool_preview");
            }
        }
    }
}

#[tokio::test]
async fn bounded_output_buffers_under_the_call_id_and_returns_head() {
    let (chat, _sink) = test_chat().await;
    let big = "x".repeat(20_000);
    let result = chat.bounded_output("call_1", &big).await;

    // head (first 8000 chars) + the ref marker (with buf_read usage hints);
    // the reference IS the producing tool call's id.
    assert!(result.starts_with(&"x".repeat(8000)));
    assert!(result.contains("call_1"));
    assert!(result.contains("buf_read"));
    assert!(
        result.chars().count() <= 8500,
        "inline head must be bounded, got {} chars",
        result.chars().count()
    );

    // Small results are unaffected (no store write).
    assert_eq!(chat.bounded_output("call_2", "small").await, "small");
    assert!(
        chat.store
            .load_buf_entry("test-chat", "call_2")
            .await
            .unwrap()
            .is_none()
    );

    // The full content is persisted under the call id (write-through).
    let stored = chat
        .store
        .load_buf_entry("test-chat", "call_1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.chars().count(), 20_000);
}

#[tokio::test]
async fn overflow_entries_are_anchored_persistent_and_never_superseded() {
    let (chat, _sink) = test_chat().await;
    async fn read(chat: &Chat, call_id: &str, offset: u64) -> String {
        let args: HashMap<String, Value> = serde_json::from_value(serde_json::json!({
            "ref": call_id,
            "offset": offset,
            "limit": 6000
        }))
        .unwrap();
        chat.execute(
            &flux_core::ToolCall {
                id: call_id.to_string(),
                name: flux_core::BUF_READ_TOOL.into(),
                arguments: "{}".into(),
            },
            args,
            flux_core::ToolCtx::new(),
        )
        .await
    }

    // Two overflows from two different calls: 20_000 chars each (head 8000
    // + 12000 buffered under each call id).
    let r1 = chat.bounded_output("call_a", &"y".repeat(20_000)).await;
    let r2 = chat.bounded_output("call_b", &"z".repeat(20_000)).await;
    assert!(r1.contains("call_a") && r2.contains("call_b"));

    // Paging round-trip through the store: reassemble the part after the
    // head of call_a.
    let mut collected = String::new();
    let mut offset = 8000u64;
    loop {
        let page = read(&chat, "call_a", offset).await;
        let body_start = page.find("]\n").map(|i| i + 2).unwrap_or(0);
        let body_end = page.rfind("\n\n--- (").unwrap_or(page.len());
        collected.push_str(&page[body_start..body_end]);
        if page.contains("end of buffer") {
            break;
        }
        offset = page
            .rsplit("continue with offset: ")
            .next()
            .and_then(|s| s.split(')').next())
            .and_then(|s| s.parse::<u64>().ok())
            .expect("continuation footer");
    }
    assert_eq!(collected.chars().count(), 12_000);
    assert!(collected.chars().all(|c| c == 'y'));

    // Round boundaries change nothing (persist_messages is a no-op for the
    // buffer now) and the SECOND entry is not superseded by the first's
    // paging: both stay readable.
    chat.persist_messages(&[flux_core::Message::user("next round")])
        .await;
    assert!(read(&chat, "call_a", 0).await.contains('y'));
    assert!(read(&chat, "call_b", 0).await.contains('z'));

    // Unknown refs degrade gracefully (the model re-runs the tool).
    let miss = read(&chat, "call_nope", 0).await;
    assert!(miss.contains("unknown buffer reference"));
    assert!(miss.contains("re-run the original tool"));
}

/// Build a minimal Chat for port-mapping tests.
async fn test_chat() -> (Chat, Arc<MockSink>) {
    let sink = Arc::new(MockSink::new());
    let store = Arc::new(Store::open_in_memory().await.unwrap());
    // The chat row first — buffered-output entries carry an FK to it.
    store.insert_chat("test-chat", "T").await.unwrap();
    let state_manager =
        Arc::new(StateManager::for_chat(store.clone(), "test-chat", &HashMap::new()).await);
    let kit = crate::spawn::ChatKit {
        descriptions: &HashMap::new(),
        question: question_tool(sink.clone()),
    };
    let chat = Chat {
        id: "test-chat".into(),
        state_manager: state_manager.clone(),
        tools: Arc::new(assemble_tools(
            &ToolRegistry::default(),
            state_manager,
            &kit,
            "test-chat",
            &store,
        )),
        store,
    };
    (chat, sink)
}

/// Question tool wired to the test sink (its QuestionRequired events land
/// in the same recorded stream as everything else).
fn question_tool(sink: Arc<MockSink>) -> crate::question::QuestionTool {
    crate::question::QuestionTool::new(Arc::new(crate::question::QuestionBoard::new()), sink)
}

/// Throwaway sink for tests that only inspect the assembled registry.
fn sink_for_assemble() -> Arc<MockSink> {
    Arc::new(MockSink::new())
}

// The historical collect_stream unit tests (text/reasoning/tool-call
// accumulation, usage recording, provider-error events, missing-End)
// moved into the kernel: machine.rs table tests lock the state
// transitions, runtime.rs end-to-end tests (simple_text_round_end_to_end,
// cancel_mid_stream_persists_partial, …) lock the wire behavior.

#[tokio::test]
async fn workdir_persisted_across_reloads() {
    let store = Arc::new(Store::open_in_memory().await.unwrap());
    let chat_id = "wd-test";
    store.insert_chat(chat_id, "WD").await.unwrap();

    // Simulate chat creation: create_chat persists the canonical workdir
    // into the state table before any StateManager exists.
    store
        .save_state_entry(chat_id, "workdir", "/tmp/test-dir")
        .await
        .unwrap();

    // Load — the boundary extracts into the fixed field (read-only state)
    let sm = StateManager::for_chat(store.clone(), chat_id, &HashMap::new()).await;
    assert_eq!(sm.workdir(), "/tmp/test-dir");
    assert_eq!(sm.get("workdir"), Some("/tmp/test-dir".into()));

    // Simulate reload — the persisted boundary re-extracts, and the write
    // path still refuses it.
    let sm2 = StateManager::for_chat(store.clone(), chat_id, &HashMap::new()).await;
    let restored = sm2.get("workdir");
    assert_eq!(restored.as_deref(), Some("/tmp/test-dir"));
    assert!(sm2.set("workdir", "/other".into()).await.is_err());
}

// ── Cancellation semantics ──

/// Spawn a real loop + peers chat with the given tools and connection
/// script. Returns the control handle, the provider (begin count = the
/// in-place rebuild's observable), the wire sink, and the store.
async fn spawn_scripted(
    tools: ToolRegistry,
    script: Vec<Vec<Result<StreamChunk, CoreError>>>,
) -> (ChatHandle, Arc<ScriptedProvider>, Arc<MockSink>, Arc<Store>) {
    let sink = Arc::new(MockSink::new());
    let store = Arc::new(Store::open_in_memory().await.unwrap());
    store.insert_chat("test-chat", "test").await.unwrap();
    let provider = Arc::new(ScriptedProvider::staged(vec![
        script
            .into_iter()
            .map(|round| round.into_iter().map(ScriptItem::Chunk).collect())
            .collect(),
    ]));
    let handle = spawn(
        ChatInit {
            id: "test-chat".into(),
            history: Vec::new(),
            questions: Arc::new(crate::question::QuestionBoard::new()),
            provider: provider.clone(),
        },
        Arc::from(""),
        Arc::new(tools),
        store.clone(),
        Arc::new(HashMap::new()),
        sink.clone(),
    )
    .await;
    (handle, provider, sink, store)
}

#[tokio::test]
async fn stale_cancel_issued_while_idle_does_not_kill_next_stream() {
    let (handle, _provider, sink, _store) = spawn_scripted(
        ToolRegistry::default(),
        vec![vec![
            Ok(StreamChunk::Text("still alive".into())),
            Ok(StreamChunk::End {
                finish_reason: None,
            }),
        ]],
    )
    .await;

    // A cancel pressed while no stream is active (e.g. while a question
    // prompt is open) must not be carried over into the next stream.
    handle.send_cancel();
    handle.send_user("hi".into());

    wait_for(|| *sink.stream_ends.lock().unwrap() >= 1).await;
    assert!(sink.stream_errors.lock().unwrap().is_empty());
}

/// Engine rebuild through a real spawned Chat: the rebuild command arms
/// the machine's gate, and at the fired gate the consumer RE-BEGINS its
/// connection (begin #2) IN PLACE — the consumer task never exits, and
/// the next round streams through the fresh connection.
#[tokio::test]
async fn rebuild_command_rebuilds_the_engine_in_place() {
    // Two STAGES: stage 0 feeds the first engine's connection, stage 1
    // the re-begun one (a stage = one connection's segment list).
    let sink = Arc::new(MockSink::new());
    let store = Arc::new(Store::open_in_memory().await.unwrap());
    store.insert_chat("test-chat", "test").await.unwrap();
    let chunk = |t: &str| ScriptItem::Chunk(Ok(StreamChunk::Text(t.into())));
    let provider = Arc::new(ScriptedProvider::staged(vec![
        vec![vec![
            chunk("hello"),
            ScriptItem::Chunk(Ok(StreamChunk::End {
                finish_reason: None,
            })),
        ]],
        vec![vec![
            chunk("after rebuild"),
            ScriptItem::Chunk(Ok(StreamChunk::End {
                finish_reason: None,
            })),
        ]],
    ]));
    let handle = spawn(
        ChatInit {
            id: "test-chat".into(),
            history: Vec::new(),
            questions: Arc::new(crate::question::QuestionBoard::new()),
            provider: provider.clone(),
        },
        Arc::from(""),
        Arc::new(ToolRegistry::default()),
        store,
        Arc::new(HashMap::new()),
        sink.clone(),
    )
    .await;

    // One full round → machine settles back to Idle at stream_end.
    handle.send_user("hi".into());
    wait_for(|| *sink.stream_ends.lock().unwrap() >= 1).await;
    assert_eq!(provider.begin_count(), 1);

    handle.rebuild(None);
    // The gate fires at Idle: the consumer re-begins (begin #2) and
    // KEEPS RUNNING — no exit, no done flag.
    wait_for(|| provider.begin_count() == 2).await;
    assert!(!handle.is_done(), "the engine never dies for a rebuild");

    // The rebuilt engine serves the next round through the new
    // connection (the second script stage).
    handle.send_user("again".into());
    wait_for(|| *sink.stream_ends.lock().unwrap() >= 2).await;
    assert!(
        sink.text_deltas
            .lock()
            .unwrap()
            .iter()
            .any(|d| d == "after rebuild"),
        "the post-rebuild round streamed from the re-begun connection"
    );
}

/// A rebuild racing a live round never interrupts it: the gate arms, the
/// round (here: a tool round with a continuation stream) finishes, and
/// only THEN does the gate fire — the re-begin lands after the full
/// wrap-up, and the engine keeps running.
#[tokio::test]
async fn rebuild_during_a_live_round_defers_to_its_wrap_up() {
    let (handle, provider, sink, _store) = spawn_scripted(
        ToolRegistry::default(),
        vec![
            vec![
                Ok(StreamChunk::ToolCalls(vec![ToolCall {
                    id: "t1".into(),
                    name: "dummy".into(),
                    arguments: "{}".into(),
                }])),
                Ok(StreamChunk::End {
                    finish_reason: None,
                }),
            ],
            vec![
                Ok(StreamChunk::Text("continued".into())),
                Ok(StreamChunk::End {
                    finish_reason: None,
                }),
            ],
        ],
    )
    .await;

    handle.send_user("go".into());
    // The tool dispatched (the round is live); rebuild now.
    wait_for(|| !sink.tool_starts.lock().unwrap().is_empty()).await;
    assert!(handle.rebuild(None));

    // The round completes (continuation stream + wrap) BEFORE the gate
    // fires: the re-begin follows the full wrap-up.
    wait_for(|| *sink.stream_ends.lock().unwrap() >= 1).await;
    wait_for(|| provider.begin_count() == 2).await;
    assert!(!handle.is_done());
    // The round's tool result reached the wire before the rebuild.
    assert_eq!(sink.tool_results.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cancel_during_active_stream_aborts_collection() {
    let sink = Arc::new(MockSink::new());
    let store = Arc::new(Store::open_in_memory().await.unwrap());
    store.insert_chat("test-chat", "test").await.unwrap();
    let provider = Arc::new(ScriptedProvider::once(flux_core::test_util::hang_script()));
    let handle = spawn(
        ChatInit {
            id: "test-chat".into(),
            history: Vec::new(),
            questions: Arc::new(crate::question::QuestionBoard::new()),
            provider,
        },
        Arc::from(""),
        Arc::new(ToolRegistry::default()),
        store,
        Arc::new(HashMap::new()),
        sink.clone(),
    )
    .await;

    handle.send_user("hi".into());
    // Let the stream register before cancelling.
    wait_for(|| !sink.text_deltas.lock().unwrap().is_empty()).await;
    handle.send_cancel();

    // Cancel is a user action, not an error — a distinct Cancelled event
    // goes out (never a StreamError), then the round wraps up with
    // stream_end.
    wait_for(|| sink.events.lock().unwrap().contains(&"cancelled")).await;
    wait_for(|| *sink.stream_ends.lock().unwrap() >= 1).await;
    assert!(sink.stream_errors.lock().unwrap().is_empty());
}

/// A tool that blocks until the round is cancelled, then contributes its
/// partial output — the cooperative tier of the two-tier interrupt, from
/// the tool's side.
struct UntilCancelledTool;

#[async_trait]
impl Tool for UntilCancelledTool {
    fn name(&self) -> &str {
        "until_cancelled"
    }
    fn description(&self) -> &str {
        "Blocks until the round is cancelled, then returns its partial output."
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    async fn call(&self, _args: HashMap<String, Value>, ctx: ToolCtx) -> Result<String, CoreError> {
        ctx.cancel.cancelled().await;
        Ok("partial tool output".into())
    }
}

#[tokio::test]
async fn cancel_during_tool_flight_interrupts_and_wraps_the_round() {
    // End-to-end two-tier interrupt THROUGH the consumer's flight
    // supervision: the machine's InterruptTools fact reaches the fold
    // loop, the cooperative token fires, the tool contributes its partial
    // output, and the round wraps with Cancelled (never a stream_error).
    // The next message then starts a fresh round on the same engine.
    let tools = ToolRegistry::default();
    tools.register(Arc::new(UntilCancelledTool));
    let (handle, _provider, sink, _store) = spawn_scripted(
        tools,
        vec![
            vec![
                Ok(StreamChunk::ToolCalls(vec![ToolCall {
                    id: "c1".into(),
                    name: "until_cancelled".into(),
                    arguments: "{}".into(),
                }])),
                Ok(StreamChunk::End {
                    finish_reason: None,
                }),
            ],
            vec![
                Ok(StreamChunk::Text("fresh".into())),
                Ok(StreamChunk::End {
                    finish_reason: None,
                }),
            ],
        ],
    )
    .await;

    handle.send_user("go".into());
    wait_for(|| !sink.tool_starts.lock().unwrap().is_empty()).await;
    handle.send_cancel();

    // The tool's partial result is marked interrupted (the single marking
    // site — the tool never mentions cancellation itself), then Cancelled
    // announces the wrap-up, then stream_end.
    wait_for(|| sink.events.lock().unwrap().contains(&"cancelled")).await;
    wait_for(|| *sink.stream_ends.lock().unwrap() >= 1).await;
    {
        let results = sink.tool_results.lock().unwrap();
        let (_, name, result) = &results[0];
        assert_eq!(name, "until_cancelled");
        assert!(
            result.starts_with("[interrupted by user]"),
            "the flight result must carry the interrupt mark: {result}"
        );
        assert!(
            result.contains("partial tool output"),
            "the partial output survives the interrupt: {result}"
        );
    }
    assert!(sink.stream_errors.lock().unwrap().is_empty());

    // The engine is intact: the next message starts a fresh round.
    handle.send_user("next".into());
    wait_for(|| sink.text_deltas.lock().unwrap().contains(&"fresh".into())).await;
    wait_for(|| *sink.stream_ends.lock().unwrap() >= 2).await;
}

#[tokio::test]
async fn stream_end_follows_stream_error_on_provider_failure() {
    let (handle, _provider, sink, _store) = spawn_scripted(
        ToolRegistry::default(),
        vec![vec![Err(CoreError::Provider("boom".into()))]],
    )
    .await;

    handle.send_user("hi".into());
    wait_for(|| sink.events.lock().unwrap().contains(&"stream_end")).await;

    {
        let events = sink.events.lock().unwrap();
        let err_idx = events
            .iter()
            .position(|e| *e == "stream_error")
            .expect("stream_error emitted");
        let end_idx = events
            .iter()
            .position(|e| *e == "stream_end")
            .expect("stream_end emitted");
        assert!(
            end_idx > err_idx,
            "stream_end must follow stream_error: {events:?}"
        );
    }
}

#[tokio::test]
async fn partial_stream_text_is_persisted_on_provider_error() {
    let (handle, _provider, sink, store) = spawn_scripted(
        ToolRegistry::default(),
        vec![vec![
            Ok(StreamChunk::Text("partial ".into())),
            Ok(StreamChunk::Text("answer".into())),
            Err(CoreError::Provider("boom".into())),
        ]],
    )
    .await;

    handle.send_user("hi".into());
    wait_for(|| sink.events.lock().unwrap().contains(&"stream_end")).await;

    let messages = store.load_messages("test-chat").await.unwrap();
    let assistant = messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .expect("partial assistant reply persisted");
    assert_eq!(assistant.content, "partial answer");
}

#[tokio::test]
async fn tool_errors_map_to_tool_execution_code() {
    let (handle, _provider, sink, _store) = spawn_scripted(
        ToolRegistry::default(),
        vec![vec![Err(CoreError::Tool("bad tool".into()))]],
    )
    .await;

    handle.send_user("hi".into());
    wait_for(|| !sink.stream_error_codes.lock().unwrap().is_empty()).await;
    {
        let codes = sink.stream_error_codes.lock().unwrap();
        assert_eq!(codes[0], Some(ErrorCode::ToolExecution));
    }
}

#[tokio::test]
async fn invalid_arguments_errors_map_to_invalid_arguments_code() {
    let (handle, _provider, sink, _store) = spawn_scripted(
        ToolRegistry::default(),
        vec![vec![Err(CoreError::InvalidArguments(
            "missing field".into(),
        ))]],
    )
    .await;

    handle.send_user("hi".into());
    wait_for(|| !sink.stream_error_codes.lock().unwrap().is_empty()).await;
    {
        let codes = sink.stream_error_codes.lock().unwrap();
        assert_eq!(codes[0], Some(ErrorCode::InvalidArguments));
    }
}

// ── ToolPort / PersistencePort mapping ──
// The adapter owns the whole per-call pipeline (existence → ctx enrichment
// → execute → output bounding). These tests lock that pipeline.

/// Build a minimal Chat whose registry carries `tools` (plus the chat-owned
/// defaults).
async fn chat_with_tools(tools: ToolRegistry) -> (Chat, Arc<MockSink>) {
    let sink = Arc::new(MockSink::new());
    let store = Arc::new(Store::open_in_memory().await.unwrap());
    store.insert_chat("test-chat", "T").await.unwrap();
    let state_manager =
        Arc::new(StateManager::for_chat(store.clone(), "test-chat", &HashMap::new()).await);
    let kit = crate::spawn::ChatKit {
        descriptions: &HashMap::new(),
        question: question_tool(sink.clone()),
    };
    let chat = Chat {
        id: "test-chat".into(),
        state_manager: state_manager.clone(),
        tools: Arc::new(assemble_tools(
            &tools,
            state_manager,
            &kit,
            "test-chat",
            &store,
        )),
        store,
    };
    (chat, sink)
}

/// Minimal registered tool for port-mapping tests.
struct DummyTool {
    name: &'static str,
}

#[async_trait]
impl Tool for DummyTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "dummy tool for port tests"
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    async fn call(
        &self,
        _args: HashMap<String, Value>,
        _ctx: flux_core::ToolCtx,
    ) -> Result<String, CoreError> {
        Ok("dummy ok".into())
    }
}

#[tokio::test]
async fn tool_port_execute_runs_registry_tool_without_emitting_wire_events() {
    let tools = ToolRegistry::default();
    tools.register(Arc::new(DummyTool { name: "dummy" }));
    let (chat, sink) = chat_with_tools(tools).await;
    let call = ToolCall {
        id: "c1".into(),
        name: "dummy".into(),
        arguments: "{}".into(),
    };
    let result = chat
        .execute(&call, HashMap::new(), flux_core::ToolCtx::new())
        .await;
    assert_eq!(result, "dummy ok");
    // Wire events belong to the tool-flight fold (the consumer emits
    // ToolStart at dispatch) — the adapter must not emit ToolStart/
    // ToolResult itself or the wire order doubles up.
    assert!(sink.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn tool_port_execute_state_tool_returns_content() {
    let (chat, sink) = test_chat().await;
    chat.state_manager
        .set("foo", "bar".to_string())
        .await
        .unwrap();
    let call = ToolCall {
        id: "s1".into(),
        name: "state_get".into(),
        arguments: r#"{"key":"foo"}"#.into(),
    };
    let mut args = HashMap::new();
    args.insert("key".to_string(), serde_json::json!("foo"));
    let result = chat.execute(&call, args, flux_core::ToolCtx::new()).await;
    assert_eq!(result, "bar");
    assert!(sink.events.lock().unwrap().is_empty());
}

/// A tool that reports what its ctx boundary resolved — the probe for the
/// enrichment-path tests below.
struct BoundaryProbe;

#[async_trait]
impl Tool for BoundaryProbe {
    fn name(&self) -> &str {
        "probe"
    }
    fn description(&self) -> &str {
        "reports its ctx boundary"
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    async fn call(
        &self,
        _args: HashMap<String, Value>,
        ctx: flux_core::ToolCtx,
    ) -> Result<String, CoreError> {
        let resolved = ctx.resolve("f.txt")?;
        Ok(format!(
            "workdir={}|current_dir={}|resolved={}",
            ctx.workdir.display(),
            ctx.current_dir.display(),
            resolved.display()
        ))
    }
}

#[tokio::test]
async fn tool_port_enriches_ctx_with_chat_boundary() {
    // The adapter fills the ToolCtx from authoritative state: a registered
    // tool resolves paths against the ctx boundary — no injected arguments
    // involved. The boundary comes from the persisted state (the
    // create_chat → StateManager load path).
    let sink = Arc::new(MockSink::new());
    let store = Arc::new(Store::open_in_memory().await.unwrap());
    let dir = tempfile::tempdir().unwrap();
    // The chat row first — state entries carry a FK to it (create_chat
    // inserts the chat before persisting the boundary).
    store.insert_chat("test-chat", "T").await.unwrap();
    store
        .save_state_entry("test-chat", "workdir", dir.path().to_str().unwrap())
        .await
        .unwrap();
    let state_manager =
        Arc::new(StateManager::for_chat(store.clone(), "test-chat", &HashMap::new()).await);
    state_manager
        .set("current_dir", dir.path().to_string_lossy().to_string())
        .await
        .unwrap();
    let kit = crate::spawn::ChatKit {
        descriptions: &HashMap::new(),
        question: question_tool(sink.clone()),
    };
    let tools = ToolRegistry::default();
    tools.register(Arc::new(BoundaryProbe));
    let chat = Chat {
        id: "test-chat".into(),
        state_manager: state_manager.clone(),
        tools: Arc::new(assemble_tools(
            &tools,
            state_manager,
            &kit,
            "test-chat",
            &store,
        )),
        store,
    };
    let call = ToolCall {
        id: "p1".into(),
        name: "probe".into(),
        arguments: "{}".into(),
    };
    let result = chat
        .execute(&call, HashMap::new(), flux_core::ToolCtx::new())
        .await;
    let wd = dir.path().to_string_lossy();
    assert_eq!(
        result,
        format!(
            "workdir={wd}|current_dir={wd}|resolved={}",
            dir.path().join("f.txt").display()
        )
    );
}

#[tokio::test]
async fn tool_port_fail_closed_without_boundary() {
    // A chat assembled without a persisted workdir (never happens via
    // create_chat — it aborts on persistence failure) enriches the ctx
    // with an empty boundary, and tools fail closed on resolve instead of
    // silently operating at the server's cwd.
    let tools = ToolRegistry::default();
    tools.register(Arc::new(BoundaryProbe));
    let (chat, _sink) = chat_with_tools(tools).await;
    let call = ToolCall {
        id: "p1".into(),
        name: "probe".into(),
        arguments: "{}".into(),
    };
    let result = chat
        .execute(&call, HashMap::new(), flux_core::ToolCtx::new())
        .await;
    assert_eq!(
        result,
        "Error: invalid arguments: no workdir boundary on tool context"
    );
}

#[tokio::test]
async fn tool_port_execute_unknown_tool_returns_not_found() {
    let (chat, _sink) = test_chat().await;
    let call = ToolCall {
        id: "c1".into(),
        name: "nope".into(),
        arguments: "{}".into(),
    };
    let result = chat
        .execute(&call, HashMap::new(), flux_core::ToolCtx::new())
        .await;
    assert_eq!(result, "tool not found: nope");
}

#[tokio::test]
async fn tool_error_becomes_tool_result_string() {
    // No approval phase: a tool failure (e.g. a path escaping the chat
    // boundary, surfaced as a resolve error inside the tool) is the tool's
    // result string — the model sees it and can self-correct. It never
    // blocks the round.
    struct FailingTool;

    #[async_trait]
    impl Tool for FailingTool {
        fn name(&self) -> &str {
            "fenced"
        }
        fn description(&self) -> &str {
            "always fails"
        }
        fn schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        async fn call(
            &self,
            _args: HashMap<String, Value>,
            _ctx: flux_core::ToolCtx,
        ) -> Result<String, CoreError> {
            Err(CoreError::Tool("path outside workdir: /etc".into()))
        }
    }

    let tools = ToolRegistry::default();
    tools.register(Arc::new(FailingTool));
    let (chat, _sink) = chat_with_tools(tools).await;
    let call = ToolCall {
        id: "c1".into(),
        name: "fenced".into(),
        arguments: "{}".into(),
    };
    let result = chat
        .execute(&call, HashMap::new(), flux_core::ToolCtx::new())
        .await;
    assert_eq!(result, "Error: tool error: path outside workdir: /etc");
}

#[tokio::test]
async fn persist_messages_persists_messages() {
    let (chat, _sink) = test_chat().await;
    chat.persist_messages(&[Message::user("hi")]).await;
    let messages = chat.store.load_messages("test-chat").await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, Role::User);
}

#[tokio::test]
async fn state_tools_emit_tool_events() {
    let (handle, _provider, sink, _store) = spawn_scripted(
        ToolRegistry::default(),
        vec![
            vec![
                Ok(StreamChunk::ToolCalls(vec![ToolCall {
                    id: "s1".into(),
                    name: "state_set".into(),
                    arguments: r#"{"key":"workdir","value":"/tmp/wd"}"#.into(),
                }])),
                Ok(StreamChunk::End {
                    finish_reason: None,
                }),
            ],
            vec![
                Ok(StreamChunk::Text("done".into())),
                Ok(StreamChunk::End {
                    finish_reason: None,
                }),
            ],
        ],
    )
    .await;

    handle.send_user("go".into());
    wait_for(|| !sink.tool_results.lock().unwrap().is_empty()).await;
    {
        let starts = sink.tool_starts.lock().unwrap();
        assert_eq!(starts.len(), 1, "state tools must emit tool_start");
        assert_eq!(starts[0].0, "s1");
        assert_eq!(starts[0].1, "state_set");
    }
    {
        let results = sink.tool_results.lock().unwrap();
        assert_eq!(results.len(), 1, "state tools must emit tool_result");
        assert_eq!(results[0].0, "s1");
    }
}

#[tokio::test]
async fn assemble_tools_includes_state_tools_and_global_entries() {
    let global = ToolRegistry::default();
    global.register(Arc::new(DummyTool { name: "dummy" }));
    let store = Arc::new(Store::open_in_memory().await.unwrap());
    let sm = Arc::new(StateManager::for_chat(store.clone(), "test-chat", &HashMap::new()).await);
    let kit = crate::spawn::ChatKit {
        descriptions: &HashMap::new(),
        question: question_tool(sink_for_assemble()),
    };
    let registry = assemble_tools(&global, sm, &kit, "c1", &store);
    let names: Vec<String> = registry
        .entries()
        .iter()
        .map(|t| t.name().to_string())
        .collect();
    assert!(
        names.contains(&"dummy".to_string()),
        "global tools preserved: {names:?}"
    );
    assert!(
        names.contains(&"state_get".to_string()),
        "state_get added: {names:?}"
    );
    assert!(
        names.contains(&"state_set".to_string()),
        "state_set added: {names:?}"
    );
    assert!(
        names.contains(&flux_core::QUESTION_TOOL.to_string()),
        "question tool added: {names:?}"
    );
    // Definitions now flow from the registry alone — chat-owned defs included.
    let defs = registry.definitions();
    assert!(defs.iter().any(|d| d.name == "state_get"));
    assert!(defs.iter().any(|d| d.name == "state_set"));
    assert!(defs.iter().any(|d| d.name == flux_core::QUESTION_TOOL));
}
