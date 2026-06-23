#!/usr/bin/env bash
# Package the VSCode extension into a .vsix file.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
ext_dir="$repo/clients/vscode"

cleanup() {
  rm -f "$ext_dir/LICENSE"
}
trap cleanup EXIT

echo "==> Copying LICENSE into extension package..."
cp "$repo/LICENSE" "$ext_dir/LICENSE"

echo "==> Installing dependencies..."
cd "$ext_dir"
npm install

echo "==> Packaging VSCode extension..."
npm run package

echo "==> Packaged VSIX:"
ls -1 *.vsix
