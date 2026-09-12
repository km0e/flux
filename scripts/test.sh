#!/usr/bin/env bash
# Run the local validation suite for the Flux workspace.
set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> Checking formatting..."
cargo fmt --check

echo "==> Running Clippy..."
cargo clippy --workspace --tests -- -D warnings

echo "==> Running Rust tests..."
cargo test --workspace

echo "==> Installing frontend dependencies (npm workspaces)..."
(cd clients && npm install)

echo "==> Proto contract checks (buf lint + TS freshness)..."
export PATH="$(pwd)/clients/node_modules/.bin:$PATH"
buf lint proto
buf generate proto
test -z "$(git status --porcelain clients/web/src/gen)" || {
  echo "generated TS drifted from proto/ — commit or investigate" >&2
  exit 1
}

echo "==> Typechecking the web UI..."
(cd clients/web && npx tsc --noEmit)

echo "==> Running web UI tests (vitest)..."
(cd clients/web && npm test)

echo "==> Building the web UI..."
(cd clients/web && npm run build)

echo "==> All checks passed."
