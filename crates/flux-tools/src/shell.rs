use crate::format_command_output;
use crate::subprocess::RunError;
use flux_core::{CoreError, ToolCtx};
use std::time::Duration;

/// Default command timeout (seconds). 30s blocked real workloads (builds,
/// installs, test runs); 5 minutes covers the vast majority of single
/// commands. A deliberate semantic change from the old hardcoded 30s.
const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// Clamp bounds. The floor keeps `timeout: 0` meaningless; the ceiling
/// bounds a single flight's hold on the round under the no-approval model
/// — genuinely long work belongs in the background (`… &` + polling).
const MIN_TIMEOUT_SECS: u64 = 1;
const MAX_TIMEOUT_SECS: u64 = 1800;

// ---------------------------------------------------------------------------
// BashTool
// ---------------------------------------------------------------------------

#[derive(Default, flux_macros::Tool, ::serde::Deserialize)]
#[tool(
    name = "bash",
    description = "Execute a shell command in the working directory. Returns stdout and stderr. Optional 'timeout' in seconds (default 300, max 1800); for longer-running work background the command ('… &') and poll its output instead. Very long output is buffered by the system and can be read back in pages with buf_read."
)]
pub struct BashTool {
    /// The shell command to execute.
    command: String,
    /// Timeout in seconds (default 300, clamped to 1..=1800).
    timeout: Option<u64>,
}

impl BashTool {
    pub fn new() -> Self {
        Self::default()
    }

    async fn execute(&self, ctx: ToolCtx) -> Result<String, CoreError> {
        let cmd = &self.command;

        if cmd.trim().is_empty() {
            return Err(CoreError::Tool("empty command".into()));
        }
        // The shell cwd is authoritative ctx state (filled by the adapter
        // from the chat's current_dir); a missing value is InvalidArguments
        // — no fallback to the server process's cwd.
        if ctx.current_dir.as_os_str().is_empty() {
            return Err(CoreError::InvalidArguments(
                "current_dir is required".into(),
            ));
        }
        let cwd = &ctx.current_dir;
        let timeout_secs = self
            .timeout
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);

        // The kill-the-whole-tree hygiene (process group, kill_on_drop,
        // armed guard, timeout) lives in the shared subprocess runner.
        let output = crate::subprocess::run_command_with_timeout(
            "bash",
            &["-c", cmd.as_str()],
            cwd,
            Duration::from_secs(timeout_secs),
            &ctx.cancel,
        )
        .await
        .map_err(|e| match e {
            RunError::Spawn(e) | RunError::Wait(e) => {
                CoreError::Tool(format!("command execution failed: {e}"))
            }
            RunError::Timeout => {
                CoreError::Tool(format!("command timed out after {timeout_secs}s"))
            }
        })?;

        // An interrupted command's killed status (exit code -9) is noise —
        // the kernel marks the interruption; keep just the partial output.
        let status = (!ctx.cancel.is_cancelled()).then_some(output.status);
        let result = format_command_output(&output.stdout, &output.stderr, status);

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::subprocess::ProcessGroupGuard;
    use crate::test_util::{boundary_ctx, processed};
    use flux_core::Tool;
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn cmd_args(cmd: &str) -> HashMap<String, Value> {
        processed(vec![("command", json!(cmd))])
    }

    #[test]
    fn bash_tool_metadata() {
        let tool = BashTool::new();
        assert_eq!(tool.name(), "bash");
        assert!(tool.description().contains("shell"));
        let schema = tool.schema();
        let props = schema["properties"].as_object().unwrap();
        // The shell cwd is authoritative ctx state — neither it nor the
        // boundary belongs in the LLM-facing schema.
        assert!(
            !props.contains_key("current_dir"),
            "current_dir must not be in the bash schema"
        );
        assert!(
            !props.contains_key("workdir"),
            "workdir must not be in the bash schema"
        );
    }

    #[tokio::test]
    async fn bash_requires_command() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new();
        let result = tool.call(cmd_args(""), boundary_ctx(dir.path())).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn bash_requires_current_dir() {
        // The shell cwd rides ToolCtx; running without it must fail loudly
        // instead of silently falling back to the server's cwd.
        let tool = BashTool::new();
        let result = tool
            .call(
                processed(vec![("command", json!("echo hello"))]),
                flux_core::ToolCtx::new(),
            )
            .await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, CoreError::InvalidArguments(_)),
            "missing current_dir should be InvalidArguments, got: {err}"
        );
    }

    #[tokio::test]
    async fn bash_uses_current_dir_when_present() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let tool = BashTool::new();
        let ctx = flux_core::ToolCtx {
            current_dir: sub.clone(),
            workdir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let result = tool.call(cmd_args("pwd"), ctx).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(
            output.contains(sub.to_string_lossy().as_ref()),
            "bash must run in current_dir, got: {output}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bash_cooperative_interrupt_keeps_partial_output_without_exit_code_noise() {
        // A command that prints progress then blocks: cancelling the ctx
        // token stops the tool and returns the partial output. The killed
        // status (exit code -9) must NOT leak into the result — the kernel
        // marks the interruption, the tool only supplies the output.
        use tokio_util::sync::CancellationToken;
        let dir = tempdir().unwrap();
        let tool = BashTool::new();
        let ctx = flux_core::ToolCtx {
            cancel: CancellationToken::new(),
            call_id: String::new(),
            workdir: dir.path().to_path_buf(),
            current_dir: dir.path().to_path_buf(),
        };
        let t = tokio::spawn({
            let ctx = ctx.clone();
            async move {
                tool.call(cmd_args("echo progress-line; sleep 30"), ctx)
                    .await
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        ctx.cancel.cancel();
        let result = t.await.unwrap().unwrap();
        assert!(
            result.contains("progress-line"),
            "partial output must be preserved, got: {result}"
        );
        assert!(
            !result.contains("exit code"),
            "killed status must not leak into an interrupted result: {result}"
        );
    }

    #[tokio::test]
    async fn bash_echo() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new();
        let result = tool
            .call(cmd_args("echo hello"), boundary_ctx(dir.path()))
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("hello"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_group_guard_kills_descendants() {
        use tokio::io::{AsyncBufReadExt, BufReader};

        // bash backgrounds a sleeper and reports its pid; the guard drop must
        // kill the whole group, including the backgrounded descendant.
        let mut child = tokio::process::Command::new("bash")
            .arg("-c")
            .arg("sleep 60 & echo $!; wait")
            .process_group(0)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pgid = child.id().unwrap() as i32;
        let guard = ProcessGroupGuard::new(pgid);

        let stdout = child.stdout.take().unwrap();
        let mut lines = BufReader::new(stdout).lines();
        let bg_pid: i32 = lines
            .next_line()
            .await
            .unwrap()
            .unwrap()
            .trim()
            .parse()
            .unwrap();

        drop(guard); // kill the whole process group
        drop(child); // kill_on_drop also reaps bash itself

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let alive = std::process::Command::new("kill")
            .args(["-0", &bg_pid.to_string()])
            .status()
            .unwrap()
            .success();
        assert!(!alive, "descendant {bg_pid} survived group kill");
    }

    #[tokio::test]
    async fn wide_multibyte_output_is_returned_untruncated() {
        // Output truncation moved to the central overflow buffer
        // (Chat::bounded_output → buf_read) — the tool returns the FULL
        // output; multibyte content passes through untouched (the old 8KB
        // byte-slice cap lived here and could panic mid-character).
        let dir = tempdir().unwrap();
        let tool = BashTool::new();
        let result = tool
            .call(
                cmd_args("printf '中%.0s' {1..3000}"),
                boundary_ctx(dir.path()),
            )
            .await;
        assert!(result.is_ok(), "execute failed: {result:?}");
        let output = result.unwrap();
        assert_eq!(
            output.chars().filter(|&c| c == '中').count(),
            3000,
            "all 3000 chars must be returned untruncated"
        );
    }

    #[tokio::test]
    async fn command_not_found_reports_exit_code() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new();
        let result = tool
            .call(
                cmd_args("nonexistent_command_xyz"),
                boundary_ctx(dir.path()),
            )
            .await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(
            output.contains("command not found") || output.contains("exit code: 127"),
            "unexpected output: {output}"
        );
    }

    #[tokio::test]
    async fn bash_allows_pipes_and_redirections() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new();
        let result = tool
            .call(
                cmd_args("echo hello | tr a-z A-Z"),
                boundary_ctx(dir.path()),
            )
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("HELLO"));
    }

    #[test]
    fn bash_timeout_default_is_300() {
        // The deliberate semantic change from the old hardcoded 30s —
        // pinned so a silent drift back is caught.
        assert_eq!(DEFAULT_TIMEOUT_SECS, 300);
    }

    #[test]
    fn bash_timeout_schema_is_optional_integer() {
        let tool = BashTool::new();
        let schema = tool.schema();
        let props = schema["properties"].as_object().unwrap();
        let timeout = props
            .get("timeout")
            .expect("timeout must be in the bash schema");
        assert_eq!(timeout["type"], "integer");
        let required = schema["required"].as_array().unwrap();
        assert!(
            !required.contains(&json!("timeout")),
            "timeout must be optional"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bash_timeout_override_times_out_with_dynamic_message() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new();
        let result = tool
            .call(
                processed(vec![("command", json!("sleep 5")), ("timeout", json!(1))]),
                boundary_ctx(dir.path()),
            )
            .await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, CoreError::Tool(ref m) if m.contains("timed out after 1s")),
            "expected a 1s timeout error, got: {err}"
        );
    }

    #[tokio::test]
    async fn bash_timeout_clamps_out_of_range_values() {
        // 0 clamps up to the 1s floor, 99999 clamps down to the 30min
        // ceiling — both must still run a quick command cleanly.
        let dir = tempdir().unwrap();
        let tool = BashTool::new();
        for value in [0u64, 99999] {
            let result = tool
                .call(
                    processed(vec![
                        ("command", json!("echo ok")),
                        ("timeout", json!(value)),
                    ]),
                    boundary_ctx(dir.path()),
                )
                .await;
            assert!(
                result.is_ok(),
                "timeout {value} must clamp to a working value: {result:?}"
            );
        }
    }

    #[tokio::test]
    async fn bash_empty_command_is_rejected() {
        let tool = BashTool::new();
        let result = tool.call(cmd_args("   "), flux_core::ToolCtx::new()).await;
        assert!(result.is_err());
    }
}
