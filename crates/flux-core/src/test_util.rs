//! Test-only fakes for the contract-layer traits (`Provider` / `Connection`).
//!
//! Gated behind the `test-util` feature so production builds never carry
//! them. Live here — not in the consumer crates — because every plane
//! that spawns conversations (flux-chat's data-plane tests, flux-session's
//! control-plane tests) fakes the SAME flux-core traits; one canonical
//! set beats per-crate duplication.

use crate::{
    Connection, CoreError, Message, Provider, StreamChunk, StreamEvent, StreamHandle, StreamSink,
    ToolDefinition,
};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex as StdMutex};

pub struct DummyConnection;

#[async_trait::async_trait]
impl Connection for DummyConnection {
    async fn open(
        &mut self,
        _pending: &[Message],
        _sink: StreamSink,
    ) -> Result<StreamHandle, CoreError> {
        unimplemented!("stub connection: open is never reached in unit tests")
    }
}

/// A provider whose `begin`s hand out stub connections (tests that spawn
/// but never open a stream).
pub struct DummyProvider;

impl Provider for DummyProvider {
    fn begin(
        &self,
        _system_prompt: &str,
        _tools: &[ToolDefinition],
        _history: &[Message],
    ) -> Box<dyn Connection + Send> {
        Box::new(DummyConnection)
    }
}

/// One scripted stream: chunks, an error, or a hang (the push stops — the
/// round never ends until the test cancels).
pub enum ScriptItem {
    Chunk(Result<StreamChunk, CoreError>),
    /// The stream stalls here (for cancel tests).
    Hang,
}

pub type Script = Vec<Vec<ScriptItem>>;

/// A connection replaying one script segment per `open`; the entire
/// remaining segment is taken each time (the `Arc` exists because
/// `CoreError` is not `Clone`).
pub struct ScriptedConnection {
    script: Arc<StdMutex<VecDeque<Vec<ScriptItem>>>>,
}

impl ScriptedConnection {
    pub fn new(script: Script) -> Self {
        Self {
            script: Arc::new(StdMutex::new(script.into_iter().collect())),
        }
    }
}

#[async_trait::async_trait]
impl Connection for ScriptedConnection {
    async fn open(
        &mut self,
        _pending: &[Message],
        sink: StreamSink,
    ) -> Result<StreamHandle, CoreError> {
        let items = self.script.lock().unwrap().pop_front().unwrap_or_default();
        let (handle, token) = StreamHandle::new();
        tokio::spawn(async move {
            for item in items {
                // Respect cancellation: a dropped handle stops the push.
                if token.is_cancelled() {
                    return;
                }
                match item {
                    ScriptItem::Chunk(Ok(c)) => sink(StreamEvent::Chunk(c)),
                    ScriptItem::Chunk(Err(e)) => {
                        sink(StreamEvent::Failed {
                            message: e.to_string(),
                            code: Some(e.error_code()),
                        });
                        return;
                    }
                    // Push stops without an End — the machine waits (the
                    // same semantics the old HangSession pump had).
                    ScriptItem::Hang => {
                        token.cancelled().await;
                        return;
                    }
                }
            }
        });
        Ok(handle)
    }
}

/// A connection whose `open`s are recorded (the swap's landing proof: the
/// consumer opened the NEW connection over the live context).
pub struct RecordingConnection {
    pub opens: Arc<StdMutex<Vec<Vec<Message>>>>,
}

#[async_trait::async_trait]
impl Connection for RecordingConnection {
    async fn open(
        &mut self,
        pending: &[Message],
        sink: StreamSink,
    ) -> Result<StreamHandle, CoreError> {
        self.opens.lock().unwrap().push(pending.to_vec());
        let (handle, token) = StreamHandle::new();
        let _ = token;
        sink(StreamEvent::Chunk(StreamChunk::Text("on swap".into())));
        sink(StreamEvent::Chunk(StreamChunk::End {
            finish_reason: None,
        }));
        Ok(handle)
    }
}

/// A provider whose `begin`s record the history they were seeded with (the
/// live-context proof for a swap apply) and hand out recording connections.
pub struct RecordingProvider {
    pub opens: Arc<StdMutex<Vec<Vec<Message>>>>,
}

impl Provider for RecordingProvider {
    fn begin(
        &self,
        _system_prompt: &str,
        _tools: &[ToolDefinition],
        history: &[Message],
    ) -> Box<dyn Connection + Send> {
        self.opens.lock().unwrap().push(history.to_vec());
        Box::new(RecordingConnection {
            opens: Arc::clone(&self.opens),
        })
    }
}

/// A provider handing out scripted connections per `begin`.
///
/// - [`ScriptedProvider::replay`]: every begin takes the ENTIRE remaining
///   shared script (a respawn replays what is left — the task-reuse tests
///   hand it two segments and require exactly ONE begin).
/// - [`ScriptedProvider::once`]: exactly one begin; a second panics
///   loudly (a respawn would mean the test's round assumptions are wrong).
pub struct ScriptedProvider {
    /// Per-begin scripts: each begin pops the next stage (an exhausted
    /// queue yields empty connections — a respawn with nothing left).
    script: StdMutex<VecDeque<Script>>,
    once: bool,
    /// How many times `begin` ran. The in-place rebuild's re-begin is
    /// observable as a growing count (tests wait on it).
    begins: std::sync::atomic::AtomicUsize,
}

impl ScriptedProvider {
    /// Multi-begin: each begin consumes the remaining shared script.
    pub fn replay(script: Script) -> Self {
        Self {
            script: StdMutex::new(VecDeque::from([script])),
            once: false,
            begins: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// How many times `begin` has run.
    pub fn begin_count(&self) -> usize {
        self.begins.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Take-once: the given script feeds exactly one spawned connection.
    pub fn once(script: Script) -> Self {
        Self {
            script: StdMutex::new(VecDeque::from([script])),
            once: true,
            begins: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Per-begin stages: begin N plays script N — the engine-rebuild
    /// tests' replay factory (the respawn lands on the next stage).
    pub fn staged(scripts: Vec<Script>) -> Self {
        Self {
            script: StdMutex::new(scripts.into()),
            once: false,
            begins: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl Provider for ScriptedProvider {
    fn begin(
        &self,
        _system_prompt: &str,
        _tools: &[ToolDefinition],
        _history: &[Message],
    ) -> Box<dyn Connection + Send> {
        self.begins
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut queue = self.script.lock().unwrap();
        let Some(script) = queue.pop_front() else {
            if self.once {
                panic!("scripted provider: begin called twice (unexpected task respawn)");
            }
            return Box::new(ScriptedConnection::new(Vec::new()));
        };
        Box::new(ScriptedConnection::new(script))
    }
}

/// A connection that pushes one delta then stalls (the cancel path's
/// trigger) — the old `HangSession` as a connection script.
pub fn hang_script() -> Script {
    vec![vec![
        ScriptItem::Chunk(Ok(StreamChunk::Text("started".into()))),
        ScriptItem::Hang,
    ]]
}

/// Parse a recorded JSON frame (null on garbage — assertions stay total).
pub fn parse_json(line: &str) -> serde_json::Value {
    serde_json::from_str(line).unwrap_or(serde_json::Value::Null)
}

/// Poll `cond` until it returns true or 5 seconds elapse.
pub async fn wait_for(mut cond: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if cond() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("condition not met within 5s");
}
