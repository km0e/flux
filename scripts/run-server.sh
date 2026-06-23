#!/usr/bin/env bash
# Run the Flux server locally.
# Usage: run-server.sh [stdio | tcp <port> | --config <path> ...]
set -euo pipefail

cd "$(dirname "$0")/.."

cargo run --release -p flux-server -- "$@"
