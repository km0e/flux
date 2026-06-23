# Flux

A general-purpose coding agent framework in Rust with a VSCode extension frontend.
It is built on top of [`rig-core`](https://github.com/0xPlaygrounds/rig) and uses
stdio JSON-RPC to communicate between the Rust backend and the VSCode panel.

## Features

- **Built-in tools**: `file_read`, `file_write`, `list_dir`, `grep`, `shell`.
- **LLM providers**: OpenAI / OpenAI-compatible endpoints and Anthropic, configured
  via JSON-RPC `initialize`.
- **Streaming**: `chat/stream` returns incremental assistant text through JSON-RPC
  notifications.
- **MCP client**: launch external [MCP](https://modelcontextprotocol.io) servers as
  child processes and expose their tools to the agent.
- **Conversation history**: server-side session history plus per-workspace persistence in the VSCode extension.
- **Markdown rendering**: assistant replies are rendered with `marked` and sanitized with `DOMPurify`.
- **VSCode extension**: chat panel as an editor tab with MCP server configuration.

## Project layout

```
flux/
├── crates/
│   ├── flux-tools/     # Built-in Rig tools (file, shell, search)
│   ├── flux-mcp/       # MCP client bridge for Rig
│   └── flux-server/    # JSON-RPC server (stdio / tcp)
│       └── src/
│           ├── main.rs       # CLI entry
│           ├── config.rs     # TOML configuration loading
│           ├── agent.rs      # Rig agent construction
│           ├── rpc.rs        # JSON-RPC protocol & routing
│           ├── transport.rs  # stdio / tcp transport
│           ├── streaming.rs  # chat/stream handling
│           ├── chat.rs       # chat handling
│           ├── tools.rs      # tools/list handling
│           └── state.rs      # per-connection server state
├── clients/
│   └── vscode/          # VSCode extension
├── docs/
│   └── technology_evaluation_report.md
└── README.md
```

## Build

Convenience scripts are provided for both Unix and Windows:

```bash
# Bash (Linux/macOS/Git Bash)
./scripts/build.sh

# PowerShell (Windows)
.\scripts\build.ps1
```

### Rust workspace

```bash
cargo build --release
```

The server binary is produced at `target/release/flux-server`.

### VSCode extension

```bash
cd clients/vscode
npm install
npm run compile
```

## Run

### Network (TCP) mode

For easier Windows extension testing or remote deployments, `flux-server` can
listen on a TCP port instead of stdio:

```bash
./target/release/flux-server tcp 8080
# or during development
cargo run -p flux-server -- tcp 8080
```

Then connect with any TCP client (e.g. `nc`, `telnet`, or the VSCode extension
in `tcp` mode) and send newline-delimited JSON-RPC messages.

### Configuration file

`flux-server` can load defaults from a TOML config file. The file is resolved in this order:

1. `--config <path>` CLI argument
2. `FLUX_CONFIG` environment variable
3. `flux.toml` in the current working directory

A sample is provided at `config/config.example.toml`. Copy it to `flux.toml` in the project root (which is `.gitignore`d) and fill in your API key:

```bash
cp config/config.example.toml flux.toml
```

Example `flux.toml`:

```toml
[server]
mode = "stdio"    # or "tcp"
port = 8080

[provider]
name = "openai"   # or "anthropic"
model = "gpt-4o-mini"
# base_url = "https://api.openai.com/v1"  # optional
api_key = "sk-..."

workdir = "/path/to/workspace"

# Optional system prompt / instructions sent to the agent on every request.
# If omitted, a default coding-assistant prompt is used.
# preamble = "You are a senior Rust engineer. Be concise and use the provided tools when needed."

[[mcp_servers]]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/workspace"]
```

Values in `flux.toml` take precedence over client-provided JSON-RPC `initialize` parameters, so the file is the recommended place for project-wide provider and model settings.

To see server logs, set `RUST_LOG`:

```bash
RUST_LOG=info ./scripts/run-server.sh stdio
```

### Manual stdio test

```bash
./target/release/flux-server stdio
# or during development
cargo run -p flux-server -- stdio

# with a config file
./target/release/flux-server --config flux.toml stdio
```

Then send newline-delimited JSON-RPC messages, for example:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"provider":{"name":"openai","model":"gpt-4o-mini","apiKey":"sk-..."}}}
{"jsonrpc":"2.0","id":2,"method":"chat","params":{"message":"hello"}}
{"jsonrpc":"2.0","id":3,"method":"tools/list"}
```

### With MCP servers

Add a list of MCP server configs to `initialize`:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "initialize",
  "params": {
    "provider": {
      "name": "openai",
      "model": "gpt-4o-mini",
      "apiKey": "sk-..."
    },
    "mcpServers": [
      {
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/workspace"]
      }
    ]
  }
}
```

### Agent configuration

The agent is configured through the `[provider]` table in `flux.toml` (or via `initialize` when no server config is set):

- `name` — provider family: `"openai"` (also used for OpenAI-compatible endpoints such as DeepSeek) or `"anthropic"`.
- `model` — model identifier, e.g. `"gpt-4o-mini"`, `"claude-3-5-sonnet-20240620"`, `"deepseek-chat"`.
- `api_key` — provider API key. Can also be supplied through the standard environment variables (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`).
- `base_url` *(OpenAI only)* — custom API base URL. Required for OpenAI-compatible endpoints such as DeepSeek.
- `api` *(OpenAI only)* — endpoint family:
  - `"completions"` (default) uses `/chat/completions` and works with OpenAI and most compatible endpoints.
  - `"responses"` uses OpenAI's native `/responses` endpoint.
- `preamble` *(top-level)* — system prompt / instructions sent to the agent on every request. If omitted, a default coding-assistant prompt is used.

### Conversation history

The server keeps a per-session message history in `ServerState.history`. Both `chat` and `chat/stream` append each turn to this history, so the model retains context across multiple turns.

The VSCode extension also persists the displayed conversation per workspace in its `workspaceState`. When the chat panel is reopened, previous messages are restored and sent back to the server in the `history` field of `chat/stream` requests, so context survives server restarts.

### Streaming chat

Request:

```json
{"jsonrpc":"2.0","id":2,"method":"chat/stream","params":{"message":"hello","stream_id":"abc"}}
```

The server responds immediately with:

```json
{"jsonrpc":"2.0","id":2,"result":{"accepted":true,"stream_id":"abc"}}
```

Then it emits notifications:

```json
{"jsonrpc":"2.0","method":"stream/chunk","params":{"stream_id":"abc","delta":"..."}}
{"jsonrpc":"2.0","method":"stream/done","params":{"stream_id":"abc","content":"..."}}
```

### VSCode extension

1. Build the server: `cargo build --release`.
2. Install the packaged `.vsix` from `clients/vscode/` via the Command Palette: **Extensions: Install from VSIX**.
3. Click the **Flux** chat icon in the editor title bar (top-right) to open Flux Chat as a new editor tab. You can also run the **Open Flux Chat** command from the Command Palette.

If you want to test a freshly packaged `.vsix` without reloading your main VSCode window, use the isolated test script (see [Development](#development)).

Configure the server connection:

- `flux.serverMode`: `"stdio"` (spawn local binary) or `"tcp"` (connect to a running server)
- `flux.serverPath`: path to `flux-server` binary when using `stdio`
- `flux.serverHost` / `flux.serverPort`: TCP host/port when using `tcp`

Configure optional MCP servers:

- `flux.mcpServers`: list of `{ command, args?, env? }` objects

LLM provider / API key is configured server-side via `flux.toml`, not in the extension settings.

## Development

Use the convenience test script to run the full local validation suite:

```bash
# Bash (Linux/macOS/Git Bash)
./scripts/test.sh
./scripts/test-vscode-e2e.sh      # extension E2E test against a release server
./scripts/package-vscode.sh       # package the extension into a .vsix
./scripts/test-vscode-package.sh  # package + test .vsix in an isolated VSCode window

# PowerShell (Windows)
.\scripts\test.ps1
.\scripts\test-vscode-e2e.ps1
.\scripts\package-vscode.ps1
.\scripts\test-vscode-package.ps1
```

Manual equivalents:

```bash
# Format check
cargo fmt --check

# Run all Rust tests
cargo test --workspace

# Lint
cargo clippy --workspace --tests -- -D warnings

# VSCode extension
cd clients/vscode
npm install
npm run compile
```

### Tests

- Rust integration tests: `crates/flux-tools/tests/fs_tools.rs`
- VSCode extension E2E test: `clients/vscode/test/e2e.js`

## Roadmap

- [x] Workspace migration to `rig-core`
- [x] Built-in Rig tools
- [x] Rig-based agent with tool-call loop
- [x] stdio JSON-RPC server
- [x] VSCode extension chat panel
- [x] Streaming responses
- [x] MCP client support
- [x] Memory / conversation history
- [ ] Multi-agent orchestration (future)
