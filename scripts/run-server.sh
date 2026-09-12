#!/usr/bin/env bash
# Run the Flux server locally.
#
# Usage: run-server.sh [--web-assets-dir PATH] [--web-build] [--no-web-build]
#                      [--db-path PATH] [server args...]   (e.g. --port / --host / --no-web)
#
# The server has NO config file — every option is a CLI flag, passed
# through verbatim (`flux-server --help` lists them).
#
# Database: ~/.flux/flux.db by default (the binary's global-home default —
# no repo litter anywhere); --db-path overrides.
#
# Web (served BY DEFAULT — the UI rides the SAME listener as /ws):
#   Unless --no-web is passed, the script ensures the UI build exists:
#   builds clients/web via package-web.sh when the dist is missing (force a
#   rebuild with --web-build; never build with --no-web-build), then pins
#   the repo dist via --web-assets-dir. Skipped when --web-assets-dir is
#   given (used as-is).
set -euo pipefail

cd "$(dirname "$0")/.."
repo="$PWD"

no_web=false
force_build=false
no_build=false
web_assets=""

i=1
args=("$@")
while [ $i -le ${#args[@]} ]; do
    arg="${args[$((i-1))]}"
    case "$arg" in
        --web-assets-dir)
            web_assets="${args[$i]:-}"
            i=$((i+1))
            ;;
        --web-build)
            force_build=true
            ;;
        --no-web-build)
            no_build=true
            ;;
        --no-web)
            no_web=true
            ;;
    esac
    i=$((i+1))
done
passthrough=("${args[@]}")

# ── Web UI assets: build when missing, unless disabled or already pinned ──
if ! $no_web && [ -z "$web_assets" ]; then
    if ! $no_build && { $force_build || [ ! -f clients/web/dist/index.html ]; }; then
        ./scripts/package-web.sh
    fi
    if [ -f clients/web/dist/index.html ]; then
        passthrough+=("--web-assets-dir" "$repo/clients/web/dist")
    fi
fi

cargo run --release -p flux-server -- "${passthrough[@]}"
