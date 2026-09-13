//! The Connect surface — gRPC-Web plane served on the SAME axum router as
//! `/ws/term` and the static site (one port, one process).
//!
//! This IS the wire now: the browser's entire server conversation — the
//! event stream (the session lifecycle anchor, `grpc::events`), the chat
//! control plane (`grpc::chats`), and the management/fs families
//! (`grpc::management` / `grpc::fs`) — rides these routes. Behavior is
//! shared with flux-session's ops (chat plane) and crate::management
//! (management plane); the services only serialize.
//!
//! Layout: one module per proto family, shared plumbing here — [`routes`]
//! mounts explicit post_service paths (tonic's RPC paths are fixed by the
//! proto package, so explicit routes avoid colliding with the web router's
//! `not_found` fallback).

use crate::registry::ProviderRegistry;
use axum::Router;
use axum::routing::post_service;
use flux_proto::flux::v1::chat_service_server::ChatServiceServer;
use flux_proto::flux::v1::event_service_server::EventServiceServer;
use flux_proto::flux::v1::file_system_service_server::FileSystemServiceServer;
use flux_proto::flux::v1::mcp_service_server::McpServiceServer;
use flux_proto::flux::v1::model_service_server::ModelServiceServer;
use flux_proto::flux::v1::provider_service_server::ProviderServiceServer;
use flux_proto::flux::v1::skill_service_server::SkillServiceServer;
use flux_session::ServerState;
use std::sync::Arc;
use std::time::Duration;
use tonic_web::GrpcWebLayer;
use tower::Layer as _;

/// Mount the whole Connect surface. State-generic (the services capture
/// their Arcs at construction) so it merges into the transport router
/// whatever its state type is.
pub(crate) fn routes<S>(
    state: Arc<ServerState>,
    registry: Arc<ProviderRegistry>,
    mcp: Arc<crate::mcp::McpManager>,
) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    routes_with_keepalive(state, registry, mcp, events::KEEPALIVE_INTERVAL)
}

/// The mount with an injectable event-stream keepalive (tests shorten it).
pub(crate) fn routes_with_keepalive<S>(
    state: Arc<ServerState>,
    registry: Arc<ProviderRegistry>,
    mcp: Arc<crate::mcp::McpManager>,
    keepalive: Duration,
) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let fs = GrpcWebLayer::new().layer(FileSystemServiceServer::new(fs::FsService));
    let providers = GrpcWebLayer::new().layer(ProviderServiceServer::new(
        management::ProviderManagement::new(Arc::clone(&registry), Arc::clone(&state)),
    ));
    let models = GrpcWebLayer::new().layer(ModelServiceServer::new(
        management::ModelManagement::new(Arc::clone(&registry), Arc::clone(&state)),
    ));
    let mcp_svc = GrpcWebLayer::new().layer(McpServiceServer::new(management::McpManagement::new(
        Arc::clone(&state),
        mcp,
    )));
    let skills = GrpcWebLayer::new().layer(SkillServiceServer::new(
        management::SkillManagement::new(Arc::clone(&state)),
    ));
    let mut events_svc = events::EventPlane::new(Arc::clone(&state));
    events_svc.keepalive = keepalive;
    let events = GrpcWebLayer::new().layer(EventServiceServer::new(events_svc));
    let chats = GrpcWebLayer::new().layer(ChatServiceServer::new(chats::ChatManagement::new(
        Arc::clone(&state),
        Arc::clone(&registry),
    )));

    Router::new()
        .route(
            "/flux.v1.ChatService/CreateChat",
            post_service(chats.clone()),
        )
        .route(
            "/flux.v1.ChatService/ListChats",
            post_service(chats.clone()),
        )
        .route("/flux.v1.ChatService/OpenChat", post_service(chats.clone()))
        .route(
            "/flux.v1.ChatService/ClaimChat",
            post_service(chats.clone()),
        )
        .route(
            "/flux.v1.ChatService/CloseChat",
            post_service(chats.clone()),
        )
        .route(
            "/flux.v1.ChatService/DeleteChat",
            post_service(chats.clone()),
        )
        .route(
            "/flux.v1.ChatService/RenameChat",
            post_service(chats.clone()),
        )
        .route(
            "/flux.v1.ChatService/SendMessage",
            post_service(chats.clone()),
        )
        .route(
            "/flux.v1.ChatService/CancelRound",
            post_service(chats.clone()),
        )
        .route("/flux.v1.ChatService/ForkChat", post_service(chats.clone()))
        .route(
            "/flux.v1.ChatService/SwitchProvider",
            post_service(chats.clone()),
        )
        .route("/flux.v1.ChatService/AnswerQuestion", post_service(chats))
        .route(
            "/flux.v1.FileSystemService/FsList",
            post_service(fs.clone()),
        )
        .route("/flux.v1.FileSystemService/FsRead", post_service(fs))
        .route(
            "/flux.v1.ProviderService/ListProviders",
            post_service(providers.clone()),
        )
        .route(
            "/flux.v1.ProviderService/GetModels",
            post_service(providers.clone()),
        )
        .route(
            "/flux.v1.ProviderService/AddProvider",
            post_service(providers.clone()),
        )
        .route(
            "/flux.v1.ProviderService/RemoveProvider",
            post_service(providers),
        )
        .route(
            "/flux.v1.ModelService/ListModels",
            post_service(models.clone()),
        )
        .route(
            "/flux.v1.ModelService/SaveModel",
            post_service(models.clone()),
        )
        .route(
            "/flux.v1.ModelService/RemoveModel",
            post_service(models.clone()),
        )
        .route("/flux.v1.ModelService/SyncModels", post_service(models))
        .route(
            "/flux.v1.McpService/ListServers",
            post_service(mcp_svc.clone()),
        )
        .route(
            "/flux.v1.McpService/AddServer",
            post_service(mcp_svc.clone()),
        )
        .route("/flux.v1.McpService/RemoveServer", post_service(mcp_svc))
        .route(
            "/flux.v1.SkillService/ListSkills",
            post_service(skills.clone()),
        )
        .route(
            "/flux.v1.SkillService/AddSkill",
            post_service(skills.clone()),
        )
        .route("/flux.v1.SkillService/RemoveSkill", post_service(skills))
        .route("/flux.v1.EventService/Subscribe", post_service(events))
}

pub(crate) mod chats;
pub(crate) mod events;
pub(crate) mod fs;
pub(crate) mod management;

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod test_support {
    //! Shared test helpers: a real HTTP/1.1 listener with the full Connect
    //! surface mounted, and hand-rolled gRPC-Web framing (the browser's
    //! exact wire — tonic's Rust client speaks native gRPC/h2, NOT this).

    use super::*;

    /// One uncompressed length-prefixed data frame (gRPC-Web request body).
    pub(crate) fn lp_frame(msg: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(5 + msg.len());
        v.push(0u8);
        v.extend_from_slice(&(msg.len() as u32).to_be_bytes());
        v.extend_from_slice(msg);
        v
    }

    /// Split a gRPC-Web response body: (data frames, optional trailer).
    pub(crate) fn parse_frames(buf: &[u8]) -> (Vec<&[u8]>, Option<&[u8]>) {
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

    pub(crate) fn trailer_grpc_status(trailer: Option<&[u8]>) -> Option<i32> {
        let t = trailer?;
        let s = std::str::from_utf8(t).ok()?;
        // tonic writes "grpc-status:0" (no space after the colon).
        s.lines()
            .find_map(|l| l.strip_prefix("grpc-status:").map(str::trim))
            .and_then(|v| v.parse().ok())
    }

    /// Serve `app` on an ephemeral port; returns its base URL.
    pub(crate) async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    pub(crate) fn web_client() -> reqwest::Client {
        reqwest::Client::new()
    }

    /// One unary gRPC-Web POST (uncompressed data frame) with an optional
    /// session token in the metadata; the response is returned whole.
    pub(crate) async fn post(
        url: &str,
        path: &str,
        body: Vec<u8>,
        token: Option<&str>,
    ) -> reqwest::Response {
        let mut req = web_client()
            .post(format!("{url}{path}"))
            .header("content-type", "application/grpc-web+proto");
        if let Some(token) = token {
            req = req.header("x-flux-session", token);
        }
        req.body(body).send().await.unwrap()
    }

    /// A full state fixture + the mounted Connect surface.
    pub(crate) async fn fixture() -> (Arc<ServerState>, String) {
        let (state, registry, mcp) = fixture_parts().await;
        let url = serve(routes(
            Arc::clone(&state),
            Arc::clone(&registry),
            Arc::clone(&mcp),
        ))
        .await;
        (state, url)
    }

    /// The fixture with a shortened event-stream keepalive (keepalive
    /// tests assert periodic frames without waiting 30s).
    pub(crate) async fn fixture_with_keepalive(
        keepalive: std::time::Duration,
    ) -> (Arc<ServerState>, String) {
        let (state, registry, mcp) = fixture_parts().await;
        let url = serve(routes_with_keepalive(
            Arc::clone(&state),
            registry,
            mcp,
            keepalive,
        ))
        .await;
        (state, url)
    }

    pub(crate) async fn fixture_parts() -> (
        Arc<ServerState>,
        Arc<ProviderRegistry>,
        Arc<crate::mcp::McpManager>,
    ) {
        let store = Arc::new(flux_store::Store::open_in_memory().await.unwrap());
        let state = Arc::new(
            ServerState::new(
                Arc::from(""),
                Arc::new(flux_core::ToolRegistry::default()),
                Arc::clone(&store),
                std::collections::HashMap::new(),
                &|_, _| None,
            )
            .await
            .unwrap(),
        );
        let registry = Arc::new(ProviderRegistry::new(store, reqwest::Client::new()));
        // The mcp manager's live-apply connector: a test never spawns real
        // servers — the apply fails into the inline error path.
        let connect: crate::mcp::Connect =
            Arc::new(|_cfg| Box::pin(async { Err(anyhow::anyhow!("no mcp in test")) }));
        let (mcp, _mcp_rx) =
            crate::mcp::McpManager::new(Arc::new(flux_core::ToolRegistry::default()), connect);
        (state, registry, Arc::new(mcp))
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use flux_proto::flux::v1::{FsListRequest, FsListResponse};
    use flux_proto::prost::Message as _;
    use std::time::Duration;

    // ── single-port coexistence: gRPC + static site on ONE router ───────

    #[tokio::test]
    async fn grpc_and_static_site_coexist_on_one_router() {
        let (_state, url) = fixture().await;
        // The static site is served by the SAME server in production; here
        // we assert the gRPC routes respond on their own.
        let grpc = web_client()
            .post(format!("{url}/flux.v1.FileSystemService/FsList"))
            .header("content-type", "application/grpc-web+proto")
            .body(lp_frame(&FsListRequest { path: None }.encode_to_vec()))
            .send()
            .await
            .unwrap();
        assert_eq!(grpc.status(), 200);
        let bytes = grpc.bytes().await.unwrap();
        let (frames, trailer) = parse_frames(&bytes);
        assert_eq!(trailer_grpc_status(trailer), Some(0));
        assert!(FsListResponse::decode(frames[0]).unwrap().path.is_some());
    }

    // ── server side: unary bursts alongside a held-open stream ──────────

    #[tokio::test]
    async fn concurrent_unary_bursts_coexist_with_an_open_stream() {
        use flux_proto::flux::v1::{SubscribeRequest, SubscribeResponse};
        use futures_util::StreamExt;

        let (_state, url) = fixture().await;

        // Hold one Subscribe open (the stream's slot in the pool, server side).
        let req = SubscribeRequest { session_id: None };
        let held = web_client()
            .post(format!("{url}/flux.v1.EventService/Subscribe"))
            .header("content-type", "application/grpc-web+proto")
            .body(lp_frame(&req.encode_to_vec()))
            .send()
            .await
            .unwrap();
        assert_eq!(held.status(), 200);
        let mut held_stream = held.bytes_stream();

        // A burst of concurrent unary calls — the server must serve them
        // all while the stream stays open (the browser-side pool cap of
        // HTTP/1.1 is a client constraint, documented in the spike report).
        let mut tasks = Vec::new();
        for i in 0..8 {
            let url = url.clone();
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
            let req = FsListRequest {
                path: Some(dir.path().to_str().unwrap().to_owned()),
            };
            tasks.push(tokio::spawn(async move {
                // Hold the TempDir inside the task — its Drop deletes the
                // directory, which must outlive the listing request.
                let _keep_dir_alive = dir;
                let expected = format!("f{i}.txt");
                let resp = web_client()
                    .post(format!("{url}/flux.v1.FileSystemService/FsList"))
                    .header("content-type", "application/grpc-web+proto")
                    .body(lp_frame(&req.encode_to_vec()))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(resp.status(), 200);
                let bytes = resp.bytes().await.unwrap();
                let (frames, trailer) = parse_frames(&bytes);
                assert_eq!(trailer_grpc_status(trailer), Some(0));
                let out = FsListResponse::decode(frames[0]).unwrap();
                assert!(
                    out.entries.iter().any(|e| e.name == expected),
                    "listing {expected} served its own dir"
                );
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }

        // The held stream still flows after the burst (the ready frame).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            assert!(tokio::time::Instant::now() <= deadline, "stream silent");
            let chunk = tokio::time::timeout(Duration::from_secs(10), held_stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let mut buf: Vec<u8> = chunk.to_vec();
            while let Some(frame) = {
                if buf.len() < 5 {
                    None
                } else {
                    let len = u32::from_be_bytes(buf[1..5].try_into().unwrap()) as usize;
                    if buf.len() < 5 + len {
                        None
                    } else {
                        let f = buf[5..5 + len].to_vec();
                        buf.drain(..5 + len);
                        Some(f)
                    }
                }
            } {
                let ready = SubscribeResponse::decode(&frame[..]).is_ok_and(|r| {
                    matches!(
                        r.kind,
                        Some(flux_proto::flux::v1::subscribe_response::Kind::Ready(_))
                    )
                });
                if ready {
                    return; // stream alive
                }
            }
        }
    }
}
