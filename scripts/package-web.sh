#!/usr/bin/env bash
# Build and package the browser chat UI.
#
# Usage: package-web.sh [--out DIR] [--tar NAME.tar.gz]
#   Builds clients/web (vite) → a self-contained servable root. The artifact
#   is a DIRECTORY (flux-server's static layer serves a directory, not
#   an archive): <out>/ contains index.html + assets/*.{js,css} (content-hashed).
#   Default out: clients/web/dist (the in-repo build location run-server.sh
#   pins via --web-assets-dir). The dist release pipeline stages it as
#   web-ui/ next to the binary inside every archive (include = ["web-ui/"]).
#
#   --tar NAME.tar.gz additionally packs the servable root as a release
#   asset (dist extra-artifacts): a tarball whose top-level dir is web-ui/,
#   so `tar xz -C ~/.flux` lands it on the server's asset-fallback path
#   (~/.flux/web-ui). Emits NAME.tar.gz + NAME.tar.gz.sha256 in the CWD.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
out="$repo/clients/web/dist"
tar_name=""
while [ $# -gt 0 ]; do
    case "$1" in
        --out) out="${2:?--out requires a directory}"; shift 2 ;;
        --tar) tar_name="${2:?--tar requires a filename}"; shift 2 ;;
        *) echo "unknown argument: $1 (usage: package-web.sh [--out DIR] [--tar NAME.tar.gz])" >&2; exit 1 ;;
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

if [ -n "$tar_name" ]; then
    staging="$(mktemp -d)"
    trap 'rm -rf "$staging"' EXIT
    cp -r "$out" "$staging/web-ui"
    tar czf "$tar_name" -C "$staging" web-ui
    (
        cd "$(dirname "$tar_name")" && \
            sha256sum "$(basename "$tar_name")" > "$(basename "$tar_name").sha256"
    )
    ls -lh "$tar_name" "$tar_name.sha256"
fi

echo "==> Web UI packaged: $out"
