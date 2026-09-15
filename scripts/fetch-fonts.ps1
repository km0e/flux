# Refresh the bundled fonts from their official releases.
#
# All families are OFL-1.1 (see clients\web\src\assets\fonts\LICENSE.md);
# the files are COMMITTED, so this script is only for upgrades - the build
# never fetches anything.
#
#   IBM Plex Sans v1.1.0           (woff2, complete faces - the UI voice)
#   JetBrains Mono v2.304          (woff2, base face + box-drawing)
#   JetBrainsMono Nerd Font v3.4.0 (TTF upstream - the NF release ships no
#                                   woff2; converted here to woff2, the
#                                   icon range prompts use)
#
# The NF conversion needs `woff2_compress` (google woff2 tool) on PATH.
#
# Usage: .\scripts\fetch-fonts.ps1
$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..")
$fonts = Join-Path $repo "clients\web\src\assets\fonts"

$PlexVer = "1.1.0"
$JbmVer = "2.304"
$NfVer = "3.4.0"
$tmp = New-Item -ItemType Directory -Force -Path (Join-Path $env:TEMP "flux-fonts")
try {
    Write-Host "==> IBM Plex Sans v$PlexVer"
    $plexZip = Join-Path $tmp "plex.zip"
    Invoke-WebRequest -Uri "https://github.com/IBM/plex/releases/download/%40ibm%2Fplex-sans%40$PlexVer/ibm-plex-sans.zip" -OutFile $plexZip
    $plexDir = Join-Path $tmp "plex"
    Expand-Archive -Path $plexZip -DestinationPath $plexDir -Force
    foreach ($w in @("Regular", "Medium", "SemiBold", "Italic")) {
        Copy-Item (Join-Path $plexDir "ibm-plex-sans\fonts\complete\woff2\IBMPlexSans-$w.woff2") $fonts -Force
    }

    Write-Host "==> JetBrains Mono v$JbmVer"
    $jbmZip = Join-Path $tmp "jbm.zip"
    Invoke-WebRequest -Uri "https://github.com/JetBrains/JetBrainsMono/releases/download/v$JbmVer/JetBrainsMono-$JbmVer.zip" -OutFile $jbmZip
    $jbmDir = Join-Path $tmp "jbm"
    Expand-Archive -Path $jbmZip -DestinationPath $jbmDir -Force
    foreach ($w in @("Regular", "Bold", "Italic")) {
        Copy-Item (Join-Path $jbmDir "fonts\webfonts\JetBrainsMono-$w.woff2") $fonts -Force
    }

    Write-Host "==> JetBrainsMono Nerd Font Mono v$NfVer"
    $nfZip = Join-Path $tmp "nf.zip"
    Invoke-WebRequest -Uri "https://github.com/ryanoasis/nerd-fonts/releases/download/v$NfVer/JetBrainsMono.zip" -OutFile $nfZip
    $nfDir = Join-Path $tmp "nf"
    Expand-Archive -Path $nfZip -DestinationPath $nfDir -Force

    # Convert the NF TTF to woff2 (2.4 MB -> ~1.0 MB). The NF release ships
    # no woff2, so the conversion happens here - once per upgrade, committed.
    $woff2Compress = Get-Command woff2_compress -ErrorAction SilentlyContinue
    if (-not $woff2Compress) {
        Write-Error "woff2_compress not found on PATH (winget/brew install woff2, or apt install woff2 under WSL)"
        throw "woff2_compress missing"
    }
    & $woff2Compress.Source (Join-Path $nfDir "JetBrainsMonoNerdFontMono-Regular.ttf")
    if ($LASTEXITCODE -ne 0) { throw "woff2_compress failed" }
    Remove-Item (Join-Path $fonts "JetBrainsMonoNerdFontMono-Regular.ttf") -Force -ErrorAction SilentlyContinue
    Copy-Item (Join-Path $nfDir "JetBrainsMonoNerdFontMono-Regular.woff2") $fonts -Force

    Write-Host "==> Done (committed files - run git status to see what changed):"
    Get-ChildItem $fonts
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
