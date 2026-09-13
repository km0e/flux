//! Read-side guard for the transcript invariant: a history handed to
//! `Connection::begin` must be provider-valid — every assistant
//! `tool_call` is answered by a matching tool result, or an
//! OpenAI-compatible endpoint rejects the whole request with 400.
//!
//! The machine already guarantees this at every commit point (round-
//! atomic commits, `void_cancelled_tools`, the exactly-one-
//! ToolFinished contract), and `append_messages` commits each batch in
//! one transaction — so a dangling tool_call is structurally
//! impossible today. This pass re-checks at the LOAD boundary so a
//! future machine regression (or a new incremental commit point)
//! degrades to a synthesized `INTERRUPTED_MARK` result + a warn log
//! instead of a chat that can never begin again.

use flux_core::{INTERRUPTED_MARK, Message};
use std::collections::HashSet;

/// Check the transcript invariant and repair violations in memory
/// (never written back — the load path stays read-only and the pass is
/// idempotent). A `tool_call` without any matching tool result gets a
/// synthesized `INTERRUPTED_MARK` result inserted immediately after the
/// assistant message that owns it (results must FOLLOW their call).
/// IDs that do have a real result somewhere later in the history are
/// left alone — the shape is odd but the pair exists.
pub fn validate_history(history: Vec<Message>) -> Vec<Message> {
    // IDs that have a real tool result anywhere in the history.
    let answered: HashSet<String> = history
        .iter()
        .filter_map(|m| m.tool_call_id.clone())
        .collect();

    let mut missing = 0usize;
    let mut out = Vec::with_capacity(history.len());
    for msg in history {
        let calls: Vec<String> =
            if msg.role == flux_core::Role::Assistant && !msg.tool_calls.is_empty() {
                msg.tool_calls.iter().map(|c| c.id.clone()).collect()
            } else {
                Vec::new()
            };
        let requires_results = !calls.is_empty();
        out.push(msg);
        if requires_results {
            for id in &calls {
                if !answered.contains(id) {
                    missing += 1;
                    out.push(Message::tool(id.clone(), INTERRUPTED_MARK));
                }
            }
        }
    }
    if missing > 0 {
        // Structurally unreachable today (see the module doc) — a hit is
        // a strong signal the machine's commit invariant regressed.
        tracing::warn!(
            missing,
            "history invariant violated: synthesized missing tool result(s)"
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::{Message, Role, ToolCall};

    fn assistant_with_calls(ids: &[&str]) -> Message {
        Message {
            role: Role::Assistant,
            content: String::new(),
            reasoning_content: None,
            tool_calls: ids
                .iter()
                .map(|id| ToolCall {
                    id: id.to_string(),
                    name: "bash".into(),
                    arguments: "{}".into(),
                })
                .collect(),
            tool_call_id: None,
        }
    }

    fn tool_result(id: &str) -> Message {
        Message::tool(id, "ok")
    }

    fn plain_assistant(content: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: content.into(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    #[test]
    fn clean_history_passes_through_unchanged() {
        let history = vec![
            Message::user("hi"),
            assistant_with_calls(&["a1"]),
            tool_result("a1"),
            plain_assistant("done"),
        ];
        assert_eq!(validate_history(history.clone()), history);
    }

    #[test]
    fn trailing_dangling_call_is_synthesized() {
        let out = validate_history(vec![Message::user("hi"), assistant_with_calls(&["a1"])]);
        assert_eq!(out.len(), 3);
        assert_eq!(out[2].tool_call_id.as_deref(), Some("a1"));
        assert_eq!(out[2].content, INTERRUPTED_MARK);
    }

    #[test]
    fn partially_answered_batch_gets_only_the_missing_results() {
        // assistant calls a1+a2, only a1 answered: a2 is synthesized right
        // after the assistant, the real a1 result stays where it is.
        let out = validate_history(vec![
            Message::user("hi"),
            assistant_with_calls(&["a1", "a2"]),
            tool_result("a1"),
        ]);
        assert_eq!(out.len(), 4);
        assert_eq!(out[2].tool_call_id.as_deref(), Some("a2"));
        assert_eq!(out[2].content, INTERRUPTED_MARK);
        assert_eq!(out[3].tool_call_id.as_deref(), Some("a1"));
    }

    #[test]
    fn plain_assistant_text_and_tool_roles_without_calls_are_untouched() {
        let history = vec![plain_assistant("text only"), tool_result("x")];
        assert_eq!(validate_history(history.clone()), history);
    }
}
