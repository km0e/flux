# Run the local validation suite for the Flux workspace.
$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot

Write-Host "==> Checking formatting..."
cargo fmt --check --manifest-path "$repo\Cargo.toml"

Write-Host "==> Running Clippy..."
cargo clippy --workspace --tests --manifest-path "$repo\Cargo.toml" -- -D warnings

Write-Host "==> Running Rust tests..."
cargo test --workspace --manifest-path "$repo\Cargo.toml"

Write-Host "==> Installing frontend dependencies (npm workspaces)..."
Push-Location "$repo\clients"
npm install
Pop-Location

Write-Host "==> Typechecking the web UI..."
Push-Location "$repo\clients\web"
npx tsc --noEmit

Write-Host "==> Running web UI tests (vitest)..."
npm test

Write-Host "==> Building the web UI..."
npm run build
Pop-Location

Write-Host "==> All checks passed."
