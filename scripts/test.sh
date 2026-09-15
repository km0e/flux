#!/usr/bin/env bash
# Run the local validation suite for the Flux workspace.
set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> Checking formatting..."
cargo fmt --check

echo "==> Running Clippy..."
cargo clippy --workspace --tests -- -D warnings

echo "==> Running Rust tests..."
# Loopback test fixtures must never ride an ambient proxy: with http_proxy
# set system-wide, reqwest's system-proxy routes 127.0.0.1 mocks through
# the proxy, which cannot reach the machine's own loopback and answers
# 502 (the same scope-no_proxy-for-loopback hygiene e2e/ui-check.mjs
# applies to its server child). Non-loopback traffic still uses the proxy.
export no_proxy="127.0.0.1,localhost" NO_PROXY="127.0.0.1,localhost"
cargo test --workspace

echo "==> Installing frontend dependencies (pnpm)..."
# Silence pnpm 11's per-invocation ExperimentalWarning (its vendored
# `debug` probes Node's experimental localStorage; newer Node exposes the
# getter by default). Guarded: old Node rejects the flag and keeps the
# warning instead of breaking the run.
if node --disable-warning=ExperimentalWarning -e '' 2>/dev/null; then
    export NODE_OPTIONS="${NODE_OPTIONS:+$NODE_OPTIONS }--disable-warning=ExperimentalWarning"
fi
if ! command -v pnpm >/dev/null 2>&1; then
    echo "==> pnpm not found — installing globally via npm"
    pnpm_major="$(node -p 'require(process.argv[1]).packageManager.match(/pnpm@(\d+)/)[1]' \
        ./clients/package.json)"
    npm install -g "pnpm@$pnpm_major"
fi
echo "==> toolchain: node $(node --version), pnpm $(pnpm --version)"
(cd clients && pnpm install)

echo "==> Proto contract checks (buf lint + TS freshness)..."
export PATH="$(pwd)/clients/web/node_modules/.bin:$PATH"
command -v buf >/dev/null 2>&1 || {
    echo "buf binary missing after install — is @bufbuild/buf in clients/web/package.json?" >&2
    exit 1
}
buf lint proto
buf generate proto
test -z "$(git status --porcelain clients/web/src/gen)" || {
  echo "generated TS drifted from proto/ — commit or investigate" >&2
  exit 1
}

echo "==> Typechecking the web UI..."
(cd clients/web && pnpm exec tsc --noEmit)

echo "==> Running web UI tests (vitest)..."
(cd clients/web && pnpm test)

echo "==> Building the web UI..."
(cd clients/web && pnpm run build)

echo "==> All checks passed."
