use crate::config::ServerConfig;
use crate::rpc::{handle_request, make_error, JsonRpcRequest};
use crate::state::ServerState;
use async_trait::async_trait;
use serde::Serialize;
use std::io::Write;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tracing::{debug, info};

/// Abstraction over where JSON-RPC messages are written.
#[async_trait]
pub(crate) trait Notifier: Clone + Send + Sync + 'static {
    async fn send_json<T: Serialize + Send>(&self, value: T);
}

/// Writes messages to stdout with a trailing newline.
#[derive(Clone)]
pub(crate) struct StdoutNotifier;

#[async_trait]
impl Notifier for StdoutNotifier {
    async fn send_json<T: Serialize + Send>(&self, value: T) {
        let s = serde_json::to_string(&value).unwrap();
        let mut stdout = std::io::stdout();
        writeln!(stdout, "{}", s).ok();
        stdout.flush().ok();
        debug!("send: {}", s);
    }
}

/// Writes messages to a TCP socket with a trailing newline.
#[derive(Clone)]
pub(crate) struct TcpNotifier(Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>);

impl TcpNotifier {
    pub fn new(writer: tokio::net::tcp::OwnedWriteHalf) -> Self {
        Self(Arc::new(Mutex::new(writer)))
    }
}

#[async_trait]
impl Notifier for TcpNotifier {
    async fn send_json<T: Serialize + Send>(&self, value: T) {
        let s = serde_json::to_string(&value).unwrap();
        let mut writer = self.0.lock().await;
        writer.write_all(s.as_bytes()).await.ok();
        writer.write_all(b"\n").await.ok();
        writer.flush().await.ok();
        debug!("send: {}", s);
    }
}

pub(crate) async fn run_stdio(config: ServerConfig) -> anyhow::Result<()> {
    info!("flux-server stdio starting");

    let config = Arc::new(config);
    let state = Arc::new(Mutex::new(ServerState::new()));
    let notifier = StdoutNotifier;
    let stdin = tokio::io::stdin();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        debug!("recv: {}", line);

        let req: JsonRpcRequest = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(r) if r.jsonrpc == "2.0" => r,
            Ok(_) => {
                notifier
                    .send_json(make_error(
                        None,
                        -32600,
                        "Invalid JSON-RPC version".to_string(),
                        None,
                    ))
                    .await;
                continue;
            }
            Err(e) => {
                notifier
                    .send_json(make_error(None, -32700, format!("Parse error: {e}"), None))
                    .await;
                continue;
            }
        };

        handle_request(
            req,
            Arc::clone(&state),
            notifier.clone(),
            Arc::clone(&config),
        )
        .await;
    }

    info!("flux-server stdio shutting down");
    Ok(())
}

pub(crate) async fn run_tcp(port: u16, config: ServerConfig) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    info!("flux-server tcp listening on {}", listener.local_addr()?);

    loop {
        let (socket, addr) = listener.accept().await?;
        info!("accepted connection from {}", addr);
        let config = Arc::new(config.clone());
        let state = Arc::new(Mutex::new(ServerState::new()));
        tokio::spawn(handle_connection(socket, state, config));
    }
}

async fn handle_connection(
    socket: TcpStream,
    state: Arc<Mutex<ServerState>>,
    config: Arc<ServerConfig>,
) {
    let (reader, writer) = socket.into_split();
    let notifier = TcpNotifier::new(writer);
    let reader = BufReader::new(reader);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        debug!("recv: {}", line);

        let req: JsonRpcRequest = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(r) if r.jsonrpc == "2.0" => r,
            Ok(_) => {
                notifier
                    .send_json(make_error(
                        None,
                        -32600,
                        "Invalid JSON-RPC version".to_string(),
                        None,
                    ))
                    .await;
                continue;
            }
            Err(e) => {
                notifier
                    .send_json(make_error(None, -32700, format!("Parse error: {e}"), None))
                    .await;
                continue;
            }
        };

        handle_request(
            req,
            Arc::clone(&state),
            notifier.clone(),
            Arc::clone(&config),
        )
        .await;
    }

    info!("tcp connection closed");
}
