//! The `question` tool — the model asks the user a question mid-round.
//!
//! The ecological successor of the old approval prompt: round-blocking,
//! lease-holder-answered, delivered on the router's priority control
//! channel with parking/re-delivery on claim — but the content direction
//! is reversed. The approval prompt was machine-graded policy output; the
//! question is entirely agent-produced (text + options), and the user's
//! answer flows back as the tool's result string.
//!
//! Wiring: [`QuestionBoard`] is the per-chat pending-answer registry
//! shared between the tool (awaits its oneshot inside the supervised
//! flight) and the ops layer (`question_response` looks up the id and
//! resolves it). The tool emits `WireEvent::QuestionRequired` through the
//! chat's [`OutputPort`] — the router delivers it to the lease holder and
//! parks it until answered. Cancellation rides the kernel's cooperative
//! token: a user cancel resolves the wait and the flight reports the
//! interruption uniformly (the kernel marks it; the tool never does).

use async_trait::async_trait;
use flux_core::{CoreError, OutputPort, QUESTION_TOOL, Tool, ToolCtx};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::oneshot;
use tracing::warn;

/// Upper bound on agent-provided options — a picker longer than this is
/// unusable, and a runaway list would flood the question card.
const MAX_OPTIONS: usize = 8;

/// Per-chat pending-question registry. The tool registers a oneshot per
/// invocation; the session layer's ops (`question_response`) resolves it
/// when the lease holder answers.
#[derive(Default)]
pub struct QuestionBoard {
    pending: StdMutex<HashMap<String, oneshot::Sender<String>>>,
}

impl QuestionBoard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a responder for `id`; returns the receiving end.
    fn register(&self, id: &str) -> oneshot::Receiver<String> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), tx);
        rx
    }

    /// Resolve the pending question `id` with `answer`. Unknown/stale ids
    /// (answered twice, or the round died) are logged and dropped — there
    /// is no caller-actionable failure on that path.
    /// Returns whether the answer REACHED a waiting flight: a registration
    /// whose flight is already gone (cancelled round) reports `false`, so
    /// the ops layer's outcome stays honest.
    pub fn respond(&self, id: &str, answer: &str) -> bool {
        match self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
        {
            Some(tx) => {
                let delivered = tx.send(answer.to_string()).is_ok();
                if !delivered {
                    warn!(question_id = %id, "answer arrived after the asking flight died; dropped");
                }
                delivered
            }
            None => {
                warn!(question_id = %id, "answer for unknown or already-answered question; ignored");
                false
            }
        }
    }

    /// Drop a pending registration without answering — the round ended
    /// without the question being resolved (user cancel, force-terminated
    /// flight). A later answer for the id is then unknown and dropped by
    /// [`Self::respond`]; the map does not leak the dead entry.
    pub fn deregister(&self, id: &str) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }
}

/// The per-chat `question` tool. Registered in the chat's own registry
/// (like `buf_read`/`state_get`/`state_set`); its name is reserved so MCP
/// tools can never shadow it and bypass the board.
#[derive(Clone)]
pub(crate) struct QuestionTool {
    board: Arc<QuestionBoard>,
    out: Arc<dyn OutputPort>,
}

impl QuestionTool {
    pub(crate) fn new(board: Arc<QuestionBoard>, out: Arc<dyn OutputPort>) -> Self {
        Self { board, out }
    }
}

#[derive(Deserialize)]
struct QuestionArgs {
    question: String,
    #[serde(default)]
    options: Option<Vec<String>>,
}

/// Result the model sees when the user dismisses the prompt (Escape /
/// closed card). A neutral outcome — not an error — so the round
/// continues and the model decides what to do without the answer.
const DISMISSED: &str = "(no answer: the user dismissed the question)";

#[async_trait]
impl Tool for QuestionTool {
    fn name(&self) -> &str {
        QUESTION_TOOL
    }

    fn description(&self) -> &str {
        "Ask the user a question and wait for their answer. Provide the question text and \
         2-8 concrete answer options; the user picks one, types a free-form reply, or \
         dismisses. Use it when a decision materially changes what you do next and the \
         options are not equivalent — never for confirmation of work you can just do."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question to ask, phrased so each option is a complete answer."
                },
                "options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 2,
                    "maxItems": MAX_OPTIONS,
                    "description": "2-8 concrete answer options, each a complete reply the user can pick as-is."
                }
            },
            "required": ["question"],
            "additionalProperties": false
        })
    }

    async fn call(
        &self,
        arguments: HashMap<String, Value>,
        ctx: ToolCtx,
    ) -> Result<String, CoreError> {
        let parsed: QuestionArgs = serde_json::from_value(Value::Object(
            arguments.into_iter().collect(),
        ))
        .map_err(|e| CoreError::InvalidArguments(format!("invalid question arguments: {e}")))?;
        let text = parsed.question.trim().to_string();
        if text.is_empty() {
            return Err(CoreError::InvalidArguments("question text is empty".into()));
        }
        let options = match parsed.options {
            Some(opts) => {
                let opts: Vec<String> = opts
                    .into_iter()
                    .map(|o| o.trim().to_string())
                    .filter(|o| !o.is_empty())
                    .collect();
                if opts.len() < 2 {
                    return Err(CoreError::InvalidArguments(
                        "question requires at least 2 non-empty options (omit `options` for free-form input)"
                            .into(),
                    ));
                }
                if opts.len() > MAX_OPTIONS {
                    return Err(CoreError::InvalidArguments(format!(
                        "question allows at most {MAX_OPTIONS} options"
                    )));
                }
                Some(opts)
            }
            None => None,
        };

        let id = uuid::Uuid::new_v4().to_string();
        let rx = self.board.register(&id);
        self.out
            .emit(flux_core::WireEvent::QuestionRequired {
                id: id.clone(),
                text: text.clone(),
                options: options.clone(),
            })
            .await;

        // Wait for the lease holder's answer, or stop promptly when the
        // user cancels the round (the kernel's two-tier interruption —
        // a dismissed window must not wedge the flight). The cancellation
        // path deregisters its own entry: the board must not leak it (and
        // a late answer for a dead question is dropped, not delivered).
        let answer = tokio::select! {
            answer = rx => match answer {
                Ok(a) => a,
                // Sender dropped (chat teardown) — treat as dismissed.
                Err(_) => DISMISSED.to_string(),
            },
            _ = ctx.cancel.cancelled() => {
                self.board.deregister(&id);
                return Err(CoreError::Tool("question cancelled by user".into()));
            }
        };
        Ok(format!("{text}\n→ {answer}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::WireEvent;

    fn args(question: &str, options: Option<Vec<&str>>) -> HashMap<String, Value> {
        let mut map = HashMap::new();
        map.insert("question".to_string(), Value::String(question.into()));
        if let Some(opts) = options {
            map.insert(
                "options".to_string(),
                Value::Array(
                    opts.into_iter()
                        .map(|o| Value::String(o.to_string()))
                        .collect(),
                ),
            );
        }
        map
    }

    /// Output sink collecting emitted wire events.
    #[derive(Clone)]
    struct Collect(Arc<StdMutex<Vec<WireEvent>>>);

    #[async_trait]
    impl OutputPort for Collect {
        async fn emit(&self, event: WireEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    async fn run_question(
        args: HashMap<String, Value>,
    ) -> (
        Result<String, CoreError>,
        Arc<StdMutex<Vec<WireEvent>>>,
        Arc<QuestionBoard>,
    ) {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let board = Arc::new(QuestionBoard::new());
        let tool = QuestionTool::new(board.clone(), Arc::new(Collect(events.clone())));
        // Spawn the tool call; it registers + emits asynchronously.
        let exec = tokio::spawn(async move { tool.call(args, ToolCtx::default()).await });
        // Wait for the emitted question event, then answer from the
        // "ops layer" through the board.
        let id = loop {
            if let Some(WireEvent::QuestionRequired { id, .. }) = events.lock().unwrap().first() {
                break id.clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        };
        loop {
            if board.pending.lock().unwrap().contains_key(&id) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(board.respond(&id, "second option"));
        (exec.await.unwrap(), events, board)
    }

    #[tokio::test]
    async fn question_emits_event_and_returns_answer() {
        let (result, events, board) =
            run_question(args("Which database?", Some(vec!["postgres", "sqlite"]))).await;
        let out = result.unwrap();
        assert!(out.contains("Which database?") && out.contains("second option"));
        assert!(matches!(
            events.lock().unwrap().first(),
            Some(WireEvent::QuestionRequired { .. })
        ));
        assert!(
            board.pending.lock().unwrap().is_empty(),
            "answered question deregistered"
        );
    }

    #[tokio::test]
    async fn question_validates_options() {
        let board = Arc::new(QuestionBoard::new());
        let events = Arc::new(StdMutex::new(Vec::new()));
        let tool = QuestionTool::new(board, Arc::new(Collect(events)));
        let err = tool
            .call(args("q?", Some(vec!["only-one"])), ToolCtx::default())
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::InvalidArguments(_)));
        let err = tool
            .call(
                args(
                    "q?",
                    Some(vec!["a", "b", "c", "d", "e", "f", "g", "h", "i"]),
                ),
                ToolCtx::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::InvalidArguments(_)));
        let err = tool
            .call(args("   ", None), ToolCtx::default())
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn question_cancel_stops_promptly() {
        // The kernel's cooperative token fires (user cancelled mid-question):
        // the tool must stop promptly with a tool error, and the registry
        // entry is dropped with the future (stale answers are ignored by
        // the board).
        let board = Arc::new(QuestionBoard::new());
        let events = Arc::new(StdMutex::new(Vec::new()));
        let tool = QuestionTool::new(board.clone(), Arc::new(Collect(events)));
        let ctx = ToolCtx::default();
        let token = ctx.cancel.clone();
        let exec =
            tokio::spawn(async move { tool.call(args("q?", Some(vec!["a", "b"])), ctx).await });
        loop {
            if !board.pending.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        token.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), exec)
            .await
            .expect("cancelled question must not wedge")
            .unwrap();
        assert!(matches!(result, Err(CoreError::Tool(_))));
    }

    #[tokio::test]
    async fn respond_to_unknown_id_is_ignored() {
        let board = QuestionBoard::new();
        assert!(!board.respond("ghost", "x"));
    }
}
