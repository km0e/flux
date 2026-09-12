mod fsbrowse;
mod grpc;
mod management;
mod mcp;
mod models_dev;
mod registry;
mod skills;
mod terminal;
mod transport;
mod web;

use crate::registry::ProviderRegistry;

use anyhow::Context;
use clap::Parser;
use flux_core::ToolRegistry;
use flux_session::ServerState;
use flux_store::Store;
use flux_tools::{
    BashTool, EditFileTool, GlobTool, GrepTool, ListDirectoryTool, ReadFileTool, ReplaceLinesTool,
    RustInitTool, RustVerifyTool, SkillListTool, SkillReadTool, WriteFileTool,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use terminal::TerminalHub;
use tracing::info;

/// HTTP timeouts for the provider client (seconds). Constants — the
/// config-file tunables died with the config file. `read` bounds a DEAD
/// SSE stream (no data within the window); a live long stream is never
/// killed by a total timeout.
const CONNECT_TIMEOUT_SECS: u64 = 30;
const READ_TIMEOUT_SECS: u64 = 30;

const DEFAULT_PREAMBLE: &str =
    "You are a helpful coding assistant. Use the provided tools when needed.";

/// The default database location: `$HOME/.flux/flux.db` (`USERPROFILE`
/// fallback) — the global flux home, consistent with the global skills
/// dir (`$HOME/.flux/skills`). Running `flux-server` in ANY directory
/// must not litter the database into that directory. CWD-relative
/// `flux.db` survives only as the no-home fallback (headless/service
/// contexts), warned at startup.
fn default_db_path() -> PathBuf {
    match std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        Some(home) => Path::new(&home).join(".flux").join("flux.db"),
        None => {
            tracing::warn!("no HOME/USERPROFILE set — database falls back to ./flux.db");
            PathBuf::from("flux.db")
        }
    }
}

/// Flux server — there is NO config file. Everything is a CLI flag
/// (host/port/db/preamble/web), the database (providers, MCP servers,
/// chats) or the UI. One listener serves both the web UI and `/ws`.
#[derive(Parser)]
#[command(
    name = "flux-server",
    about = "Flux agent server (web UI + WS on one port)"
)]
struct Args {
    /// Host address to bind to. Loopback by default (no app-level auth —
    /// expose remotely only behind a TLS reverse proxy with its own auth).
    #[arg(long, value_name = "HOST", default_value = "127.0.0.1")]
    host: String,
    /// Port to listen on (the web UI and the WS endpoint share it).
    #[arg(long, short, value_name = "PORT", default_value_t = 8080)]
    port: u16,
    /// SQLite database path (chats, providers, MCP servers). Default:
    /// `~/.flux/flux.db` — the global flux home (USERPROFILE fallback on
    /// Windows), next to the global skills dir; NEVER the process CWD.
    #[arg(long, value_name = "PATH")]
    db_path: Option<PathBuf>,
    /// System prompt / instructions sent to the agent on every request.
    #[arg(long, value_name = "TEXT")]
    preamble: Option<String>,
    /// Do NOT serve the browser chat UI (headless WS-only). The UI is
    /// served by default.
    #[arg(long)]
    no_web: bool,
    /// Web UI assets directory override (default: `web-ui/` next to the
    /// binary — the packaged layout; no assets found = WS-only).
    #[arg(long, value_name = "PATH")]
    web_assets_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    let db_path = args.db_path.unwrap_or_else(default_db_path);
    // The global home (`~/.flux`) may not exist yet — create it so the
    // SQLite file (and its WAL/SHM siblings) has somewhere to land.
    if let Some(parent) = db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating database directory {}", parent.display()))?;
    }
    let system_prompt: Arc<str> = Arc::from(
        args.preamble
            .clone()
            .unwrap_or_else(|| DEFAULT_PREAMBLE.to_string()),
    );

    // ── Tools ────────────────────────────────────────────────────────────
    let tool_registry = Arc::new(ToolRegistry::new());

    tool_registry.register(Arc::new(ReadFileTool::new()));
    tool_registry.register(Arc::new(GlobTool::new()));
    tool_registry.register(Arc::new(GrepTool::new()));
    tool_registry.register(Arc::new(ListDirectoryTool::new()));
    tool_registry.register(Arc::new(RustInitTool::new()));
    tool_registry.register(Arc::new(RustVerifyTool::new()));
    tool_registry.register(Arc::new(EditFileTool::new()));
    tool_registry.register(Arc::new(WriteFileTool::new()));
    tool_registry.register(Arc::new(ReplaceLinesTool::new()));
    tool_registry.register(Arc::new(BashTool::new()));
    // Agent Skills (progressive disclosure by tools): the tools' own
    // descriptions are the only always-visible surface — content loads
    // only when the model calls skill_read (see flux_tools::skills).
    tool_registry.register(Arc::new(SkillListTool::new()));
    tool_registry.register(Arc::new(SkillReadTool::new()));

    // ── Store ────────────────────────────────────────────────────────────
    info!(db_path = %db_path.display(), "opening store");
    let store = Arc::new(
        Store::open(&db_path)
            .await
            .context("failed to open store")?,
    );
    store.optimize().await;

    // ── Providers ────────────────────────────────────────────────────────
    // The registry lives ONLY in the database (managed from the web UI);
    // hydrate the in-memory map. Zero providers is legal: the server starts
    // and `chat_create` rejects unknown pins until the UI adds one — no
    // server-side default exists to pick on the client's behalf.
    let http_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .read_timeout(Duration::from_secs(READ_TIMEOUT_SECS))
        .build()
        .context("failed to build HTTP client")?;
    let registry = Arc::new(ProviderRegistry::new(store.clone(), http_client));
    let hydrated = registry
        .hydrate()
        .await
        .context("failed to load the provider registry")?;
    if hydrated == 0 {
        info!("provider registry empty — add a provider from the web UI");
    } else {
        info!(providers = hydrated, "provider registry hydrated");
    }

    // ── MCP servers ─────────────────────────────────────────────────────
    // The launch list lives in the database (UI-managed; changes are
    // applied LIVE — the manager spawns/connects, registers into the
    // global registry, and the caller fans an engine rebuild out to the
    // chats). At startup the manager RESTORES: rows connect CONCURRENTLY
    // (one hung server costs only its own init timeout) and a row that
    // fails is SKIPPED with a warning, never blocking startup (it stays
    // fixable from the UI and retried at the next restart).
    let mcp_manager = Arc::new(mcp::McpManager::new(
        Arc::clone(&tool_registry),
        mcp::production_connect(),
    ));
    mcp_manager.restore(&store).await;

    let initial_state: HashMap<String, String> = flux_chat::INITIAL_STATE
        .iter()
        .map(|(k, d)| (k.to_string(), d.to_string()))
        .collect();

    // ── ServerState ──────────────────────────────────────────────────────
    // The hydration closure resolves each cached chat's persisted pin ONCE
    // at startup — used here, never stored, so no resident
    // provider-management surface grows back into the chat layer. A miss
    // leaves the chat's instance empty; its first message then fails with
    // an error naming the dead pin (recovery: chat_provider swap).
    let lookup_registry = Arc::clone(&registry);
    let lookup =
        move |id: &str, model: &str| lookup_registry.instance(id, model).ok().map(|(p, _, _)| p);
    let server_state = Arc::new(
        ServerState::new(
            Arc::clone(&system_prompt),
            tool_registry,
            store.clone(),
            initial_state,
            &lookup,
        )
        .await
        .context("failed to build server state")?,
    );

    // Periodic vacuum to reclaim free pages in the WAL
    let vacuum_store = store.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        loop {
            interval.tick().await;
            if let Err(e) = vacuum_store.vacuum_if_needed().await {
                tracing::warn!(error = %e, "periodic vacuum failed");
            }
        }
    });

    // Web UI static site — served by DEFAULT on the SAME listener as the
    // WS endpoint (one port; the page connects back same-origin to `/ws`).
    // No startup validation: an incomplete build surfaces at request time
    // (decisions.md T-06).
    let web_root = if args.no_web {
        None
    } else {
        let root = resolve_assets_dir(args.web_assets_dir.as_deref())?;
        if root.is_none() {
            info!(
                "no web UI assets found (--web-assets-dir, or web-ui/ next to the binary) \
                 — serving WS only"
            );
        }
        root
    };

    // Terminal side channel — one PTY per chat over /ws/term, scoped to
    // the session identity (survives a refresh within the grace window).
    let terminal_hub = TerminalHub::new(Arc::clone(&server_state));

    transport::run(
        &args.host,
        args.port,
        server_state,
        registry,
        mcp_manager,
        terminal_hub,
        web_root,
    )
    .await
    .context("Failed to initialize Flux server. Check the database path and port availability.")?;
    Ok(())
}

/// Web assets directory resolution (first match wins):
/// 1. `--web-assets-dir` (CLI, used as-is)
/// 2. `web-ui/` next to the running binary (packaged distribution layout)
///
/// Otherwise `None` — the UI is simply not served (WS-only). There is no
/// CWD-relative repo guess: a hardcoded `clients/web/dist` silently works
/// or breaks depending on where the process was launched from. Devs use
/// `run-server.sh`, which always pins the repo dist with an absolute flag.
fn resolve_assets_dir(cli: Option<&Path>) -> anyhow::Result<Option<PathBuf>> {
    if let Some(d) = cli {
        return Ok(Some(d.to_path_buf()));
    }
    let packaged = std::env::current_exe()?
        .parent()
        .map(|d| d.join("web-ui"))
        .context("failed to locate the executable directory")?;
    Ok(packaged.is_dir().then_some(packaged))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_flag_wins_and_is_used_as_is() {
        let dir = std::env::temp_dir().join("flux-assets-cli");
        std::fs::create_dir_all(&dir).unwrap();
        let got = resolve_assets_dir(Some(&dir)).unwrap();
        assert_eq!(got, Some(dir));
    }

    #[test]
    fn no_cli_and_no_packaged_dir_means_no_ui() {
        // No CLI value, no web-ui/ next to the test binary → None (WS-only);
        // never a CWD-relative repo guess.
        let got = resolve_assets_dir(None).unwrap();
        assert_eq!(got, None);
    }
}
