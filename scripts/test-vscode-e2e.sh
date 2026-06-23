#!/usr/bin/env bash
# Run the VSCode extension end-to-end test against a release build of flux-server.
set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> Building release server..."
cargo build --release

echo "==> Running VSCode extension E2E test..."
cd clients/vscode
node test/e2e.js
