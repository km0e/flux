use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;
use tokio::process::Command;

use crate::{tool_definition, ToolError};

/// Execute a shell command in the working directory. Use with caution.
pub struct Shell {
    workdir: PathBuf,
    allowed: bool,
}

impl Shell {
    pub fn new(workdir: impl Into<PathBuf>) -> Self {
        Self {
            workdir: workdir.into(),
            allowed: true,
        }
    }

    pub fn disabled() -> Self {
        Self {
            workdir: PathBuf::from("."),
            allowed: false,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShellArgs {
    /// Shell command to execute
    pub command: String,
    /// Timeout in seconds (default 30)
    pub timeout: Option<u64>,
}

impl Tool for Shell {
    const NAME: &'static str = "shell";
    type Args = ShellArgs;
    type Output = String;
    type Error = ToolError;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        tool_definition::<ShellArgs>(
            Self::NAME,
            "Execute a shell command in the working directory. Use with caution.",
        )
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if !self.allowed {
            return Err(ToolError::Other(
                "Shell tool is disabled by configuration".to_string(),
            ));
        }

        let timeout_seconds = args.timeout.unwrap_or(30);

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_seconds),
            Command::new("sh")
                .arg("-c")
                .arg(&args.command)
                .current_dir(&self.workdir)
                .output(),
        )
        .await
        .map_err(|_| {
            ToolError::CommandFailed(format!("Shell command timed out after {timeout_seconds}s"))
        })?
        .map_err(ToolError::Io)?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            return Err(ToolError::CommandFailed(format!(
                "Command failed ({}): {stderr}",
                output.status.code().unwrap_or(-1)
            )));
        }

        Ok(if stderr.is_empty() {
            stdout
        } else {
            format!("{stdout}\n{stderr}")
        })
    }
}
