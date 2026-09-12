use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::Notify;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

/// Maximum bytes captured from a child's stdout/stderr before truncation.
/// `wait_with_output` would buffer the entire pipe in memory, so a
/// `cat /dev/zero`-style flood (or any tool dumping a multi-GiB stream)
/// can OOM the process before the outer timeout fires. Bounding capture
/// here — far below the OOM threshold, well above any legitimate tool
/// output — is the memory-backstop; per-tool display truncation (bash's
/// 8 KiB) is a separate, later concern.
const MAX_COMMAND_OUTPUT: usize = 16 * 1024 * 1024;

/// Deadline for draining the pipes after a cooperative-cancel kill. The
/// group kill closes the pipes (SIGKILL), so EOF arrives promptly; the
/// deadline only bounds the pathological case (kill failed).
const CANCEL_DRAIN: Duration = Duration::from_secs(2);

/// Kill the whole process group. Idempotent against an already-dead group.
#[cfg(unix)]
fn kill_group(pgid: i32) {
    if pgid > 0 {
        let _ = std::process::Command::new("kill")
            .args(["-9", &format!("-{pgid}")])
            .status();
    }
}

/// Kills the whole process group on drop unless disarmed. Ensures a timeout
/// or task abort does not leave the spawned process's descendants behind as
/// orphans (bash `-c` grandchildren, cargo's rustc/test children, ...).
#[cfg(unix)]
pub(crate) struct ProcessGroupGuard {
    pgid: i32,
    armed: bool,
}

#[cfg(unix)]
impl ProcessGroupGuard {
    pub(crate) fn new(pgid: i32) -> Self {
        Self { pgid, armed: true }
    }

    /// Called after a normal exit so intentionally backgrounded jobs survive.
    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

#[cfg(unix)]
impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if self.armed {
            kill_group(self.pgid);
        }
    }
}

/// Failure modes of [`run_command_with_timeout`]. The runner reports which
/// stage failed; each caller renders its own error string, since the
/// existing messages are per-tool observable behavior.
#[derive(Debug)]
pub(crate) enum RunError {
    Spawn(std::io::Error),
    Timeout { secs: u64 },
    Wait(std::io::Error),
}

/// Read at most `cap` bytes from `r` into the shared `buf`, returning
/// whether the stream was truncated (i.e. more bytes were available than
/// the cap).
///
/// The buffer is shared (`Arc<Mutex<..>>`) so the captured prefix survives
/// the future being dropped — the cooperative-cancel path kills the group
/// and drains with a deadline; if the drain times out, whatever was
/// captured so far is still returned. The lock is held only for the
/// per-chunk append (never across an await).
///
/// The `truncated` flag disambiguates the two cap-hit cases:
///  - `n > remaining` — a single chunk read *past* the cap; definitely more
///    data existed, so it is truncated.
///  - `n == remaining` — the cap was filled *exactly* by this chunk. Whether
///    the stream actually overflowed is only settled by one more read: EOF
///    → it fit exactly (NOT truncated); more bytes → overflowed. The probe
///    returns immediately in both real cases — an infinite producer always
///    has more data ready, a finite producer that fit exactly has already
///    reached EOF. (Only a producer that both filled the cap and then hangs
///    without writing/closing would block the probe; the outer timeout backs
///    that pathological case up.)
///
/// On a confirmed truncation the read also fires `overflow` once, so the
/// runner can kill the child's process group promptly (the other pipe's read
/// then EOFs when the child dies) instead of waiting for the whole
/// `join!`/outer timeout.
async fn read_capped<R: AsyncRead + Unpin>(
    r: Option<R>,
    cap: usize,
    buf: Arc<Mutex<Vec<u8>>>,
    overflow: &Arc<Notify>,
) -> bool {
    let Some(mut r) = r else {
        return false;
    };
    let mut chunk = vec![0u8; 8192];
    loop {
        let n = match r.read(&mut chunk).await {
            Ok(0) => return false, // EOF — the whole stream fit within the cap.
            Ok(n) => n,
            Err(_) => return false,
        };
        let remaining = cap.saturating_sub(buf.lock().unwrap().len());
        if n >= remaining {
            buf.lock().unwrap().extend_from_slice(&chunk[..remaining]);
            if n > remaining {
                // This chunk already carried more than the cap — overflowed.
                overflow.notify_one();
                return true;
            }
            // n == remaining: the cap is full but this chunk may have been the
            // stream's end. Probe once to tell exactly-at-cap from overflowed.
            return match r.read(&mut chunk).await {
                Ok(0) | Err(_) => false, // EOF right after the cap → fit exactly
                Ok(_) => {
                    overflow.notify_one();
                    true // more data follows → overflowed
                }
            };
        }
        buf.lock().unwrap().extend_from_slice(&chunk[..n]);
    }
}

/// Run a command with a hard timeout and capture its output. The child runs
/// in its own process group so `kill_on_drop` plus the group guard kill the
/// whole tree on timeout, abort, or user interrupt (bash `-c` descendants,
/// cargo's rustc/test children, ...), not just the direct child.
///
/// Capture is bounded: stdout and stderr are read **concurrently**, each up to
/// [`MAX_COMMAND_OUTPUT`] bytes, so neither pipe can fill while the other is
/// read (a full, unread pipe would block the child on write and deadlock the
/// pair). A stream that hits its cap returns early; once either side has
/// overflowed, the child's process group is killed (a group `kill` — the
/// child may otherwise sit blocked on a write to a pipe nobody reads). The
/// captured prefix is returned as a normal `Output`; truncation is a per-tool
/// display-layer concern, so callers on the happy path keep the same shape.
///
/// Cooperative cancel: when `cancel` fires, the process group is killed
/// immediately and the pipes are drained with a [`CANCEL_DRAIN`] deadline —
/// the returned `Output` carries whatever the child produced before the
/// interrupt (partial output for the transcript). The caller detects the
/// interrupt via `cancel.is_cancelled()`; the runner keeps the `Output`
/// shape so happy-path callers are unchanged.
pub(crate) async fn run_command_with_timeout(
    cmd: &str,
    args: &[&str],
    dir: &Path,
    duration: Duration,
    cancel: &CancellationToken,
) -> Result<std::process::Output, RunError> {
    #[cfg(not(unix))]
    let _ = cancel; // no process-group kill off unix: cancel falls through to abort
    let mut command = tokio::process::Command::new(cmd);
    command.args(args).current_dir(dir);
    command.kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(RunError::Spawn)?;
    #[cfg(unix)]
    let mut guard = ProcessGroupGuard::new(child.id().map(|p| p as i32).unwrap_or(0));

    // Read both pipes concurrently inside the outer timeout, and kill the
    // group as soon as either side overflows. Sequential reads would
    // deadlock a child that floods both streams: the second pipe fills its
    // 64 KiB buffer and blocks the child on write while nobody reads it.
    // `tokio::join!` drives both `read_capped` futures together, so each cap
    // is the only thing a flood can outrun. Naively waiting for the whole
    // join would hang on a one-sided flood: stdout caps and is no longer
    // read, the child blocks writing stdout, and stderr — albeit empty —
    // never EOFs, so the join stalls until the outer timeout and discards
    // the captured prefix. Instead, the first overflow fires `overflow`, the
    // select arm kills the group NOW, and the other read's pipe closes on the
    // child's death (EOF) so the join drains promptly. The outer timeout
    // remains the hard backstop for a child that neither overflows nor exits.
    let overflow = Arc::new(Notify::new());
    // Take the pipe handles out of `child` up front so `joined` does not
    // borrow `child` mutably (which would conflict with the later
    // `child.wait()` after the join completes). Buffers are shared
    // (`Arc<Mutex<..>>`) so the captured prefix survives a dropped read
    // (cancel drain deadline) and can be read after any select outcome.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_buf = Arc::new(Mutex::new(Vec::new()));
    let err_buf = Arc::new(Mutex::new(Vec::new()));
    let joined = async {
        tokio::join!(
            read_capped(stdout, MAX_COMMAND_OUTPUT, out_buf.clone(), &overflow),
            read_capped(stderr, MAX_COMMAND_OUTPUT, err_buf.clone(), &overflow),
        )
    };
    tokio::pin!(joined);
    let overflowed = overflow.notified();
    tokio::pin!(overflowed);
    let cancelled = cancel.cancelled();
    tokio::pin!(cancelled);

    // Three exits share one inner race (join vs overflow kill); the outer
    // select adds the cooperative-cancel arm: kill now, drain with a
    // deadline, keep whatever was captured.
    enum Exit {
        Normal(bool, bool), // (out_trunc, err_trunc)
        Timeout,
        Interrupted,
    }
    let inner = async {
        tokio::select! {
            r = &mut joined => Exit::Normal(r.0, r.1),
            _ = &mut overflowed => {
                // One side hit its cap: the child may still be producing.
                // Kill the group now so an infinite producer can't block
                // forever on a pipe nobody reads; the other read's pipe
                // closes on the child's death (EOF) so the join drains
                // the captured prefix promptly.
                #[cfg(unix)]
                kill_group(guard.pgid);
                let (ot, et) = joined.await;
                Exit::Normal(ot, et)
            }
        }
    };
    tokio::pin!(inner);
    let exit = tokio::select! {
        r = timeout(duration, &mut inner) => match r {
            Ok(exit) => exit,
            Err(_) => Exit::Timeout,
        },
        _ = &mut cancelled => {
            // User interrupt: kill the whole group, then drain briefly —
            // the pipes EOF on the child's death, so the join completes
            // with the captured prefix; the deadline only bounds the
            // pathological case. Partial output is preserved for the
            // transcript (the caller/kernel marks the interruption).
            #[cfg(unix)]
            kill_group(guard.pgid);
            #[cfg(not(unix))]
            let _ = &mut inner;
            let _ = timeout(CANCEL_DRAIN, &mut inner).await;
            Exit::Interrupted
        }
    };

    // If either stream hit the cap but the select's joined arm happened to
    // win before the overflow notification was observed (e.g. a finite
    // producer finishing exactly at the boundary), the child may have
    // descendants still to reap — kill the group as a belt-and-suspenders
    // so no orphan is left behind. Idempotent against an already-dead group.
    match exit {
        Exit::Timeout => {
            return Err(RunError::Timeout {
                secs: duration.as_secs(),
            });
        }
        Exit::Normal(out_trunc, err_trunc) =>
        {
            #[cfg(unix)]
            if out_trunc || err_trunc {
                kill_group(guard.pgid);
            }
        }
        Exit::Interrupted => {}
    }

    let status = child.wait().await.map_err(RunError::Wait)?;
    #[cfg(unix)]
    guard.disarm();
    let stdout = std::mem::take(&mut *out_buf.lock().unwrap());
    let stderr = std::mem::take(&mut *err_buf.lock().unwrap());
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn run_command_output_is_bounded() {
        // A command emitting far more than the cap must not be buffered whole
        // in memory: the captured stdout is truncated to MAX_COMMAND_OUTPUT,
        // not len == the full producer output.
        let dir = tempdir().unwrap();
        let wanted = MAX_COMMAND_OUTPUT + 4 * 1024 * 1024; // 4 MiB over the cap.
        let cmd = "head";
        let args = ["-c", &wanted.to_string(), "/dev/zero"];
        let output = run_command_with_timeout(
            cmd,
            &args,
            dir.path(),
            Duration::from_secs(10),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            output.stdout.len(),
            MAX_COMMAND_OUTPUT,
            "stdout must be capped at {MAX_COMMAND_OUTPUT}, got {}",
            output.stdout.len()
        );
    }

    #[tokio::test]
    async fn one_sided_flood_fails_fast_after_cap_not_timeout() {
        // Regression: `yes` floods stdout indefinitely with a silent stderr.
        // Before the fix the group kill was deferred until the whole join
        // completed — stdout caps, the child blocks writing to an unread
        // pipe, stderr never EOFs, so it hung until the 20s outer timeout
        // AND discarded the captured prefix as RunError::Timeout. Now the
        // first overflow fires the kill promptly, so this returns quickly
        // with the truncated (capped) stdout as a normal Output.
        let dir = tempdir().unwrap();
        let started = std::time::Instant::now();
        let output = run_command_with_timeout(
            "yes",
            &[],
            dir.path(),
            Duration::from_secs(20),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "must fail fast after the cap, not wait out the 20s timeout ({:?})",
            started.elapsed()
        );
        assert_eq!(
            output.stdout.len(),
            MAX_COMMAND_OUTPUT,
            "stdout must be capped at {MAX_COMMAND_OUTPUT}"
        );
        // The captured prefix is returned, not discarded as a timeout error.
        assert!(output.stdout.contains(&b'\n'));
    }

    #[tokio::test]
    async fn run_command_exactly_at_cap_is_not_truncated_or_killed() {
        // Regression (#6): a stream whose output equals MAX_COMMAND_OUTPUT
        // exactly must NOT be reported as truncated — and crucially must not
        // get its process group killed (a killed child yields a non-success
        // status). `head -c 16M /dev/zero` produces exactly the cap: the
        // final chunk fills it precisely, the probe read then sees EOF (head
        // exits naturally), so it is classified as "fit exactly" and the
        // child is left alone.
        let dir = tempdir().unwrap();
        let started = std::time::Instant::now();
        let output = run_command_with_timeout(
            "head",
            &["-c", &MAX_COMMAND_OUTPUT.to_string(), "/dev/zero"],
            dir.path(),
            Duration::from_secs(10),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(
            output.status.success(),
            "exactly-at-cap child must exit normally, not be killed: {output:?}"
        );
        assert_eq!(
            output.stdout.len(),
            MAX_COMMAND_OUTPUT,
            "the exact-cap output is preserved whole"
        );
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "must not wait out the timeout ({:?})",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn run_command_small_output_is_preserved() {
        // Output under the cap is a normal, complete capture — no truncation.
        let dir = tempdir().unwrap();
        let output = run_command_with_timeout(
            "bash",
            &["-c", "printf hello"],
            dir.path(),
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(output.stdout, b"hello");
        assert!(output.status.success());
    }

    #[tokio::test]
    async fn cooperative_cancel_kills_group_and_keeps_partial_output() {
        // A command that prints progress then blocks forever: cancelling the
        // token must kill the process group promptly AND return the partial
        // output produced before the interrupt (partial output for the
        // transcript is the whole point of the cooperative tier).
        let dir = tempdir().unwrap();
        let cancel = CancellationToken::new();
        let started = std::time::Instant::now();
        let t = tokio::spawn({
            let cancel = cancel.clone();
            async move {
                run_command_with_timeout(
                    "bash",
                    &["-c", "echo progress-line; sleep 30"],
                    dir.path(),
                    Duration::from_secs(60),
                    &cancel,
                )
                .await
            }
        });
        // Let the child start and print its line, then interrupt.
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.cancel();
        let output = t.await.unwrap().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "cancel must return promptly, not wait out the 60s timeout ({:?})",
            started.elapsed()
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("progress-line"),
            "partial output must be preserved, got {:?}",
            output.stdout
        );
        assert!(
            !output.status.success(),
            "the killed child must not report success"
        );
    }
}
