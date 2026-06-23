# Run the Flux server locally.
# Usage: run-server.ps1 [stdio | tcp <port> | -config <path> ...]
$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..")

cargo run --release -p flux-server --manifest-path "$repo\Cargo.toml" -- $args
