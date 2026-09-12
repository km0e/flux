use serde::{Deserialize, Serialize};

/// The role of a chat message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Conversation kind — how the context accumulates.
///
/// [`ChatKind::Classic`] accumulates the full history for the model.
/// [`ChatKind::Feature`] is feature-mode: an `feature_done` tool fences a
/// rebase-to-latest at the end of each feature round, so every feature
/// starts from a clean context — the tool's orchestrated result is what
/// carries the project context forward (mechanism = generic rebase, no
/// separate driver).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(strum::Display)]
#[strum(serialize_all = "snake_case")]
pub enum ChatKind {
    Classic,
    Feature,
}

impl Default for ChatKind {
    /// Absent kind (e.g. chats persisted before kind existed) = classic.
    fn default() -> Self {
        ChatKind::Classic
    }
}

/// A single chat message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Reasoning / thinking content — must be round-tripped for
    /// tool-calling conversations (DeepSeek).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// A tool call requested by the LLM.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// A tool definition sent to the LLM describing available tools.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// A single chunk from a streaming LLM response.
///
/// The conversation loop's stream vocabulary — pure data, shared by the
/// provider implementations (producers) and the loop (consumer).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamChunk {
    /// A text delta — streamed incrementally.
    Text(String),
    /// Reasoning / thinking content (DeepSeek). Accumulated by concatenation.
    Reasoning(String),
    /// All complete tool calls of the response, delivered together just
    /// before [`StreamChunk::End`]. Providers accumulate fragments
    /// internally and emit the batch once each call's fields (id, name,
    /// arguments) are present.
    ToolCalls(Vec<ToolCall>),
    /// A tool call the model is still FORMING — forward signaling only,
    /// no state: the complete [`StreamChunk::ToolCalls`] batch remains the
    /// sole dispatch source at stream end. The identity event (name set)
    /// fires the moment a call's id + name are both parsed; argument-
    /// fragment events (args_delta set) follow while the arguments stream
    /// in. Carries no state — never accumulated, never persisted.
    ToolCallPreview {
        id: String,
        /// Set only on the identity event (first emission for this call).
        name: Option<String>,
        /// Verbatim raw-JSON argument fragment.
        args_delta: Option<String>,
    },
    /// Token usage for this completion (typically emitted as the last chunk).
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        cached_tokens: u32,
    },
    /// The stream finished normally. Emitted as the last chunk. Carries the
    /// provider's `finish_reason` when the gateway reported one (e.g. "stop",
    /// "length", "tool_calls") — `"length"` / `"content_filter"` signal a
    /// truncated answer, which consumers can surface instead of treating the
    /// partial reply as complete.
    End { finish_reason: Option<String> },
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Role ──

    #[test]
    fn role_display() {
        for (role, expected) in [
            (Role::System, "system"),
            (Role::User, "user"),
            (Role::Assistant, "assistant"),
            (Role::Tool, "tool"),
        ] {
            assert_eq!(role.to_string(), expected);
        }
    }

    #[test]
    fn role_serialize_roundtrip() {
        let roles = vec![Role::System, Role::User, Role::Assistant, Role::Tool];
        for role in roles {
            let json = serde_json::to_string(&role).unwrap();
            let back: Role = serde_json::from_str(&json).unwrap();
            assert_eq!(role, back);
        }
    }

    // ── Message ──

    #[test]
    fn message_user_constructor() {
        let msg = Message::user("Hello");
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.content, "Hello");
    }

    #[test]
    fn message_tool_constructor() {
        let msg = Message::tool("call_1", "file contents");
        assert_eq!(msg.role, Role::Tool);
        assert_eq!(msg.content, "file contents");
        assert_eq!(msg.tool_call_id, Some("call_1".into()));
    }

    #[test]
    fn message_serialize_roundtrip() {
        let msg = Message::user("hello world");
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back.role, Role::User);
        assert_eq!(back.content, "hello world");
    }

    #[test]
    fn message_tool_serialize_omits_nulls() {
        let msg = Message::user("text");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("tool_calls"));
        assert!(!json.contains("tool_call_id"));
    }

    // ── ToolCall ──

    #[test]
    fn tool_call_serialize_roundtrip() {
        let tc = ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"/tmp/test"}"#.into(),
        };
        let json = serde_json::to_string(&tc).unwrap();
        let back: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "call_1");
        assert_eq!(back.name, "read_file");
    }

    // ── ToolDefinition ──

    #[test]
    fn tool_definition_serialize_roundtrip() {
        let td = ToolDefinition {
            name: "read_file".into(),
            description: "Reads a file".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        };
        let json = serde_json::to_string(&td).unwrap();
        let back: ToolDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "read_file");
        assert_eq!(back.description, "Reads a file");
    }
}

/// Round state — the machine's authoritative summary, mirrored into a
/// shared slot by the loop after every step and snapshotted on
/// subscription replies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatStateKind {
    /// No round in flight — the machine awaits a user message.
    Idle,
    /// A provider stream or tool batch is in flight (a pending
    /// `question` tool counts — its flight is a tool execution).
    Streaming,
}
