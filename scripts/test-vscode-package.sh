#!/usr/bin/env bash
# Package the VSCode extension and test the resulting .vsix in an isolated
# VSCode window. This avoids reloading your main VSCode workspace.
set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v code &>/dev/null; then
  echo "Error: 'code' CLI not found in PATH."
  echo "Install it from VSCode: Ctrl+Shift+P -> 'Shell Command: Install code command in PATH'"
  exit 1
fi

echo "==> Packaging extension..."
./scripts/package-vscode.sh

vsix=$(ls -1 clients/vscode/*.vsix | head -n1)
ext_dir="$PWD/.vscode-test/ext"

echo "==> Installing $vsix into isolated extensions dir..."
rm -rf "$ext_dir"
mkdir -p "$ext_dir"
code --extensions-dir "$ext_dir" --install-extension "$vsix"

echo "==> Launching VSCode with isolated extensions..."
code --extensions-dir "$ext_dir" .
