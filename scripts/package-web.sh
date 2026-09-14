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

echo "==> Installing client dependencies (npm workspaces)..."
if [ ! -d "$repo/clients/node_modules" ]; then
    (cd "$repo/clients" && npm install)
fi

# The TS contract is DERIVED, never committed (see AGENTS.md) — any clean
# checkout lacks clients/web/src/gen, so derive it here or vite cannot
# resolve "../gen/..." imports. buf resolves the protoc-gen-es plugin via
# PATH: inject the npm-local bin (no global install required anywhere).
echo "==> Deriving the TS contract (buf generate)..."
export PATH="$repo/clients/node_modules/.bin:$PATH"
(cd "$repo" && buf generate proto)

echo "==> Building web UI (vite)..."
(cd "$repo/clients/web" && npm run build:fast)

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
