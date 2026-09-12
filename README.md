# Flux

A general-purpose coding agent framework in Rust with a browser chat UI frontend.

## Features

- **Built-in tools**: `read_file`, `edit_file`, `glob`, `grep`, `list_directory`, `rust_init`, `rust_verify`, `bash` — plus per-chat `question` (ask the user), `state_get`/`state_set`, and `buf_read` (paged overflow output). Output limits prevent context overflow: every tool result passes a central 8000-char inline budget — larger outputs are stored whole in a per-chat overflow buffer the model pages through with `buf_read`; grep shapes matches to a window (500 max), glob caps at 500 entries.
- **Agent Skills**: lazy-loading capability packages (`SKILL.md` directories per the [Agent Skills standard](https://agentskills.io)) — the model discovers them via `skill_list` and loads full instructions on demand via `skill_read` (nothing injected into prompts; project skills in `<workdir>/.flux/skills/`, global in `~/.flux/skills/`, project wins name collisions). The web UI's Skills dialog manages them: install from a local directory or a git URL (optional subpath for multi-skill repos), remove global entries — immediately effective, no restart.
- **LLM providers**: OpenAI / OpenAI-compatible endpoints — pure endpoints (id · url · api key) managed from the web UI's Providers dialog and stored in the server database (there is no config file).
- **Streaming**: real-time text + reasoning deltas over WebSocket — reception and rendering are decoupled by one animation frame (deltas append to a raw buffer; a coalesced rAF render folds them in, at most one incremental render per frame). Committed paragraphs render once and are appended append-only, code blocks type in stably and highlight exactly once, and a pi-web style stick-to-bottom state machine keeps the follow smooth without yanking the reader back.
- **MCP client**: launch external MCP servers as child processes and expose their tools. Managed from the web UI's MCP dialog (stored in the server database; changes take effect after a restart — every chat sees exactly the startup tool set).
- **No approvals (pi-style)**: tools execute directly; argument preprocessing (workdir-boundary expansion, authoritative state injection) is decision-free. Real isolation comes from the OS/container boundary. The built-in `question` tool lets the model ask the user a question mid-round (agent-produced text + options, answered via an inline card in the conversation).
- **Stream cancellation**: stop generation mid-stream (Stop button or Escape key); sending while a round runs parks the message as an interject (cancel + fresh round).
- **Chat history**: persistent conversations with full message history across restarts.
- **Built-in terminal**: interactive shells (e4pty PTY, xterm.js UI) over a dedicated `/ws/term` side channel — multiple per chat, added on demand via the dock's "+" or the sidebar button; kept alive across tab switches and re-attached after a page refresh within the session grace window.
- **Tabbed right dock**: opened files accumulate as tabs (multi-file, editor-style); file bodies scroll horizontally; `.md` files render through the shared markdown pipeline with a Raw toggle.
- **Feature mode** (code-engineering mode): `kind = "feature"` chats re-orchestrate the context at every feature boundary — the model sees only a project scaffold (profile, tree, git status, convention files, last-feature decisions), never archived history. Project-level tuning lives in `<workdir>/.flux/config.toml`; multi-project (monorepo) aware, and prefix-cache friendly: info blocks are measured by change frequency (git history / content hash) and ordered stable-first.
- **Web UI**: a React chat UI served BY DEFAULT by flux-server on the SAME port as the WS endpoint (one listener) — `./scripts/run-server.sh` (builds the UI when missing); `--no-web` runs headless. The page is a viewer/controller connecting back same-origin over WS. Bind beyond localhost only behind a TLS proxy — the server has no auth layer.

## Project layout

```
flux/
├── crates/                 # Rust workspace (server + kernel + tools)
├── clients/web/            # Web UI (React 19 + Radix + Tailwind v4 + zustand, Vite)
└── docs/
```

## Quick start

```bash
cargo build --release
./scripts/run-server.sh             # builds the UI when missing, then serves UI + WS
# or: cargo run -p flux-server      # WS + the packaged web-ui/ next to the binary,
                                    # if present (--web-assets-dir pins any build)
```

There is no config file — everything is a CLI flag (`--host`, `--port`,
`--db-path`, `--preamble`, `--no-web`, `--web-assets-dir`; see
`flux-server --help`) or managed from the UI into the server database.

Open `http://127.0.0.1:8080`, add a provider endpoint in the Providers dialog
(top bar), then pick a working directory in the new-chat dialog and chat.

## Documentation

| Doc | Question it answers |
|-----|---------------------|
| [`docs/architecture.md`](docs/architecture.md) | How does it work inside? (current architecture, with diagrams) |
| [`docs/decisions.md`](docs/decisions.md) | Accepted tradeoffs — what we deliberately chose NOT to improve |
| [`AGENTS.md`](AGENTS.md) | Conventions & context for AI coding agents |

## WebSocket protocol

Plain JSON with a `type` field dispatch. Connect to `ws://localhost:{port}`.

| Direction | Type | Purpose |
|-----------|------|---------|
| → | `chat_create {name, workdir, kind, provider, model}` | Create conversation — `kind`: `classic` (accumulating context) / `feature` (re-orchestrated per feature); `provider` and `model` are both REQUIRED (providers are pure endpoints managed from the UI) |
| → | `chat {chat_id, message}` | Send message (auto-claims when unleased) |
| → | `chat_claim {chat_id}` | Acquire the chat's lease — history + subscribe + lease in one message; busy → `error{chat_busy}` |
| → | `chat_open {chat_id}` | Viewer path: subscribe + history (first subscription only; idempotent when already subscribed) |
| → | `chat_close {chat_id}` | Fully exit: unsubscribe + return the lease (if held) |
| → | `cancel {chat_id}` | Cancel the active phase (stream or tool flight — tools are interrupted cooperatively, partial results kept) |
| → | `chat_rebase {chat_id, base_message_id?}` | Rebase the context to live only above `base` (`None` = latest) |
| → | `question_response {chat_id, id, answer}` | Answer to the model's `question` tool (lease holder only) |
| → | `chat_list` | Request chat list |
| → | `chat_delete {chat_id}` | Delete conversation |
| → | `chat_rename {chat_id, name}` | Rename conversation |
| → | `chat_provider {chat_id, provider, model}` | Hot-swap the chat's provider/model (lease holder only; lands at the round boundary, announced via `provider_switched`) |
| → | `session_resume {session_id}` | Replay a stored session id after reconnect — leases survive a grace window; answered by `session_resumed` |
| → | `ping` | Application-level liveness probe |
| ← | `pong {}` | Answer to `ping` |
| ← | `ready {session_id}` | Handshake ack — carries the freshly minted session identity |
| ← | `text_delta {chat_id, delta}` | Streaming text |
| ← | `reasoning_delta {chat_id, delta}` | Streaming reasoning |
| ← | `tool_start {chat_id, id, name, arguments}` | Tool execution start |
| ← | `tool_result {chat_id, id, result}` | Tool execution result (frontend matches cards by id) |
| ← | `question_required {chat_id, id, question}` | The `question` tool awaits the user's answer (direct to lease holder; parked until answered) |
| ← | `chat_history {chat_id, messages}` | Chat history snapshot |
| ← | `chat_state {chat_id, state}` | Authoritative round-state snapshot (idle/streaming) |
| ← | `context_rebased {chat_id, base_message_id}` | Feature mode: context re-based at a feature boundary |
| ← | `usage {chat_id, prompt_tokens, completion_tokens, cached_tokens}` | Token usage |
| ← | `stream_end {chat_id, finish_reason?}` | Assistant turn complete (abnormal `finish_reason` = truncated) |
| ← | `stream_cancelled {chat_id}` | Round cancelled (user cancel; broadcast to viewers) |
| ← | `error {chat_id?, code, message}` | Unified error channel (chat-level / connection-level) |
| ← | `chats {chats}` | Chat list |
| ← | `chat_created {chat}` | New chat ack |

Management and browse frames — `provider_list` / `provider_add` / `provider_remove` /
`provider_models`, `mcp_list` / `mcp_add` / `mcp_remove`, `fs_list` / `fs_read` — and
their replies are omitted here for brevity; see `docs/architecture.md` §3.8.

## Web frontend

React 19 + TypeScript on Vite; Radix UI primitives own dialog/menu/tooltip/tabs
behavior; zustand carries app state; Tailwind CSS v4 drives component styling;
marked + DOMPurify + highlight.js drive the imperative markdown/streaming pipeline
(rAF-coalesced, append-only paragraphs — see `docs/architecture.md` §4).

```bash
cd clients && npm install   # npm workspace root
cd web
npm test                    # vitest unit tests
npm run build               # tsc --noEmit + vite build → dist/
```

## Scripts

Helper scripts for common development tasks. All scripts have `.sh` (Linux/macOS) and `.ps1` (Windows) versions.

| Script | Description |
|--------|-------------|
| `scripts/build.sh` | Build Rust workspace (release) + the web UI |
| `scripts/run-server.sh` | Start flux-server via `cargo run --release` (CLI flags pass through). The web UI is served by default — the script builds it when missing (`--no-web` runs headless); the database defaults to `~/.flux/flux.db` (`--db-path` overrides). |
| `scripts/test.sh` | Full validation: `fmt` → `clippy` → `cargo test` → `tsc` → `vitest` → `vite build` |
| `scripts/package-web.sh` | Assemble the servable web UI directory (`index.html` + `assets/{bundle.js,bundle.css}`) |
| `scripts/package.sh` | Build server (host or cross-compile) + web UI → `dist/` |
| `scripts/fetch-fonts.sh` | Refresh the bundled terminal fonts (JetBrains Mono + Nerd Font patch, OFL-1.1) from their official releases |

Common workflows:

```bash
# Local development
./scripts/build.sh                          # build everything
./scripts/run-server.sh                     # start server + UI (web is default-on)
./scripts/test.sh                           # run all checks

# Release packaging
./scripts/package.sh                                    # host target only
./scripts/package.sh x86_64-unknown-linux-gnu          # single cross-compile
./scripts/package.sh x86_64-unknown-linux-gnu \        # multiple targets
                     aarch64-apple-darwin \
                     x86_64-pc-windows-msvc
```
