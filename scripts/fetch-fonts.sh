#!/usr/bin/env bash
# Refresh the bundled fonts from their official releases.
#
# All families are OFL-1.1 (see clients/web/src/assets/fonts/LICENSE.md);
# the files are COMMITTED, so this script is only for upgrades — the build
# never fetches anything.
#
#   IBM Plex Sans v1.1.0           (woff2, complete faces — the UI voice)
#   JetBrains Mono v2.304          (woff2, base face + box-drawing)
#   JetBrainsMono Nerd Font v3.4.0 (TTF upstream — the NF release ships no
#                                   woff2; converted here to woff2, the
#                                   icon range prompts use)
#
# The NF conversion needs `woff2_compress` (the google woff2 tool — apt
# `woff2` / brew `woff2`) or python fontTools+brotli. One of them must be
# on PATH; without either the NF face cannot be refreshed.
#
# Usage: fetch-fonts.sh
set -euo pipefail
cd "$(dirname "$0")/.."

FONTS="clients/web/src/assets/fonts"
PLEX_VER="1.1.0"
JBM_VER="2.304"
NF_VER="3.4.0"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "==> IBM Plex Sans v${PLEX_VER}"
curl -fsSL -o "$tmp/plex.zip" \
    "https://github.com/IBM/plex/releases/download/%40ibm%2Fplex-sans%40${PLEX_VER}/ibm-plex-sans.zip"
unzip -o -q "$tmp/plex.zip" \
    "ibm-plex-sans/fonts/complete/woff2/IBMPlexSans-Regular.woff2" \
    "ibm-plex-sans/fonts/complete/woff2/IBMPlexSans-Medium.woff2" \
    "ibm-plex-sans/fonts/complete/woff2/IBMPlexSans-SemiBold.woff2" \
    "ibm-plex-sans/fonts/complete/woff2/IBMPlexSans-Italic.woff2" \
    -d "$tmp/plex"
cp "$tmp"/plex/ibm-plex-sans/fonts/complete/woff2/*.woff2 "$FONTS/"

echo "==> JetBrains Mono v${JBM_VER}"
curl -fsSL -o "$tmp/jbm.zip" \
    "https://github.com/JetBrains/JetBrainsMono/releases/download/v${JBM_VER}/JetBrainsMono-${JBM_VER}.zip"
unzip -o -q "$tmp/jbm.zip" \
    "fonts/webfonts/JetBrainsMono-Regular.woff2" \
    "fonts/webfonts/JetBrainsMono-Bold.woff2" \
    "fonts/webfonts/JetBrainsMono-Italic.woff2" \
    -d "$tmp/jbm"
cp "$tmp"/jbm/fonts/webfonts/*.woff2 "$FONTS/"

echo "==> JetBrainsMono Nerd Font Mono v${NF_VER}"
curl -fsSL -o "$tmp/nf.zip" \
    "https://github.com/ryanoasis/nerd-fonts/releases/download/v${NF_VER}/JetBrainsMono.zip"
unzip -o -q "$tmp/nf.zip" "JetBrainsMonoNerdFontMono-Regular.ttf" -d "$tmp/nf"

# Convert the NF TTF to woff2 (2.4 MB → ~1.0 MB). The NF release ships no
# woff2, so the conversion happens here — once per upgrade, committed.
if command -v woff2_compress >/dev/null 2>&1; then
    woff2_compress "$tmp/nf/JetBrainsMonoNerdFontMono-Regular.ttf"
elif python3 -c 'import fontTools, brotli' >/dev/null 2>&1; then
    python3 - "$tmp/nf/JetBrainsMonoNerdFontMono-Regular.ttf" << 'PYEOF'
import sys
from fontTools.ttLib import TTFont
f = TTFont(sys.argv[1])
f.flavor = "woff2"
f.save(sys.argv[1].rsplit(".", 1)[0] + ".woff2")
PYEOF
else
    echo "ERROR: need woff2_compress (apt install woff2 / brew install woff2)" >&2
    echo "       or python fontTools+brotli (pip install fonttools brotli)" >&2
    exit 1
fi
rm -f "$FONTS/JetBrainsMonoNerdFontMono-Regular.ttf"
cp "$tmp/nf/JetBrainsMonoNerdFontMono-Regular.woff2" "$FONTS/"

echo "==> Done (committed files — run git status to see what changed):"
ls -lh "$FONTS"
