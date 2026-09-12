# Build and package the browser chat UI.
#
# Usage: package-web.ps1 [-Out <dir>]
#   Builds clients/web -> a self-contained servable root (index.html +
#   assets/*.{js,css} (content-hashed)). Default out: clients/web/dist.
param(
    [string]$Out
)

$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..")
if (-not $Out) {
    $Out = Join-Path $repo "clients\web\dist"
}

Write-Host "==> Installing client dependencies (npm workspaces)..."
if (-not (Test-Path (Join-Path $repo "clients\node_modules"))) {
    Push-Location (Join-Path $repo "clients")
    npm install
    Pop-Location
}

Write-Host "==> Building web UI (vite)..."
Push-Location (Join-Path $repo "clients\web")
npm run build:fast
Pop-Location

if ((Resolve-Path $Out).Path -ne (Resolve-Path (Join-Path $repo "clients\web\dist")).Path) {
    Write-Host "==> Assembling servable root at $Out..."
    if (Test-Path $Out) { Remove-Item -Recurse -Force $Out }
    New-Item -ItemType Directory -Force -Path (Split-Path $Out) | Out-Null
    Copy-Item -Recurse (Join-Path $repo "clients\web\dist") $Out
}

# Sanity: the server's static layer requires index.html + a JS + a CSS asset.
# The build emits content-hashed names - check by kind, not by name.
if (-not (Test-Path (Join-Path $Out "index.html")) -or
    -not (Get-ChildItem "$Out\assets" -Filter *.js -ErrorAction SilentlyContinue) -or
    -not (Get-ChildItem "$Out\assets" -Filter *.css -ErrorAction SilentlyContinue)) {
    Write-Host "Error: packaged web UI is incomplete (missing index.html or assets)"
    exit 1
}

Write-Host "==> Web UI packaged: $Out"
