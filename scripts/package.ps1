# Package Flux artifacts for distribution.
# Usage: .\scripts\package.ps1 [-Targets <target...>]
#
# Examples:
#   .\scripts\package.ps1
#   .\scripts\package.ps1 -Targets "x86_64-pc-windows-msvc","aarch64-pc-windows-msvc"
param(
    [string[]]$Targets
)

# ── Help ─────────────────────────────────────────────────────────────────
if ($args -contains "-h" -or $args -contains "--help") {
    Write-Host "Usage: .\scripts\package.ps1 [-Targets <target...>]"
    Write-Host "  Build server + web UI -> dist\"
    Write-Host "  No args: build for host target"
    Write-Host "  With -Targets: cross-compile for listed Rust target triples"
    Write-Host ""
    Write-Host "Installed targets:"
    rustup target list --installed 2>$null | ForEach-Object { Write-Host "  $_" }
    Write-Host ""
    Write-Host "Examples:"
    Write-Host "  .\scripts\package.ps1"
    Write-Host "  .\scripts\package.ps1 -Targets 'x86_64-pc-windows-msvc','aarch64-pc-windows-msvc'"
    exit 0
}

$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..")
$dist = Join-Path $repo "dist"

Remove-Item -Recurse -Force $dist -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $dist | Out-Null

if (-not $Targets -or $Targets.Count -eq 0) {
    $hostTarget = (rustc -vV | Select-String "host:" | ForEach-Object { $_ -replace "host:\s*", "" }).Trim()
    $Targets = @($hostTarget)
}

# ── Build server for each target ──
foreach ($target in $Targets) {
    Write-Host "==> Building flux-server for $target..."
    if ($target -like "*-pc-windows-msvc") {
        if (-not (Get-Command cargo-xwin -ErrorAction SilentlyContinue)) {
            Write-Host "Error: 'cargo xwin' is required for MSVC targets. Install it: cargo install cargo-xwin"
            exit 1
        }
        cargo xwin build --release --target "$target" --manifest-path "$repo\Cargo.toml"
    } else {
        cargo build --release --target "$target" --manifest-path "$repo\Cargo.toml"
    }

    if ($target -match "windows") {
        $ext = ".exe"
    } else {
        $ext = ""
    }
    $binName = "flux-server-${target}${ext}"

    Copy-Item "$repo\target\$target\release\flux-server${ext}" "$dist\$binName"
}

# ── Build web UI (served by flux-server's static layer) ──
Write-Host "==> Packaging web UI..."
& "$repo\scripts\package-web.ps1" -Out (Join-Path $dist "web-ui")

Write-Host "==> Done. Artifacts:"
Get-ChildItem $dist
Write-Host "  (web UI served by default - web-ui/ next to the binary is picked up automatically)"
