mod agent;
mod chat;
mod config;
mod rpc;
mod state;
mod streaming;
mod tools;
mod transport;

use clap::{Parser, ValueEnum};
use config::ServerConfig;
use std::path::{Path, PathBuf};
use tracing::info;

#[derive(Debug, Clone, ValueEnum)]
enum Mode {
    /// JSON-RPC over standard input/output.
    Stdio,
    /// JSON-RPC over TCP.
    Tcp,
}

#[derive(Parser)]
#[command(name = "flux-server", about = "Flux JSON-RPC server")]
struct Args {
    /// Path to a TOML configuration file.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Transport mode.
    #[arg(value_enum, value_name = "MODE")]
    mode: Option<Mode>,

    /// TCP port (only used when mode is tcp).
    #[arg(value_name = "PORT")]
    port: Option<u16>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    let config = resolve_config(args.config.as_deref());
    let effective_mode = args
        .mode
        .clone()
        .unwrap_or(match config.server.mode.as_str() {
            "tcp" => Mode::Tcp,
            _ => Mode::Stdio,
        });
    let effective_port = args.port.unwrap_or(config.server.port);
    info!(mode = ?effective_mode, port = effective_port, "Flux server starting");

    match effective_mode {
        Mode::Stdio => transport::run_stdio(config).await,
        Mode::Tcp => {
            let port = args.port.unwrap_or(config.server.port);
            transport::run_tcp(port, config).await
        }
    }
}

/// Resolve the configuration file path using, in order:
/// 1. Explicit `--config` argument
/// 2. `FLUX_CONFIG` environment variable
/// 3. `flux.toml` in the current working directory
fn resolve_config(explicit: Option<&Path>) -> ServerConfig {
    if let Some(path) = explicit {
        return ServerConfig::load_or_default(path);
    }
    if let Ok(path) = std::env::var("FLUX_CONFIG") {
        return ServerConfig::load_or_default(&PathBuf::from(path));
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    ServerConfig::load_or_default(&cwd.join("flux.toml"))
}
