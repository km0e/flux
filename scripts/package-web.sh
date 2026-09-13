#!/usr/bin/env bash
# Build and package the browser chat UI.
#
# Usage: package-web.sh [--out DIR]
#   Builds clients/web (vite) → a self-contained servable root. The artifact
#   is a DIRECTORY (flux-server's static layer serves a directory, not
#   an archive): <out>/ contains index.html + assets/*.{js,css} (content-hashed).
#   Default out: clients/web/dist (the in-repo build location run-server.sh
#   pins via --web-assets-dir). The dist release pipeline stages it as
#   web-ui/ next to the binary inside every archive (include = ["web-ui/"]).
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
out="$repo/clients/web/dist"
if [ "${1:-}" = "--out" ]; then
    out="${2:?--out requires a directory}"
fi

echo "==> Installing client dependencies (npm workspaces)..."
if [ ! -d "$repo/clients/node_modules" ]; then
    (cd "$repo/clients" && npm install)
fi

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

echo "==> Web UI packaged: $out"
