# Package the VSCode extension into a .vsix file.
$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..")
$extDir = Join-Path $repo "clients\vscode"

try {
  Write-Host "==> Copying LICENSE into extension package..."
  Copy-Item "$repo\LICENSE" "$extDir\LICENSE" -Force

  Write-Host "==> Installing dependencies..."
  Push-Location $extDir
  npm install

  Write-Host "==> Packaging VSCode extension..."
  npm run package

  Write-Host "==> Packaged VSIX:"
  Get-ChildItem *.vsix
} finally {
  Remove-Item "$extDir\LICENSE" -ErrorAction SilentlyContinue
  Pop-Location
}
