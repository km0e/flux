# Flux — Agent Guide

This file contains project-specific context for AI coding agents working on Flux.

## Project overview

Flux is a general-purpose coding agent framework in Rust with a VSCode extension frontend.
It is built as a Cargo workspace and relies on:

- [`rig-core`](https://github.com/0xPlaygrounds/rig) for the agent loop, LLM providers, and tool abstractions.
- [`schemars`](https://github.com/GREsau/schemars) for tool JSON Schema generation.
- [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk) for MCP client support.
- TypeScript / VSCode API for the frontend.

## Workspace layout

```
flux/
├── crates/
│   ├── flux-tools/     # Built-in Rig tools: file_read, file_write, list_dir, grep, shell
│   ├── flux-mcp/       # MCP client bridge: spawns external MCP servers and exposes their tools
│   └── flux-server/    # JSON-RPC server (stdio / tcp)
│       └── src/
│           ├── main.rs       # CLI entry and mode dispatch
│           ├── config.rs     # TOML configuration loading
│           ├── agent.rs      # Rig agent construction
│           ├── rpc.rs        # JSON-RPC types and request routing
│           ├── transport.rs  # Notifier abstraction + stdio/tcp runners
│           ├── streaming.rs  # chat/stream implementation
│           ├── chat.rs       # chat implementation
│           ├── tools.rs      # tools/list implementation
│           └── state.rs      # per-connection server state
├── clients/vscode/      # VSCode extension
└── docs/
    └── technology_evaluation_report.md   # local reference, not committed
```

## Build & test

Convenience scripts are provided in `scripts/`:

```bash
# Bash (Linux/macOS/Git Bash)
./scripts/build.sh                 # build Rust release + VSCode extension
./scripts/test.sh                  # run fmt check, clippy, tests, VSCode compile
./scripts/test-vscode-e2e.sh       # run the VSCode extension E2E test (requires release server)
./scripts/package-vscode.sh        # package the VSCode extension into a .vsix
./scripts/test-vscode-package.sh   # package + test .vsix in an isolated VSCode window
./scripts/run-server.sh stdio
./scripts/run-server.sh tcp 8080

# PowerShell (Windows)
.\scripts\build.ps1
.\scripts\test.ps1
.\scripts\test-vscode-e2e.ps1
.\scripts\package-vscode.ps1
.\scripts\test-vscode-package.ps1
.\scripts\run-server.ps1 stdio
.\scripts\run-server.ps1 tcp 8080
```

Manual equivalents:

```bash
# Rust workspace
cargo build --release
cargo test --workspace
cargo clippy --workspace --tests -- -D warnings
cargo fmt --check

# Run server manually
cargo run -p flux-server -- stdio
cargo run -p flux-server -- tcp 8080

# With a TOML config file
cargo run -p flux-server -- --config flux.toml stdio

# VSCode extension
cd clients/vscode
npm install
npm run compile
```

## Coding conventions

- Run `cargo fmt` and `cargo clippy --workspace --tests -- -D warnings` before committing.
- Prefer `anyhow::Result` for application code and `thiserror` for library error enums.
- Keep modules small and single-purpose; the server is already split by concern.
- When adding a new JSON-RPC method, update `rpc::handle_request` and document it in `README.md`.
- Do not commit `target/`, `clients/vscode/node_modules/`, `clients/vscode/out/`, or `*.vsix`.
- `Cargo.lock` is tracked because `flux-server` is a binary.

## Adding a built-in tool

1. Define a parameter struct with `#[derive(Deserialize, JsonSchema)]` in `crates/flux-tools/src/`.
2. Implement `rig_core::tool::Tool` for a new struct.
3. Re-export the tool from `crates/flux-tools/src/lib.rs`.
4. Add it to the tool list in `crates/flux-server/src/agent.rs::build_server_state`.

## Adding an LLM provider

Flux currently supports OpenAI / OpenAI-compatible endpoints and Anthropic through `rig-core`.
To add another `rig-core` provider:

1. Add a new variant to `rpc::ProviderParams`.
2. Build the corresponding `rig_core::providers::*::Client` in `agent.rs::build_agent`.
3. Provider selection is server-side only; no VSCode extension enum update is required.

## MCP servers

External MCP servers are configured via the `initialize` request:

```json
{
  "mcpServers": [
    {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/workspace"]
    }
  ]
}
```

`flux-mcp` spawns each server as a child process and converts its tools into `Box<dyn ToolDyn>`.
The `McpSession` handles must stay alive for as long as the agent is used.

## Agent configuration

The agent's system prompt (a.k.a. instructions / preamble) can be set via the top-level `preamble` key in `flux.toml`. If omitted, `agent.rs` falls back to a default coding-assistant prompt. Provider-specific model, API key, `base_url`, and OpenAI `api` mode are configured under `[provider]` in the same file.

## Prompt construction

A single LLM request is assembled by `rig-core` from three sources (see `crates/flux-server/src/agent.rs`):

1. **System prompt / preamble** — set via `AgentBuilder::preamble`. It comes from the top-level `preamble` key in `flux.toml`, or a default coding-assistant prompt.
2. **Conversation history** — a `Vec<Message>` kept in `ServerState`. The current user message is appended, then the agent calls the provider.
3. **Tools** — built-in tools and MCP tools are attached to the agent once at build time. Rig injects their JSON Schema definitions into each provider request and handles any tool-call turns automatically.

The resulting message list looks like:

```text
System:  <preamble>
User:    <history turn 1 user>
Assistant: <history turn 1 assistant>
...
User:    <current message>
```

## VSCode extension

- Main logic: `clients/vscode/src/extension.ts`
- Server connection wrapper: `clients/vscode/src/server.ts`
- Chat UI: opens as a `WebviewPanel` editor tab via `flux.openChat` (title-bar icon or Command Palette).
- Supports two server connection modes:
  - `stdio`: spawns `flux-server` from `target/release/flux-server` (with `.exe` on Windows).
  - `tcp`: connects to a running server via `flux.serverHost` / `flux.serverPort`.
- To test a packaged `.vsix` without reloading your main VSCode window, run `./scripts/test-vscode-package.sh` (or `.ps1`). It installs the extension into an isolated `--extensions-dir` and opens a fresh VSCode window.
- Conversation history is persisted per workspace via `workspaceState` and restored when the panel reopens.
- Chat messages are rendered with `marked` (GFM, tables, code blocks, lists) and sanitized with `DOMPurify`. The UMD bundles are copied from `node_modules` to `media/` during `npm run compile`.

## CI

`.github/workflows/ci.yml` runs on push/PR to `main`:

- Rust: format check, tests, clippy.
- VSCode: `npm ci` + `npm run compile`.

## Common pitfalls

- `rig-core` 0.39 does not expose `Client::new`; use `Client::builder().api_key(...).build()`.
- `ToolDyn` objects require the `ToolDyn` trait in scope for `Box<dyn ToolDyn>` coercion.
- Streaming uses `rig_core::streaming::StreamingChat`; do not call the private `.send()` method — `await` the `StreamingPromptRequest` directly. Pass the session `history` to `stream_chat` so multi-turn context is preserved.
- When spawning an MCP server, `flux-mcp` depends on `rmcp` with `transport-child-process` feature enabled.
