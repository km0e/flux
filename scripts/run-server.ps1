# Run the Flux server locally.
#
# Usage: run-server.ps1 [-WebAssetsDir <path>] [-WebBuild] [-NoWebBuild]
#                       [--db-path <path>] [server args...]   (e.g. --port / --host / --no-web)
#
# The server has NO config file — every option is a CLI flag, passed
# through verbatim (`flux-server --help` lists them).
#
# Database: ~/.flux/flux.db by default (the binary's global-home default —
# no repo litter anywhere); --db-path overrides.
#
# Web (served BY DEFAULT — the UI rides the SAME listener as /ws):
#   Unless --no-web is passed, the script ensures the UI build exists:
#   builds clients/web via package-web.ps1 when the dist is missing (force
#   with -WebBuild; never build with -NoWebBuild), then pins the repo dist
#   via --web-assets-dir. Skipped when -WebAssetsDir is given (used as-is).
$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..")

$forceBuild = $false
$noBuild = $false
$noWeb = $false
$webAssets = ""

$i = 0
while ($i -lt $args.Count) {
    $arg = $args[$i]
    switch ($arg) {
        { $_ -in @('--web-assets-dir', '-web-assets-dir') } { $webAssets = $args[$i + 1]; $i += 2; continue }
        { $_ -in @('--web-build', '-web-build') } { $forceBuild = $true; $i++; continue }
        { $_ -in @('--no-web-build', '-no-web-build') } { $noBuild = $true; $i++; continue }
        { $_ -in @('--no-web') } { $noWeb = $true; $i++; continue }
        default { $i++; continue }
    }
}
$passthrough = @($args)

if (-not $noWeb -and -not $webAssets) {
    $distIndex = Join-Path $repo "clients\web\dist\index.html"
    if (-not $noBuild -and ($forceBuild -or -not (Test-Path $distIndex))) {
        & "$repo\scripts\package-web.ps1"
    }
    if (Test-Path $distIndex) {
        $passthrough += @('--web-assets-dir', (Join-Path $repo "clients\web\dist"))
    }
}

cargo run --release -p flux-server --manifest-path "$repo\Cargo.toml" -- $passthrough
