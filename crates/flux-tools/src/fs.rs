use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;

use crate::{resolve_path, tool_definition, ToolError};

/// Read the contents of a file.
pub struct FileRead {
    workdir: PathBuf,
}

impl FileRead {
    pub fn new(workdir: impl Into<PathBuf>) -> Self {
        Self {
            workdir: workdir.into(),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FileReadArgs {
    /// Relative or absolute file path
    pub path: String,
}

impl Tool for FileRead {
    const NAME: &'static str = "file_read";
    type Args = FileReadArgs;
    type Output = String;
    type Error = ToolError;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        tool_definition::<FileReadArgs>(Self::NAME, "Read the contents of a file.")
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let resolved = resolve_path(&self.workdir, &args.path)?;
        tokio::fs::read_to_string(&resolved)
            .await
            .map_err(ToolError::Io)
    }
}

/// Write content to a file, overwriting if it exists.
pub struct FileWrite {
    workdir: PathBuf,
}

impl FileWrite {
    pub fn new(workdir: impl Into<PathBuf>) -> Self {
        Self {
            workdir: workdir.into(),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FileWriteArgs {
    /// Relative or absolute file path
    pub path: String,
    /// Content to write
    pub content: String,
}

impl Tool for FileWrite {
    const NAME: &'static str = "file_write";
    type Args = FileWriteArgs;
    type Output = String;
    type Error = ToolError;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        tool_definition::<FileWriteArgs>(
            Self::NAME,
            "Write content to a file, overwriting if it exists.",
        )
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let resolved = resolve_path(&self.workdir, &args.path)?;
        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let len = args.content.len();
        tokio::fs::write(&resolved, args.content).await?;
        Ok(format!("Wrote {} bytes to {}", len, resolved.display()))
    }
}

/// List files and directories inside a directory.
pub struct ListDir {
    workdir: PathBuf,
}

impl ListDir {
    pub fn new(workdir: impl Into<PathBuf>) -> Self {
        Self {
            workdir: workdir.into(),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListDirArgs {
    /// Relative or absolute directory path
    pub path: String,
}

impl Tool for ListDir {
    const NAME: &'static str = "list_dir";
    type Args = ListDirArgs;
    type Output = String;
    type Error = ToolError;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        tool_definition::<ListDirArgs>(Self::NAME, "List files and directories inside a directory.")
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let resolved = resolve_path(&self.workdir, &args.path)?;
        let mut entries: Vec<String> = Vec::new();
        let mut reader = tokio::fs::read_dir(&resolved).await?;
        while let Some(entry) = reader.next_entry().await? {
            let meta = entry.metadata().await?;
            let kind = if meta.is_dir() { "dir" } else { "file" };
            entries.push(format!("{} {}", kind, entry.file_name().to_string_lossy()));
        }
        entries.sort();
        Ok(entries.join("\n"))
    }
}
