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
use clap::{CommandFactory, Parser};
use clap_complete::Shell;
use flux_core::ToolRegistry;
use flux_session::ServerState;
use flux_store::Store;
use flux_tools::{
    BashTool, EditFileTool, EditFilesTool, GlobTool, GrepTool, ListDirectoryTool, ReadFileTool,
    ReadFilesTool, ReplaceLinesTool, SkillListTool, WriteFileTool,
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

/// The default system prompt. Tool schemas (name, description, parameter
/// JSON) ride every request's `tools` array (flux-provider serializes them
/// at `Connection::begin`), so the prompt carries NO tool enumeration —
/// only orientation and usage policy; a duplicated list would drift from
/// the registry and pay tokens twice. Deliberately free of dates/times and
/// of anything per-chat so the base stays byte-stable: the per-chat text
/// (the workdir boundary line, the skill catalog) is composed onto this
/// base at every `begin` by the chat layer (`flux_chat::skills::
/// compose_system_prompt`), not baked in here.
const DEFAULT_PREAMBLE: &str = r#"You are an expert coding assistant operating inside Flux, a coding agent framework. You help users by reading files, executing commands, editing code, and writing new files.

Tool definitions (name, description, parameter schema) ride every request — file read/edit/write/list, bash, grep/glob, skills (skill_list/skill_read), shared state (state_get/state_set), buffered-output paging (buf_read), and user questions (question), plus any MCP-provided tools. The guidelines below govern how to use them.

Guidelines:
- Be concise in your responses.
- Show file paths clearly when working with files.
- Tools execute without user approval — never ask permission to use one. When a tool fails, the error is in the result text: read it, adjust, and retry.
- Path arguments resolve inside the chat's working directory automatically. Use relative paths (or absolute paths under the workdir); no tool takes a boundary parameter — an out-of-bounds path comes back as a tool error, so correct the path and continue.
- Prefer grep/glob over bash for locating code — they are faster and return structured results.
- When a tool result says its output was truncated and names a ref, fetch the rest with buf_read before claiming you could not see it.
- Treat a result marked INTERRUPTED as an aborted flight: its effects may be partial, so re-check state before continuing.
- Use question only when a decision materially changes what you do next and the options are not equivalent — never to confirm work you can simply do.
- For non-trivial tasks, call skill_list first and skill_read any skill that matches before concluding a capability is missing."#;

/// The default database location: `$HOME/.flux/flux.db` (`USERPROFILE`
/// fallback) — the global flux home, consistent with the global skills
/// dir (`$HOME/.flux/skills`) and the web UI override dir
/// (`$HOME/.flux/web-ui`). Running `flux-server` in ANY directory
/// must not litter the database into that directory. CWD-relative
/// `flux.db` survives only as the no-home fallback (headless/service
/// contexts), warned at startup.
fn default_db_path() -> PathBuf {
    match flux_home() {
        Some(home) => home.join("flux.db"),
        None => {
            tracing::warn!("no HOME/USERPROFILE set — database falls back to ./flux.db");
            PathBuf::from("flux.db")
        }
    }
}

/// The global flux home: `$HOME/.flux` (`USERPROFILE` fallback on
/// Windows). Shared by the database default, the global skills dir and
/// the web UI override dir.
fn flux_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|home| home.join(".flux"))
}

/// Flux server — there is NO config file. Everything is a CLI flag
/// (host/port/db/preamble/web), the database (providers, MCP servers,
/// chats) or the UI. One listener serves the Connect surface, the
/// terminal side channel, and the web UI.
/// `--help` footer: copy-pasteable usage examples.
const AFTER_HELP: &str = "\
Examples:
  flux-server                            Serve the UI + API on 127.0.0.1:8080
  flux-server --port 9000 --no-web       Headless: API only, no browser UI
  flux-server --db-path ./dev.db         Project-local database
  flux-server --generate-completions bash >> ~/.bash_completion";

#[derive(Parser)]
#[command(
    name = "flux-server",
    version,
    about = "Flux agent server (Connect API + terminal channel + web UI on one port)",
    after_help = AFTER_HELP
)]
struct Args {
    /// Host address to bind to. Loopback by default (no app-level auth —
    /// expose remotely only behind a TLS reverse proxy with its own auth).
    #[arg(
        long,
        value_name = "HOST",
        default_value = "127.0.0.1",
        env = "FLUX_HOST"
    )]
    host: String,
    /// Port to listen on (the Connect surface, the terminal side channel,
    /// and the web UI share it).
    #[arg(
        long,
        short,
        value_name = "PORT",
        default_value_t = 8080,
        env = "FLUX_PORT"
    )]
    port: u16,
    /// SQLite database path (chats, providers, MCP servers). Default:
    /// `~/.flux/flux.db` — the global flux home (USERPROFILE fallback on
    /// Windows), next to the global skills dir; NEVER the process CWD.
    #[arg(long, value_name = "PATH", env = "FLUX_DB_PATH")]
    db_path: Option<PathBuf>,
    /// System prompt / instructions sent to the agent on every request.
    #[arg(long, value_name = "TEXT", env = "FLUX_PREAMBLE")]
    preamble: Option<String>,
    /// Do NOT serve the browser chat UI (headless: API only). The UI is
    /// served by default.
    #[arg(long, env = "FLUX_NO_WEB")]
    no_web: bool,
    /// Web UI override directory (Gitea `custom/` semantics): files here
    /// shadow the embedded bundle per path — index.html, an extra asset,
    /// anything the base serves. No directory = the pure embedded UI.
    #[arg(long, value_name = "PATH", env = "FLUX_WEB_ASSETS_DIR")]
    web_assets_dir: Option<PathBuf>,
    /// Generate a shell completion script for SHELL, print it to stdout,
    /// and exit. The script wires up flag/tab completion for this binary.
    #[arg(long, value_enum)]
    generate_completions: Option<Shell>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Shell completion generation: print to stdout and exit before ANY
    // server (or logging) setup, so the stream stays clean.
    if let Some(shell) = args.generate_completions {
        let mut cmd = Args::command();
        clap_complete::generate(shell, &mut cmd, "flux-server", &mut std::io::stdout());
        return Ok(());
    }

    tracing_subscriber::fmt()
        // RUST_LOG wins when set; without it the filter defaults to INFO
        // (an empty EnvFilter shows ERRORS only, hiding the startup and
        // models.dev status logs the operator is told to look at).
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .with_writer(std::io::stderr)
        .init();

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
    tool_registry.register(Arc::new(ReadFilesTool::new()));
    tool_registry.register(Arc::new(GlobTool::new()));
    tool_registry.register(Arc::new(GrepTool::new()));
    tool_registry.register(Arc::new(ListDirectoryTool::new()));
    tool_registry.register(Arc::new(EditFileTool::new()));
    tool_registry.register(Arc::new(EditFilesTool::new()));
    tool_registry.register(Arc::new(WriteFileTool::new()));
    tool_registry.register(Arc::new(ReplaceLinesTool::new()));
    tool_registry.register(Arc::new(BashTool::new()));
    // Agent Skills (progressive disclosure by tools): the tools' own
    // descriptions are the only always-visible surface — content loads
    // only when the model calls skill_read (see flux_tools::skills).
    // Agent Skills: skill_list is the live-scan listing surface; the
    // skill catalog additionally rides the system prompt as a begin-time
    // snapshot, and skill_read is CHAT-OWNED (activation dedup, see
    // flux_chat::skills) — assembled per chat in spawn::assemble_tools.
    tool_registry.register(Arc::new(SkillListTool::new()));

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
    let (mcp_manager, mcp_events_rx) =
        mcp::McpManager::new(Arc::clone(&tool_registry), mcp::production_connect());
    let mcp_manager = Arc::new(mcp_manager);
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

    // Periodic freelist trim — incremental_vacuum (auto_vacuum=INCREMENTAL,
    // ensured at Store::open before the listener bound) is a short normal
    // write transaction: no whole-db rewrite window while serving.
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

    // MCP supervisor events (self-healing) → the SAME broadcast/rebuild
    // machinery the mutation path uses.
    tokio::spawn(mcp::consume_events(
        Arc::clone(&server_state),
        Arc::clone(&mcp_manager),
        mcp_events_rx,
    ));

    // Web UI static site — served by DEFAULT on the SAME listener as the
    // Connect surface and the terminal side channel (one port; the page
    // connects back same-origin). The UI bundle rides INSIDE the binary
    // (feature `web-ui-embed`); the override dir is optional user
    // customization. No startup validation: an incomplete override or a
    // placeholder embed surfaces at request time (decisions.md T-14).
    let web = if args.no_web {
        None
    } else {
        let override_dir = resolve_override_dir(args.web_assets_dir.as_deref());
        match &override_dir {
            None => info!("serving the web UI from the embedded bundle (no override dir)"),
            Some(dir) => {
                info!(
                    override = %dir.display(),
                    "serving the web UI — embedded bundle + per-path disk override"
                );
                // The embedded half cannot mismatch (same build as the
                // binary). An override dir CAN: a directory populated by an
                // older release shadows files of the new one, which is the
                // stale-UI symptom back through the user-customization door.
                // The stamp file is advisory-only, as before.
                if let Some(stamp) = bundle_version(dir) {
                    let server = env!("CARGO_PKG_VERSION");
                    if stamp != server {
                        tracing::warn!(
                            bundle = %stamp,
                            server = %server,
                            "the web UI override dir was populated for a different Flux version \
                             — its files shadow the embedded bundle; refresh or remove it \
                             (default: ~/.flux/web-ui)"
                        );
                    }
                }
            }
        }
        Some(web::WebUi { override_dir })
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
        web,
    )
    .await
    .context("Failed to initialize Flux server. Check the database path and port availability.")?;
    Ok(())
}

/// The web bundle's version stamp file (scripts/package-web.sh writes it
/// into every built servable root; only override DIRS still carry it —
/// the embedded bundle is built with the binary and cannot mismatch).
const BUNDLE_VERSION_FILE: &str = "web-ui-version.txt";

/// The resolved bundle's version stamp, when one is present (trimmed;
/// blank or absent = nothing to compare — the dev-flow dist).
fn bundle_version(root: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(root.join(BUNDLE_VERSION_FILE)).ok()?;
    let stamp = raw.trim();
    (!stamp.is_empty()).then(|| stamp.to_string())
}

/// The web UI override directory (first match wins):
/// 1. `--web-assets-dir` (CLI, used as-is)
/// 2. `<flux-home>/web-ui` (`$HOME/.flux` — the user-customization slot,
///    Gitea `custom/` style; only used when it EXISTS)
///
/// Otherwise `None` — the pure embedded bundle is served. There is no
/// CWD-relative repo guess: a hardcoded `clients/web/dist` silently works
/// or breaks depending on where the process was launched from. Devs use
/// `run-server.sh`, which always pins the repo dist with an absolute flag.
fn resolve_override_dir(cli: Option<&Path>) -> Option<PathBuf> {
    let flux_home = flux_home();
    resolve_override_dir_in(cli, flux_home.as_deref())
}

/// The resolution chain with the environment sources injected — the pure,
/// testable core of [`resolve_override_dir`].
fn resolve_override_dir_in(cli: Option<&Path>, flux_home: Option<&Path>) -> Option<PathBuf> {
    if let Some(d) = cli {
        return Some(d.to_path_buf());
    }
    if let Some(d) = flux_home.map(|d| d.join("web-ui"))
        && d.is_dir()
    {
        return Some(d);
    }
    None
}

// Tests are hermetic w.r.t. the host shell: proxy vars (`http_proxy` et
// al.) silently hijack reqwest's in-process clients — every 127.0.0.1
// request detours through the proxy and comes back 502, which surfaced
// as 17 phantom grpc test failures — and `FLUX_*` vars re-point the very
// flags the tests assert on (the e2e child processes inherit them too).
// The guard lives in flux-test-support (single home for the var list);
// the macro's ctor runs before `main`, i.e. before the harness spawns ANY
// test thread. connect_e2e.rs is a SEPARATE binary and carries its own.
// cfg(test): the crate is a dev-dependency, invisible to the release build.
#[cfg(test)]
flux_test_support::test_env_guard!();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_flag_wins_and_is_used_as_is() {
        let dir = std::env::temp_dir().join("flux-assets-cli");
        std::fs::create_dir_all(&dir).unwrap();
        // The flag wins even over a populated flux home, and is used as-is:
        // it is an override pointer, NOT an existence-checked resolution.
        let got = resolve_override_dir_in(Some(&dir), Some(&dir));
        assert_eq!(got, Some(dir));
    }

    #[test]
    fn flux_home_web_ui_is_the_default_override_slot() {
        let base = std::env::temp_dir().join("flux-assets-home");
        // The parameter is the FLUX home ($HOME/.flux), matching what the
        // caller gets from flux_home().
        let flux_home = base.join("home/.flux");
        std::fs::create_dir_all(flux_home.join("web-ui")).unwrap();

        let got = resolve_override_dir_in(None, Some(&flux_home));
        assert_eq!(got, Some(flux_home.join("web-ui")));
    }

    #[test]
    fn absent_flux_home_override_means_pure_embedded() {
        let base = std::env::temp_dir().join("flux-assets-none");
        let flux_home = base.join("home/.flux"); // no web-ui inside
        std::fs::create_dir_all(&flux_home).unwrap();

        let got = resolve_override_dir_in(None, Some(&flux_home));
        assert_eq!(got, None);
    }

    #[test]
    fn bundle_version_stamps_are_read_trimmed_or_ignored() {
        let dir = std::env::temp_dir().join("flux-bundle-version-read");
        std::fs::create_dir_all(&dir).unwrap();
        let stamp = dir.join(BUNDLE_VERSION_FILE);

        std::fs::write(&stamp, "0.1.5\n").unwrap();
        assert_eq!(bundle_version(&dir).as_deref(), Some("0.1.5"));

        // Whitespace-only stamp = no usable version (nothing to compare).
        std::fs::write(&stamp, "   \n").unwrap();
        assert_eq!(bundle_version(&dir), None);

        // No stamp at all (a dev dist) — nothing to compare either.
        let bare = std::env::temp_dir().join("flux-bundle-version-absent");
        let _ = std::fs::remove_dir_all(&bare);
        std::fs::create_dir_all(&bare).unwrap();
        assert_eq!(bundle_version(&bare), None);
    }
}
