#!/usr/bin/env bash
# Build the Flux workspace (Rust release + VSCode extension).
set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> Building Rust workspace (release)..."
cargo build --release

echo "==> Installing/compiling VSCode extension..."
cd clients/vscode
npm install
npm run compile

echo "==> Build complete."
