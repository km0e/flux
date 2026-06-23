use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;

use crate::{resolve_path, tool_definition, ToolError};

/// Search for a pattern inside files under a directory.
pub struct Grep {
    workdir: PathBuf,
}

impl Grep {
    pub fn new(workdir: impl Into<PathBuf>) -> Self {
        Self {
            workdir: workdir.into(),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GrepArgs {
    /// Regular expression or literal to search
    pub pattern: String,
    /// Directory or file to search (default: working directory)
    pub path: Option<String>,
}

impl Tool for Grep {
    const NAME: &'static str = "grep";
    type Args = GrepArgs;
    type Output = String;
    type Error = ToolError;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        tool_definition::<GrepArgs>(
            Self::NAME,
            "Search for a pattern inside files under a directory.",
        )
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let search_path = args.path.as_deref().unwrap_or(".");
        let resolved = resolve_path(&self.workdir, search_path)?;

        let mut matches = Vec::new();
        Self::search_dir(&resolved, &args.pattern, &mut matches).await?;
        matches.sort();

        if matches.is_empty() {
            Ok("No matches found.".to_string())
        } else {
            Ok(matches.join("\n"))
        }
    }
}

impl Grep {
    async fn search_dir(
        dir: &PathBuf,
        pattern: &str,
        matches: &mut Vec<String>,
    ) -> Result<(), ToolError> {
        let mut reader = tokio::fs::read_dir(dir).await?;
        while let Some(entry) = reader.next_entry().await? {
            let path = entry.path();
            let meta = entry.metadata().await?;
            if meta.is_dir() {
                Box::pin(Self::search_dir(&path, pattern, matches)).await?;
            } else if meta.is_file() {
                Self::search_file(&path, pattern, matches).await?;
            }
        }
        Ok(())
    }

    async fn search_file(
        path: &PathBuf,
        pattern: &str,
        matches: &mut Vec<String>,
    ) -> Result<(), ToolError> {
        let content = match tokio::fs::read_to_string(path).await {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        for (i, line) in content.lines().enumerate() {
            if line.contains(pattern) {
                matches.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
            }
        }
        Ok(())
    }
}
