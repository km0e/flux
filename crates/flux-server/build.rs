//! Build-time driver for the embedded web UI (feature `web-ui-embed`,
//! Meilisearch-style: the build script owns everything the proc macro
//! cannot).
//!
//! Two load-bearing jobs, both verified against rust-embed 8.12:
//!
//! 1. **Existence.** rust-embed's derive REQUIRES the folder to exist at
//!    macro-expansion time — in every mode, including debug (dynamic)
//!    builds. A fresh clone without node/pnpm would fail to compile
//!    outright, so when the UI build is impossible this script writes a
//!    minimal placeholder into `clients/web/dist` instead: the binary
//!    builds, serves a plain explanatory page, and the failure mode stays
//!    loud-but-nonfatal (the static layer never blocks the agent — see
//!    docs/decisions.md T-14).
//! 2. **Change detection.** Embedded mode expands to per-file
//!    `include_bytes!`, which cargo tracks — a file CONTENT change
//!    recompiles on its own. But a vite rebuild produces NEW hashed
//!    filenames, and cargo has no reason to re-run the proc macro for
//!    files it never saw. Re-running THIS script on the web sources
//!    forces the crate to rebuild, and the macro re-walks the folder.
//!
//! Rebuild trigger: `dist/index.html` missing or older than any tracked
//! input (web sources, package manifest, lockfile, proto contract — the
//! TS bindings are derived from proto/ via buf). Mtime-based, so a
//! no-op build costs one directory stat.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Repo root = crate dir / ../..
fn repo_root() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    Path::new(&manifest)
        .join("../..")
        .canonicalize()
        .expect("repo root must resolve")
}

fn main() {
    // Never track the default (whole crate dir): the embedded UI's inputs
    // live outside the crate, so the list below is the complete set.
    let root = repo_root();
    for rel in [
        "clients/web/dist",
        "clients/web/src",
        "clients/web/public",
        "clients/web/index.html",
        "clients/web/vite.config.ts",
        "clients/package.json",
        "clients/pnpm-lock.yaml",
        // The TS wire contract is DERIVED from proto/ (buf generate) — a
        // contract edit must rebuild the UI that carries it.
        "proto",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(rel).display());
    }

    if std::env::var_os("CARGO_FEATURE_WEB_UI_EMBED").is_none() {
        return; // headless-only build; nothing to embed
    }

    let dist = root.join("clients/web/dist");
    // A placeholder dist counts as NOT current: a later build with the
    // toolchain available upgrades it automatically.
    if !is_placeholder(&dist) && ui_is_current(&root, &dist) {
        return;
    }

    // Explicit opt-out for hermetic/offline builds: skip the toolchain and
    // fall through to the placeholder.
    if std::env::var_os("FLUX_WEB_UI_NO_BUILD").is_some_and(|v| v == "1") {
        if !is_placeholder(&dist) {
            write_placeholder(&dist);
        }
        return;
    }

    // Silence the retry noise on machines that will never succeed: a
    // placeholder already in place means a previous attempt failed — the
    // attempt still runs (cheap failure, auto-upgrade when the toolchain
    // appears), but no more warnings.
    let quiet = is_placeholder(&dist);
    if build_ui(&root, quiet) && dist.join("index.html").is_file() {
        let _ = std::fs::remove_file(dist.join(PLACEHOLDER_MARKER));
        return;
    }

    // The platform build script failed. If a placeholder is already in
    // place (a toolchain-less machine), keep it SILENT — warning on every
    // cargo build would be spam; the placeholder page itself carries the
    // fix. A real toolchain error surfaces through the script's own
    // captured output above.
    if is_placeholder(&dist) {
        return;
    }
    // A genuine (non-placeholder) dist survived the failed build — keep it.
    if dist.join("index.html").is_file() {
        return;
    }
    write_placeholder(&dist);
}

/// The marker file build.rs leaves next to a placeholder index.html —
/// distinguishes "UI not built (yet)" from a real (possibly stale) dist.
const PLACEHOLDER_MARKER: &str = "web-ui-placeholder";

fn is_placeholder(dist: &Path) -> bool {
    dist.join(PLACEHOLDER_MARKER).is_file()
}

/// True when `dist/index.html` exists and no tracked input is newer.
fn ui_is_current(root: &Path, dist: &Path) -> bool {
    let Ok(built) = std::fs::metadata(dist.join("index.html")).and_then(|m| m.modified()) else {
        return false;
    };
    for rel in [
        "clients/web/src",
        "clients/web/public",
        "clients/web/index.html",
        "clients/web/vite.config.ts",
        "clients/package.json",
        "clients/pnpm-lock.yaml",
        "proto",
    ] {
        let p = root.join(rel);
        if newer_than(&p, built) {
            return false;
        }
    }
    true
}

/// Does any file under `path` (recursive) have an mtime strictly newer
/// than `built`? (Missing path = nothing newer; the build script will
/// rebuild anyway once the dist check fails.)
fn newer_than(path: &Path, built: std::time::SystemTime) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if meta.is_file() {
        return meta.modified().is_ok_and(|t| t > built);
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    entries
        .filter_map(|e| e.ok())
        .any(|e| newer_than(&e.path(), built))
}

/// Run the repo's own UI build procedure — the SAME scripts devs and CI
/// use (`scripts/package-web.{sh,ps1}`), so the build procedure has one
/// home. The script provisions pnpm from the packageManager pin, derives
/// the TS contract (buf generate) and runs the vite build into
/// `clients/web/dist`.
fn build_ui(root: &Path, quiet: bool) -> bool {
    if !quiet {
        println!(
            "cargo:warning=flux-server: embedded web UI dist missing or stale — building clients/web (node/pnpm required; FLUX_WEB_UI_NO_BUILD=1 skips)"
        );
    }
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("powershell");
        c.arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg(root.join("scripts/package-web.ps1"));
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new("bash");
        c.arg(root.join("scripts/package-web.sh"));
        c
    };
    cmd.current_dir(root);
    // CAPTURE the script's stdout/stderr: cargo swallows build-script stdio
    // when the build script itself succeeds (ours handles the failure and
    // keeps building), so without capture a script failure is completely
    // invisible — exactly what made the v0.2.0 Windows release job
    // undiagnosable. On failure the tail is re-emitted as cargo:warning.
    //
    // WATCHDOG (20 min): a hung `pnpm install` (network) would otherwise
    // stall the build silently and indefinitely — indistinguishable from
    // the fat-LTO link phase, which also prints nothing for minutes. On
    // timeout we fall through to the placeholder; the orphaned script may
    // still land a dist later, and the next build's rerun-if-changed check
    // embeds it then.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let out = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).output();
        let _ = tx.send(out);
    });
    match rx.recv_timeout(Duration::from_secs(20 * 60)) {
        Ok(Ok(out)) if out.status.success() => true,
        Ok(Ok(out)) => {
            if !quiet {
                println!(
                    "cargo:warning=flux-server: web UI build script exited with {} — embedding a placeholder; run scripts/package-web.sh manually for the real UI",
                    out.status
                );
                for line in String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .chain(String::from_utf8_lossy(&out.stdout).lines())
                    .filter(|l| !l.trim().is_empty())
                    .rev()
                    .take(30)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                {
                    println!("cargo:warning=flux-server: [web-build] {line}");
                }
            }
            false
        }
        Ok(Err(e)) => {
            if !quiet {
                println!(
                    "cargo:warning=flux-server: web UI build script failed to launch ({e}) — embedding a placeholder; install node 24 + pnpm or set FLUX_WEB_UI_NO_BUILD=1"
                );
            }
            false
        }
        Err(_) => {
            println!(
                "cargo:warning=flux-server: web UI build script timed out after 20min — embedding a placeholder; check network/proxy for pnpm, or set FLUX_WEB_UI_NO_BUILD=1"
            );
            false
        }
    }
}

/// A minimal servable page for toolchain-less builds: the binary compiles,
/// the agent works headless-adjacent (UI up, obviously empty), and the
/// page itself explains the fix. Only written when `dist/index.html` is
/// absent, so it can never shadow a real build.
fn write_placeholder(dist: &Path) {
    println!(
        "cargo:warning=flux-server: embedding a PLACEHOLDER web UI (no node/pnpm toolchain) — install node 24 + pnpm and rebuild, or set FLUX_WEB_UI_NO_BUILD=1 to silence"
    );
    if std::fs::create_dir_all(dist).is_err() {
        return;
    }
    // Vite empties its outDir, so a successful build removes the marker
    // naturally; a belt-and-braces removal anyway.
    let _ = std::fs::write(dist.join(PLACEHOLDER_MARKER), "");
    let _ = std::fs::remove_file(dist.join("web-ui-version.txt"));
    // Minimal hashed-shape assets: the embedded-serving unit tests drive
    // assertions off WebAssets::iter() and expect at least one
    // assets/*.js — keep the placeholder bundle self-consistent so
    // toolchain-less builds (hermetic CI) pass the same suite.
    let _ = std::fs::create_dir_all(dist.join("assets"));
    let _ = std::fs::write(
        dist.join("assets/index-placeholder.js"),
        "// flux placeholder bundle",
    );
    let _ = std::fs::write(
        dist.join("assets/index-placeholder.css"),
        "/* flux placeholder bundle */",
    );
    let index = r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Flux — web UI not built</title></head>
<body style="font-family:system-ui;max-width:40rem;margin:4rem auto;line-height:1.5">
<h1>Flux web UI: placeholder</h1>
<p>This binary was built without the <code>node</code>/<code>pnpm</code> toolchain,
so the browser UI could not be compiled into it. The Connect API and the
terminal channel work normally.</p>
<p>To get the real UI: install <a href="https://nodejs.org">Node 24 LTS</a> and
<a href="https://pnpm.io">pnpm 11</a>, then rebuild (or run
<code>scripts/package-web.sh</code> and point <code>--web-assets-dir</code> at the
dist).</p>
</body></html>
"#;
    let _ = std::fs::write(dist.join("index.html"), index);
}
