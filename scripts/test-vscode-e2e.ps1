# Run the VSCode extension end-to-end test against a release build of flux-server.
$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot

Write-Host "==> Building release server..."
cargo build --release --manifest-path "$repo\Cargo.toml"

Write-Host "==> Running VSCode extension E2E test..."
Push-Location "$repo\clients\vscode"
node test/e2e.js
Pop-Location
