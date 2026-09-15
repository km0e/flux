# Flux — Agent Guide

This file contains project-specific context for AI coding agents working on Flux.

## Documentation map

| Doc | Role | Question it answers |
|-----|------|---------------------|
| `README.md` | What the project is, quick start (Chinese) | "What is this?" |
| `README-en.md` | English README (mirrors `README.md`) | English mirror |
| `CHANGELOG.md` | Release-by-release changes (Keep a Changelog; dist parses it as the GitHub release notes) | "What changed, when?" |
| `docs/architecture.md` / `architecture-en.md` | The current architecture (the "what"), with Mermaid diagrams | "How does it work inside?" |
| `docs/decisions.md` / `decisions-en.md` | **Accepted tradeoffs** (`T-xx`) — existing compromises that are explicitly accepted and must not be "fixed" as defects | "What did we deliberately choose NOT to improve?" |

Maintenance rules: rationale lives where it belongs — the architecture doc carries the current "what", code comments carry local "why"; `docs/decisions.md` is reserved for accepted tradeoffs; `CHANGELOG.md` gains one section per release (its `[x.y.z]` heading feeds dist's GitHub release notes). The architecture doc describes only the present tense.

## Project overview

Flux is a general-purpose coding agent framework in Rust with a browser chat UI frontend.
It is built as a Cargo workspace:

- [`flux-core`](crates/flux-core/) — the agent-runtime contract layer: pure types (`Message`, `Role`, `ToolCall`, `CoreError`, `ErrorCode`, `ChatStateKind`), `WireEvent` (kernel output vocabulary), `Tool` trait + `ToolRegistry` + `ToolCtx` (cancel token, call id, sandbox boundary with `resolve`), the four kernel ports, and the `Provider` session factory (deps: `serde`, `serde_json`, `strum`, `thiserror`, `async-trait`, `tracing`, `futures`).
- [`flux-macros`](crates/flux-macros/) — proc-macro for `#[derive(Tool)]`.
- [`flux-provider`](crates/flux-provider/) — OpenAI-compatible implementation + SSE client, implementing flux-core's `Provider` session factory (each instance is model-pinned; `begin` opens a `Connection`).
- [`flux-tools`](crates/flux-tools/) — built-in filesystem, shell, search, and skill tools; tools resolve paths against the chat boundary via `ToolCtx::resolve` .
- [`flux-mcp`](crates/flux-mcp/) — MCP client bridge: connects external MCP servers — stdio child processes OR remote Streamable HTTP endpoints (the launch list lives in the DB, UI-managed, persist-first + live-apply) and exposes their tools.
- [`flux-store`](crates/flux-store/) — SQLite persistence layer (chats, messages, state, provider registry).
- [`flux-loop`](crates/flux-loop/) — conversation kernel: pure state machine (`machine.rs`) + the single pump (`runtime.rs` — machine + two channels, zero I/O); the I/O vocabulary (`LoopInput`/`LoopFact`/`StreamEvent`/`StreamHandle`/`Connection`) and the tool/boundary contracts live in flux-core, the round consumer (which supervises the tool flights) is the flux-chat channel peer.
- [`flux-session`](crates/flux-session/) — session layer, CONTROL plane: the chat manager (`ServerState` — global config, the chat cache hydrated from the store, the session identity registry), lease/viewers operations (`ops`), session identity objects (`Session`/`SessionRef` — opaque handles carrying the typed sink; lease/viewers keyed by handle, not string), session detach/resume/reap, the per-chat event router (mapping the kernel's WireEvents onto the proto stream elements), and task lifecycle. Depends on flux-chat and flux-proto; never the reverse.
- [`flux-chat`](crates/flux-chat/) — session layer, DATA plane: the per-chat task machinery — the chat entity (`chat` ToolPort + persistence helpers, `domain` state, `handle` control handle, `spawn` assembly, `round` round consumer (also supervises the tool flights), `tool_exec` flight supervision, `buf` overflow buffer, per-chat `question` tool, reserved tool-name check). The chat boundary (`workdir`/`current_dir`) fills the per-invocation `ToolCtx` at dispatch (adapter-side). The engine-rebuild flow is an in-place gate rebuild INSIDE the consumer (truth-source change → `Rebuild` ctrl → machine gate → re-assemble at `GateReleased`); the engine never dies for one; output rides flux-core's `OutputPort`.
- [`flux-server`](crates/flux-server/) — ONE axum transport hosting the Connect surface (`/flux.v1.*`, gRPC-Web) + the terminal side channel (`/ws/term`) + the browser UI static site (single port), filesystem browsing for the workdir picker, server wiring.
- Web UI (`clients/web/`) — React + TypeScript browser chat UI (the single frontend), served by flux-server's static layer.

Dependencies: `reqwest` (HTTP), `serde` / `serde_json`, `axum` + `tower-http` (single HTTP transport: Connect surface + static site via ServeDir/CompressionLayer), `tonic`/`tonic-web` + `prost` (the generated flux.v1 surface via `flux-proto`), `sqlx` (SQLite persistence), `rmcp` (MCP protocol), `tokio`, `futures`, `uuid`. (`tokio-tungstenite` remains only as the terminal side channel's e2e client.)
Frontend (clients/web,): React 19, zustand, Radix UI (dialog/dropdown-menu/tooltip/tabs), Tailwind CSS v4, react-arborist (workdir tree), marked 18 + DOMPurify 3 + highlight.js (the imperative markdown/streaming pipeline), Vite (build).

## Workspace layout

```
flux/
├── proto/ # The wire-protocol single contract source (.proto → codegen → flux-proto + web TS)
├── crates/
│ ├── flux-core/ # Contract layer (types / wire / tool / boundary / ports / Provider session factory)
│ │ └── src/
│ │ ├── lib.rs # Re-exports (types / wire / tool / boundary / ports / Provider)
│ │ ├── types.rs # Message, Role, ToolCall, ToolDefinition
│ │ ├── error.rs # CoreError enum
│ │ ├── provider.rs # Provider session-factory trait
│ │ ├── tool.rs # Tool trait + ToolRegistry (RwLock-free)
│ │ ├── boundary.rs # resolve_path — sandbox-boundary path resolution behind ToolCtx::resolve 
│ │ ├── ports.rs # ToolPort (tool execution) + OutputPort (adapter-side wire sink) — persistence is a fact-trace fold, the provider is a Connection
│ │ ├── loop_io.rs # Loop I/O vocabulary: LoopInput / LoopFact + RoundOutcome / StreamEvent / StreamHandle / Connection
│ │ └── wire.rs # WireEvent vocabulary
│ ├── flux-macros/ # #[derive(Tool)] proc-macro
│ │ ├── src/lib.rs
│ │ └── tests/derive_tool.rs
│ ├── flux-provider/ # OpenAI impl + SSE client (implements flux-core's Provider session factory)
│ │ └── src/
│ │ ├── lib.rs # Re-exports
│ │ ├── openai.rs # OpenAI-compatible: prefix/suffix cache, SSE parsing
│ │ └── sse.rs # SSE client + byte parser
│ ├── flux-proto/ # Generated protobuf/tonic contracts (build-time codegen from proto/)
│ ├── flux-tools/ # Built-in tool implementations
│ │ └── src/
│ │ ├── lib.rs # Tool registration
│ │ ├── fs.rs # read_file, edit_file (str_replace), write_file, replace_lines, list_directory
│ │ ├── shell.rs # bash
│ │ ├── search.rs # glob, grep
│ │ ├── subprocess.rs # Shared command runner (kill-hygiene: process-group timeout + kernel parent-death signal)
│ │ ├── test_util.rs # Test-only helpers (cfg(test))
│ ├── flux-mcp/ # MCP client bridge
│ │ └── src/lib.rs
│ ├── flux-store/ # SQLite persistence (sqlx)
│ │ ├── src/
│ │ │ ├── lib.rs # Store struct, migrations, open, vacuum
│ │ │ ├── chats.rs # insert_chat, list_chats, delete_chat, rename_chat
│ │ │ ├── mcp.rs # list/insert/delete MCP-server connect rows (the MCP launch list's ONLY home; kind = stdio | http)
│ │ ├── messages.rs # load_messages, append_messages
│ │ │ ├── providers.rs # list_providers, insert_provider, delete_provider — the provider registry's ONLY home
│ │ │ └── state.rs # load_state, save_state_entry
│ │ └── migrations/
│ │ └── 001_consolidated_schema.sql # 单一合并 schema
│ ├── flux-loop/ # Conversation kernel (pure machine + pump; zero I/O)
│ │ └── src/
│ │ ├── lib.rs # Re-exports (Machine, Loop, OUT_CAPACITY)
│ │ ├── machine.rs # Pure reducer: State × LoopInput → Vec<LoopFact> (table tests)
│ │ └── runtime.rs # The pump: input FIFO → machine → bounded fact trace; RoundState on kind transitions
│ ├── flux-session/ # Session layer, CONTROL plane (spans chats & sessions; depends on flux-chat)
│ │ └── src/
│ │ ├── lib.rs # Re-exports (ServerState, Session/SessionRef/Sink, ops outcomes, protocol types)
│ │ ├── manager.rs # ServerState (ChatManager) — global config + Chat cache (RwLock) + session registry (live/detached), UUID IDs
│ │ ├── ops.rs # Lease/subscribe/broadcast + session detach/resume/reap + claim (3-way snapshot)
│ │ ├── identity.rs # Session identity object (opaque handle: token/sn/sink/detached state) + SessionSink trait
│ │ ├── router.rs # Per-chat event router (fanout, slow-viewer gap handling, parked questions, delta batching, activity touch)
│ │ ├── lifecycle.rs # Task lifecycle (lazy spawn, stale replacement, engine rebuild: Rebuild cmd → in-place gate rebuild)
│ │ ├── tests.rs # Provider pin + hot-swap integration tests (through ServerState)
│ │ └── test_util.rs # Test-only control fixtures (recording sinks, state builders)
│ ├── flux-chat/ # Session layer, DATA plane (one conversation task; flux-core ports only)
│ │ └── src/
│ │ ├── lib.rs # Re-exports (ResolvedPin, INITIAL_STATE, QuestionBoard, OutputBuf, context keys)
│ │ ├── chat.rs # Chat entity (execute pipeline: existence → ctx boundary fill → execute → bound) + ChatInit/ResolvedPin
│ │ ├── domain.rs # ChatState, INITIAL_STATE, state tools
│ │ ├── handle.rs # ChatHandle control handle (send/cancel/rebuild/state snapshot)
│ │ ├── spawn.rs # Task assembly (ChatInit incl. questions, loop + peers wiring)
│ │ ├── round.rs # Round consumer (fact-trace fold; Rebuild cmd → in-place gate rebuild)
│ │ ├── tool_exec.rs # Supervised tool flights (two-tier interrupt, panic capture) — a library folded by the round consumer, no task/channel
│ │ ├── buf.rs # Overflow buffer, store-backed (call-id-anchored, never overwritten; fork copies)
│ │ ├── reserved.rs # Reserved tool-name check (state_get/state_set/buf_read/question)
│ │ ├── question.rs # `question` tool: agent-produced prompt to the user (board + tool)
│ │ └── tests.rs # Data-plane tests (spawn/round/buf/question/domain)
│ └── flux-server/ # Transport + Session + wiring
│ ├── src/
│ │ ├── main.rs # CLI entry (NO config file — flags only), DI assembly, MCP restore (DB rows, skip-with-warn), start transport
│ │ ├── registry.rs # ProviderRegistry — DB-backed (hydrate at startup; Add/RemoveProvider persist-first), instance building, model probes
│ │ ├── mcp.rs # MCP launch-list management (persist-first + live apply: connect/register/unregister; self-healing supervisor; env values redacted to keys)
│ │ ├── management.rs # Management-plane operations (providers/models/mcp/skills mutations — shared by the RPC shims and the MCP supervisor)
│ │ ├── models_dev.rs # models.dev catalog fetch + (base_url, model) matching (lazy, TTL cache, best-effort)
│ │ ├── skills.rs # Skill install/browse/remove (local dir or git URL; global dir only)
│ │ ├── fsbrowse.rs # Filesystem listing/preview for the UI workdir picker (FsList/FsRead)
│ │ ├── web.rs # Browser UI static site (ServeDir + CompressionLayer + SetResponseHeaderLayer security headers) on the SAME listener
│ │ ├── terminal.rs # Terminal side channel /ws/term (PTY actor, auth handshake, scrollback replay, reaper)
│ │ ├── transport.rs # Single axum server: /flux.v1.* + /ws/term + static site (configurable host, session reaper)
│ │ └── grpc/ # The Connect surface serialization shims: mod.rs (routes), chats.rs, events.rs (the event plane: Subscribe anchors identity + keepalive pump), fs.rs, management.rs
│ └── tests/
│ └── connect_e2e.rs # Connect-protocol E2E (real binary, gRPC-Web framing + the /ws/term WS client)
├── clients/ # pnpm workspace root (flux-clients; pnpm-workspace.yaml: web)
│ └── web/ # @flux/web — THE frontend (React 19 + Radix + Tailwind v4 + zustand on Vite)
│ ├── index.html # Vite source (served no-store, read per request; the page connects back same-origin — no template injection)
│ ├── vite.config.ts # build (content-hashed code-split chunks) + vitest config
│ ├── dist/ # build output — what flux-server serves
│ └── src/
│ ├── main.tsx # entry: log level + mountChat
│ ├── mount.tsx # mountChat: flushSync render-first init, restore, lease-switch subscription, the title's transition-gated subscription
│ ├── components/ # React UI: App, TopBar, ChatHeader, Sidebar, ChatView, MessageList, ChatInput,
│ │ # UsageStats, Explorer (react-arborist), RightDock (round-artifacts tab + file tabs + terminal),
│ │ # RoundPanel, FileTabView, TerminalPanel, FileIcon, Toasts, ErrorBoundary
│ │ ├── ui.tsx # control primitives (Button/IconButton/TextField/Badge/Spinner) — styling authority
│ │ ├── ui/ # Radix wrappers (shadcn conventions): dialog, dropdown-menu, tooltip, tabs
│ │ └── dialogs/ # first-party dialogs: impl (service registration), ConfirmDialog,
│ │ # NewChatDialog (fs browser + kind picker), QuestionCard, SettingsDialog
│ │ # (Providers / MCP / Skills panels over one tabbed dialog; the Providers panel
│ │ # carries the LOCAL model registry — saved models + models.dev metadata;
│ │ # desktop master-detail —
│ │ # rail + preview/form detail; mobile stacked; integration-ui = shared building blocks)
│ ├── core/ # state.ts (zustand store), grpc.ts (Connect clients), grpc-connection.ts (Subscribe stream = the identity anchor), session.ts, prefs.ts (+theme), viewport.ts (keyboard-safe --fx-vvh), bridge.ts, failsafe.ts, types.ts
│ ├── services/ # panes, stream, stream-handler, history, dispatch, handlers,
│ │ # artifacts (per-round touched files + invocations, F-11), filePreview, code-copy, lease, forkDraft,
│ │ # drafts (per-chat composer drafts, sessionStorage write-through), title (document-title
│ │ # composition — name / working… / background-attention counter),
│ │ # fs, new-chat, dialogs (promise-shaped impls), providers, models, mcp, skills, terminal
│ ├── lib/ # markdown (marked+DOMPurify), render (rAF pipeline), dom, follow, highlight (+hljs-bundle), cn (clsx+twMerge), fileIcons, clipboard, format, id
│ ├── styles/ # app.css (tailwind entry + @theme bridge + the flux line), tokens.css (--fx-* light-dark),
│ │ # stream.css (imperative DOM), fonts.css (@font-face for the bundled fonts)
│ ├── assets/fonts/ # bundled fonts (IBM Plex Sans UI voice + JetBrains Mono machine/terminal voice, OFL-1.1;
│ │ # scripts/fetch-fonts.sh|.ps1 refreshes them from the official releases)
│ └── logger.ts # leveled logging, pluggable sink (console default)
├── scripts/ # .sh + .ps1 pairs: test, run-server, package-web, fetch-fonts (font asset refresh;
│ # release packaging = dist — dist-workspace.toml + .github/workflows/release.yml)
└── docs/
```

## Build & test

```bash
cargo build --release
cargo test --workspace
cargo clippy --workspace --tests -- -D warnings

# Run server
cargo run -p flux-server
cargo run -p flux-server -- --port 8081 --host 0.0.0.0

# Web frontend (clients/web — the single frontend)
cd clients && pnpm install # pnpm workspace root (pnpm-workspace.yaml: web)
cd web
pnpm run build # tsc --noEmit + vite build → dist/
pnpm test # vitest frontend unit tests
pnpm run ui-check # headless-browser e2e smoke (e2e/, needs node 24 + Chrome)

# Full validation suite
./scripts/test.sh # fmt → clippy → cargo test → tsc → vitest → build

# Release packaging (dist — single config source: dist-workspace.toml; CI runs
# the same config tag-driven via .github/workflows/release.yml)
dist build # local release-shaped artifact (host target: archive + web-ui + checksums)
```

### Toolchain environment (the supported surface)

- **Rust**: the `stable` channel, pinned by `rust-toolchain.toml` (stable + rustfmt +
  clippy; rustup installs it on demand).
- **Node**: **24 LTS** (Active LTS since 2025-10, maintenance until 2028-04) — the ONE
  supported line, declared as `engines` (`">=24 <25"`) in `clients/package.json` and
  `clients/web/package.json`; pnpm warns on any other version without breaking. CI and
  the release pipeline pin Node 24. Rationale: Node releases drift ahead of the
  toolchain — odd-numbered Current lines (25) expose experimental surfaces early (25's
  always-on webstorage getter makes pnpm's vendored `debug` print an
  ExperimentalWarning on every invocation; the scripts suppress it where the Node
  supports the disable flag, guarded so other Nodes never break), and older LTS lines
  age out of the tools' own floors.
- **pnpm**: the exact version is pinned via `packageManager` in `clients/package.json`;
  the scripts self-provision it when missing. The buf CLI rides web's devDependencies —
  no global tool installs beyond pnpm itself.
- **protoc**: a system binary (`protobuf-compiler` on apt, `brew install protobuf`) —
  flux-proto's build.rs shells out to it at compile time.
- **Chrome** (headless): only for `pnpm run ui-check`.

## Architecture

> Note: the experimental feature-mode line (`ChatKind::Feature`, the
> `feature_done` tool, the `flux-context` scaffold orchestration, the
> `feature_log` table) lives on the `feature-mode` snapshot branch — it is
> deliberately absent from `main` (chat kinds were removed; every chat runs
> the single accumulating context). Re-integration is evaluated later.

### Concepts

Three core concepts:

| Concept | File | Role |
|---------|------|------|
| **ServerState** | `flux-session/src/manager.rs` | 即 ChatManager：全局配置 + `RwLock<HashMap<ChatId, CachedChat>>` + 单一 `identities` 身份注册表（token → `SessionRef`，live 与 detached 同表）+ 每 chat 路由通道。CachedChat 带 `lease`（租约持有者）、`viewers`（订阅集合）、`task`（懒 spawn 的 ChatTask）、`router`。 |
| **Chat** | `flux-chat/src/chat.rs` | Conversation entity — implements the `ToolPort` the supervised tool flights call (execute pipeline + per-invocation `ToolCtx` boundary fill) and owns the persistence helpers the fact-trace fold drives. **Model-agnostic** (resolved provider instances arrive from the server). |
| **Loop / Machine** | `flux-loop/` | The conversation itself: a pure reducer (`Machine::step` over `LoopInput`) pumped between two channels — an input FIFO every peer writes and an ordered fact trace the chat layer folds. No I/O in the kernel. |
| **EventPlane** | `flux-server/src/grpc/events.rs` | 会话级 `Subscribe` 流 = 身份生命周期锚：流开 = attach（token 采纳或新铸，首帧 `ready` 携 token+leases），流断 = detach（宽限/reaper 机械不变）；流自带 sink（双队列 + biased pump，控制优先）+ 30s keepalive（客户端 frame deadline 判半开） |

**Key design**: 任务归 ChatManager（session 无关）；对话权 = 每 chat 至多一个 `lease`（发消息/取消/应答 question/删除/改名需租约，他人操作被拒 → `ErrorEvent{chat_busy}` / failed_precondition status）；观看权 = `viewers` 集合（ClaimChat 隐含订阅；OpenChat 为 viewer 降级订阅，可并发）。流断开 = **detach**：租约保留一个宽限期等流重开采纳，过期由 reaper 释放（任务继续跑）。Chat IDs are UUID v4.

**引擎重建（generic restart primitive — 边界原地换装）**：chat 分**外壳**（CachedChat：router/lease/viewers/questions/pin/元数据）与**引擎**（ChatTask：loop+consumer+connection，飞行监督在 consumer 内，全部装配而来）。真相源变更（provider pin / 全局工具注册表）从不打断运行中的轮次：变更方写真相源（请求时持久化）→ `Rebuild` ctrl 命令（provider 热切换携带新实例）→ 消费者武装机器门（活轮次与其后排队的轮先跑完——它们属于重建前上下文）→ 在 `GateReleased` 处**原地重建**：以与初始 spawn 相同的装配函数重装注册表（当前全局注册表），以新 provider 实例在全量持久历史上 re-begin 连接。引擎永不因重建而死亡；轮次之间消费者继续折叠；重建期间到达的发送直接入内核队列。崩溃/正常退出仍走惰性替换（无崩溃观察者哲学不变）。

### Lifecycle

```
Page opens the session-scoped Subscribe stream → attach（无 token 铸新；
带 token 则宽限期内采纳——ready 首帧携权威 token + leases，握手坍缩进流开）
 → 控制面/管理面/浏览全部走 gRPC-Web unary（身份在 x-flux-session metadata，
 与 WS 时代同一 SessionRef 对象；租约门拒绝 = 标准 status）
 → 打开 chat（操作者）= ClaimChat：历史/轮次快照走身份 sink（= 该流，单点投递）
 → 订阅 → 授租约；chat_open 是 viewer 降级路径与 stream_gap 重订阅
 → ServerState.create_chat: 建记录，租约+订阅授予创建者；任务懒创建
 → 首次发消息: ensure_task 懒 spawn Chat loop → 挂 router → WireEvent 经
 router 映射为 proto 流元素广播给 viewers（含租约持有者；R2 seq 直写 chat_seq）
 → 真相源变更（SwitchProvider / MCP 增删）: 先持久化 + 公告
 → Rebuild ctrl 命令 → 引擎在机器门原地重建（活轮次收尾 → 注册表/连接换装）
 → 切走/关闭: CloseChat（完全退出 = 退订 + 还租约）
 → 流断开 = detach：viewer 注册移除、租约保留 30s 宽限期等流重开采纳；
 过期由 reaper 释放并广播（任务继续跑完轮次后闲置）；
 半开检测 = 服务端 30s keepalive 帧，客户端 frame deadline（3×）判死重开
```

### Conversation kernel (`crates/flux-loop/`) — two channels, zero I/O

The kernel is a pure pump: it consumes a single input channel and emits a
single (bounded) fact trace — no I/O, no trait objects, no provider, no
persistence. Every collaborator is a channel peer wired by the adapter;
the shared vocabulary (`LoopInput` / `LoopFact` / `StreamEvent` /
`StreamHandle` / `Connection`) lives in flux-core so peers depend only on
the contract layer.

- **Loop** (`runtime.rs`) — machine + two channels: consume `LoopInput`,
  step, forward `LoopFact`s, and follow each step with a
  `RoundState` fact on transitions (the post-step truth). The bounded
  output channel is the loop's backpressure surface. The loop owns the
  active stream's `StreamHandle` (a pure cancellation capability delivered
  via the input channel) and DROPS it on cancel/round-wrap — the drop
  stops the provider's push immediately.
- **Machine** (`machine.rs`) — a total reducer
  `State × LoopInput → Vec<LoopFact>`. States: `Idle`, `Streaming`,
  `ProcessingTools` (no approval state — tools execute directly). Facts
  are semantic, past-tense: `Wire(WireEvent)`, `TranscriptCommitted`,
  `ModelInputRequested`, `ToolDispatched`, `InterruptTools`,
  `RoundState`, `GateReleased`, `RoundEnded(RoundOutcome)` — the round's
  semantic terminal classification (Completed / Cancelled / Failed),
  emitted at the single wrap-up point so consumers fold semantics
  instead of scraping wire events. The transcript commits are
  BATCH-ATOMIC: the user message at round start, each tool batch
  (assistant tool_calls message + the batch's results) at its boundary —
  BEFORE the continuation stream opens — and the final segment at round
  end. A crash (SIGKILL/OOM — no graceful path involved) therefore loses
  at most the live segment, and the persisted tail never carries a
  dangling tool_call (OpenAI-compatible APIs reject those with 400).
- **Connection** (`flux-provider`, the `Connection` trait in flux-core) —
  one stateful session per chat (the prefix cache); a single interface:
  `open(pending, sink) -> StreamHandle` pushes parsed stream events into
  the loop's input channel (drop = cancel; stall watchdog and EOF-
  truncation detection live inside). A connection is never mutated in
  place — a truth-source change lands at the machine's gate, where the
  round consumer re-begins a fresh connection over the persisted
  history and the old one drops with the swap.
- **Tool flights** (`flux-chat/src/tool_exec.rs`, a library folded by the
  round consumer) — the consumer's select loop dispatches one flight at a
  time, interrupts with two tiers (cooperative `ToolCtx` token → 5s
  grace → force-drop so Drop-based process-group cleanup runs), and
  pushes exactly one `ToolFinished` per dispatch into the loop's FIFO
  (panic captured structurally; results kernel-marked `INTERRUPTED_MARK`).
- **Round consumer** (`flux-chat/src/round.rs`, the fact-trace fold) —
  ONE task per chat, living for the chat's lifetime, interprets the
  trace in order: persistence fold (`TranscriptCommitted` → store
  append, awaited inline so the persist-before-announce order
  survives), routing fold (`Wire` facts → router), provider triggering
  (`ModelInputRequested` → `connection.open`), the engine rebuild (the
  ONE control command: the consumer answers `Rebuild` with a `Hold` —
  the machine's own gate; queued turns run pre-gate, inside the
  pre-rebuild context — and at `GateReleased` rebuilds IN PLACE:
  re-assembles the registry from the current global truth, re-begins
  the connection on the carried provider over the full persisted
  transcript), and the
  round-state slot write (`RoundState` → the authoritative subscription
  snapshot). The consumer never
  mutates a RUNNING round — rebuilds land only at the machine's
  boundary, and the task exits only when the loop dies or the handle
  drops.

Committed tool batches dispatch one flight at a time (single flight;
`tool_start` goes out at dispatch, `tool_result` when the outcome is
collected). While the model is still FORMING a tool call the provider
emits `ToolCallPreview` chunks (identity the moment id+name are parsed,
then per argument fragment) which pass through the machine as pure wire
signaling — the client shows a pending card before the arguments finish
streaming; the `tool_start`/`tool_result` pair supersedes it and a
never-upgraded preview is voided client-side at round end (never
persisted). The chat boundary (`workdir`/`current_dir`) fills the
invocation `ToolCtx` inside the adapter's execute pipeline; tools resolve
paths via `ToolCtx::resolve` and any failure is the tool's result string,
never a round-level block. A cancel is an ordinary queue event: the
machine drops the stream handle or emits `InterruptTools`, stale cancels
are absorbed in every state. A user turn arriving mid-round is QUEUED
(machine turn queue), not dropped — it starts in the same step that wraps
the current round (the boundary rides a step-local `RoundState(Idle)`
fact); a cancel discards the queue. Sends therefore never wait for a
round boundary — including while a rebuild gate is armed (the turn runs
pre-gate, inside the pre-rebuild context).

`StreamEnd` signals the assistant turn is complete. The frontend flushes the
render loop and clears the streaming state on receipt. An abnormal
`finish_reason` on `stream_end` (`length` / `content_filter`) means the reply
was cut short — the frontend shows a neutral notice instead of treating the
partial answer as complete. The reason is announced only at round end: a
tool round keeps streaming after the first stream's `End` (its reason is
dropped by design), so the single `StreamEnd` carries the follow-up stream's
own reason (`"stop"` after a normal tool wrap-up).

### Output overflow buffer (`crates/flux-chat/src/buf.rs`) — anchored, persisted, never overwritten

Every tool result passes `Chat::bounded_output` — outputs over the 8000-char inline budget
are stored whole in the per-chat `buf_entries` table (write-through, keyed by the
producing tool call's id — `ToolCtx::call_id`) and the tool returns a bounded head plus a
reference that IS the call id; the model reads the rest with
`buf_read {ref, offset, limit}` — **char-based** paging (buffered output is arbitrary, it
may be a single megabyte line; pages ≤ 6000 chars, strictly below the inline budget, so
buf_read never overflows recursively; read-through to the store). The reference is
self-describing and stable — the same call id sits in the transcript — so entries survive
engine rebuilds AND process restarts with no shell handoff (the in-memory buffer is
gone; the store is the only truth). Entries are **never overwritten** (a call id maps to
exactly one output) and there is no generation wipe or GC: an entry lives exactly as
long as its chat (a transcript only grows — there is no archive boundary); a FORK
copies the entries of the calls its copied transcript carries; chat deletion
cascades. Entries capped at
1M chars with an in-buffer drop marker. Per-tool caps merged into this layer: bash 8KB and
read_file line-length/total caps removed; grep keeps its match-window shaping (500
chars/line around the match) and the match cap rose to 500.

### Connect protocol (`proto/flux/v1` — the single contract)

The wire is gRPC-Web (tonic-web on the same axum router; the browser talks
`@connectrpc/connect-web`). `proto/flux/v1/*.proto` is the ONE source of
truth — the three-place manual sync (protocol.rs / types.ts / docs) is
GONE; the TS contract derives at build time (prebuild/pretest → `buf
generate`), the Rust binding at cargo build (`flux-proto`, OUT_DIR).
`buf lint` (STANDARD) is the naming authority; CI runs `buf breaking`.

**Identity & lifecycle**: the session-scoped `Subscribe` stream anchors it
— open = attach (absent `session_id` mints; a stored token adopts within
the grace window), the first `ready` frame carries the authoritative token
PLUS the held leases (the resume handshake collapsed into it), stream
close = detach (grace/reaper unchanged). Server keepalives every 30s; the
client's frame deadline (3×) detects a half-open connection. Unary calls
carry the token in `x-flux-session` metadata → the SAME `SessionRef`
object the fanout holds; lease-gate refusals map to standard statuses
(busy → failed_precondition, unknown → not_found, no/unknown token →
unauthenticated).

**Error model (D4')**: application-level failures ride the response's
inline `error` field (request-scoped UI data — provider/model/mcp/skill
mutations, create validation, fs browse); transport/infrastructure
failures are gRPC statuses. The stream's `ErrorEvent` elements (code
enum) are the app-level error channel (round errors, in-band demotion,
gap notices).

| Service | RPC | Purpose |
|---------|-----|---------|
| ChatService | CreateChat | Create (provider+model REQUIRED; inline validation error); creator gets lease+subscription |
| | ListChats | Global list (deliberately NOT gated) |
| | OpenChat / ClaimChat / CloseChat | Viewer open (snapshot on the stream) / operator claim — snapshot rides the identity's sink, single-point delivery; a claim ALWAYS grants (another holder's lease is STOLEN, the previous holder demoted in-band via `error{chat_busy}`); close = unsubscribe + release |
| | SendMessage / CancelRound / ForkChat / SwitchProvider / AnswerQuestion | User turn (queued; `client_msg_id` idempotency key — a resend is absorbed as `duplicate: true`) / cancel (stale absorbed) / fork (a NEW chat copying the source transcript up to but EXCLUDING a user message — the redo turn re-enters only when re-sent, whose content the client prefills into the fork's composer; the source is untouched, any viewer may fork, the ack carries the new `ChatInfo`) / hot-swap (validation rides the inline `error`; applied at the gate) / question answer (stale dropped) |
| | DeleteChat / RenameChat | Lease-gated mutations (statuses) |
| EventService | Subscribe | THE stream: ready{session_id, leases} → events (below) + keepalives; close = detach |
| SessionService | — | (deleted: resume collapsed into Subscribe, ping replaced by keepalives) |
| FileSystemService | FsList / FsRead | Workdir picker + explorer (git status per entry; inline errors; 256KB preview budget) |
| ProviderService | ListProviders / GetModels / AddProvider / RemoveProvider | Registry (api_key never leaves); probe = GET /models (inline error) |
| ModelService | ListModels / SaveModel / RemoveModel / SyncModels | LOCAL model registry (`params_json`/`meta_json` verbatim passthrough; models.dev auto-fill on create; matching chats rebuild) |
| McpService | ListServers / AddServer / RemoveServer | Launch list (persist-first + live apply; apply failure rides the ack inline, row stays); rows are stdio child processes OR Streamable HTTP endpoints (url + headers, header values never leave) |
| SkillService | ListSkills / AddSkill / RemoveSkill | Global skills (immediate effect; chat_id scopes project skills, read-only) |

**Subscribe stream elements** (`SubscribeResponse {chat_seq, chat_id, kind}` —
R2: the per-chat monotonic seq reconciles snapshots against the live
stream; a client drops gated content STRICTLY BELOW the snapshot's value,
the equal one is the first live element after it; session-level elements
carry seq 0 + empty chat_id):

| kind | Purpose |
|------|---------|
| `ready {session_id, leases}` | First frame — identity + leases (fresh or adopted) |
| `keepalive` | Server-injected liveness mark (client frame deadline keys on ANY element) |
| `text_delta` / `reasoning_delta` / `stream_end {finish_reason?}` / `stream_cancelled` | Streaming content; abnormal finish_reason = truncated notice |
| `tool_start` / `tool_result` | Tool flights (cards match by id) |
| `tool_call_preview {id, name?, arguments_delta?}` | The model is still forming the call — pending card before `tool_start`; voided at round end if never upgraded |
| `usage` | Per-round token totals |
| `question_required {id, question}` | The model's question — priority control lane, parked until answered (re-delivered on claim) |
| `chat_history` / `chat_state` | The claim/open snapshots THROUGH the stream (single-point delivery with the events they reconcile against) |
| `error {code, message}` | App-level error channel (round errors, demotion, gap) |
| `provider_switched` | Hot-swap landing notice |
| `message_persisted {id, content}` | A user message persisted (announced at turn acceptance) — the sender's client matches `content` against its own live user bubbles that do not yet carry an id and attaches the fork affordance (the id is the fork point carried by ForkChatRequest) without waiting for the next history snapshot; a cancelled turn was never persisted, so its bubble never gains an id |
| `chats` / `chat_created` / `providers` / `models` / `mcp_servers` / `skills` | Global broadcasts (session-level) |

### Terminal side channel (`/ws/term`)

A DEDICATED WebSocket per terminal on the same listener — terminal I/O is
high-frequency binary and never interleaves with the chat protocol. NO query
params (the token is a bearer credential and never rides the URL): the FIRST
frame the client sends is the auth frame — `{type:"auth", session, chat,
term?}` (`session` = resume token, validated read-only with the same
adoptability rule as the stream open; `chat` = cwd source; `term` = reattach
id, remembered per chat in sessionStorage and restored after a refresh) — a
bad/late/absent frame answers `error` + close. Frames: binary =
raw PTY bytes both ways; text = JSON control only — server→client
`hello {term, attached}` / `exited {code}` / `error {message}`,
client→server `resize {cols, rows}` / `close`. PTY allocation = e4pty
(tokio-async, Unix openpty + Windows ConPTY); interactive shell (`$SHELL`,
fallback bash; PowerShell on Windows) with `TERM`/`COLORTERM`/`TERM_PROGRAM`
set. Kill = e4pty's explicit termination (`PtyCtl::kill` — SIGKILL /
TerminateProcess, e4pty 0.3.1) + `wait` for the exit code, reported to the
attached socket as the usual `exited` frame; the handle drops that follow
tear down survivors (master close → SIGHUP). Not persisted, not part of the
kernel, no lease gate (same-origin trust; a UI affordance for the human).

### Persistence

`Store` (SQLite via `sqlx` with connection pool):
- Chat metadata: `insert_chat`, `list_chats`, `delete_chat`, `rename_chat`
- Messages: `load_messages`, `append_messages`
- State: `load_state`, `save_state_entry`
- Providers: `list_providers`, `insert_provider`（duplicate → `Ok(false)`）, `delete_provider` —— 注册表的唯一家（服务端无 config 文件；启动时 hydrate 入内存注册表，UI 经 AddProvider/RemoveProvider RPC 管理，**先写库后改内存**）
- Saved models: `list_models`, `upsert_model`（**只写 params 保留 meta**）, `update_model_meta`（**只写 meta 保留 params**）, `delete_model` —— 本地模型注册表的唯一家（`(provider_id, model_id)` 主键 + `params`/`meta` 两个写权分离的 JSON 列；provider 删除级联；UI 经 `model_save`/`model_remove`/`model_sync` 管理，models.dev 填充仅在创建/刷新路径写 `meta`）
- MCP servers: `list_mcp_servers`, `insert_mcp_server`, `delete_mcp_server` —— MCP 启动列表的唯一家（kind = `stdio` 子进程启动三无组 / `http` Streamable HTTP 端点 url+headers；headers 值同 env 值：入库不上线。启动时 McpManager 读行连接注册，UI 管理变更 **persist-first + 即时应用**，连接失败的行保留、下次启动重试）
- Buffered outputs: `save_buf_entry`, `load_buf_entry` —— 溢出缓冲的唯一家（按 tool call id 锚定、不覆盖；生命周期 = chat 生命周期，fork 复制其副本携带的调用条目；chat 删除级联）
- Called from `ServerState` and `Chat`. Async `sqlx::SqlitePool`, WAL mode.
- Opens with `auto_vacuum = INCREMENTAL` ensured (a legacy NONE-mode database's one-time rebuild VACUUM runs at open, before the listener binds); hourly conditional `PRAGMA incremental_vacuum` (freelist ≥ 1000 pages) for maintenance — a short normal write transaction, never a whole-db VACUUM while serving.

### Web frontend architecture (clients/web)

**Stack** : React 19 + TypeScript, zustand (state), Radix UI primitives
(dialog/dropdown-menu/tooltip/tabs — shadcn-style wrappers in `components/ui/`),
Tailwind CSS v4 (CSS-first, `@theme inline` bridges the `--fx-*` tokens into
utilities), react-arborist (Explorer tree), Vite (build + vitest). The pure-logic
layers (`core/`, `lib/`, `services/`) are framework-free and carry most of the
behavior — the streaming pipeline is imperative DOM.

**Layered structure**: `core/` → `lib/` → `services/` → `hooks/` → `components/` → `mount.tsx`

**State (zustand)**: one store (`core/state.ts` `useFlux`) holds chats, activeChatId,
streaming flags, usage totals, readonly marks, UI prefs, the provider registry
(`providers`) with its probed model catalogs (`providerModels` + `providerProbeErrors`),
the MCP launch list (`mcpServers`) + the forwarded MCP notice bell
(`mcpNotices` + `mcpNoticesUnread`), and the current round's artifacts
(`roundArtifacts` — written by `services/artifacts.ts`, the dock's Round tab reads it).
Components subscribe via
selectors (`useFlux((s) => s.chats)`); the imperative services read/write through
`useFlux.getState` / store actions — no React, no hooks below the component layer.
**`DispatchContext.state` MUST be wired as a getter** — zustand `setState` replaces
the state object, so a snapshot taken at mount would read stale fields forever.

**Dialogs are first-party**: `services/dialogs.ts` exposes promise-shaped `confirmDelete` /
`pickNewChat` / `askQuestion`; `components/dialogs/impl.tsx` registers the UI
implementations at mount (Radix dialogs rendered into a body overlay with their own
React roots; the question renders as an inline card inside the active chat pane).
Tests inject stubs via `setDialogImpls`.

**TopBar** (`components/TopBar.tsx`): the GLOBAL bar — toggle (Menu icon) → brand mark →
spacer → streaming indicator (click = cancel) → MCP notice bell (warning+ log notices,
unread badge, cap 100) → connection (reconnect button when down) →
Settings menu (Providers / MCP / Skills — one dropdown whose items open the one tabbed
SettingsDialog on the chosen section) → theme
toggle (auto/dark/light, persisted in localStorage, applied pre-paint by an inline script
in index.html — no flash). It carries NO chat state and never appears/disappears with the
active chat.

**ChatHeader** (`components/ChatHeader.tsx`): the conversation's header row above the
message column — chat name · kind badge · workdir (mono) → spacer → per-chat UsageStats.
Chat identity lives HERE, not in the TopBar.

**Sidebar**: Radix Tabs styled as a segmented control (Chats/Files — the app's one tab language) + new-chat button + client-side filter
(name/workdir substring) + chat rows (name, kind badge, In-use badge, relative time,
workdir line) + Radix DropdownMenu row actions (rename = inline controlled input,
Enter/blur commit, Esc revert; delete = confirm dialog). Selection = a lease handover
(the activeChatId subscription in mount.tsx sends chat_claim). The MOBILE regime (`<768px`, the one breakpoint) runs
the sidebar as an overlay drawer; desktop drag-resize (160–360px) persists width.
The sidebar's width is AUTHORED in app.css (`#sidebar { width: var(--fx-sidebar-w) }`,
`overflow: hidden`) — tab switches, new chats, or tree loads can never reflow it
(pinned by app.test). During a drag the width
rides the CSS var directly (zero React renders per pointermove); the store commit +
persist happen on pointerup.

**Mobile regime** (`<768px`, the ONE breakpoint — `max-md:` utilities + the app.css
media queries): the sidebar is an overlay drawer (safe-area padded) and the right dock
flips to a FULL-SCREEN sheet (an activity, not a pane — persisted desktop width is
neutralized). Platform chain: `100vh → 100dvh → var(--fx-vvh)` (the visualViewport
height published by `core/viewport.ts` — iOS keyboards overlay the layout viewport, so
dvh alone buries the composer; Chrome Android rides `interactive-widget=resizes-content`),
`viewport-fit=cover` + safe-area insets on the top bar/drawer/dock, and the composer
textarea is 16px on mobile — the ONE deliberate exception to the type scale (iOS zooms
any focused input under 16px). Touch affordances: hover reveals are guarded by the
`touch:` custom variant (`@media (hover: none)`) so row menus / copy buttons stay
visible, and tap targets hold a 36px floor (40px for primary controls; the e2e pins the
geometry at 390×844). PWA: `manifest.webmanifest` + theme-color — NO service worker on
purpose (the app is server-bound; caching only risks stale assets).

**Token usage** (`components/UsageStats.tsx`): four independent stat units
(↑ input · ↓ output · R cache-read · W cache-write) — icons muted, values full
contrast; exact breakdown rides the Radix tooltip.

**Streaming render** (unchanged by the refactor — framework-free): reception and
rendering are decoupled by exactly one animation frame (P0-1) — stream deltas append to
the raw buffer (`body.dataset.raw`) and schedule a coalesced re-render
(`requestAnimationFrame`; `flushRenderNow` sync-flushes at segment boundaries: tool
cards, finalize, dispose). At most ONE incremental render runs per frame regardless
of delta burst size. The incremental pipeline (`lib/render.ts`) keeps per-frame cost
O(new content): `ParagraphSplitter` maintains the committed-parts/tail split
incrementally, and committed paragraphs are append-only DOM (P0-2) — each commit
appends ONE `.stream-part` wrapper, never rebuilding earlier nodes, so highlight.js
runs exactly once per code block.

- **rAF-merged stick-to-bottom follow** (`dom.ts` `scheduleFollow`): deltas schedule at most
 one scroll per paint. A stick state machine gates it: attached = leave-bottom-8px detaches AND
 cancels the pending frame; re-attach = scrolling down into bottom-96px.
- **140ms entry transitions** for newly committed streaming paragraphs
 (`.stream-parts > :last-child` — only the newly appended wrapper animates).
- `renderStableSlice` renders a slice ending inside an unclosed ``` construct as
 escaped text — stable frame-to-frame until the closing ``` arrives.
- Code enhancement (`enhanceHtml` in lib/markdown.ts: highlight, copy button, language
 badge, table-wrap) touches only **committed** `<pre><code>`/`<table>` blocks.
- The live text tail is a plain `.stream-tail` container — the streaming caret is GONE
 (removed CSS-only); streaming state is signaled by the typing indicator, the Stop button,
 and the composer's streaming border.
- `overflow-anchor: none` on `.chat-pane` + `contain: content` on streaming regions.

**Reasoning**: same pipeline and caches (`reasoningCache` reset per reasoning segment);
`closeReasoning` finalizes the block and renames the summary to "Thought for Ns".
Mid-stream reasoning after text creates a new block without discarding the earlier
bubble; text resuming after reasoning (or a tool card) starts a FRESH bubble.

**Tool cards**: `data-tool-call-id` DOM lookup; expand/collapse via
`grid-template-rows` transition; keyboard accessible (Enter/Space on the
`role="button"` header); running cards pulse, completed cards pop once.

**Cancellation**: ChatInput unified Send/Stop button, the TopBar streaming indicator
(click = cancel), and Escape (`useEscapeKey` in App.tsx). Sending while streaming is
the R1 interrupt-send — ONE `SendMessage{interrupt}` RPC: the server fuses "cancel
the live round" and "queue my message" onto one kernel FIFO, so the interject order
holds by construction (no client-side park/flush; the replacement round starts
server-side at the wrap-up). An explicit stop after an interrupt-send kills the
queued turn too ("stop means stop").

**Question walkthrough (inline card)**: `question_required` shows a wait hint in the
pane and mounts the QuestionCard into the active pane (options as buttons + free-form
input); the answer goes back as a single `question_response`. Dismissal resolves with
the neutral `QUESTION_DISMISSED` string.

**Read-only viewer**: another window holding the lease flips the composer into the
viewer bar (reactive off `readonlyChats`) with an explicit Take over button — the
service layer only sets the flag and sends `chat_open` (pinned by handlers.test).

**File explorer**: `components/Explorer.tsx` on react-arborist — nodes addressed by
absolute path (id IS the path), directories lazy-load children via `fs_list` on first
expand (`onToggle`), files open as TABS in the right dock (`RightDock`: the round-
artifacts tab (present only while the current round has artifacts — file rows open
previews, invocation rows pulse their tool card) + multi-file
preview tabs + a pinned Terminal tab; drag-resizable, persisted width; `.md` renders
markdown + Raw toggle, truncated badge carries a size hint; wrap support removed —
file bodies always scroll horizontally). Entries carry
git working-tree status (`fs_listing` 的 `git` 字段；server 每次列目录跑一次
`git status --porcelain`（pathspec 限定被列子树 + `-unormal` 折叠 + blocking 线程池执行），文件直接状态、目录聚合子树——名称着色 + 字母徽标
M/A/U/C，跟随手动/自动刷新更新). A toolbar owns a
manual refresh button + a fixed-cadence auto-refresh (loaded dirs re-list in place,
expansion preserved); listing/read failures surface on the unified toast stack
(`Toasts.tsx`), never inline on rows or panes. Drag-and-drop is disabled.

**Terminal**: interactive shells (e4pty PTY on the server, xterm.js here) over
the dedicated `/ws/term` side-channel WebSocket — binary terminal bytes never
interleave with chat frames. Terminal fonts are bundled (JetBrains Mono + the Nerd
Font Mono patch, OFL-1.1 — `styles/fonts.css` local()-first, icons load on
glyph demand; `scripts/fetch-fonts.sh|.ps1` refreshes them, and also carries the IBM Plex Sans UI face): Added on demand via the dock — its tab strip's “+” or the empty state's New-terminal action — MULTIPLE per chat, never auto-spawned — and kept alive in the background
across tab/chat switches (output keeps buffering). The dock itself has a DIRECT toggle
in ChatHeader (PanelRight, aria-pressed) + Ctrl/Cmd+J; its empty state offers a
New-terminal action so opening it never requires content first. Scoped to the session
identity: a page refresh restores the tabs silently (a closed dock stays closed) and
re-attaches to the same PTYs within the session grace window — the server replays a
256 KiB scrollback ring after the reattach hello, so output written while detached
rebuilds on the client; when the identity is reaped the PTYs die. Each terminal tab
closes independently (kills its PTY); a CLEAN shell exit (code 0) auto-closes
its tab server-notification-style — the PTY is already gone and the exit was
deliberate — while a FAILED shell (non-zero) stays with its exit-code status
line for diagnosis.

**Styles**: `styles/tokens.css` defines the `--fx-*` semantic contract with CSS
`light-dark` (one declaration carries both themes; `color-scheme` +
`[data-theme]` picks) — the palette is DERIVED FROM THE BRAND MARK (the wave's
teal family; dark surfaces live in the tile's world) with a radius hierarchy
(xs 3 / sm 5 / md 8 / lg 10 — pills only for true pills) and a two-voice type
contract (IBM Plex Sans for people, JetBrains Mono for machine facts, applied
ONLY where the content is machine output); `styles/app.css` is the Tailwind entry
whose `@theme inline` maps tokens into utilities (`bg-panel`, `text-muted`, …),
owns the ID-addressed shell layout (`#sidebar-layer`/`#sidebar` width/
`#sidebar-resizer`/backdrop + the <768px drawer media query) — layout rules for ids
Tailwind cannot target live there — and carries the FLUX LINE (the composer's
top-edge sweep, the one non-user-triggered animation, encoding round state);
`styles/stream.css` styles the imperative streaming DOM (bubbles/tool cards with
their status rail/prose/hljs via the `--fx-code-*` voice + the empty-state prompt
card `.fx-empty-*`) which cannot carry utilities. Control primitives in
`components/ui.tsx` own control styling.

**Build toolchain**

| Tool | Purpose |
|------|---------|
| Vite + @vitejs/plugin-react | Bundles `src/main.tsx` → code-split content-hashed chunks: the entry (~149 KB min / ~48 KB gzip) carries FIRST-PARTY code only; always-loaded vendor code rides four stable `manualChunks` groups (react ~196 / rpc ~116 / radix ~96 / markdown ~70 KB), so an app-only deploy re-downloads only the entry; highlight.js ~129 KB chunk prefetched at bootstrap, Files tree ~132 KB chunk on first Files-tab activation, xterm ~329 KB chunk on first terminal creation, Settings dialog ~33 KB chunk on first gear click, one CSS. `dynamic import` + `manualChunks` + `cssCodeSplit: false`; names carry content hashes → the server serves `immutable`. |
| Tailwind v4 (@tailwindcss/vite) | Utility CSS generated at build; tokens bridged via `@theme inline` (no config JS). |
| tsc --noEmit | Type-checks all frontend source (wired into `pnpm run build`). |
| vitest + jsdom + RTL | Unit tests. jsdom gaps are patched in `src/test/setup.ts` (ResizeObserver, PointerEvent, pointer-capture, scrollIntoView). |

**Connection (Connect plane)**: `ConnectConnection` opens the session-scoped
Subscribe stream (stored token rides the open — adoption in the handshake),
translates elements onto the handler vocabulary with the R2 snapshot
reconciliation, and translates ClientMessages onto the ChatService RPCs
(lease-gate statuses synthesize the error frames). Reconnection:
exponential backoff (2s → 30s, 5 retries), `connecting` guard prevents
concurrent connects, pending sends flush after the ready frame; a
half-open connection is detected by the frame deadline (90s — the
server's 30s keepalives) and reopens the stream.

## Trust model (no approvals)

Flux is a no-approval agent: **tools execute without user confirmation**. There is no
approval pipeline, no remembered allow-lists, no fail-closed default. What the old
approval layer contributed beyond prompting survives as decision-free mechanism in the
tool-execution contract :

- **Boundary via `ToolCtx`**: the adapter fills `workdir`/`current_dir` into every
 invocation context; tools resolve path arguments via `ToolCtx::resolve` (in-boundary
 only; escapes, dangling symlinks, and `..` tails are tool errors the model sees and
 self-corrects). No tool schema carries a boundary parameter — forged path arguments
 are structurally inert.
- **Skill reads are name-keyed, not path-keyed**: `skill_read` never takes a filesystem
 path — it addresses a discovered skill by name and resolves the requested file strictly
 inside that skill's directory (containment check, symlink-safe). Global skills live in
 `$HOME/.flux/skills/` — outside the workdir boundary by design (user-installed content,
 the same trust tier as MCP servers launched from the DB).
- **The per-chat workdir boundary** (functional confinement, not security): one server
 serves many projects, so each chat's file/search/shell tools resolve inside the workdir
 carried at `chat_create`. The boundary is read-only state — `set("workdir", …)` is
 refused at the single write point, and `current_dir` is canonicalized inside the
 boundary by the `state_set` tool. An escapable-by-the-model boundary would be no
 boundary.

Real isolation comes from the OS or a container boundary: flux
runs with the permissions of the user account that starts it, treats files writable by
that user as inside the local trust boundary, and considers prompt injection from
repository content an expected local-agent risk. For untrusted repos run flux inside a
container/VM.

## Web UI (the frontend)

`clients/web` IS the frontend;
`crates/flux-server/src/web.rs` serves it. Key contracts:

- **One listener, one process**: everything — the WS upgrade (`/ws`) and the static UI
 (`/` + `/assets/*`) — rides a single axum server (one `--port`). The page connects back
 SAME-ORIGIN to `/ws` (no template injection); the scheme follows the page
 (`wss:` under a TLS proxy). No auth layer: the
 server binds 127.0.0.1 by default; remote exposure = reverse proxy with TLS + its own auth.
- **Asset serving (web.rs)**: delegated to tower-http — `ServeDir` (generic by-name serving,
 MIME, HEAD/405, traversal guard) + `CompressionLayer` (on-the-fly gzip; content-hashed names
 → `Cache-Control: immutable` via `SetResponseHeaderLayer`, so compression cost lands on cold
 loads only). Security headers are crate-native too: CSP + nosniff via two
 `SetResponseHeaderLayer`s over every route and the fallback. `img-src 'self'` is
 load-bearing — the page runs on plain http, without it the same-origin favicon is blocked;
 `manifest-src 'self'` likewise — `default-src` is 'none', so without it the PWA
 webmanifest fetch is blocked.
 `index.html` and `manifest.webmanifest` read per request (`tokio::fs`) and serve `no-store` —
 a rebuild is picked up
 without a restart. No startup validation of the build layout (T-06): an incomplete build
 surfaces at request time — index 500 + a warn log, missing assets 404. The brand icon ships
 as a verbatim build asset (`public/assets/favicon.svg` → `/assets/favicon.svg`, unhashed
 name + immutable cache — fine for a mark whose content never changes).
 The build emits one CSS (`cssCodeSplit: false`) and code-split JS chunks: the first-party entry
 (~149 KB) + four always-loaded vendor groups (react/rpc/radix/markdown, ~196/~116/~96/~70 KB) + a
 highlighter chunk prefetched at bootstrap + the Files-tree chunk (first Files-tab activation)
 + the Settings-dialog chunk (first gear click) + the xterm chunk (~329 KB, first terminal
 creation).
- **No workdir allowlist**: chat creation accepts ANY resolvable directory (no-allowlist trust
 model — the server runs with the starting user's permissions; isolation is the OS/container's
 job). The new-chat dialog browses the filesystem over the Connect surface (`FsList`/`FsRead`,
 `fs_read` → `fs_content`; errors ride the reply frames, never the global error channel).
 `ChatInfo.workdir` is delivered on the wire for the cwd display.
- **File explorer**: the sidebar Files tab — the active chat's workdir tree (react-arborist)
 with lazy `fs_list` loads, manual + fixed-cadence auto refresh, and a right-dock
 `fs_read` preview. The dock is a flex sibling of the chat column: widening it pushes
 the conversation left (overlay only on narrow viewports). `.md` files render through the
 shared markdown pipeline (`renderMarkdown` + `.prose` typography + code-copy chrome) with a
 Raw toggle; the truncated badge carries a size hint (server reports the full `size`).
- **Assets resolution**: `--web-assets-dir` > `web-ui/` next to the binary (packaged) >
 `$HOME/.flux/web-ui` (script-installer layout) > none (headless; no CWD-relative repo
 guess). The web UI is served BY DEFAULT (`--no-web` runs headless); `run-server` builds
 the UI when the dist is missing and pins the repo dist via `--web-assets-dir`.
- Packaging: `scripts/package-web.sh` assembles the servable root; the dist pipeline
 (`dist-workspace.toml` → `.github/workflows/release.yml`, tag-driven) builds each target
 natively on its runner and stages the root as `web-ui/` next to the binary inside every
 archive (`include = ["web-ui/"]`, produced per-runner by `.github/build-setup.yml`).
 Local release-shaped artifacts: `dist build [--target …]`. The shell/powershell
 installers carry BINARIES ONLY (`include` reaches archives, not binary installers —
 upstream #307/#543): script installs run headless (`GET /` 404s, no error). The UI is
 published standalone via extra-artifacts (`flux-web-ui.tar.gz`, built ONCE in the global
 job by `package-web.sh --tar`) for script-installer users to fetch into `~/.flux/web-ui`;
 README documents the two commands.

## Working conventions

- Comments are written in **English**; rationale lives in code comments next to the code it explains (no separate decision/CHANGELOG docs — `docs/decisions.md` records only accepted tradeoffs).
- **`proto/flux/v1` is the single contract source** — the wire is the generated `flux.v1` surface (Rust: `crates/flux-proto`, tonic/prost at build time, OUT_DIR; TS: `clients/web/src/gen`, derived by `buf generate` via the web package's prebuild/pretest hooks — NOT committed, never hand-edit). `buf lint` (STANDARD) is the naming authority (no per-rule exemptions; names change to satisfy the lint, not the reverse); CI re-derives the TS, runs `buf breaking` against origin/main, and checks generation freshness. The frontend's internal handler vocabulary (core/types.ts ServerMessage shapes) is a UI-side adapter fed by core/grpc-connection.ts's translation — protocol changes touch ONLY the proto + the translation/mappers.
- The frontend package manager is **pnpm** (`clients/package.json` pins the exact version via `packageManager`; the scripts self-provision it when missing). npm/node commands run **only under `clients/`** — never at the repo root. pnpm's strict, symlinked layout replaces npm's hoisting: undeclared imports fail at resolve time (no more phantom-dependency/`node_modules`-pollution class of breakage), so anything web code imports must be declared in `clients/web/package.json`. Dependency build scripts are deny-by-default (`allowBuilds` in `clients/pnpm-workspace.yaml` allowlists esbuild + @bufbuild/buf — pnpm 11's build allowlist; the pre-11 name `onlyBuiltDependencies` is inert).
- `vitest` does NOT typecheck — run `pnpm exec tsc --noEmit` (or `pnpm run build`, which typechecks first) after protocol/type changes.
- Large refactors prove equivalence by migrating existing tests unchanged and keeping them green; semantic changes are listed explicitly and pinned by tests, never smuggled in.
- Deletions are justified by zero-consumer evidence (grep + compiler exhaustiveness), not vibes.

## Coding conventions

- Run `cargo fmt` and `cargo clippy --workspace --tests -- -D warnings` before committing.
- `flux-core` owns the agent-runtime contracts — core types and errors, `Tool` trait + registry + `ToolCtx` (with the boundary resolver), kernel ports, and the `Provider` session factory. It is a pure library with zero knowledge of persistence, transport, or provider MANAGEMENT (the registry lives in flux-server; the chat layer receives resolved instances and only calls `begin`). The wire vocabulary is flux-proto's generated `flux.v1` surface; flux-session's router maps WireEvents onto the stream elements (the ONE mapping point), and flux-server only imports it.
- No `#[allow(dead_code)]` without a comment explaining why.
- Frontend: React 19 + TypeScript on Vite; zustand for state; Radix UI primitives (shadcn-style wrappers in `components/ui/`) own dialog/menu/tooltip/tabs behavior; `marked` + `DOMPurify` + highlight.js drive the imperative markdown pipeline; Tailwind v4 for component styling, `styles/stream.css` for the imperative DOM; `components/ui.tsx` primitives own control styling .
- Frontend code is organized by layer — see the layered structure above. Dependencies flow inward.
- Chat IDs: UUID v4 (`uuid` crate).
- Transport: default bind `127.0.0.1`. No application-level auth — remote exposure goes through a reverse proxy with TLS and its own authentication.
