use crate::subprocess::{RunError, run_command_with_timeout};
use crate::{format_command_output, require_workdir};
use flux_core::CoreError;
use flux_core::ToolCtx;
use std::time::Duration;

/// Render the shared runner's failure as this tool family's error strings.
/// (Distinct from BashTool's strings — both are live observable behavior, so
/// each caller maps the shared [`RunError`] to its own messages.)
fn map_run_error(e: RunError) -> CoreError {
    match e {
        RunError::Spawn(e) => CoreError::Tool(format!("failed to start: {e}")),
        RunError::Timeout { secs } => CoreError::Tool(format!("timed out after {secs}s")),
        RunError::Wait(e) => CoreError::Tool(format!("failed: {e}")),
    }
}

// ---------------------------------------------------------------------------
// RustInitTool
// ---------------------------------------------------------------------------

#[derive(Default, flux_macros::Tool, ::serde::Deserialize)]
#[tool(
    name = "rust_init",
    description = "Initialize a new Rust project with 'cargo new'."
)]
pub struct RustInitTool {
    /// Project name.
    name: String,
}

impl RustInitTool {
    pub fn new() -> Self {
        Self::default()
    }

    async fn execute(&self, ctx: ToolCtx) -> Result<String, CoreError> {
        // New projects are created inside the chat boundary (authoritative
        // ctx state; a missing one fails closed).
        let wd = ctx.workdir.clone();
        require_workdir(&wd)?;
        let name = self.name.trim();

        // `cargo new` accepts paths, so an unvalidated name can create a
        // project tree outside the workdir. Enforce a plain crate name.
        // A leading `-` would parse as a cargo flag (`cargo new --bin`) — rejected.
        if name.is_empty()
            || name.starts_with('.')
            || name.starts_with('-')
            || name.contains('/')
            || name.contains('\\')
            || name.contains("..")
            || std::path::Path::new(name).is_absolute()
        {
            return Err(CoreError::Tool(format!(
                "invalid crate name: {:?}",
                self.name
            )));
        }

        let output = run_command_with_timeout(
            "cargo",
            &["new", name],
            &wd,
            Duration::from_secs(120),
            &ctx.cancel,
        )
        .await
        .map_err(|e| CoreError::Tool(format!("cargo new {}", map_run_error(e))))?;

        Ok(format_command_output(
            &output.stdout,
            &output.stderr,
            Some(output.status),
        ))
    }
}

// ---------------------------------------------------------------------------
// RustVerifyTool
// ---------------------------------------------------------------------------

#[derive(Default, flux_macros::Tool, ::serde::Deserialize)]
#[tool(
    name = "rust_verify",
    description = "Run cargo fmt, check, clippy, and test in sequence."
)]
pub struct RustVerifyTool {
    /// Path to the Rust project. Defaults to the chat workdir.
    path: Option<String>,
}

impl RustVerifyTool {
    pub fn new() -> Self {
        Self::default()
    }

    async fn execute(&self, ctx: ToolCtx) -> Result<String, CoreError> {
        // Resolve against the chat boundary — escapes are tool errors the
        // model sees and self-corrects.
        let dir = ctx.resolve(self.path.as_deref().unwrap_or("."))?;

        let mut results = Vec::new();

        for (cmd, args) in [
            ("cargo", &["fmt", "--", "--check"][..]),
            ("cargo", &["check"][..]),
            ("cargo", &["clippy", "--", "-D", "warnings"][..]),
            ("cargo", &["test"][..]),
        ] {
            let output =
                run_command_with_timeout(cmd, args, &dir, Duration::from_secs(120), &ctx.cancel)
                    .await;

            match output {
                Ok(o) => {
                    let label = format!("{} {}", cmd, args.join(" "));
                    let status = if o.status.success() { "✓" } else { "✗" };
                    let output = format_command_output(&o.stdout, &o.stderr, None);
                    results.push(format!("{status} {label}\n{output}").trim().to_string());
                    if !o.status.success() {
                        break;
                    }
                }
                Err(e) => {
                    results.push(format!("✗ {cmd}: {}", map_run_error(e)));
                    break;
                }
            }
        }

        Ok(results.join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{boundary_ctx, processed};
    use flux_core::Tool;
    use serde_json::json;
    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_descendant_processes() {
        // A command that backgrounds a sleeper and then blocks forever: on
        // timeout the whole process group must be killed — the backgrounded
        // descendant included — not just the direct child.
        let dir = tempdir().unwrap();
        let pidfile = dir.path().join("bg.pid");
        let script = format!("sleep 60 & echo $! > {}; wait", pidfile.display());
        let result = run_command_with_timeout(
            "bash",
            &["-c", &script],
            dir.path(),
            Duration::from_secs(2),
            &CancellationToken::new(),
        )
        .await;
        assert!(result.is_err());

        // Give the kill a moment to land, then verify the descendant died.
        // Poll for the pidfile so an overloaded CI box can't panic on the
        // read before bash has echoed the pid.
        let bg_pid: i32 = loop {
            if let Ok(text) = std::fs::read_to_string(&pidfile) {
                break text.trim().parse().unwrap();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        // A single kill -0 check races SIGKILL delivery on a loaded box —
        // poll for up to 2s and fail only if the pid stays alive the whole
        // window (a real survivor never dies, so the assertion keeps its
        // teeth).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let alive = std::process::Command::new("kill")
                .args(["-0", &bg_pid.to_string()])
                .status()
                .unwrap()
                .success();
            if !alive {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("descendant {bg_pid} survived the timeout");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn rust_init_rejects_path_like_names() {
        let dir = tempdir().unwrap();
        let tool = RustInitTool::new();
        // A leading `-` parses as a cargo flag (`cargo new --bin`) — rejected like path escapes.
        for bad in ["../escape", "a/b", ".hidden", "..", "--bin", "-lib"] {
            let result = tool
                .call(
                    processed(vec![("name", json!(bad))]),
                    boundary_ctx(dir.path()),
                )
                .await;
            assert!(result.is_err(), "must reject name {bad:?}, got: {result:?}");
        }
    }

    #[tokio::test]
    async fn rust_init_requires_workdir() {
        // The boundary rides ToolCtx; a ctx without one fails closed.
        let tool = RustInitTool::new();
        let result = tool
            .call(
                processed(vec![("name", json!("proj"))]),
                flux_core::ToolCtx::new(),
            )
            .await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, CoreError::InvalidArguments(_)),
            "missing workdir should be InvalidArguments, got: {err}"
        );
    }

    #[tokio::test]
    async fn rust_verify_resolves_relative_path_inside_boundary() {
        // The verify target resolves against the ctx boundary — a relative
        // path lands inside the chat workdir.
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(dir.path().join("src.rs"), "").unwrap();
        let tool = RustVerifyTool::new();
        // cargo itself will fail on this stub project — the assertion is
        // only that the path RESOLVED (no boundary escape error).
        let result = tool
            .call(
                processed(vec![("path", json!("."))]),
                boundary_ctx(dir.path()),
            )
            .await;
        if let Err(e) = result {
            assert!(
                !e.to_string().contains("path escape"),
                "relative path must resolve inside the boundary, got: {e}"
            );
        }
    }
}
