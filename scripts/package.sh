#!/usr/bin/env bash
# Package Flux artifacts for distribution.
# Usage: ./scripts/package.sh [target...]
#   No args  → build for host target only.
#   With args → cross-compile for listed targets.
#
# Examples:
#   ./scripts/package.sh
#   ./scripts/package.sh x86_64-unknown-linux-gnu aarch64-apple-darwin
set -euo pipefail
cd "$(dirname "$0")/.."

usage() {
    echo "Usage: $0 [target...]"
    echo "  Build server + web UI → dist/"
    echo "  No args: build for host target ($(rustc -vV | sed -n 's/host: //p'))"
    echo "  With args: cross-compile for listed Rust target triples"
    echo ""
    echo "Installed targets:"
    rustup target list --installed 2>/dev/null | sed 's/^/  /' || echo "  (rustup not found)"
    echo ""
    echo "Examples:"
    echo "  $0"
    echo "  $0 x86_64-unknown-linux-gnu aarch64-apple-darwin"
    echo "  $0 x86_64-pc-windows-msvc  # needs: cargo install cargo-xwin"
    exit 0
}

for arg in "${@}"; do
    if [[ "$arg" == "-h" || "$arg" == "--help" ]]; then
        usage
    fi
    if [[ "$arg" == -* ]]; then
        echo "Error: invalid argument '$arg' — expected Rust target triple(s). Use -h for help."
        exit 1
    fi
done

DIST="$PWD/dist"
rm -rf "$DIST"
mkdir -p "$DIST"

TARGETS=("${@}")
if [ ${#TARGETS[@]} -eq 0 ]; then
    TARGETS=("$(rustc -vV | sed -n 's/host: //p')")
fi

# ── Build server for each target ──
for target in "${TARGETS[@]}"; do
    echo "==> Building flux-server for $target..."

    if [[ "$target" == *-pc-windows-msvc ]]; then
        if ! command -v cargo-xwin &>/dev/null; then
            echo "Error: 'cargo xwin' is required for MSVC targets. Install it: cargo install cargo-xwin"
            exit 1
        fi
        cargo xwin build --release --target "$target"
    else
        cargo build --release --target "$target"
    fi

    if [[ "$target" == *windows* ]]; then
        ext=".exe"
    else
        ext=""
    fi
    bin_name="flux-server-${target}${ext}"

    cp "target/${target}/release/flux-server${ext}" "$DIST/$bin_name"
done

# ── Build web UI (served by flux-server's static layer) ──
echo "==> Packaging web UI..."
./scripts/package-web.sh --out "$DIST/web-ui"

echo "==> Done. Artifacts:"
ls -lh "$DIST/"
echo "  (web UI served by default — web-ui/ next to the binary is picked up automatically)"
