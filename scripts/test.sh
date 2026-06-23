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

echo "==> Checking VSCode extension compiles..."
cd clients/vscode
npm run compile

echo "==> All checks passed."
