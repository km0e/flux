# Build the Flux workspace (Rust release + web UI).
$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot

Write-Host "==> Building Rust workspace (release)..."
cargo build --release --manifest-path "$repo\Cargo.toml"

Write-Host "==> Building web UI (clients/web)..."
Push-Location "$repo\clients"
npm install
Pop-Location
Push-Location "$repo\clients\web"
npm run build
Pop-Location

Write-Host "==> Build complete."
