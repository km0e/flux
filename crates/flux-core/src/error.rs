use serde::{Deserialize, Serialize};

/// Structured error codes for the unified `error` message.
/// Required on every `error` — clients dispatch on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    ProviderConnection,
    /// A tool execution failed (maps from `CoreError::Tool`).
    ToolExecution,
    /// Tool arguments failed to parse (maps from `CoreError::InvalidArguments`).
    InvalidArguments,
    /// Another session holds the chat's lease — the operation is refused.
    ChatBusy,
    /// The chat id does not exist in the manager.
    ChatNotFound,
    /// Default code for a stream error without a specific mapping (e.g.
    /// internal failure). The chat returns to idle.
    StreamCrashed,
    /// A viewer dropped stream frames; reload history to resync.
    StreamGap,
    /// The request was malformed or out of order.
    InvalidRequest,
    /// Unexpected internal failure.
    Internal,
}

/// Core error type for the flux-core crate.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// An error from the LLM provider.
    #[error("provider error: {0}")]
    Provider(String),

    /// A tool execution error.
    #[error("tool error: {0}")]
    Tool(String),

    /// Failed to parse tool arguments.
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),

    /// An unexpected internal failure (persistence, orchestration, …).
    #[error("internal error: {0}")]
    Internal(String),
}

impl CoreError {
    /// The wire error code this core error surfaces to clients. The mapping
    /// belongs with the type (both live in flux-core) — callers just ask
    /// `e.error_code()` instead of keeping a parallel lookup elsewhere.
    pub fn error_code(&self) -> ErrorCode {
        match self {
            CoreError::Provider(_) => ErrorCode::ProviderConnection,
            CoreError::Tool(_) => ErrorCode::ToolExecution,
            CoreError::InvalidArguments(_) => ErrorCode::InvalidArguments,
            CoreError::Internal(_) => ErrorCode::Internal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_error_display() {
        let err = CoreError::Provider("rate limited".into());
        assert_eq!(err.to_string(), "provider error: rate limited");
    }

    #[test]
    fn tool_error_display() {
        let err = CoreError::Tool("permission denied".into());
        assert_eq!(err.to_string(), "tool error: permission denied");
    }

    #[test]
    fn invalid_arguments_display() {
        let err = CoreError::InvalidArguments("missing field".into());
        assert_eq!(err.to_string(), "invalid arguments: missing field");
    }

    #[test]
    fn error_code_maps_each_variant() {
        use ErrorCode;
        assert_eq!(
            CoreError::Provider("x".into()).error_code(),
            ErrorCode::ProviderConnection
        );
        assert_eq!(
            CoreError::Tool("x".into()).error_code(),
            ErrorCode::ToolExecution
        );
        assert_eq!(
            CoreError::InvalidArguments("x".into()).error_code(),
            ErrorCode::InvalidArguments
        );
    }
}
