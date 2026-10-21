#!/bin/bash
# Regenerate the macOS app icon (AppIcon.icns) from the checked-in SVG.
#
# Source of truth: crates/app/assets/macos/AppIcon.svg — the sidebar brand
# (gallery-vertical-end on the Aurora primary, docs/0056) composed on
# Apple's macOS icon grid (1024 canvas, 824×824 body, 185.4px radius).
#
# The generated icns is committed (crates/app/assets/macos/AppIcon.icns);
# run this only when the brand changes. Requires resvg + iconutil:
#   cargo install resvg
set -euo pipefail

cd "$(dirname "$0")/.."
SRC="crates/app/assets/macos/AppIcon.svg"
OUT="crates/app/assets/macos/AppIcon.icns"
ICONSET="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$ICONSET"

command -v resvg >/dev/null || { echo "resvg missing: cargo install resvg" >&2; exit 1; }
command -v iconutil >/dev/null || { echo "iconutil missing (needs macOS)" >&2; exit 1; }

# The complete classic icon set (sizes in px; @2x doubles the point size).

render() { # render <px> <iconset-name>
  resvg --width "$1" --height "$1" "$SRC" "$ICONSET/$2"
}
render 16   icon_16x16.png
render 32   icon_16x16@2x.png
render 32   icon_32x32.png
render 64   icon_32x32@2x.png
render 128  icon_128x128.png
render 256  icon_128x128@2x.png
render 256  icon_256x256.png
render 512  icon_256x256@2x.png
render 512  icon_512x512.png
render 1024 icon_512x512@2x.png

iconutil -c icns "$ICONSET" -o "$OUT"
echo "wrote $OUT ($(wc -c <"$OUT" | tr -d ' ') bytes)"
