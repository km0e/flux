//! Terminal side channel — one interactive PTY per chat over a dedicated
//! WebSocket (`/ws/term`), separate from the session protocol (terminal
//! I/O is high-frequency binary and must never interleave with chat
//! frames).
//!
//! PTY allocation is e4pty (tokio-async, Unix openpty + Windows ConPTY).
//! The terminal is a UI affordance for the HUMAN, not a model tool: it
//! rides the same trust posture as everything else (same-origin, no auth
//! layer) and needs no lease — but it IS scoped to a chat (cwd = the
//! chat's workdir) and to a session identity:
//!
//! - **Auth**: the auth frame's `session` must name a resolvable identity
//!   (live, or detached within the grace window) — the same adoptability
//!   rule `session_resume` applies; terminals never adopt, they only
//!   validate.
//! - **Reattach (session identity reuse)**: on page refresh the socket
//!   dies but the PTY stays alive. The new socket re-attaches by terminal
//!   id (`term` param, remembered in the client's sessionStorage); the
//!   actor replays its scrollback ring buffer (256 KiB, drop-oldest) after
//!   the hello frame, so output written while detached — the refresh gap
//!   itself included — rebuilds on the client. When the session identity
//!   is finally reaped (no resume within the grace window), its terminals
//!   die with it.
//! - **Lifecycle**: the shell exits → `exited` frame + entry removed + socket closed (the//!   terminal's lifecycle ends with its shell — no zombie channel into a dead actor);
//!   the client may kill explicitly (`close` frame); the reaper kills
//!   detached-expired, identity-gone, or chat-deleted entries. Kill =
//!   e4pty's explicit termination (`PtyCtl::kill`, new in 0.3.1 — SIGKILL
//!   / TerminateProcess) followed by `wait` for the exit code, which is
//!   reported to the attached socket as the usual `exited` frame; the
//!   handle drops that follow tear down whatever the signal missed
//!   (master close → SIGHUP to the session leader's group / ConPTY
//!   close). A server-initiated kill (reaper) thus never leaves an
//!   attached client typing into a dead actor.
//!
//! Frames: binary = raw terminal bytes (both directions); text = JSON
//! control frames only. The FIRST text frame a client sends must be the
//! auth frame — `{type:"auth", session, chat, term?}` (term optional, the
//! reattach id): the session token is a bearer credential and never rides
//! the URL (query params land in access/proxy logs). The server answers
//! `hello {term, attached}` on success or `error {message}` + close on a
//! bad/absent auth frame (5s deadline). Later client→server frames:
//! `{type:"resize",cols,rows}` / `{type:"close"}`; server→client (post
//! auth): `hello` / `exited {code}` / `error {message}`.

use axum::extract::ws::Message;
use axum::extract::{State, WebSocketUpgrade};
use axum::response::Response;
use e4pty::prelude::*;
use flux_session::ServerState;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::{info, warn};

/// How often the reaper sweeps for dead-weight terminals.
const REAP_INTERVAL: Duration = Duration::from_secs(5);

/// Server→client terminal frames (text, JSON).
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TermServerFrame {
    Hello { term: String, attached: bool },
    Exited { code: i32 },
    Error { message: String },
}

/// Client→server terminal control frames (text, JSON).
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TermClientFrame {
    Resize { cols: u16, rows: u16 },
    Close {},
}

/// The FIRST client frame: `{type:"auth", session, chat, term?}` —
/// identity validation + terminal selection in one. Parsed separately
/// from [`TermClientFrame`] (it is a handshake, not stream control).
#[derive(Deserialize)]
pub(crate) struct TermAuth {
    /// The session resume token (identity validation only).
    pub session: String,
    /// The chat whose workdir the shell starts in.
    pub chat: String,
    /// An existing terminal id to re-attach to (refresh flow).
    pub term: Option<String>,
}

/// How long the server waits for the auth frame after the upgrade — a
/// client that never authenticates is dead weight.
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);

/// Commands into a terminal's actor task (the single owner of the PTY
/// handles — plain channel commands, no shared mutable handles).
pub(crate) enum TermCmd {
    Input(Vec<u8>),
    Resize {
        cols: u16,
        rows: u16,
    },
    Attach(mpsc::UnboundedSender<Message>),
    /// Re-send the scrollback buffer to the attached socket — the reattach
    /// path (refresh flow) fires it right after the hello frame so the
    /// client's xterm rebuilds what was on screen before the disconnect.
    Replay,
    Kill,
}

/// Scrollback ring-buffer cap per terminal. A refresh's replay rebuilds
/// the client's xterm from it; 256 KiB comfortably covers xterm's 5000-line
/// client scrollback for shell-size bursts while bounding the memory per
/// terminal (a runaway `yes` trims the OLDEST bytes, never blocks the PTY).
const SCROLLBACK_CAP: usize = 256 * 1024;

/// One live terminal: the actor's command channel plus the bookkeeping
/// the hub and socket-close path need. The PTY handles themselves live
/// ONLY inside the actor task.
pub(crate) struct TermEntry {
    /// The owning session's resume token.
    owner: String,
    chat_id: String,
    cmd_tx: mpsc::UnboundedSender<TermCmd>,
    /// Whether a terminal socket is currently attached. Set by the attach
    /// path; cleared by the socket-close path (generation-guarded so a
    /// stale socket's teardown never detaches a newer attach).
    attached: AtomicBool,
    /// Bumped on every attach; stale close handlers compare against it.
    generation: AtomicU64,
    /// When the last socket went away (None = attached or never attached).
    detached_at: std::sync::Mutex<Option<Instant>>,
}

impl TermEntry {
    /// Mark detached iff no newer attach superseded this socket.
    fn socket_closed(self: &Arc<Self>) {
        if !self.attached.swap(false, Ordering::SeqCst) {
            return; // already detached (or never attached)
        }
        // A refresh race: an older socket closing must not detach a newer
        // attach. The generation was sampled by the closer.
        *self.detached_at.lock().unwrap() = Some(Instant::now());
    }
}

/// Registry of live terminals + the validation surface (session/chat).
pub(crate) struct TerminalHub {
    state: Arc<ServerState>,
    entries: std::sync::Mutex<HashMap<String, Arc<TermEntry>>>,
}

impl TerminalHub {
    pub(crate) fn new(state: Arc<ServerState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            entries: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// Validate the request and either re-attach an existing terminal or
    /// spawn a fresh one. Returns the terminal id, the attach generation,
    /// and the actor's command channel (the socket pump drives it).
    pub(crate) async fn open(
        self: &Arc<Self>,
        auth: &TermAuth,
        out: mpsc::UnboundedSender<Message>,
    ) -> Result<(String, u64, mpsc::UnboundedSender<TermCmd>), String> {
        if !self.state.identity_resolvable(&auth.session).await {
            return Err("unknown or expired session".into());
        }
        let workdir = {
            let guard = self.state.chat_info_guard().await;
            guard
                .info(&auth.chat)
                .map(|i| i.workdir.to_string())
                .ok_or_else(|| "unknown chat".to_string())?
        };
        let workdir = std::path::PathBuf::from(workdir);

        // Re-attach: same owner, still alive.
        if let Some(term) = &auth.term {
            let entry = self
                .entries
                .lock()
                .unwrap()
                .get(term)
                .filter(|e| e.owner == auth.session)
                .cloned();
            if let Some(entry) = entry {
                let generation = entry.generation.fetch_add(1, Ordering::SeqCst) + 1;
                entry.attached.store(true, Ordering::SeqCst);
                *entry.detached_at.lock().unwrap() = None;
                let _ = entry.cmd_tx.send(TermCmd::Attach(out));
                return Ok((term.clone(), generation, entry.cmd_tx.clone()));
            }
        }

        // Fresh spawn: an interactive shell in the chat's workdir. No args —
        // every mainstream shell turns interactive on a tty stdin; TERM and
        // COLORTERM advertise what xterm.js renders.
        let (program, args) = shell_command();
        let pty = PtyBuilder::new(WindowSize::default(), Script::Exec { program, args })
            .current_dir(&workdir)
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("TERM_PROGRAM", "flux")
            .spawn()
            .map_err(|e| format!("failed to spawn shell: {e}"))?;

        let term_id = uuid::Uuid::new_v4().to_string();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let entry = Arc::new(TermEntry {
            owner: auth.session.clone(),
            chat_id: auth.chat.clone(),
            cmd_tx: cmd_tx.clone(),
            attached: AtomicBool::new(true),
            generation: AtomicU64::new(1),
            detached_at: std::sync::Mutex::new(None),
        });
        self.entries.lock().unwrap().insert(term_id.clone(), entry);

        let hub = Arc::clone(self);
        let spawn_term = term_id.clone();
        tokio::spawn(async move {
            run_pty(hub, spawn_term, pty, cmd_rx, Some(out)).await;
        });
        info!(term = %term_id, chat = %auth.chat, "terminal spawned");
        Ok((term_id, 1, cmd_tx))
    }

    /// Socket-close bookkeeping (generation-guarded detach mark).
    pub(crate) fn socket_closed(self: &Arc<Self>, term: &str, generation: u64) {
        let entries = self.entries.lock().unwrap();
        if let Some(entry) = entries.get(term) {
            // Only the socket that owns THIS generation may detach it.
            if entry.generation.load(Ordering::SeqCst) == generation {
                entry.socket_closed();
            }
        }
    }

    /// Remove a finished terminal (actor exit or reaper kill).
    fn remove(&self, term: &str) {
        self.entries.lock().unwrap().remove(term);
    }

    /// Kill a terminal on the client's explicit request.
    pub(crate) fn close(&self, term: &str) {
        if let Some(entry) = self.entries.lock().unwrap().get(term) {
            let _ = entry.cmd_tx.send(TermCmd::Kill);
        }
    }

    /// Periodic sweep: detached-expired, identity-gone, and chat-deleted
    /// terminals die. Runs for the transport's lifetime.
    pub(crate) fn spawn_reaper(self: &Arc<Self>) {
        let hub = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(REAP_INTERVAL);
            loop {
                interval.tick().await;
                hub.reap_once().await;
            }
        });
    }

    async fn reap_once(self: &Arc<Self>) {
        let grace = self.state.session_grace();
        // Snapshot ALL entries — not only detached-expired ones. The
        // detached-expired pre-filter used here previously meant a terminal
        // with its chat deleted (or identity gone) while ATTACHED was never
        // reaped: the socket keeps it alive, so the orphaned shell of a
        // deleted chat survived forever. The per-candidate async checks below
        // are cheap at terminal counts; correctness wins over the filter.
        let doomed: Vec<(String, Arc<TermEntry>)> = self
            .entries
            .lock()
            .unwrap()
            .iter()
            .map(|(id, e)| (id.clone(), Arc::clone(e)))
            .collect();
        for (id, entry) in doomed {
            // Second-level checks need async state queries — done per
            // candidate, not under the entries lock.
            let identity_gone = !self.state.identity_resolvable(&entry.owner).await;
            let chat_gone = !self.state.chat_exists(&entry.chat_id).await;
            let detached_expired = entry
                .detached_at
                .lock()
                .unwrap()
                .is_some_and(|t| t.elapsed() >= grace);
            if detached_expired || identity_gone || chat_gone {
                info!(term = %id, "terminal reaped");
                let _ = entry.cmd_tx.send(TermCmd::Kill);
                self.remove(&id);
            }
        }
    }
}

/// The shell to run per platform: `$SHELL` (fallback bash) on Unix, an
/// interactive PowerShell on Windows. No args on Unix — a shell with a tty
/// stdin is interactive by definition; ConPTY + `-NoLogo` on Windows.
fn shell_command() -> (std::ffi::OsString, Vec<std::ffi::OsString>) {
    #[cfg(unix)]
    {
        (
            std::env::var_os("SHELL").unwrap_or_else(|| "/bin/bash".into()),
            Vec::new(),
        )
    }
    #[cfg(windows)]
    {
        ("powershell".into(), vec!["-NoLogo".into()])
    }
}

/// The terminal actor: the ONLY owner of the PTY handles. Driven by the
/// command channel, forwards PTY output to whichever socket is attached,
/// and watches for child exit (`ctl.wait()` as a select arm — tokio
/// documents `Child::wait` cancel-safe and e4pty's Windows wait reads a
/// cached exit state, so a lost race drops the future without losing the
/// status). Kill (`TermCmd::Kill` or a closed command channel) uses
/// e4pty's explicit termination (0.3.1 `PtyCtl::kill`) instead of the
/// handle-drop side effects, then reports the collected exit code to the
/// attached socket — a server-initiated kill (reaper: chat deleted while
/// attached) no longer leaves the client hanging on a dead actor.
async fn run_pty(
    hub: Arc<TerminalHub>,
    term_id: String,
    pty: Pty,
    mut cmd_rx: mpsc::UnboundedReceiver<TermCmd>,
    mut out: Option<mpsc::UnboundedSender<Message>>,
) {
    let shell_pid = pty.pid();
    let Pty {
        mut reader,
        mut writer,
        mut ctl,
    } = pty;
    info!(term = %term_id, ?shell_pid, "terminal spawned");

    let mut buf = vec![0u8; 8192];
    let mut scrollback: std::collections::VecDeque<Vec<u8>> = std::collections::VecDeque::new();
    let mut scrollback_bytes: usize = 0;
    // The terminal's lifecycle ENDS with its shell: on exit the client is
    // told (Exited frame) and the socket is closed (no zombie channel into
    // a dead actor — keystrokes after exit would vanish silently).
    let finish = |out: &Option<mpsc::UnboundedSender<Message>>, code: i32| {
        if let Some(o) = out {
            let frame = TermServerFrame::Exited { code };
            let _ = o.send(Message::Text(serde_json::to_string(&frame).unwrap().into()));
            let _ = o.send(Message::Close(None));
        }
    };
    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                None | Some(TermCmd::Kill) => {
                    // Explicit termination (SIGKILL / TerminateProcess) —
                    // deterministic, unlike the drop side effects — then reap
                    // for the exit code. The handle drops on return tear down
                    // whatever the signal missed (master close → SIGHUP to
                    // surviving descendants / ConPTY close). A channel close
                    // (None) means the entry is already gone — nobody can
                    // re-attach, so kill is the right response there too.
                    let _ = ctl.kill().await;
                    let code = ctl.wait().await.unwrap_or(-1);
                    info!(term = %term_id, code, killed = true, "terminal finished");
                    finish(&out, code);
                    hub.remove(&term_id);
                    return;
                }
                Some(TermCmd::Input(bytes)) => {
                    if writer.write_all(&bytes).await.is_err() {
                        warn!(term = %term_id, "pty write failed");
                    }
                }
                Some(TermCmd::Resize { cols, rows }) => {
                    if let Err(e) = writer.window_change(cols, rows).await {
                        warn!(term = %term_id, error = %e, "window_change failed");
                    }
                }
                Some(TermCmd::Attach(sender)) => out = Some(sender),
                Some(TermCmd::Replay) => {
                    // Reattach replay: drain the scrollback to the attached
                    // socket. A dropped-oldest boundary may split an escape
                    // sequence — xterm drops the incomplete prefix, one
                    // glyph of cosmetic risk at the very top of the replay.
                    for chunk in &scrollback {
                        if let Some(o) = &out {
                            let _ = o.send(Message::Binary(chunk.clone().into()));
                        }
                    }
                }
            },
            n = reader.read(&mut buf) => match n {
                Ok(0) | Err(_) => {
                    // EOF — the child (or its last slave holder) is gone.
                    // `select!` may hand THIS arm the win while the exit is
                    // equally ready (random among ready branches) — the old
                    // code broke here without telling the client, leaving
                    // the tab 'running' on a dead PTY. Collect the exit code
                    // (usually already published) and finish properly.
                    let code = ctl.wait().await.unwrap_or(-1);
                    info!(term = %term_id, code, killed = false, "terminal finished");
                    finish(&out, code);
                    hub.remove(&term_id);
                    return;
                }
                Ok(n) => {
                    // Capture ALWAYS (attached or not): the buffer is what a
                    // reattach replays, so output written while detached
                    // (a page refresh, a background tab) survives.
                    scrollback.push_back(buf[..n].to_vec());
                    scrollback_bytes += n;
                    while scrollback_bytes > SCROLLBACK_CAP {
                        if let Some(oldest) = scrollback.front_mut() {
                            let take = oldest.len().min(scrollback_bytes - SCROLLBACK_CAP);
                            oldest.drain(..take);
                            scrollback_bytes -= take;
                            if oldest.is_empty() {
                                scrollback.pop_front();
                            }
                        } else {
                            scrollback_bytes = 0;
                        }
                    }
                    if let Some(o) = &out
                        && o.send(Message::Binary(buf[..n].to_vec().into())).is_err()
                    {
                        out = None; // socket gone — detach, keep the PTY
                    }
                }
            },
            code = ctl.wait() => {
                // The child was reaped first (Windows reader EOF lands a
                // grace period after this — the waiter closes ConPTY only
                // then; Unix EOF may lag when descendants hold the slave).
                let code = code.unwrap_or(-1);
                info!(term = %term_id, code, killed = false, "terminal finished");
                finish(&out, code);
                hub.remove(&term_id);
                return;
            }
        }
    }
}

/// The `/ws/term` upgrade handler. No query params — the handshake rides
/// the first text frame (the session token never touches the URL).
pub(crate) async fn upgrade(
    State((_, _, _, hub)): State<crate::transport::SharedState>,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade.on_upgrade(move |socket| async move { serve(hub, socket).await })
}

async fn serve(hub: Arc<TerminalHub>, socket: axum::extract::ws::WebSocket) {
    let (mut sink, mut rx) = socket.split();

    // The handshake: the FIRST text frame must be the auth frame. A bad,
    // missing, or late frame is answered with an error frame + close.
    let auth = match tokio::time::timeout(AUTH_TIMEOUT, rx.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<TermAuth>(&text) {
            Ok(a) => a,
            Err(e) => {
                warn!(error = %e, "bad terminal auth frame");
                let frame = TermServerFrame::Error {
                    message: "expected the auth frame".into(),
                };
                let _ = sink
                    .send(Message::Text(serde_json::to_string(&frame).unwrap().into()))
                    .await;
                return;
            }
        },
        _ => {
            // Closed before auth, a non-text first frame, or the deadline.
            let frame = TermServerFrame::Error {
                message: "expected the auth frame".into(),
            };
            let _ = sink
                .send(Message::Text(serde_json::to_string(&frame).unwrap().into()))
                .await;
            return;
        }
    };

    let (out_tx, mut out_rx) = mpsc::unbounded_channel();

    let opened = hub.open(&auth, out_tx.clone()).await;
    let (term_id, generation, cmd_tx) = match opened {
        Ok(ok) => ok,
        Err(message) => {
            let frame = TermServerFrame::Error { message };
            let _ = sink
                .send(Message::Text(serde_json::to_string(&frame).unwrap().into()))
                .await;
            return;
        }
    };

    // Outbound pump: actor frames → socket. A send error (socket closed)
    // ends it; the trailing Close frame makes the explicit-close path end
    // too. The close bookkeeping lives HERE (the pump is the last thing
    // holding the socket): generation-guarded detach mark.
    let hub2 = Arc::clone(&hub);
    let term2 = term_id.clone();
    tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let closing = matches!(msg, Message::Close(_));
            if sink.send(msg).await.is_err() {
                break;
            }
            if closing {
                break;
            }
        }
        hub2.socket_closed(&term2, generation);
    });

    let hello = TermServerFrame::Hello {
        term: term_id.clone(),
        attached: auth.term.is_some(),
    };
    let _ = out_tx.send(Message::Text(serde_json::to_string(&hello).unwrap().into()));

    // Reattach: after hello, replay the scrollback (the actor owns the
    // buffer; the command ordering guarantees hello precedes the replay).
    if auth.term.is_some() {
        let _ = cmd_tx.send(TermCmd::Replay);
    }

    // Inbound loop: binary = keystrokes; text = control frames.
    while let Some(msg) = rx.next().await {
        match msg {
            Ok(Message::Binary(bytes)) => {
                let _ = cmd_tx.send(TermCmd::Input(bytes.to_vec()));
            }
            Ok(Message::Text(text)) => match serde_json::from_str::<TermClientFrame>(&text) {
                Ok(TermClientFrame::Resize { cols, rows }) => {
                    let _ = cmd_tx.send(TermCmd::Resize { cols, rows });
                }
                Ok(TermClientFrame::Close {}) => {
                    hub.close(&term_id);
                    break;
                }
                Err(e) => warn!(term = %term_id, error = %e, "bad terminal frame"),
            },
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {}
        }
    }

    // End the outbound pump (the Close frame) and drop this side's channel
    // handles. The actor keeps running — the PTY survives the disconnect
    // until the reaper or the next re-attach.
    let _ = out_tx.send(Message::Close(None));
    drop(out_tx);
    drop(cmd_tx);
}
