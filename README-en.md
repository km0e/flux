# Flux

> 中文说明：[README.md](README.md)

A general-purpose coding agent framework in Rust with a browser chat UI frontend.

## Features

- **Built-in tools**: `read_file`, `edit_file`, `write_file`, `replace_lines`, `list_directory`, `glob`, `grep`, `bash` — plus per-chat `question` (ask the user), `state_get`/`state_set`, and `buf_read` (paged overflow output). Output limits prevent context overflow: every tool result passes a central 8000-char inline budget — larger outputs are stored whole in a per-chat overflow buffer the model pages through with `buf_read`; grep shapes matches to a window (500 max), glob caps at 500 entries.
- **Agent Skills**: lazy-loading capability packages (`SKILL.md` directories per the [Agent Skills standard](https://agentskills.io)) — the model discovers them via `skill_list` and loads full instructions on demand via `skill_read` (nothing injected into prompts; project skills in `<workdir>/.flux/skills/`, global in `~/.flux/skills/`, project wins name collisions). The web UI's Skills dialog manages them: install from a local directory or a git URL (optional subpath for multi-skill repos), remove global entries — immediately effective, no restart.
- **LLM providers**: OpenAI / OpenAI-compatible endpoints — pure endpoints (id · url · api key) managed from the web UI's Providers dialog and stored in the server database (there is no config file). A LOCAL model registry saves per-model request params (auto-enriched from [models.dev](https://models.dev) metadata); switching a chat's provider hot-swaps at the round boundary — the engine re-begins over the full history without dying.
- **Streaming**: real-time text + reasoning deltas over gRPC-Web — reception and rendering are decoupled by one animation frame (deltas append to a raw buffer; a coalesced rAF render folds them in, at most one incremental render per frame). Committed paragraphs render once and are appended append-only, code blocks type in stably and highlight exactly once, and a stick-to-bottom state machine keeps the follow smooth without yanking the reader back.
- **MCP client**: connect external MCP servers and expose their tools — two transports: `stdio` (local child processes) and `http` (remote Streamable HTTP endpoints, `url` + `headers`; header values live in the server database and never leave it). Managed from the web UI's MCP dialog (stored in the server database; persist-first + LIVE apply — the manager spawns the child / opens the session, registers the tools, and the matching chats rebuild at the round boundary), with a self-healing supervisor that respawns a dead session with capped backoff. Tool-set changes (`tools/list_changed`) reload automatically, and log notifications forward to the top-bar bell through server-side rate limiting.
- **No approvals**: tools execute directly — there is no confirmation step. The chat's workdir boundary reaches tools as invocation context (`ToolCtx`), paths resolve inside it, and tool errors come back as result text the model reads and self-corrects. Real isolation comes from the OS/container boundary. The built-in `question` tool lets the model ask the user a question mid-round (agent-produced text + options, answered via an inline card in the conversation).
- **Stream cancellation & interjecting**: stop generation mid-stream (Stop button or Escape key); sending while a round runs is ONE interrupt-send RPC that fuses "cancel the live round" and "queue my message" server-side, so the interject order holds by construction.
- **Fork from any message**: a non-destructive branch — the new chat copies the source transcript up to (but excluding) the chosen user turn, whose content is prefilled into the fork's composer; the source is untouched.
- **Chat history**: persistent conversations with full message history across restarts; a 30s session grace window keeps leases alive across a page refresh.
- **Built-in terminal**: interactive shells (e4pty PTY, xterm.js UI) over a dedicated `/ws/term` side channel — multiple per chat, added on demand via the dock's "+" or the empty-state action; kept alive across tab switches and re-attached after a page refresh within the session grace window (256 KiB scrollback replay).
- **Tabbed right dock**: opened files accumulate as tabs (multi-file, editor-style); file bodies scroll horizontally; `.md` files render through the shared markdown pipeline with a Raw toggle; a pinned Terminal tab lives beside them; a Round tab lists what the current round changed — files open in one click, tool calls jump back into the stream.
- **Web UI**: a React chat UI served BY DEFAULT by flux-server on the SAME port as the Connect API (one listener) — `./scripts/run-server.sh` (builds the UI when missing); `--no-web` runs headless. Bind beyond localhost only behind a TLS proxy — the server has no auth layer.

## Project layout

```
flux/
├── proto/flux/v1/          # The wire contract (single source of truth → Rust + TS codegen)
├── crates/                 # Rust workspace (server + kernel + tools)
├── clients/web/            # Web UI (React 19 + Radix + Tailwind v4 + zustand, Vite)
└── docs/
```

## Environment

| Tool | Version | Notes |
|------|---------|-------|
| Rust | stable channel | Pinned by `rust-toolchain.toml` (stable + rustfmt/clippy); rustup provisions it |
| Node | **24 LTS** | the only supported line (`engines` declares it; pnpm warns on others; nvm recommended; version-sensitive script flags are guarded). **`cargo build` needs it by default**: the browser UI is embedded in the binary and built at compile time via build.rs → `package-web.sh` (a toolchain-less build embeds a placeholder; `FLUX_WEB_UI_NO_BUILD=1` skips) |
| pnpm | 11.x | Version pinned via `packageManager` in `clients/package.json`; the scripts self-provision it when missing |
| protoc | system binary | `protobuf-compiler` (apt) / `brew install protobuf` — called at compile time by `flux-proto` |
| Chrome | any recent build | Only for the headless e2e smoke (`pnpm run ui-check`) |

Frontend contract tools (buf etc.) all resolve from `clients/web` devDependencies (pnpm's strict layout) — no global installs needed.

## Quick start

```bash
cargo build --release               # the browser UI is embedded in the binary
                                    # (build.rs runs package-web.sh when the dist
                                    # is missing or stale)
./scripts/run-server.sh             # serves UI + API; pre-builds the dist when missing
# or: cargo run -p flux-server      # same embedded UI; --web-assets-dir points at a
                                    # disk override dir (Gitea custom/ semantics)
```

There is no config file — everything is a CLI flag (`--host`, `--port`,
`--db-path`, `--preamble`, `--no-web`, `--web-assets-dir`; each flag has a
same-named `FLUX_*` environment fallback, precedence flag > env > default;
`--generate-completions bash|zsh|fish|powershell|elvish` emits shell
completions; see `flux-server --help`) or managed from the UI into the
server database.

Open `http://127.0.0.1:8080`, add a provider endpoint in the Providers dialog
(top bar), then pick a working directory in the new-chat dialog and chat.

## Install (release artifacts)

No source checkout needed — use the [Releases](https://github.com/km0e/flux/releases) artifacts:

```bash
# server binary — the browser UI is EMBEDDED in it
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/km0e/flux/releases/latest/download/flux-server-installer.sh | sh
```

(Windows PowerShell likewise, via `flux-server-installer.ps1`; installs to
`~/.cargo/bin` and writes the PATH scripts and uninstall receipt for you.)

**Optional customization (Gitea `custom/` semantics)**: same-named files in
`~/.flux/web-ui/` (or whatever `--web-assets-dir` points at) override the
embedded UI PER PATH — tweak one index.html, add one asset, everything else
keeps falling through to the embedded base. No directory = the pure embedded
UI. A directory carrying a stale `web-ui-version.txt` gets a startup warning.

## Documentation

| Doc | Question it answers |
|-----|---------------------|
| [`docs/architecture.md`](docs/architecture.md) | How does it work inside? (current architecture, with diagrams; English version alongside) |
| [`docs/decisions.md`](docs/decisions.md) | Accepted tradeoffs — what we deliberately chose NOT to improve |
| [`CHANGELOG.md`](CHANGELOG.md) | What changed in each release (dist feeds it to the GitHub release notes) |
| [`AGENTS.md`](AGENTS.md) | Conventions & context for AI coding agents |

## Wire protocol (Connect / gRPC-Web)

The browser talks [Connect](https://connectrpc.com) (gRPC-Web) on the same
port as the UI. `proto/flux/v1/*.proto` is the ONE contract source — the
Rust bindings are generated at build time (`flux-proto`), the TypeScript
bindings by `buf generate` (the web package's prebuild/pretest hooks).

| Family | RPCs | Purpose |
|--------|------|---------|
| EventService | `Subscribe` | THE session-scoped stream: the identity anchor (open = attach/adopt, first `ready` frame carries token + leases), all chat events, keepalives; close = detach |
| ChatService | CreateChat · ListChats · OpenChat · ClaimChat · CloseChat · DeleteChat · RenameChat · SendMessage · CancelRound · ForkChat · SwitchProvider · AnswerQuestion | Conversation control (lease-gated; the caller's session token rides `x-flux-session` metadata) |
| ProviderService / ModelService | ListProviders · GetModels · AddProvider · RemoveProvider / ListModels · SaveModel · RemoveModel · SyncModels | Provider registry + LOCAL model registry (api_key never leaves the server) |
| McpService | ListServers · AddServer · RemoveServer | MCP launch list (persist-first + live apply) |
| SkillService | ListSkills · AddSkill · RemoveSkill | Global skills management |
| FileSystemService | FsList · FsRead | Workdir picker + file explorer |

Application-level failures ride the response's inline `error` field;
transport/infrastructure failures are gRPC statuses. The full semantics —
identity lifecycle, lease/viewer model, stream elements, snapshot
reconciliation — are documented in `docs/architecture.md` §3.8.

## Web frontend

React 19 + TypeScript on Vite; Radix UI primitives own dialog/menu/tooltip/tabs
behavior; zustand carries app state; Tailwind CSS v4 drives component styling;
marked + DOMPurify + highlight.js drive the imperative markdown/streaming pipeline
(rAF-coalesced, append-only paragraphs — see `docs/architecture.md` §4).

```bash
cd clients && pnpm install   # pnpm workspace root
cd web
pnpm test                    # vitest unit tests
pnpm run build               # tsc --noEmit + vite build → dist/
```

## Scripts

Helper scripts for common development tasks. All scripts have `.sh` (Linux/macOS) and `.ps1` (Windows) versions.

| Script | Description |
|--------|-------------|
| `scripts/run-server.sh` | Start flux-server via `cargo run --release` (CLI flags pass through). The web UI is served by default — the script builds it when missing (`--no-web` runs headless); the database defaults to `~/.flux/flux.db` (`--db-path` overrides). |
| `scripts/test.sh` | Full validation: `fmt` → `clippy` → `cargo test` → `tsc` → `vitest` → `vite build` |
| `scripts/package-web.sh` | Build the web UI (Vite) and assemble the servable root — `index.html` + content-hashed `assets/*` — at `clients/web/dist` (or `--out DIR`) |
| `scripts/fetch-fonts.sh` | Refresh the bundled fonts (JetBrains Mono + Nerd Font patch, IBM Plex Sans, OFL-1.1) from their official releases. The font files are committed — this script is only for manual upgrades; the build never fetches anything |

Common workflows:

```bash
# Local development
./scripts/run-server.sh                     # start server + UI (web is default-on; auto-built when missing)
./scripts/test.sh                           # run all checks

# Release packaging (dist — single config source: dist-workspace.toml; local and CI read the same definition)
cargo install cargo-dist --locked           # once
dist build                                  # local release-shaped artifact (host target: archive + web-ui + checksums)
dist build --target aarch64-unknown-linux-gnu   # local cross-compile (needs cargo-zigbuild; Windows targets need cargo-xwin)

# Cutting a release
git tag v0.x.y && git push origin v0.x.y    # release.yml: native builds on 5 platforms → GitHub Release
```
