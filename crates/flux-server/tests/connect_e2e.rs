//! Connect E2E — the REAL flux-server binary on ONE port, exercising the
//! wire end to end. This is the protocol: every client conversation rides
//! gRPC-Web unary calls (hand-rolled framing — the browser's exact wire;
//! tonic's Rust client speaks native h2, which is NOT the browser path)
//! plus ONE session-scoped Subscribe stream that anchors the identity
//! (open = attach, close = detach), carries the snapshots and the events,
//! and takes over the lease/viewer semantics wholesale.
//!
//! The terminal side channel (`/ws/term`) stays a raw WebSocket (binary
//! PTY bytes) — those tests connect it with tungstenite directly.
//!
//! Streaming content is asserted at the flux-chat unit level (scripted
//! providers); here the fake provider (base url points at the server
//! itself, which never answers /v1) produces `provider_connection` errors
//! on every round — perfect for proving router fanout across streams.

use std::process::Stdio;
use std::time::{Duration, Instant};

use flux_proto::flux::v1::subscribe_response::Kind as ResponseKind;
use flux_proto::flux::v1::{
    AddProviderRequest, AddServerRequest, AddServerResponse, CancelRoundRequest, ClaimChatRequest,
    ClaimChatResponse, CloseChatRequest, CreateChatRequest, CreateChatResponse, DeleteChatRequest,
    ErrorCode, FsListRequest, FsListResponse, FsReadRequest, FsReadResponse, ListChatsRequest,
    ListChatsResponse, ListProvidersRequest, ListProvidersResponse, ListServersRequest,
    ListServersResponse, ListSkillsRequest, ListSkillsResponse, McpServersBroadcast,
    OpenChatRequest, ProviderSummary, ProvidersBroadcast, RemoveProviderRequest,
    RemoveProviderResponse, RemoveServerRequest, RemoveServerResponse, RenameChatRequest,
    RenameChatResponse, SendMessageRequest, SubscribeRequest, SubscribeResponse,
};
use flux_proto::prost::Message;
use futures_util::{SinkExt as _, StreamExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

/// Serializes SERVER STARTS across the parallel tests. The start sequence
/// (pick a "free" port → release → the child binds it) has an unavoidable
/// race window: the OS can re-hand the port to an ephemeral outbound
/// connection, or another test's child can win the bind — under parallel
/// load that surfaced as flaky connection failures. Starts are cheap; one
/// at a time removes the window entirely (the servers then run their
/// sessions fully in parallel).
static START_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

async fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn test_db_path() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    (dir, db_path)
}

/// A spawned server under test. Dropped at test end (killed).
struct Server {
    child: tokio::process::Child,
    base: String,
    /// Keeps the temp database dir alive for the server's lifetime
    /// (None when the test owns the dir — restart-persistence cases).
    _dir: Option<tempfile::TempDir>,
}

impl Server {
    async fn start() -> Server {
        let (dir, db_path) = test_db_path();
        let mut server = Self::start_on(&db_path).await;
        server._dir = Some(dir);
        server
    }

    async fn start_on(db_path: &std::path::Path) -> Server {
        let _start = START_LOCK.lock().await;
        let mut last_err: Option<String> = None;
        for _ in 0..5 {
            match Self::try_start(db_path).await {
                Ok(s) => return s,
                Err(e) => last_err = Some(e),
            }
        }
        panic!(
            "server failed to start after retries: {}",
            last_err.unwrap_or_default()
        );
    }

    async fn try_start(db_path: &std::path::Path) -> Result<Server, String> {
        let port = free_port().await;
        // There is NO config file — everything rides CLI flags.
        let server_bin = std::env::var("CARGO_BIN_EXE_flux-server")
            .unwrap_or_else(|_| "./target/debug/flux-server".to_string());
        let mut child = tokio::process::Command::new(&server_bin)
            .arg("--port")
            .arg(port.to_string())
            .arg("--db-path")
            .arg(db_path)
            // Terminal tests pin bash: the server runs the login $SHELL
            // (fish discards typeahead sent before its prompt is ready,
            // which would make the exit-flow test test fish, not us).
            .env("SHELL", "/bin/bash")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to start server: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                return Ok(Server {
                    child,
                    base: format!("http://127.0.0.1:{port}"),
                    _dir: None,
                });
            }
            if let Ok(Some(status)) = child.try_wait() {
                return Err(format!(
                    "server exited before accepting on port {port}: {status}"
                ));
            }
            if Instant::now() >= deadline {
                let _ = child.start_kill();
                return Err(format!(
                    "server did not accept connections on port {port} within 20s"
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

// ── gRPC-Web framing (the browser's exact wire) ─────────────────────────────

fn lp_frame(msg: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(5 + msg.len());
    v.push(0u8);
    v.extend_from_slice(&(msg.len() as u32).to_be_bytes());
    v.extend_from_slice(msg);
    v
}

fn parse_frames(buf: &[u8]) -> (Vec<&[u8]>, Option<&[u8]>) {
    let mut data = Vec::new();
    let mut trailer = None;
    let mut i = 0;
    while i + 5 <= buf.len() {
        let flag = buf[i];
        let len = u32::from_be_bytes(buf[i + 1..i + 5].try_into().unwrap()) as usize;
        let end = (i + 5 + len).min(buf.len());
        let frame = &buf[i + 5..end];
        i = end;
        if flag & 0x80 != 0 {
            trailer = Some(frame);
        } else {
            data.push(frame);
        }
        if i >= buf.len() {
            break;
        }
    }
    (data, trailer)
}

fn trailer_field(trailer: Option<&[u8]>, name: &str) -> Option<String> {
    let t = std::str::from_utf8(trailer?).ok()?;
    t.lines()
        .find_map(|l| l.strip_prefix(&format!("{name}:")).map(str::trim))
        .map(str::to_owned)
}

/// A refused call: the gRPC status (trailers-only responses carry it in
/// the HTTP headers; otherwise it rides the body's trailer frame).
#[derive(Debug)]
struct RpcStatus {
    code: i32,
    /// Kept for failure messages (asserts read it via the Debug output).
    #[allow(dead_code)]
    message: String,
}

/// One unary RPC over the browser's framing. Returns the decoded response,
/// or the gRPC status for transport-level refusals (lease gates, unknown
/// identities). Application-level failures ride the response's inline
/// `error` fields (D4') — tests assert those on the typed structs.
async fn unary<Req, Res>(
    base: &str,
    path: &str,
    req: Req,
    token: Option<&str>,
) -> Result<Res, RpcStatus>
where
    Req: Message,
    Res: Message + Default,
{
    let mut http = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .header("content-type", "application/grpc-web+proto");
    if let Some(token) = token {
        http = http.header("x-flux-session", token);
    }
    let resp = http
        .body(lp_frame(&req.encode_to_vec()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "transport error on {path}");
    // Trailers-only refusal: grpc-status in the HTTP headers, empty body.
    if let Some(code) = resp
        .headers()
        .get("grpc-status")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i32>().ok())
    {
        let message = resp
            .headers()
            .get("grpc-message")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        return Err(RpcStatus { code, message });
    }
    let bytes = resp.bytes().await.unwrap();
    let (frames, trailer) = parse_frames(&bytes);
    let status = trailer_field(trailer, "grpc-status")
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(0);
    if status != 0 {
        return Err(RpcStatus {
            code: status,
            message: trailer_field(trailer, "grpc-message").unwrap_or_default(),
        });
    }
    Ok(Res::decode(frames[0]).expect("response decodes"))
}

// ── the Subscribe stream client ─────────────────────────────────────────────

/// One open Subscribe stream + the identity it anchored. The struct IS a
/// session: the token it carries rides every subsequent RPC's metadata.
struct Stream {
    rx: futures_util::stream::BoxStream<'static, reqwest::Result<bytes::Bytes>>,
    buf: Vec<u8>,
    /// The authoritative identity (the ready frame's token).
    token: String,
    /// The chats whose lease the identity held at attach (empty on a fresh
    /// mint — the resume handshake collapsed into this frame).
    leases: Vec<String>,
}

impl Stream {
    /// Open the stream (adopting `token` when given) and read the ready.
    async fn open(base: &str, token: Option<String>) -> Stream {
        let req = SubscribeRequest { session_id: token };
        let resp = reqwest::Client::new()
            .post(format!("{base}/flux.v1.EventService/Subscribe"))
            .header("content-type", "application/grpc-web+proto")
            .header("x-grpc-web", "1")
            .body(lp_frame(&req.encode_to_vec()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let mut me = Stream {
            rx: resp.bytes_stream().boxed(),
            buf: Vec::new(),
            token: String::new(),
            leases: Vec::new(),
        };
        let ready = me.expect_kind("ready").await;
        match ready.kind {
            Some(ResponseKind::Ready(r)) => {
                me.token = r.session_id;
                me.leases = r.leases;
            }
            other => panic!("first frame must be ready, got {other:?}"),
        }
        me
    }

    /// The token this stream anchored (the credential for RPC metadata).
    fn token(&self) -> &str {
        &self.token
    }

    async fn next_response(&mut self) -> SubscribeResponse {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            while let Some(frame) = self.take_frame() {
                if let Ok(resp) = SubscribeResponse::decode(&frame[..]) {
                    return resp;
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let chunk = timeout(remaining, self.rx.next())
                .await
                .expect("stream chunk timed out")
                .expect("stream ended unexpectedly")
                .expect("stream chunk error");
            self.buf.extend_from_slice(chunk.as_ref());
        }
    }

    fn take_frame(&mut self) -> Option<Vec<u8>> {
        if self.buf.len() < 5 {
            return None;
        }
        let len = u32::from_be_bytes(self.buf[1..5].try_into().unwrap()) as usize;
        if self.buf.len() < 5 + len {
            return None;
        }
        let frame = self.buf[5..5 + len].to_vec();
        self.buf.drain(..5 + len);
        Some(frame)
    }

    /// Consume elements until one of `kind` arrives; unrelated frames
    /// (broadcasts, snapshots, keepalives) are skipped.
    async fn expect_kind(&mut self, kind: &str) -> SubscribeResponse {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let el = self.next_response().await;
            let this = match &el.kind {
                Some(ResponseKind::Ready(_)) => "ready",
                Some(ResponseKind::Keepalive(_)) => "keepalive",
                Some(ResponseKind::TextDelta(_)) => "text_delta",
                Some(ResponseKind::Error(_)) => "error",
                Some(ResponseKind::ReasoningDelta(_)) => "reasoning_delta",
                Some(ResponseKind::Usage(_)) => "usage",
                Some(ResponseKind::ToolStart(_)) => "tool_start",
                Some(ResponseKind::ToolCallPreview(_)) => "tool_call_preview",
                Some(ResponseKind::ToolResult(_)) => "tool_result",
                Some(ResponseKind::QuestionRequired(_)) => "question_required",
                Some(ResponseKind::StreamEnd(_)) => "stream_end",
                Some(ResponseKind::StreamCancelled(_)) => "stream_cancelled",
                Some(ResponseKind::ChatState(_)) => "chat_state",
                Some(ResponseKind::ChatHistory(_)) => "chat_history",
                Some(ResponseKind::MessagePersisted(_)) => "message_persisted",
                Some(ResponseKind::ProviderSwitched(_)) => "provider_switched",
                Some(ResponseKind::Chats(_)) => "chats",
                Some(ResponseKind::ChatCreated(_)) => "chat_created",
                Some(ResponseKind::Providers(_)) => "providers",
                Some(ResponseKind::Models(_)) => "models",
                Some(ResponseKind::McpServers(_)) => "mcp_servers",
                Some(ResponseKind::Skills(_)) => "skills",
                None => "<none>",
            };
            if this == kind {
                return el;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {kind}, got: {el:?}"
            );
        }
    }

    /// The next `error` element with the given code.
    async fn expect_error(&mut self, code: ErrorCode) -> (String, String) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let el = self.next_response().await;
            if let Some(ResponseKind::Error(e)) = &el.kind
                && e.code == code as i32
            {
                return (el.chat_id.clone(), e.message.clone());
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for error {code:?}, got: {el:?}"
            );
        }
    }
}

// ── shared steps ────────────────────────────────────────────────────────────

/// Register the "default" fake provider (its base url points at the server
/// itself, which never answers /v1 — every round ends with
/// `provider_connection`, the fanout signal the tests assert).
async fn add_default_provider(server: &Server, token: &str) {
    let resp: AddProviderResponseShim = unary(
        &server.base,
        "/flux.v1.ProviderService/AddProvider",
        AddProviderRequest {
            id: "default".into(),
            protocol: "openai".into(),
            url: Some(format!("{}/v1", server.base)),
            api_key: Some("test-key".into()),
        },
        Some(token),
    )
    .await
    .expect("add provider");
    assert!(
        resp.error.is_none(),
        "default provider registration failed: {resp:?}"
    );
}

/// The AddProviderResponse type without importing the full family set.
type AddProviderResponseShim = flux_proto::flux::v1::AddProviderResponse;

async fn create_chat(server: &Server, token: &str, name: &str, workdir: &str) -> String {
    let resp: CreateChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/CreateChat",
        CreateChatRequest {
            name: name.into(),
            workdir: workdir.into(),
            provider: "default".into(),
            model: "test-model".into(),
        },
        Some(token),
    )
    .await
    .expect("create chat");
    assert!(resp.error.is_none(), "create failed: {:?}", resp.error);
    resp.chat.unwrap().chat_id
}

// ── the tests ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn connect_round_trip_create_list() {
    let server = Server::start().await;
    let s = Stream::open(&server.base, None).await;
    assert!(!s.token().is_empty(), "ready carries the minted identity");
    add_default_provider(&server, s.token()).await;

    // Create: the ack carries the chat; the chats broadcast rides the
    // stream (order-free — independent channels).
    let created: CreateChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/CreateChat",
        CreateChatRequest {
            name: "E2E Test".into(),
            workdir: "/tmp".into(),
            provider: "default".into(),
            model: "test-model".into(),
        },
        Some(s.token()),
    )
    .await
    .expect("create");
    assert!(created.error.is_none(), "{:?}", created.error);
    let chat_id = created.chat.unwrap().chat_id;

    let list: ListChatsResponse = unary(
        &server.base,
        "/flux.v1.ChatService/ListChats",
        ListChatsRequest {},
        Some(s.token()),
    )
    .await
    .expect("list");
    assert!(list.chats.iter().any(|c| c.chat_id == chat_id));
}

#[tokio::test]
async fn send_idempotency_key_dedups_resends_over_the_wire() {
    let server = Server::start().await;
    let s = Stream::open(&server.base, None).await;
    add_default_provider(&server, s.token()).await;
    let chat_id = create_chat(&server, s.token(), "dedup", "/tmp").await;

    let send = |key: Option<String>| {
        unary::<SendMessageRequest, flux_proto::flux::v1::SendMessageResponse>(
            &server.base,
            "/flux.v1.ChatService/SendMessage",
            SendMessageRequest {
                chat_id: chat_id.clone(),
                message: "hi".into(),
                interrupt: false,
                client_msg_id: key,
            },
            Some(s.token()),
        )
    };
    // First send: accepted, not a duplicate.
    assert!(!send(Some("key-1".into())).await.expect("send").duplicate);
    // A resend with the same key: absorbed as a duplicate (no second turn).
    assert!(send(Some("key-1".into())).await.expect("resend").duplicate);
    // A fresh key: a new turn.
    assert!(!send(Some("key-2".into())).await.expect("send").duplicate);
    // No key: no dedup.
    assert!(!send(None).await.expect("send").duplicate);
}

/// Forking over the wire: the response carries the NEW chat (named after
/// the source, with the fork provenance), the fork's transcript holds the
/// copied user turn, and the source is untouched.
#[tokio::test]
async fn fork_chat_creates_a_new_chat_from_a_message() {
    let server = Server::start().await;
    let mut s = Stream::open(&server.base, None).await;
    add_default_provider(&server, s.token()).await;
    let chat_id = create_chat(&server, s.token(), "source", "/tmp").await;

    // Claim BEFORE acting — the same flow the real client runs (create ack
    // → auto-select → claim): the claim registers the viewer slot, so the
    // chat's events reach this stream from here on.
    let _: flux_proto::flux::v1::ClaimChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/ClaimChat",
        flux_proto::flux::v1::ClaimChatRequest {
            chat_id: chat_id.clone(),
        },
        Some(s.token()),
    )
    .await
    .expect("claim");
    s.expect_kind("chat_history").await;

    // Persist one turn so a fork point exists (the provider endpoint is
    // dead, but the user message lands before the round fails).
    let _: flux_proto::flux::v1::SendMessageResponse = unary(
        &server.base,
        "/flux.v1.ChatService/SendMessage",
        SendMessageRequest {
            chat_id: chat_id.clone(),
            message: "seed".into(),
            interrupt: false,
            client_msg_id: None,
        },
        Some(s.token()),
    )
    .await
    .expect("send");
    // The persisted announcement carries the fork point (row id).
    let persisted = s.expect_kind("message_persisted").await;
    let fork_point = match persisted.kind {
        Some(ResponseKind::MessagePersisted(m)) => m.id,
        other => panic!("expected message_persisted, got {other:?}"),
    };
    // The round fails (dead provider) — drain the error event.
    let _ = s.expect_error(ErrorCode::ProviderConnection).await;

    let resp: flux_proto::flux::v1::ForkChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/ForkChat",
        flux_proto::flux::v1::ForkChatRequest {
            chat_id: chat_id.clone(),
            fork_point,
        },
        Some(s.token()),
    )
    .await
    .expect("fork");
    let chat = resp.chat.expect("the fork ack carries the new chat");
    assert_eq!(chat.name, "source (fork)");
    assert_eq!(chat.forked_from_chat_id.as_deref(), Some(chat_id.as_str()));
    assert_ne!(chat.chat_id, chat_id);

    // Claiming the fork delivers the COPIED transcript — EMPTY: the copy
    // stops before the fork point (the redo turn re-enters the fork only
    // when re-sent; the client prefills the composer with its content).
    let _: flux_proto::flux::v1::ClaimChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/ClaimChat",
        flux_proto::flux::v1::ClaimChatRequest {
            chat_id: chat.chat_id.clone(),
        },
        Some(s.token()),
    )
    .await
    .expect("claim");
    let el = s.expect_kind("chat_history").await;
    match el.kind {
        Some(ResponseKind::ChatHistory(h)) => {
            assert!(
                h.messages.is_empty(),
                "the copy excludes the fork point: {:?}",
                h.messages
            );
        }
        other => panic!("expected chat_history, got {other:?}"),
    }

    // A fork point that is not a user message rides the inline error.
    let bad: flux_proto::flux::v1::ForkChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/ForkChat",
        flux_proto::flux::v1::ForkChatRequest {
            chat_id: chat_id.clone(),
            fork_point: fork_point + 1,
        },
        Some(s.token()),
    )
    .await
    .expect("fork (bad point)");
    assert!(bad.chat.is_none());
    assert!(bad.error.is_some());
}

#[tokio::test]
async fn create_rename_delete_broadcast_to_all_sessions() {
    let server = Server::start().await;
    let mut a = Stream::open(&server.base, None).await;
    let mut b = Stream::open(&server.base, None).await;
    add_default_provider(&server, a.token()).await;

    let chat_id = create_chat(&server, a.token(), "shared", "/tmp").await;

    // BOTH sessions receive the chats broadcast (identity-registry fanout).
    for s in [&mut a, &mut b] {
        let el = s.expect_kind("chats").await;
        match el.kind {
            Some(ResponseKind::Chats(c)) => {
                assert_eq!(c.chats.len(), 1);
                assert_eq!(c.chats[0].chat_id, chat_id);
                assert!(c.chats[0].active, "the creator holds the lease");
            }
            other => panic!("expected chats, got {other:?}"),
        }
    }

    // A renames → both receive the fresh list.
    let _: RenameChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/RenameChat",
        RenameChatRequest {
            chat_id: chat_id.clone(),
            name: "renamed".into(),
        },
        Some(a.token()),
    )
    .await
    .expect("rename");
    for s in [&mut a, &mut b] {
        let el = s.expect_kind("chats").await;
        match el.kind {
            Some(ResponseKind::Chats(c)) => {
                assert_eq!(c.chats[0].name, "renamed");
            }
            other => panic!("expected chats, got {other:?}"),
        }
    }

    // A deletes → both receive an empty list.
    let _: flux_proto::flux::v1::DeleteChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/DeleteChat",
        DeleteChatRequest {
            chat_id: chat_id.clone(),
        },
        Some(a.token()),
    )
    .await
    .expect("delete");
    for s in [&mut a, &mut b] {
        let el = s.expect_kind("chats").await;
        match el.kind {
            Some(ResponseKind::Chats(c)) => assert!(c.chats.is_empty()),
            other => panic!("expected chats, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn lease_gates_ops_and_viewers_receive_stream_errors() {
    let server = Server::start().await;
    let mut a = Stream::open(&server.base, None).await;
    let mut b = Stream::open(&server.base, None).await;
    add_default_provider(&server, a.token()).await;
    let chat_id = create_chat(&server, a.token(), "c", "/tmp").await;

    // A claims right after creating (the real client's flow): the claim
    // registers A's viewer slot, so the chat's events reach A's stream —
    // and it drains the creation fanout, so the later error reads land
    // precisely.
    let _: flux_proto::flux::v1::ClaimChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/ClaimChat",
        ClaimChatRequest {
            chat_id: chat_id.clone(),
        },
        Some(a.token()),
    )
    .await
    .expect("claim");
    a.expect_kind("chat_history").await;

    // B subscribes as a viewer (OpenChat): the snapshot rides B's STREAM
    // (single-point delivery — history + state, never the unary response).
    let _: flux_proto::flux::v1::OpenChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/OpenChat",
        OpenChatRequest {
            chat_id: chat_id.clone(),
        },
        Some(b.token()),
    )
    .await
    .expect("open");
    let hist = b.expect_kind("chat_history").await;
    assert_eq!(hist.chat_id, chat_id);
    let st = b.expect_kind("chat_state").await;
    assert_eq!(st.chat_id, chat_id);
    match &st.kind {
        Some(ResponseKind::ChatState(s)) => {
            assert_eq!(s.state, flux_proto::flux::v1::ChatStateKind::Idle as i32);
        }
        other => panic!("expected chat_state, got {other:?}"),
    }

    // B sends while A holds the lease → FAILED_PRECONDITION (the lease gate
    // at the transport level; D4' keeps the inline errors for validation).
    let err = unary::<SendMessageRequest, flux_proto::flux::v1::SendMessageResponse>(
        &server.base,
        "/flux.v1.ChatService/SendMessage",
        SendMessageRequest {
            chat_id: chat_id.clone(),
            message: "mine".into(),
            interrupt: false,
            client_msg_id: None,
        },
        Some(b.token()),
    )
    .await
    .expect_err("busy");
    assert_eq!(err.code, 9, "FAILED_PRECONDITION for a lease-gate refusal");

    // B's delete is refused the same way.
    let err = unary::<DeleteChatRequest, flux_proto::flux::v1::DeleteChatResponse>(
        &server.base,
        "/flux.v1.ChatService/DeleteChat",
        DeleteChatRequest {
            chat_id: chat_id.clone(),
        },
        Some(b.token()),
    )
    .await
    .expect_err("busy");
    assert_eq!(err.code, 9);

    // A sends → the fake provider fails to connect → both streams receive
    // the ErrorEvent (router fanout across streams).
    let _: flux_proto::flux::v1::SendMessageResponse = unary(
        &server.base,
        "/flux.v1.ChatService/SendMessage",
        SendMessageRequest {
            chat_id: chat_id.clone(),
            message: "go".into(),
            interrupt: false,
            client_msg_id: None,
        },
        Some(a.token()),
    )
    .await
    .expect("send");
    let (a_chat, _) = a.expect_error(ErrorCode::ProviderConnection).await;
    let (b_chat, _) = b.expect_error(ErrorCode::ProviderConnection).await;
    assert_eq!(a_chat, chat_id);
    assert_eq!(b_chat, chat_id);

    // A exits (CloseChat = unsubscribe + release) → B claims successfully:
    // the claim delivers history + state on B's stream.
    let _: flux_proto::flux::v1::CloseChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/CloseChat",
        CloseChatRequest {
            chat_id: chat_id.clone(),
        },
        Some(a.token()),
    )
    .await
    .expect("close");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let claim: ClaimChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/ClaimChat",
        ClaimChatRequest {
            chat_id: chat_id.clone(),
        },
        Some(b.token()),
    )
    .await
    .expect("claim");
    assert!(!claim.already_owned);
    let hist = b.expect_kind("chat_history").await;
    assert_eq!(hist.chat_id, chat_id);

    // B sends a message and receives provider_connection (the lease
    // handover moved the stream's operator role).
    let _: flux_proto::flux::v1::SendMessageResponse = unary(
        &server.base,
        "/flux.v1.ChatService/SendMessage",
        SendMessageRequest {
            chat_id: chat_id.clone(),
            message: "mine now".into(),
            interrupt: false,
            client_msg_id: None,
        },
        Some(b.token()),
    )
    .await
    .expect("send");
    let (b_chat, _) = b.expect_error(ErrorCode::ProviderConnection).await;
    assert_eq!(b_chat, chat_id);

    // B now holds the lease and may delete; A sees the empty list.
    let _: flux_proto::flux::v1::DeleteChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/DeleteChat",
        DeleteChatRequest {
            chat_id: chat_id.clone(),
        },
        Some(b.token()),
    )
    .await
    .expect("delete");
    b.expect_kind("chats").await;
    a.expect_kind("chats").await;
}

#[tokio::test]
async fn claim_steals_the_lease_from_a_live_holder() {
    // Take over: B's claim of a chat whose lease A holds SUCCEEDS — the
    // lease transfers and A is demoted in-band (the same ErrorEvent a
    // rejected send produces, so its client degrades itself to a read-only
    // viewer). Without the steal, the viewer pane's Take over button could
    // only ever click into a rejection.
    let server = Server::start().await;
    let mut a = Stream::open(&server.base, None).await;
    let mut b = Stream::open(&server.base, None).await;
    add_default_provider(&server, a.token()).await;
    let chat_id = create_chat(&server, a.token(), "c", "/tmp").await;

    // B's claim: Granted — history + state arrive on B's stream with the
    // lease (single-point delivery, no busy rejection on the claim path).
    let claim: ClaimChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/ClaimChat",
        ClaimChatRequest {
            chat_id: chat_id.clone(),
        },
        Some(b.token()),
    )
    .await
    .expect("claim");
    assert!(!claim.already_owned);
    let hist = b.expect_kind("chat_history").await;
    assert_eq!(hist.chat_id, chat_id);
    b.expect_kind("chat_state").await;

    // A is demoted in-band: the chat_busy ErrorEvent on ITS stream.
    let (demoted_chat, _) = a.expect_error(ErrorCode::ChatBusy).await;
    assert_eq!(demoted_chat, chat_id);

    // A's send is now refused (it holds no lease); B operates freely.
    let err = unary::<SendMessageRequest, flux_proto::flux::v1::SendMessageResponse>(
        &server.base,
        "/flux.v1.ChatService/SendMessage",
        SendMessageRequest {
            chat_id: chat_id.clone(),
            message: "mine".into(),
            interrupt: false,
            client_msg_id: None,
        },
        Some(a.token()),
    )
    .await
    .expect_err("busy");
    assert_eq!(err.code, 9);
    let _: flux_proto::flux::v1::SendMessageResponse = unary(
        &server.base,
        "/flux.v1.ChatService/SendMessage",
        SendMessageRequest {
            chat_id: chat_id.clone(),
            message: "mine".into(),
            interrupt: false,
            client_msg_id: None,
        },
        Some(b.token()),
    )
    .await
    .expect("send");
    let (b_chat, _) = b.expect_error(ErrorCode::ProviderConnection).await;
    assert_eq!(b_chat, chat_id);
}

#[tokio::test]
async fn session_resume_preserves_lease_across_reconnect() {
    // A stream drop DETACHES (grace window) instead of releasing — the
    // client reopens the stream with its token, the ready frame reports the
    // adoption (same token + leases), and it continues operating the same
    // chat without a busy refusal.
    let server = Server::start().await;
    let a = Stream::open(&server.base, None).await;
    let minted = a.token().to_owned();
    add_default_provider(&server, a.token()).await;
    let chat_id = create_chat(&server, a.token(), "c", "/tmp").await;

    // A disconnects → detach (lease survives the grace window).
    drop(a);
    tokio::time::sleep(Duration::from_millis(300)).await;

    // A2 (the reloaded page) reopens the stream with the token: the ready
    // carries the SAME identity + the lease list.
    let mut a2 = Stream::open(&server.base, Some(minted.clone())).await;
    assert_eq!(
        a2.token(),
        minted,
        "the identity was adopted by the stream open"
    );
    assert_eq!(
        a2.leases,
        vec![chat_id.clone()],
        "the lease list rides ready"
    );

    // Re-claim: owner-without-viewer (detach dropped viewer entries) still
    // delivers the history + state snapshot.
    let claim: ClaimChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/ClaimChat",
        ClaimChatRequest {
            chat_id: chat_id.clone(),
        },
        Some(a2.token()),
    )
    .await
    .expect("claim");
    assert!(
        claim.already_owned,
        "the adopted identity re-claims its own lease"
    );
    let hist = a2.expect_kind("chat_history").await;
    assert_eq!(hist.chat_id, chat_id);

    // Operating under the adopted lease: no busy — the fake provider's
    // connection error proves the round actually routed.
    let _: flux_proto::flux::v1::SendMessageResponse = unary(
        &server.base,
        "/flux.v1.ChatService/SendMessage",
        SendMessageRequest {
            chat_id: chat_id.clone(),
            message: "go".into(),
            interrupt: false,
            client_msg_id: None,
        },
        Some(a2.token()),
    )
    .await
    .expect("send");
    let (chat, _) = a2.expect_error(ErrorCode::ProviderConnection).await;
    assert_eq!(chat, chat_id);
}

#[tokio::test]
async fn stale_teardown_after_resume_keeps_the_lease() {
    // The OLD stream's teardown landing AFTER an adoption must not release
    // the adopted identity's leases (the ptr_eq stale-teardown guard).
    let server = Server::start().await;
    let a = Stream::open(&server.base, None).await;
    let sid = a.token().to_owned();
    add_default_provider(&server, a.token()).await;
    let chat_id = create_chat(&server, a.token(), "c", "/tmp").await;

    // Adopt the SAME identity from a second stream while the first is
    // still open (fast-refresh race) — the sink is superseded.
    let mut a2 = Stream::open(&server.base, Some(sid.clone())).await;
    assert_eq!(a2.token(), sid);

    // NOW the old stream dies — its teardown must be a no-op.
    drop(a);
    tokio::time::sleep(Duration::from_millis(300)).await;

    // The adopted stream still holds the lease: claim (history snapshot),
    // then send — no busy refusal.
    let claim: ClaimChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/ClaimChat",
        ClaimChatRequest {
            chat_id: chat_id.clone(),
        },
        Some(a2.token()),
    )
    .await
    .expect("claim");
    assert!(
        claim.already_owned,
        "the adopted identity re-claims its own lease"
    );
    let hist = a2.expect_kind("chat_history").await;
    assert_eq!(hist.chat_id, chat_id);
    let _: flux_proto::flux::v1::SendMessageResponse = unary(
        &server.base,
        "/flux.v1.ChatService/SendMessage",
        SendMessageRequest {
            chat_id: chat_id.clone(),
            message: "go".into(),
            interrupt: false,
            client_msg_id: None,
        },
        Some(a2.token()),
    )
    .await
    .expect("send");
    let (chat, _) = a2.expect_error(ErrorCode::ProviderConnection).await;
    assert_eq!(chat, chat_id);
}

#[tokio::test]
async fn unknown_session_token_mints_fresh() {
    let server = Server::start().await;
    let s = Stream::open(
        &server.base,
        Some("8f0e11c2-0000-4000-8000-000000000000".into()),
    )
    .await;
    assert_ne!(
        s.token(),
        "8f0e11c2-0000-4000-8000-000000000000",
        "not resumable → a fresh identity is minted"
    );
    assert!(s.leases.is_empty(), "a fresh identity holds no leases");
}

#[tokio::test]
async fn fs_list_and_fs_read_drive_the_workdir_picker() {
    // The UI's filesystem browser: FsList on a temp dir returns a
    // directories-first listing; FsRead previews a file head with the
    // truncated flag; failures ride the response's inline `error` (never a
    // transport status).
    let server = Server::start().await;
    let s = Stream::open(&server.base, None).await;

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("note.txt"), "hello").unwrap();
    let dir_str = dir.path().to_str().unwrap();

    let listing: FsListResponse = unary(
        &server.base,
        "/flux.v1.FileSystemService/FsList",
        FsListRequest {
            path: Some(dir_str.into()),
        },
        Some(s.token()),
    )
    .await
    .expect("list");
    assert_eq!(listing.requested, dir_str);
    assert_eq!(
        listing.path.as_deref(),
        Some(dir.path().canonicalize().unwrap().to_str().unwrap())
    );
    assert_eq!(listing.entries.len(), 2);
    assert_eq!(listing.entries[0].name, "sub");
    assert_eq!(
        listing.entries[0].kind,
        flux_proto::flux::v1::FsEntryKind::Dir as i32
    );
    assert_eq!(listing.entries[1].name, "note.txt");
    assert_eq!(
        listing.entries[1].kind,
        flux_proto::flux::v1::FsEntryKind::File as i32
    );
    assert_eq!(listing.entries[1].size, Some(5));

    // EMPTY directory: `entries` still rides as [] (never omitted).
    let empty_dir = tempfile::tempdir().unwrap();
    let empty: FsListResponse = unary(
        &server.base,
        "/flux.v1.FileSystemService/FsList",
        FsListRequest {
            path: Some(empty_dir.path().to_str().unwrap().into()),
        },
        Some(s.token()),
    )
    .await
    .expect("empty list");
    assert!(empty.entries.is_empty());

    // File preview: content head + not truncated + the full size.
    let content: FsReadResponse = unary(
        &server.base,
        "/flux.v1.FileSystemService/FsRead",
        FsReadRequest {
            path: dir.path().join("note.txt").to_str().unwrap().into(),
        },
        Some(s.token()),
    )
    .await
    .expect("read");
    assert_eq!(content.content.as_deref(), Some("hello"));
    assert_eq!(content.truncated, Some(false));
    assert_eq!(content.size, Some(5));

    // Browse failure → inline error, no canonical path, entries [].
    let err: FsListResponse = unary(
        &server.base,
        "/flux.v1.FileSystemService/FsList",
        FsListRequest {
            path: Some("/nonexistent/flux-e2e".into()),
        },
        Some(s.token()),
    )
    .await
    .expect("error listing is inline, not a status");
    assert!(err.error.is_some(), "browse failure rides the inline error");
    assert!(err.path.is_none());
    assert!(err.entries.is_empty());
}

#[tokio::test]
async fn chat_create_requires_an_explicit_provider() {
    // There is no server default to fall back on: a CreateChat without a
    // provider pin is rejected inline before any state is made.
    let server = Server::start().await;
    let s = Stream::open(&server.base, None).await;

    let resp: CreateChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/CreateChat",
        CreateChatRequest {
            name: "No Pin".into(),
            workdir: "/tmp".into(),
            provider: "".into(),
            model: "test-model".into(),
        },
        Some(s.token()),
    )
    .await
    .expect("inline, not transport");
    assert!(resp.error.is_some(), "missing pin: {resp:?}");
    assert!(resp.chat.is_none());

    // The model is required too — providers are pure endpoints and carry
    // no default to fall back on.
    let resp: CreateChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/CreateChat",
        CreateChatRequest {
            name: "No Model".into(),
            workdir: "/tmp".into(),
            provider: "default".into(),
            model: "".into(),
        },
        Some(s.token()),
    )
    .await
    .expect("inline, not transport");
    assert!(resp.error.is_some(), "missing model: {resp:?}");

    // Nothing was created — the list comes back empty.
    let list: ListChatsResponse = unary(
        &server.base,
        "/flux.v1.ChatService/ListChats",
        ListChatsRequest {},
        Some(s.token()),
    )
    .await
    .expect("list");
    assert!(list.chats.is_empty());

    // An UNKNOWN provider id is the same story at resolution time.
    let resp: CreateChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/CreateChat",
        CreateChatRequest {
            name: "Ghost".into(),
            workdir: "/tmp".into(),
            provider: "ghost".into(),
            model: "test-model".into(),
        },
        Some(s.token()),
    )
    .await
    .expect("inline, not transport");
    assert!(resp.error.is_some(), "unknown pin: {resp:?}");
}

#[tokio::test]
async fn provider_add_remove_manages_the_registry() {
    // The registry lives in the database and the UI manages it over
    // Connect: add → ack + broadcast (id + effective url, NEVER the
    // api_key); validation failures ride the inline error; remove → ack +
    // broadcast; unknown ids reject.
    let server = Server::start().await;
    let mut s = Stream::open(&server.base, None).await;

    // Fresh server → empty registry.
    let v: ListProvidersResponse = unary(
        &server.base,
        "/flux.v1.ProviderService/ListProviders",
        ListProvidersRequest {},
        Some(s.token()),
    )
    .await
    .expect("list");
    assert!(v.providers.is_empty());

    // Add → ack + the broadcast carries the summary. The api_key must
    // never leave the server (the proto has no field for it at all).
    let ack: flux_proto::flux::v1::AddProviderResponse = unary(
        &server.base,
        "/flux.v1.ProviderService/AddProvider",
        AddProviderRequest {
            id: "main".into(),
            protocol: "openai".into(),
            url: Some("http://127.0.0.1:9/v1".into()),
            api_key: Some("sk-secret".into()),
        },
        Some(s.token()),
    )
    .await
    .expect("add");
    assert!(ack.error.is_none(), "add failed: {ack:?}");
    let el = s.expect_kind("providers").await;
    match el.kind {
        Some(ResponseKind::Providers(ProvidersBroadcast { providers })) => {
            assert_eq!(providers.len(), 1);
            let ProviderSummary { id, url, .. } = &providers[0];
            assert_eq!(id, "main");
            assert_eq!(url, "http://127.0.0.1:9/v1");
        }
        other => panic!("expected providers broadcast, got {other:?}"),
    }

    // Duplicate id → inline error, nothing changed.
    let ack: flux_proto::flux::v1::AddProviderResponse = unary(
        &server.base,
        "/flux.v1.ProviderService/AddProvider",
        AddProviderRequest {
            id: "main".into(),
            protocol: "openai".into(),
            url: None,
            api_key: None,
        },
        Some(s.token()),
    )
    .await
    .expect("inline");
    assert!(
        ack.error.as_deref().unwrap().contains("duplicate"),
        "duplicate must reject: {ack:?}"
    );

    // Empty id → inline error.
    let ack: flux_proto::flux::v1::AddProviderResponse = unary(
        &server.base,
        "/flux.v1.ProviderService/AddProvider",
        AddProviderRequest {
            id: "  ".into(),
            protocol: "openai".into(),
            url: None,
            api_key: None,
        },
        Some(s.token()),
    )
    .await
    .expect("inline");
    assert!(ack.error.is_some(), "empty id must reject: {ack:?}");

    // Unknown protocol → inline error.
    let ack: flux_proto::flux::v1::AddProviderResponse = unary(
        &server.base,
        "/flux.v1.ProviderService/AddProvider",
        AddProviderRequest {
            id: "other".into(),
            protocol: "anthropic".into(),
            url: None,
            api_key: None,
        },
        Some(s.token()),
    )
    .await
    .expect("inline");
    assert!(
        ack.error.as_deref().unwrap().contains("protocol"),
        "unknown protocol must reject: {ack:?}"
    );

    // Remove → ack + empty broadcast; a second remove rejects.
    let ack: RemoveProviderResponse = unary(
        &server.base,
        "/flux.v1.ProviderService/RemoveProvider",
        RemoveProviderRequest { id: "main".into() },
        Some(s.token()),
    )
    .await
    .expect("remove");
    assert!(ack.error.is_none(), "remove failed: {ack:?}");
    let el = s.expect_kind("providers").await;
    match el.kind {
        Some(ResponseKind::Providers(p)) => assert!(p.providers.is_empty()),
        other => panic!("expected providers broadcast, got {other:?}"),
    }
    let ack: RemoveProviderResponse = unary(
        &server.base,
        "/flux.v1.ProviderService/RemoveProvider",
        RemoveProviderRequest { id: "main".into() },
        Some(s.token()),
    )
    .await
    .expect("inline");
    assert!(
        ack.error
            .as_deref()
            .unwrap()
            .contains("unknown provider id"),
        "unknown remove must reject: {ack:?}"
    );
}

#[tokio::test]
async fn mcp_add_remove_manages_the_launch_list() {
    // The MCP launch list lives in the database and the UI manages it over
    // Connect: add → persist-first, then a LIVE apply (spawn + register).
    // The command here cannot spawn, so the ack carries the apply failure
    // INLINE while the row persists (the next restart retries it) — the
    // broadcast carries the summary with env values REDACTED (keys only).
    let server = Server::start().await;
    let mut s = Stream::open(&server.base, None).await;
    let bogus = "definitely-not-a-real-command-flux-e2e";

    // Fresh server → empty list.
    let v: ListServersResponse = unary(
        &server.base,
        "/flux.v1.McpService/ListServers",
        ListServersRequest {},
        Some(s.token()),
    )
    .await
    .expect("list");
    assert!(v.servers.is_empty());

    // Add: persist succeeds, the live apply fails fast (ENOENT) → the
    // inline error names it and the broadcast STILL lists the row.
    let ack: AddServerResponse = unary(
        &server.base,
        "/flux.v1.McpService/AddServer",
        AddServerRequest {
            id: "fs".into(),
            command: bogus.into(),
            args: vec!["-y".into(), "@scope/server".into()],
            env: std::collections::HashMap::from([("TOKEN".into(), "secret".into())]),
        },
        Some(s.token()),
    )
    .await
    .expect("inline");
    assert_eq!(ack.id, "fs");
    assert!(
        ack.error.as_deref().unwrap().contains("failed to spawn"),
        "the apply failure rides the ack inline: {ack:?}"
    );
    let el = s.expect_kind("mcp_servers").await;
    match el.kind {
        Some(ResponseKind::McpServers(McpServersBroadcast { servers })) => {
            assert_eq!(servers.len(), 1);
            assert_eq!(servers[0].id, "fs");
            assert_eq!(servers[0].command, bogus);
            assert_eq!(servers[0].args.len(), 2);
            assert_eq!(servers[0].env_keys, vec!["TOKEN".to_string()]);
        }
        other => panic!("expected mcp_servers broadcast, got {other:?}"),
    }

    // Duplicate id / empty command → inline errors (persist-layer).
    let ack: AddServerResponse = unary(
        &server.base,
        "/flux.v1.McpService/AddServer",
        AddServerRequest {
            id: "fs".into(),
            command: bogus.into(),
            args: vec![],
            env: Default::default(),
        },
        Some(s.token()),
    )
    .await
    .expect("inline");
    assert!(
        ack.error.as_deref().unwrap().contains("duplicate"),
        "dup: {ack:?}"
    );
    let ack: AddServerResponse = unary(
        &server.base,
        "/flux.v1.McpService/AddServer",
        AddServerRequest {
            id: "x".into(),
            command: "  ".into(),
            args: vec![],
            env: Default::default(),
        },
        Some(s.token()),
    )
    .await
    .expect("inline");
    assert!(ack.error.is_some(), "empty command must reject: {ack:?}");

    // Remove → ack + empty broadcast; a second remove rejects.
    let ack: RemoveServerResponse = unary(
        &server.base,
        "/flux.v1.McpService/RemoveServer",
        RemoveServerRequest { id: "fs".into() },
        Some(s.token()),
    )
    .await
    .expect("remove");
    assert!(ack.error.is_none(), "remove failed: {ack:?}");
    let el = s.expect_kind("mcp_servers").await;
    match el.kind {
        Some(ResponseKind::McpServers(m)) => assert!(m.servers.is_empty()),
        other => panic!("expected mcp_servers broadcast, got {other:?}"),
    }
    let ack: RemoveServerResponse = unary(
        &server.base,
        "/flux.v1.McpService/RemoveServer",
        RemoveServerRequest { id: "fs".into() },
        Some(s.token()),
    )
    .await
    .expect("inline");
    assert!(
        ack.error
            .as_deref()
            .unwrap()
            .contains("unknown MCP server id"),
        "unknown: {ack:?}"
    );
}

#[tokio::test]
async fn mcp_rows_survive_restart_and_a_broken_command_never_blocks_startup() {
    // A row added over Connect persists FIRST (the launch list is durable —
    // the live apply's failure rides the ack inline), and the NEXT startup
    // reads it: a command that cannot spawn must NOT block startup (the UI
    // stays reachable to fix the row) — the server skips it with a warning.
    let (dir, db_path) = test_db_path();
    let bogus = "definitely-not-a-real-command-flux-e2e";
    {
        let server_a = Server::start_on(&db_path).await;
        let s = Stream::open(&server_a.base, None).await;
        let ack: AddServerResponse = unary(
            &server_a.base,
            "/flux.v1.McpService/AddServer",
            AddServerRequest {
                id: "bad".into(),
                command: bogus.into(),
                args: vec![],
                env: Default::default(),
            },
            Some(s.token()),
        )
        .await
        .expect("inline");
        assert!(
            ack.error.as_deref().unwrap().contains("failed to spawn"),
            "the live apply fails fast and inline: {ack:?}"
        );
    } // server_a killed on drop

    // Restart on the SAME database: starts fine despite the unspawnable
    // command, and the row is still listed (fixable from the UI).
    let server_b = Server::start_on(&db_path).await;
    let s = Stream::open(&server_b.base, None).await;
    let v: ListServersResponse = unary(
        &server_b.base,
        "/flux.v1.McpService/ListServers",
        ListServersRequest {},
        Some(s.token()),
    )
    .await
    .expect("list");
    assert_eq!(v.servers.len(), 1, "the row must survive the restart");
    assert_eq!(v.servers[0].command, bogus);
    drop(server_b);
    drop(dir);
}

#[tokio::test]
async fn skills_list_reports_global_and_project_entries() {
    // The skills family: global skills ride ListSkills (the UI's Skills
    // dialog data source; the project scope rides `chat_id`). The listing
    // is the operator's real ~/.flux/skills content — the assert is the
    // WIRE SHAPE (name/description/source/removable fields), not the count.
    let server = Server::start().await;
    let s = Stream::open(&server.base, None).await;
    let v: ListSkillsResponse = unary(
        &server.base,
        "/flux.v1.SkillService/ListSkills",
        ListSkillsRequest { chat_id: None },
        Some(s.token()),
    )
    .await
    .expect("list");
    for skill in &v.skills {
        assert!(!skill.name.is_empty());
        assert!(
            skill.source == flux_proto::flux::v1::SkillSource::Global as i32
                || skill.source == flux_proto::flux::v1::SkillSource::Project as i32
        );
    }
}

// ── cancel + fork ride the same surface (lean assertions; the deep
//    round-semantics coverage lives in flux-chat's unit tests) ───────────────

#[tokio::test]
async fn cancel_is_lease_gated_and_absorbs_stale_calls() {
    let server = Server::start().await;
    let a = Stream::open(&server.base, None).await;
    let b = Stream::open(&server.base, None).await;
    add_default_provider(&server, a.token()).await;
    let chat_id = create_chat(&server, a.token(), "c", "/tmp").await;

    // B holds no lease → refused at the transport.
    let err = unary::<CancelRoundRequest, flux_proto::flux::v1::CancelRoundResponse>(
        &server.base,
        "/flux.v1.ChatService/CancelRound",
        CancelRoundRequest {
            chat_id: chat_id.clone(),
        },
        Some(b.token()),
    )
    .await
    .expect_err("busy");
    assert_eq!(err.code, 9);

    // A (holder) cancels an idle chat → absorbed no-op Ok.
    let _: flux_proto::flux::v1::CancelRoundResponse = unary(
        &server.base,
        "/flux.v1.ChatService/CancelRound",
        CancelRoundRequest {
            chat_id: chat_id.clone(),
        },
        Some(a.token()),
    )
    .await
    .expect("stale cancel absorbed");

    // Unknown chat → not_found.
    let err = unary::<CancelRoundRequest, flux_proto::flux::v1::CancelRoundResponse>(
        &server.base,
        "/flux.v1.ChatService/CancelRound",
        CancelRoundRequest {
            chat_id: "no-such".into(),
        },
        Some(a.token()),
    )
    .await
    .expect_err("unknown");
    assert_eq!(err.code, 5, "NOT_FOUND");
}

// ── the terminal side channel (raw WebSocket — binary PTY bytes) ────────────

/// Mint an identity + a chat via Connect; returns (base, token, chat_id).
async fn terminal_setup() -> (Server, String, String) {
    let server = Server::start().await;
    let s = Stream::open(&server.base, None).await;
    add_default_provider(&server, s.token()).await;
    let workdir = tempfile::tempdir().unwrap();
    let chat_id = create_chat(&server, s.token(), "term", workdir.path().to_str().unwrap()).await;
    std::mem::forget(workdir); // the shell starts there — keep it alive
    (server, s.token().to_owned(), chat_id)
}

/// Open the terminal socket and complete the first-frame auth handshake
/// (`{type:"auth", session, chat, term?}` — the token never rides the URL).
async fn term_connect(
    base: &str,
    session: &str,
    chat: &str,
    term: Option<&str>,
) -> (
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::handshake::client::Response,
) {
    use tokio_tungstenite::tungstenite::Message;
    let (mut ws, resp) =
        tokio_tungstenite::connect_async(format!("{}/ws/term", base.replace("http", "ws")))
            .await
            .unwrap();
    let mut auth = serde_json::json!({"type": "auth", "session": session, "chat": chat});
    if let Some(t) = term {
        auth["term"] = serde_json::Value::String(t.to_string());
    }
    ws.send(Message::Text(auth.to_string().into()))
        .await
        .unwrap();
    (ws, resp)
}

/// The terminal side channel: a real PTY over `/ws/term`, scoped to the
/// session identity (token minted by the Subscribe stream now). Proves the
/// full loop — connect → hello → keystrokes → shell echo — plus the auth
/// rejection for an unknown token.
#[tokio::test]
async fn terminal_side_channel_runs_a_shell_round_trip() {
    let (server, token, chat_id) = terminal_setup().await;
    use tokio_tungstenite::tungstenite::Message;

    // Auth: an unknown session token is refused with an inline error frame.
    let (mut bad, _) = term_connect(&server.base, "bogus", &chat_id, None).await;
    let msg = timeout(Duration::from_secs(5), bad.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        msg.to_text()
            .unwrap()
            .contains("unknown or expired session")
    );
    let _ = bad.close(None).await;

    // Good token: hello → binary keystrokes → the shell echoes back.
    let (mut term, _) = term_connect(&server.base, &token, &chat_id, None).await;
    term.send(Message::Binary(b"echo flux-term-e2e\n".to_vec().into()))
        .await
        .unwrap();
    let mut hello = false;
    let mut saw_echo = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !saw_echo {
        let msg = timeout(Duration::from_secs(5), term.next()).await;
        let Ok(Some(Ok(msg))) = msg else { break };
        match msg {
            Message::Text(t) => {
                if !hello {
                    assert!(t.contains("\"hello\""), "first text frame is hello: {t}");
                    hello = true;
                }
            }
            Message::Binary(b) if String::from_utf8_lossy(&b).contains("flux-term-e2e") => {
                saw_echo = true;
            }
            _ => {}
        }
    }
    assert!(hello, "never received the terminal hello");
    assert!(saw_echo, "the shell echo never came back");
    let _ = term.close(None).await;
}

/// Scrollback replay: output produced BEFORE a refresh (socket closed) is
/// rebuilt on reattach — the hello of the SECOND connection is followed by
/// the buffered bytes.
#[tokio::test]
async fn terminal_reattach_replays_scrollback() {
    let (server, token, chat_id) = terminal_setup().await;
    use tokio_tungstenite::tungstenite::Message;

    // First connection: spawn, run a marker command, read its echo, detach.
    let (mut term, _) = term_connect(&server.base, &token, &chat_id, None).await;
    term.send(Message::Binary(
        b"echo flux-scrollback-marker\n".to_vec().into(),
    ))
    .await
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut term_id = String::new();
    let mut saw_echo = false;
    while Instant::now() < deadline && !saw_echo {
        let msg = timeout(Duration::from_secs(5), term.next()).await;
        let Ok(Some(Ok(msg))) = msg else { break };
        match msg {
            Message::Text(t) => {
                if t.contains("\"hello\"")
                    && let Ok(v) = serde_json::from_str::<serde_json::Value>(&t)
                {
                    term_id = v["term"].as_str().unwrap_or("").to_string();
                }
            }
            Message::Binary(b)
                if String::from_utf8_lossy(&b).contains("flux-scrollback-marker") =>
            {
                saw_echo = true;
            }
            _ => {}
        }
    }
    assert!(saw_echo, "the marker echo never came back");
    assert!(!term_id.is_empty(), "hello never carried the terminal id");
    let _ = term.close(None).await; // the refresh: socket dies, PTY lives

    // Second connection: reattach by term id — hello then the replay.
    let (mut term2, _) = term_connect(&server.base, &token, &chat_id, Some(&term_id)).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut hello = false;
    let mut replayed = false;
    while Instant::now() < deadline && !replayed {
        let msg = timeout(Duration::from_secs(5), term2.next()).await;
        let Ok(Some(Ok(msg))) = msg else { break };
        match msg {
            Message::Text(t) => {
                assert!(t.contains("\"hello\""), "first text frame is hello: {t}");
                hello = true;
            }
            Message::Binary(b)
                if String::from_utf8_lossy(&b).contains("flux-scrollback-marker") =>
            {
                replayed = true;
            }
            _ => {}
        }
    }
    assert!(hello, "never received the reattach hello");
    assert!(replayed, "the scrollback was never replayed on reattach");
    let _ = term2.close(None).await;
}

/// Shell exit closes the loop: typing `exit` produces the `exited` frame
/// AND a server-initiated socket close — the terminal's lifecycle ends
/// with its shell.
#[tokio::test]
async fn terminal_exit_sends_frame_and_closes_socket() {
    let (server, token, chat_id) = terminal_setup().await;
    use tokio_tungstenite::tungstenite::Message;
    let (mut term, _) = term_connect(&server.base, &token, &chat_id, None).await;
    let msg = timeout(Duration::from_secs(5), term.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(msg.to_text().unwrap().contains("\"hello\""));

    term.send(Message::Binary(b"exit\n".to_vec().into()))
        .await
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut exited = false;
    let mut closed = false;
    while Instant::now() < deadline && (!exited || !closed) {
        let msg = timeout(Duration::from_secs(5), term.next()).await;
        match msg {
            Ok(Some(Ok(Message::Text(t)))) if t.contains("\"exited\"") => exited = true,
            Ok(Some(Ok(Message::Close(_)))) => closed = true,
            Ok(Some(Ok(_))) => {}
            _ => break,
        }
    }
    assert!(exited, "the exited frame never arrived");
    assert!(closed, "the server never closed the socket after exit");
}

/// Server-initiated kill reaches the attached client: deleting the chat
/// (over Connect) makes the reaper kill its terminals, and the actor
/// terminates the shell EXPLICITLY (`PtyCtl::kill`, e4pty 0.3.1) and
/// reports the exit as the usual `exited` frame + socket close.
#[tokio::test]
async fn terminal_killed_by_reaper_reports_exited_to_attached_client() {
    let (server, token, chat_id) = terminal_setup().await;
    use tokio_tungstenite::tungstenite::Message;
    let (mut term, _) = term_connect(&server.base, &token, &chat_id, None).await;
    let msg = timeout(Duration::from_secs(5), term.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(msg.to_text().unwrap().contains("\"hello\""));

    // Delete the chat over Connect. The reaper's sweep (5s cadence) sees
    // chat_gone → Kill → the shell dies explicitly and the socket learns
    // about it.
    let mut s = Stream::open(&server.base, Some(token.clone())).await;
    let _: flux_proto::flux::v1::DeleteChatResponse = unary(
        &server.base,
        "/flux.v1.ChatService/DeleteChat",
        DeleteChatRequest {
            chat_id: chat_id.clone(),
        },
        Some(s.token()),
    )
    .await
    .expect("delete");

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut exited = false;
    let mut closed = false;
    while Instant::now() < deadline && (!exited || !closed) {
        let msg = timeout(Duration::from_secs(5), term.next()).await;
        match msg {
            Ok(Some(Ok(Message::Text(t)))) if t.contains("\"exited\"") => exited = true,
            Ok(Some(Ok(Message::Close(_)))) => closed = true,
            Ok(Some(Ok(_))) => {}
            _ => break,
        }
    }
    assert!(exited, "the reaper kill never delivered the exited frame");
    assert!(closed, "the reaper kill never closed the socket");
    let _ = term.close(None).await;
    let _ = s.next_response().await; // stream drained before the drop
}

// ── graceful shutdown (SIGTERM → drain → clean exit) ─────────────────────

/// SIGTERM must exit THROUGH the drain, not via the default terminate:
/// the handler resolves on the first signal, the listener closes, the
/// (here empty) drain runs, and main returns Ok — process exit code 0.
/// Without the handler the default behavior kills the process with the
/// signal (status.success() == false), so this assertion catches a
/// regression where the signal handler stops being installed.
#[cfg(unix)]
#[tokio::test]
async fn sigterm_exits_cleanly_through_the_drain() {
    let mut server = Server::start().await;
    let pid = server.child.id().expect("server pid") as i32;
    // tokio::process has no signal API — deliver SIGTERM directly.
    unsafe {
        assert_eq!(libc::kill(pid, libc::SIGTERM), 0, "kill failed");
    }
    // No live rounds → the drain returns immediately; the ceiling only
    // guards against a hang regression in the shutdown path.
    let status = timeout(Duration::from_secs(30), async {
        loop {
            match server.child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => tokio::time::sleep(Duration::from_millis(50)).await,
                Err(e) => panic!("wait failed: {e}"),
            }
        }
    })
    .await
    .expect("server did not exit within 30s of SIGTERM");
    assert!(
        status.success(),
        "SIGTERM must exit through the drain (code 0), got: {status}"
    );
}
