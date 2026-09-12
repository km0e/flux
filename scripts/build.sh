#!/usr/bin/env bash
# Build the Flux workspace (Rust release + web UI).
set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> Building Rust workspace (release)..."
cargo build --release

echo "==> Building web UI (clients/web)..."
(cd clients && npm install)
(cd clients/web && npm run build)

echo "==> Build complete."
