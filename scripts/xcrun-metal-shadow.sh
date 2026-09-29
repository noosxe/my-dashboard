#!/usr/bin/env bash
# macOS dev-shell xcrun shadow (README "Development"; the flake shellHook
# installs this as the first PATH entry on darwin).
#
# gpui-pre >= 0.3.7 compiles Metal shaders at build time via
# `xcrun -sdk macosx metal`. Two constraints collide in the nix dev shell:
#   - the dev shell's xcbuild `xcrun` shim cannot resolve Xcode's
#     downloadable Metal Toolchain cryptex, so metal/metallib lookups must
#     go to Apple's /usr/bin/xcrun with DEVELOPER_DIR pointing at a real
#     Xcode;
#   - nix's ld cannot parse Xcode 27 .tbd files, so SDK resolution for the
#     nix cc/ld wrappers must STAY on the shim.
# So: metal/metallib -> Apple xcrun; everything else -> the stock dev-shell
# xcrun (the next one on PATH after this shadow dir).
#
# Override the Xcode location with XCODE_DEVELOPER_DIR if Xcode.app is not
# in /Applications. Standalone use outside the dev shell:
#   PATH="$(dirname "$0"):$PATH" cargo build
set -u

xcode_dev="${XCODE_DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"

for a in "$@"; do
  case "$a" in
    metal|metallib)
      exec env DEVELOPER_DIR="$xcode_dev" /usr/bin/xcrun "$@"
      ;;
  esac
done

# Not a Metal tool: strip this wrapper's own dir from PATH and re-resolve
# xcrun so SDK queries land on the stock dev-shell shim. dirname($0) is
# used verbatim — it mirrors the PATH entry that invoked us, whereas a
# physical path (pwd -P) breaks under symlinked entries like /tmp.
selfdir=$(dirname -- "$0")
rest=""
IFS=:
for p in $PATH; do
  [ "$p" = "$selfdir" ] && continue
  if [ -n "$rest" ]; then rest="$rest:$p"; else rest="$p"; fi
done
unset IFS
exec env PATH="$rest" xcrun "$@"
