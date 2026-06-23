# Run the local validation suite for the Flux workspace.
$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot

Write-Host "==> Checking formatting..."
cargo fmt --check --manifest-path "$repo\Cargo.toml"

Write-Host "==> Running Clippy..."
cargo clippy --workspace --tests --manifest-path "$repo\Cargo.toml" -- -D warnings

Write-Host "==> Running Rust tests..."
cargo test --workspace --manifest-path "$repo\Cargo.toml"

Write-Host "==> Checking VSCode extension compiles..."
Push-Location "$repo\clients\vscode"
npm run compile
Pop-Location

Write-Host "==> All checks passed."
