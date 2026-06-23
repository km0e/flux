use rig_core::completion::ToolDefinition;
use schemars::{schema_for, JsonSchema};
use std::path::{Path, PathBuf};

pub mod fs;
pub mod search;
pub mod shell;

pub use fs::{FileRead, FileWrite, ListDir};
pub use search::Grep;
pub use shell::Shell;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("path escapes working directory: {0}")]
    PathEscape(String),
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    #[error("command failed: {0}")]
    CommandFailed(String),
    #[error("{0}")]
    Other(String),
}

/// Resolve a user-provided path relative to a working directory, preventing escape.
pub fn resolve_path(workdir: &Path, input: &str) -> Result<PathBuf, ToolError> {
    let path = PathBuf::from(input);
    let joined = if path.is_absolute() {
        path
    } else {
        workdir.join(path)
    };

    let canonical = joined
        .canonicalize()
        .unwrap_or_else(|_| clean_path(&workdir.join(joined.clone())));

    let canonical_workdir = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());

    if !canonical.starts_with(&canonical_workdir) {
        return Err(ToolError::PathEscape(format!(
            "Path escapes working directory: {}",
            canonical.display()
        )));
    }

    Ok(canonical)
}

fn clean_path(path: &Path) -> PathBuf {
    let mut cleaned = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::ParentDir => {
                cleaned.pop();
            }
            std::path::Component::CurDir => {}
            other => cleaned.push(other.as_os_str()),
        }
    }
    cleaned
}

pub(crate) fn schema_for_args<T: JsonSchema>() -> serde_json::Value {
    serde_json::to_value(schema_for!(T)).expect("schema generation")
}

pub(crate) fn tool_definition<T: JsonSchema>(
    name: &'static str,
    description: &'static str,
) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        parameters: schema_for_args::<T>(),
    }
}

// Re-export for consumers.
pub use rig_core::completion::ToolDefinition as RigToolDefinition;
pub use rig_core::tool::Tool as RigTool;
