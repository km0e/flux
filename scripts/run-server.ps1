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
# Web (served BY DEFAULT — the UI rides the SAME listener as /ws and is
# EMBEDDED in the binary):
#   The embedded bundle follows the repo dist automatically (flux-server's
#   build.rs rebuilds it when stale). Unless --no-web is passed, this
#   script additionally pre-builds clients/web via package-web.ps1 when the
#   dist is missing (force with -WebBuild; never with -NoWebBuild) and pins
#   the repo dist via --web-assets-dir, so a plain `cargo build --release`
#   binary serves the FRESH UI immediately instead of waiting for the
#   embed. Skipped when -WebAssetsDir is given (used as-is).
#   -WebBuild/-NoWebBuild are SCRIPT options — consumed here, never passed
#   to the binary.
$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..")

$forceBuild = $false
$noBuild = $false
$noWeb = $false
$webAssets = ""

# Arg extraction: -WebBuild/-NoWebBuild are SCRIPT options — consumed here,
# never passed to the binary (a leak made flux-server die on an unknown
# flag). --no-web and --web-assets-dir are SERVER flags the script also
# inspects; they pass through verbatim in both space and = forms (the = form
# must be recognized, or the script would append a second --web-assets-dir
# and clap's last-wins would shadow the user's value).
$passthrough = @()
$i = 0
while ($i -lt $args.Count) {
    $arg = $args[$i]
    if ($arg -in @('--web-assets-dir', '-web-assets-dir')) {
        if ($i + 1 -lt $args.Count) {
            $webAssets = $args[$i + 1]
            $passthrough += @($arg, $args[$i + 1]); $i += 2
        } else { $passthrough += $arg; $i++ }
        continue
    }
    if ($arg -like '--web-assets-dir=*' -or $arg -like '-web-assets-dir=*') {
        $webAssets = ($arg -split '=', 2)[1]
        $passthrough += $arg; $i++; continue
    }
    if ($arg -in @('--web-build', '-web-build')) { $forceBuild = $true; $i++; continue }
    if ($arg -in @('--no-web-build', '-no-web-build')) { $noBuild = $true; $i++; continue }
    if ($arg -in @('--no-web', '-no-web')) { $noWeb = $true; $passthrough += $arg; $i++; continue }
    $passthrough += $arg; $i++
}

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
