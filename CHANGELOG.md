# Changelog

All notable changes to Flux are documented here. Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning: [SemVer](https://semver.org).

> The GitHub Release page for each tag carries its section verbatim (dist
> parses this file); the full docs live in [`README.md`](README.md) and
> [`docs/architecture.md`](docs/architecture.md).

## [0.1.3] - 2026-09-15

### Added

- **MCP `tools/list_changed` → live re-sync** — a server's tool list change now lands without a restart. Two spec-dependent receive paths, verified against the official specs: spec ≤ 2025-06-18 pushes unsolicited (handler hook); spec 2026-07-28 notifies only subscriptions/listen streams — flux opens one for `toolsListChanged` and pumps it into the same sink (rmcp routes subscription notifications exclusively to the channel, so the paths never double-fire; a legacy server's rejection degrades to the hook). Re-list + re-register over the old set, then the established rebuild + broadcast.
- **MCP server log notices forwarded to the UI** — each server's log messages ride a per-server token bucket (burst 5, sustained 10/min, 500-char message cap, drops folded into a synthetic `suppressed` notice) onto a session-level `mcp_notice` stream element (fire-and-forget, outside R2 reconciliation): warning+ surface as toasts, and the TopBar gains a bell (newest first, cap 100, unread badge). The `notifications/message` deprecation (SEP-2577, spec 2026-07-28) is handled with a scoped allow — it remains what deployed servers emit; progress notifications are deliberately NOT forwarded (request-scoped, token mapping would be guesswork).
- **Round-artifacts dock tab** — the current round's touched files and tool invocations, folded declaratively from the stream's `tool_start` events (write/edit/replace_lines → path; bash → command label; MCP tools by name; read-class/plumbing ignored). A user message resets it, a history snapshot rebuilds it from the last user message, a reconnect clears it. File rows open preview tabs (the fs-layer mock keeps intercepting); invocation rows pulse their tool card in the stream.
- **MCP panel UX** — per-tool registration results ride the AddServer ack (partial success stays success; a skipped tool carries its reason — collision/reserved — visible in the UI, never only in the log); `McpServerSummary` gains `tool_names` (the live session's registered set, state-distinguished empty); the add form's env field explains the no-inheritance / never-echoed contract in place.

### Changed

- **CI actions moved to node24 targets** — the node20 deprecation warnings cleared across the workflows and the release build-setup fragment.

### Fixed

- **rustls 0.23.43 → 0.23.45** — RUSTSEC-2026-0285.

## [0.1.2] - 2026-09-14

### Added

- **models.dev catalog status in the log** — the lazy catalog was completely silent: a fresh download logs at `info` with provider/model entry counts and elapsed time, every failure path (request, body read, HTTP status, parse) logs a `warn`, local cache hits log at `debug` with the cache age, and save-time enrichment logs its outcome. The default log filter is now `INFO` when `RUST_LOG` is unset — an empty `EnvFilter` passes errors only, which silenced startup and runtime logs alike.
- **Providers panel: filterable model catalog** — the probed upstream catalog (per provider) gains a live id-substring filter with a shown/total count, so import discovery works on catalogs that run to hundreds of ids.
- **models.dev matching ladder** — a catalog miss no longer silently drops enrichment. Matching is a three-stage best-effort ladder: literal forms (exact, `vendor/`-stripped, `:tag`-stripped), canonical equality (case + `.`/`-` spelling folded — vendors swap the two freely), and a canonical dash-bounded prefix fallback (dated snapshots like `-260828` or `-2024-08-06` extend a catalog base id; the longest catalog id wins). The zhipu/z.ai host family (`api.z.ai`, `open.bigmodel.cn`, `api.zhipu.ai`) anchors its GLM sections. An aliased match always reports the catalog's own id, and the saved-model row surfaces it inline (`matched: provider/model` + badge tooltip) — a wrong match is visible, never silent.

### Changed

- **Frontend package manager: npm → pnpm** — strict, symlinked resolution makes phantom-dependency imports structurally impossible; the pnpm version is pinned via `packageManager` and the scripts self-provision it when missing; the buf CLI rides `clients/web` devDependencies (no global installs); dependency build scripts are deny-by-default with an allowlist for the first-party toolchain (esbuild, @bufbuild/buf). Fixes the fresh-machine failure `buf: command not found` (the old skip-if-exists install could keep a stale install whose `.bin` lacked buf).
- **Node 24 LTS is the ONE supported frontend line** — `engines` declares `>=24 <25` (pnpm warns on anything else without breaking); CI and the release pipeline pin Node 24 explicitly; the scripts echo the resolved toolchain versions (`node`, `pnpm`) into the build log so environment drift is visible without digging.
- **The chat pickers' model datalist carries ONLY saved models** — the probed upstream catalog never bloats it; discovery/import lives in the Providers dialog, and free text still pins (the registry is a convenience, never a gate).

### Fixed

- **Model removal broadcasts the fresh list** — removing a saved model acked cleanly but never broadcast, so the deleted row stayed visible until the dialog was reopened; the removal now broadcasts the fresh list and rebuilds the matching chats (pinned params fall back to unset), symmetric with save.
- **pnpm 11's build-allowlist key** — `onlyBuiltDependencies` was renamed to `allowBuilds` in pnpm 11; a fresh install failed with `ERR_PNPM_IGNORED_BUILDS` before buf could resolve. The workspace yaml now uses the v11 key.
- **Script noise on newer Node** — pnpm 11's vendored `debug` probes storage backends and prints a localStorage `ExperimentalWarning` on every invocation on affected machines; the scripts suppress exactly that warning for their own node processes (guarded by a probe, so old Node that rejects the flag is unaffected).

## [0.1.1] - 2026-09-14

### Fixed

- **Shutdown is crash-only** — the graceful-shutdown machinery (`with_graceful_shutdown` + drain) is deleted: a Subscribe body never completes on its own (the keepalive pump runs forever), so waiting for connections wedged shutdown behind every open frontend. No signal handler is installed; durability is the storage layer's unconditional contract for every death mode: transcript commits are batch-atomic (a crash loses at most the live segment, and the persisted tail never carries a dangling tool_call), and tool children carry `PR_SET_PDEATHSIG` so a hard server death cannot orphan a running build.
- **`PR_SET_PDEATHSIG` is linux-only** — the `#[cfg(unix)]` gate broke macOS builds (`prctl` exists only in linux/android libc) and the unconditional re-export broke Windows; the helper is now a no-op on other platforms.

## [0.1.0] - 2026-09-14

### Added

- **Initial release** — a general-purpose coding agent framework: a Rust workspace (conversation kernel as a pure state machine + pump, OpenAI-compatible provider over SSE, supervised tool flights, MCP client bridge, SQLite persistence, session/lease layer) with a browser chat UI (React 19 + Radix + Tailwind v4) served on the same port as the Connect (gRPC-Web) API.
- Built-in tools (`read_file`/`edit_file`/`write_file`/`replace_lines`/`list_directory`/`glob`/`grep`/`bash`) with a no-approval trust model — per-chat workdir boundary via `ToolCtx::resolve`, tool errors as result text the model self-corrects.
- Streaming render pipeline (rAF-coalesced, append-only paragraphs), interrupt-send + cancellation, fork from any message, overflow output buffer with paged `buf_read`, per-chat `question` tool, provider hot-swap at the round boundary.
- Interactive terminal per chat (e4pty PTY over a dedicated `/ws/term` side channel, xterm.js), tabbed file dock with git-status explorer.
- Release pipeline: dist archives with the web UI bundled (`web-ui/` next to the binary), shell + PowerShell installers, and a standalone `flux-web-ui.tar.gz` for script-installer users.

[0.1.3]: https://github.com/km0e/flux/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/km0e/flux/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/km0e/flux/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/km0e/flux/releases/tag/v0.1.0
