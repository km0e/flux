# Build and package the browser chat UI.
#
# Usage: package-web.ps1 [-Out <dir>]
#   Builds clients/web -> a self-contained servable root (index.html +
#   assets/*.{js,css} (content-hashed)). Default out: clients/web/dist.
param(
    [string]$Out,
    [string]$Tar
)

$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..")
if (-not $Out) {
    $Out = Join-Path $repo "clients\web\dist"
}

Write-Host "==> Installing client dependencies (pnpm)..."
# Silence pnpm 11's per-invocation ExperimentalWarning (its vendored
# `debug` probes Node's experimental localStorage; newer Node exposes the
# getter by default). Guarded: old Node rejects the flag and keeps the
# warning instead of breaking the run.
node --disable-warning=ExperimentalWarning -e "" 2>$null
if ($LASTEXITCODE -eq 0) {
    $env:NODE_OPTIONS = "$env:NODE_OPTIONS --disable-warning=ExperimentalWarning".Trim()
}
# pnpm is the frontend package manager (see AGENTS.md). Provision it on
# demand; the major tracks the packageManager pin in clients/package.json.
if (-not (Get-Command pnpm -ErrorAction SilentlyContinue)) {
    Write-Host "==> pnpm not found - installing globally via npm"
    $major = node -p "require('$($repo -replace '\\','/')+'/clients/package.json').packageManager.match(/pnpm@(\d+)/)[1]"
    npm install -g "pnpm@$major"
}
Write-Host "==> toolchain: node $(node --version), pnpm $(pnpm --version)"
# Unconditional install: pnpm is store-backed and a no-op when current -
# a stale node_modules (the old skip-if-exists check) hid missing bins.
Push-Location (Join-Path $repo "clients")
pnpm install
Pop-Location

Write-Host "==> Deriving the TS contract (buf generate)..."
# The TS contract is DERIVED, never committed - a clean checkout lacks
# clients/web/src/gen and vite cannot resolve "../gen/..." without it.
# buf lives in web's devDependencies; its bin rides pnpm's package-local
# .bin - verified, not assumed.
$env:PATH = "$repo\clients\web\node_modules\.bin;$env:PATH"
if (-not (Get-Command buf -ErrorAction SilentlyContinue)) {
    Write-Host "buf binary missing after install - is @bufbuild/buf in clients/web/package.json?"
    exit 1
}
Push-Location $repo
buf generate proto
Pop-Location

Write-Host "==> Building web UI (vite)..."
Push-Location (Join-Path $repo "clients\web")
pnpm run build:fast
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

if ($Tar) {
    # Release-asset form: a tarball whose top-level dir is web-ui/ —
    # `tar xz -C ~/.flux` lands it on the server's asset-fallback path.
    $staging = Join-Path ([System.IO.Path]::GetTempPath()) ("flux-web-ui-" + [System.Guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force -Path (Join-Path $staging "web-ui") | Out-Null
    Copy-Item -Recurse -Force (Join-Path $Out "*") (Join-Path $staging "web-ui")
    tar czf $Tar -C $staging web-ui
    $hash = (Get-FileHash $Tar -Algorithm SHA256).Hash.ToLower()
    Set-Content -Path "$Tar.sha256" -Value "$hash  $(Split-Path $Tar -Leaf)"
    Remove-Item -Recurse -Force $staging
    Get-Item $Tar, "$Tar.sha256" | Format-Table Name, Length
}

Write-Host "==> Web UI packaged: $Out"
