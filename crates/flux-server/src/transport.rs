//! Single HTTP transport — ONE axum server hosting the Connect surface
//! (`/flux.v1.*`), the terminal side channel (`/ws/term`), and the browser
//! UI static site (`/`, `/assets/*`).
//!
//! One port for everything: the page is served from the same origin it
//! connects back to. The chat/event identity lifecycle anchors on the
//! Subscribe stream (grpc::events) — there is no separate main WebSocket;
//! the terminal side channel remains a WebSocket because PTY bytes are
//! binary high-frequency I/O that must never interleave with chat frames.

use crate::registry::ProviderRegistry;
use crate::terminal::TerminalHub;
use crate::web;
use anyhow::Context;
use axum::Router;
use axum::routing::get;
use flux_session::ServerState;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;

/// The shared router state: the terminal route takes the same handles the
/// Connect services captured.
pub(crate) type SharedState = (
    Arc<ServerState>,
    Arc<ProviderRegistry>,
    Arc<crate::mcp::McpManager>,
    Arc<TerminalHub>,
);

pub async fn run(
    host: &str,
    port: u16,
    state: Arc<ServerState>,
    registry: Arc<ProviderRegistry>,
    mcp: Arc<crate::mcp::McpManager>,
    hub: Arc<TerminalHub>,
    web: Option<web::WebUi>,
) -> anyhow::Result<()> {
    // Drive the resume grace window: expired detached sessions get
    // their leases released. ONE reaper for the transport's lifetime —
    // spawning per connection would leak an interval task per session.
    flux_session::spawn_session_reaper(Arc::clone(&state));
    // The terminal side channel shares the grace window: detached PTYs
    // die when their session identity is reaped (or sooner).
    hub.spawn_reaper();

    let mut app = Router::new()
        .route("/ws/term", get(crate::terminal::upgrade))
        // The Connect surface rides the SAME listener (explicit paths, no
        // fallback conflict with the web router's not_found).
        .merge(crate::grpc::routes(
            Arc::clone(&state),
            Arc::clone(&registry),
            Arc::clone(&mcp),
        ))
        .with_state((state.clone(), registry, mcp, hub) as SharedState);
    if let Some(ui) = web {
        app = app.merge(web::router(&ui));
    }

    let listener = TcpListener::bind(format!("{host}:{port}"))
        .await
        .with_context(|| format!("failed to bind {host}:{port}"))?;
    info!("listening on http://{host}:{port} (connect: /flux.v1.*, term: /ws/term)");

    // No graceful-shutdown handling ON PURPOSE (the crash-only contract):
    // no signal handler is installed, so SIGTERM/SIGINT keep their default
    // disposition — the process dies instantly, even with browser streams
    // held open. A Subscribe body never completes on its own (the keepalive
    // pump runs forever), so waiting for connections is what used to wedge
    // shutdown behind the frontend. Durability is the storage layer's
    // contract instead: every transcript commit is transactional and
    // batch-atomic (flux-loop), so ANY death mode — SIGKILL, OOM, crash,
    // power loss — lands on the same recoverable state.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("server stopped")?;
    Ok(())
}
