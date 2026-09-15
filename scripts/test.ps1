# Run the local validation suite for the Flux workspace.
$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot

Write-Host "==> Checking formatting..."
cargo fmt --check --manifest-path "$repo\Cargo.toml"

Write-Host "==> Running Clippy..."
cargo clippy --workspace --tests --manifest-path "$repo\Cargo.toml" -- -D warnings

Write-Host "==> Running Rust tests..."
# Loopback test fixtures must never ride an ambient proxy: with http_proxy
# set system-wide, reqwest's system-proxy routes 127.0.0.1 mocks through
# the proxy, which cannot reach the machine's own loopback and answers
# 502 (the same scope-no_proxy-for-loopback hygiene e2e/ui-check.mjs
# applies to its server child). Non-loopback traffic still uses the proxy.
$env:no_proxy = "127.0.0.1,localhost"; $env:NO_PROXY = "127.0.0.1,localhost"
cargo test --workspace --manifest-path "$repo\Cargo.toml"

Write-Host "==> Installing frontend dependencies (pnpm)..."
# Silence pnpm 11's per-invocation ExperimentalWarning (its vendored
# `debug` probes Node's experimental localStorage; newer Node exposes the
# getter by default). Guarded: old Node rejects the flag and keeps the
# warning instead of breaking the run.
node --disable-warning=ExperimentalWarning -e "" 2>$null
if ($LASTEXITCODE -eq 0) {
    $env:NODE_OPTIONS = "$env:NODE_OPTIONS --disable-warning=ExperimentalWarning".Trim()
}
if (-not (Get-Command pnpm -ErrorAction SilentlyContinue)) {
    Write-Host "==> pnpm not found - installing globally via npm"
    $major = node -p "require('$($repo -replace '\\','/')+'/clients/package.json').packageManager.match(/pnpm@(\d+)/)[1]"
    npm install -g "pnpm@$major"
}
Write-Host "==> toolchain: node $(node --version), pnpm $(pnpm --version)"
Push-Location "$repo\clients"
pnpm install
Pop-Location

Write-Host "==> Typechecking the web UI..."
Push-Location "$repo\clients\web"
pnpm exec tsc --noEmit

Write-Host "==> Running web UI tests (vitest)..."
pnpm test

Write-Host "==> Building the web UI..."
pnpm run build
Pop-Location

Write-Host "==> All checks passed."
