//! Reserved tool-name check for the chat layer.
//!
//! Some tools are assembled per chat INSIDE the chat's own registry (after
//! the global registry is copied) — `state_get`/`state_set`, `buf_read`,
//! `skill_read`, and the `question` tool. MCP registration must skip
//! these names so an external tool cannot shadow them:
//! `register_if_absent` at assemble time would keep the MCP entry and
//! silently drop the chat-owned one.

use crate::domain::STATE_TOOL_NAMES;
use flux_core::{BUF_READ_TOOL, QUESTION_TOOL, SKILL_READ_TOOL};

/// Whether `name` is a chat-owned tool MCP registration must skip.
pub fn is_reserved_tool_name(name: &str) -> bool {
    STATE_TOOL_NAMES.contains(&name)
        || name == BUF_READ_TOOL
        || name == SKILL_READ_TOOL
        || name == QUESTION_TOOL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_chat_owned_names_are_flagged() {
        assert!(is_reserved_tool_name("state_get"));
        assert!(is_reserved_tool_name("state_set"));
        assert!(is_reserved_tool_name("buf_read"));
        assert!(is_reserved_tool_name("skill_read"));
        assert!(is_reserved_tool_name("question"));
        assert!(!is_reserved_tool_name("read_file"));
        assert!(!is_reserved_tool_name("bash"));
        assert!(!is_reserved_tool_name("skill_list"));
    }
}
