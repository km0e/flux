# Build the Flux workspace (Rust release + VSCode extension).
$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot

Write-Host "==> Building Rust workspace (release)..."
cargo build --release --manifest-path "$repo\Cargo.toml"

Write-Host "==> Installing/compiling VSCode extension..."
Push-Location "$repo\clients\vscode"
npm install
npm run compile
Pop-Location

Write-Host "==> Build complete."
