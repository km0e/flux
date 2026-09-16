#!/usr/bin/env bash
# Build the browser chat UI.
#
# Usage: package-web.sh [--out DIR]
#   Builds clients/web (vite) → a self-contained servable root: <out>/
#   contains index.html + assets/*.{js,css,woff2} (content-hashed).
#   Default out: clients/web/dist — the folder flux-server EMBEDS via
#   rust-embed (build.rs invokes THIS script when that dist is missing or
#   stale, so the build procedure has one home). run-server.sh pins the
#   same dist via --web-assets-dir; a --web-assets-dir given to the server
#   is an override layer on top of the embedded bundle, not a replacement.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
out="$repo/clients/web/dist"
while [ $# -gt 0 ]; do
    case "$1" in
        --out) out="${2:?--out requires a directory}"; shift 2 ;;
        *) echo "unknown argument: $1 (usage: package-web.sh [--out DIR])" >&2; exit 1 ;;
    esac
done

echo "==> Installing client dependencies (pnpm)..."
# pnpm 11's vendored `debug` lib probes Node's experimental localStorage on
# startup; newer Node exposes webstorage by default (without a backing
# file), so EVERY node/pnpm invocation prints an ExperimentalWarning.
# Harmless noise — silence it for this script's node processes when the
# Node in use supports the precise disable flag (guarded: old Node
# rejects the option, keeping the warning instead of breaking the run).
if node --disable-warning=ExperimentalWarning -e '' 2>/dev/null; then
    export NODE_OPTIONS="${NODE_OPTIONS:+$NODE_OPTIONS }--disable-warning=ExperimentalWarning"
fi
# pnpm is the frontend package manager (see AGENTS.md). Provision it on
# demand — a fresh machine (new CI runner, new ssh box) needs no manual
# setup; the major tracks the packageManager pin in clients/package.json.
if ! command -v pnpm >/dev/null 2>&1; then
    echo "==> pnpm not found — installing globally via npm"
    pnpm_major="$(node -p 'require(process.argv[1]).packageManager.match(/pnpm@(\d+)/)[1]' \
        "$repo/clients/package.json")"
    npm install -g "pnpm@$pnpm_major"
fi
# The build log self-documents the environment — mismatches (a machine on
# the wrong Node line) are visible without any further digging.
echo "==> toolchain: node $(node --version), pnpm $(pnpm --version)"
# Unconditional install: pnpm is store-backed and a no-op when current —
# and a stale/partial node_modules (the old skip-if-dir-exists check) is
# exactly how `buf: command not found` happened on a fresh machine.
(cd "$repo/clients" && pnpm install)

# The TS contract is DERIVED, never committed (see AGENTS.md) — any clean
# checkout lacks clients/web/src/gen, so derive it here or vite cannot
# resolve "../gen/..." imports. buf lives in web's devDependencies; its
# bin rides pnpm's package-local .bin — verified, not assumed.
echo "==> Deriving the TS contract (buf generate)..."
export PATH="$repo/clients/web/node_modules/.bin:$PATH"
command -v buf >/dev/null 2>&1 || {
    echo "buf binary missing after install — is @bufbuild/buf in clients/web/package.json?" >&2
    exit 1
}
(cd "$repo" && buf generate proto)

echo "==> Building web UI (vite)..."
(cd "$repo/clients/web" && pnpm run build:fast)

if [ "$out" != "$repo/clients/web/dist" ]; then
    echo "==> Assembling servable root at $out..."
    rm -rf "$out"
    mkdir -p "$(dirname "$out")"
    cp -r "$repo/clients/web/dist" "$out"
fi

# Sanity: the server's static layer requires index.html + a JS + a CSS asset.
test -f "$out/index.html"
# The build emits content-hashed names — check by kind, not by name.
ls "$out"/assets/*.js >/dev/null 2>&1
ls "$out"/assets/*.css >/dev/null 2>&1

# Version stamp: flux-server's startup check compares a DISK override dir's
# stamp against the running binary (CARGO_PKG_VERSION) and warns on a
# mismatch — an override dir outlives binaries and its files shadow the
# embedded bundle, so a stale dir means a stale UI. The embedded bundle
# needs no stamp (built with the binary, cannot mismatch); a dev dist (pnpm
# build without this script) carries none and nothing is checked.
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml" | head -1)"
printf '%s' "$version" > "$out/web-ui-version.txt"

echo "==> Web UI built: $out"
