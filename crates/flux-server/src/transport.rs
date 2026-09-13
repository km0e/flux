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
use std::path::PathBuf;
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
    web_root: Option<PathBuf>,
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
    if let Some(web_root) = web_root {
        app = app.merge(web::router(&web_root));
    }

    let listener = TcpListener::bind(format!("{host}:{port}"))
        .await
        .with_context(|| format!("failed to bind {host}:{port}"))?;
    info!("listening on http://{host}:{port} (connect: /flux.v1.*, term: /ws/term)");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("server stopped")?;
    // The listener is closed (no new requests can arrive). Give live
    // rounds a bounded window to land on the machine's round boundary —
    // cancel → commit → Idle — then force-abort stragglers. Whatever
    // completed is on disk; the next rebirth resumes clean.
    state.drain(std::time::Duration::from_secs(10)).await;
    info!("drain complete; shutting down");
    Ok(())
}

/// Resolve on the FIRST of SIGINT (ctrl-c) or SIGTERM (unix). The signal
/// streams are process-global: installing the handler here replaces the
/// default terminate behavior, which is exactly the point — the process
/// exits through the drain instead of dying mid-round.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(e) => tracing::warn!(error = %e, "failed to install SIGTERM handler; ctrl-c only"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    info!("shutdown signal received");
}
