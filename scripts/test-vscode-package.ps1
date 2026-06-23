# Package the VSCode extension and test the resulting .vsix in an isolated
# VSCode window. This avoids reloading your main VSCode workspace.
$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..")

$code = Get-Command code -ErrorAction SilentlyContinue
if (-not $code) {
  Write-Error "'code' CLI not found in PATH. Install it from VSCode: Ctrl+Shift+P -> 'Shell Command: Install code command in PATH'"
  exit 1
}

Write-Host "==> Packaging extension..."
& "$repo\scripts\package-vscode.ps1"

$vsix = Get-ChildItem "$repo\clients\vscode\*.vsix" | Select-Object -First 1
$extDir = "$repo\.vscode-test\ext"

Write-Host "==> Installing $vsix into isolated extensions dir..."
Remove-Item -Recurse -Force $extDir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $extDir | Out-Null
code --extensions-dir "$extDir" --install-extension "$vsix"

Write-Host "==> Launching VSCode with isolated extensions..."
code --extensions-dir "$extDir" "$repo"
