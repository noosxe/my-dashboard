#!/usr/bin/env bash
# Dev-only macOS bundling (docs/0060 D4, docs/0061). OS-notification
# delivery requires a bundled, trusted .app — plain `cargo run` posts are
# silently dropped by macOS. Builds the app, wraps it in a .app, signs it,
# installs into ~/Applications (trusted without admin), and opens it.
#
# Release mode (default): "My Dashboard.app" from the release binary.
# Dev mode (--dev): "My Dashboard Dev.app" from the debug binary, with its
# own bundle id so the two bundles' OS-notification authorizations stay
# independent.
#
# Signing identity (docs/0061): SIGN_IDENTITY env wins; otherwise the first
# "Apple Development" identity in the keychain is used (stable across
# rebuilds, so macOS notification authorization persists); with none found,
# falls back to ad-hoc — which re-prompts on rebuild (docs/0060 D4).
set -euo pipefail

cd "$(dirname "$0")/.."
# Shared signing-identity helpers (docs/0061 D1/D3) — same resolution and
# expiry surfacing as scripts/macos-package.sh.
source "$(dirname "$0")/lib/codesign-identity.sh"

DEV=0
if [ "${1:-}" = "--dev" ]; then
    DEV=1
elif [ -n "${1:-}" ]; then
    echo "usage: $0 [--dev]" >&2
    exit 2
fi

if [ "${DEV}" = 1 ]; then
    APP_NAME="My Dashboard Dev"
    BUNDLE_ID="com.noosxe.my-dashboard.dev"
    BIN="target/debug/my-dashboard"
    BUILD=(cargo build -p dashboard-app)
else
    APP_NAME="My Dashboard"
    BUNDLE_ID="com.noosxe.my-dashboard"
    BIN="target/release/my-dashboard"
    BUILD=(cargo build --release -p dashboard-app)
fi
VERSION="0.1.0"
DEST="${HOME}/Applications/${APP_NAME}.app"

IDENTITY="$(resolve_identity)"

echo "==> build ($([ "${DEV}" = 1 ] && echo debug || echo release))"
"${BUILD[@]}"

echo "==> assembling ${DEST}"
rm -rf "${DEST}"
mkdir -p "${DEST}/Contents/MacOS"
mkdir -p "${DEST}/Contents/Resources"
# App icon (docs/0079): generated from crates/app/assets/macos/AppIcon.svg
# by scripts/app-icon.sh; CFBundleIconFile (no extension) points at it.
cp crates/app/assets/macos/AppIcon.icns "${DEST}/Contents/Resources/AppIcon.icns"
cp "${BIN}" "${DEST}/Contents/MacOS/my-dashboard"
cat > "${DEST}/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
    <key>CFBundleName</key><string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key><string>${APP_NAME}</string>
    <key>CFBundleShortVersionString</key><string>${VERSION}</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleExecutable</key><string>my-dashboard</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

echo "==> codesign (identity: ${IDENTITY})"
codesign --force --sign "${IDENTITY}" "${DEST}"
if [ "${IDENTITY}" = "-" ]; then
    echo "    note: ad-hoc signing — a rebuild may re-trigger the macOS"
    echo "    notification authorization prompt (docs/0060 D4, docs/0061)."
else
    print_expiry "${IDENTITY}"
fi

echo "==> opening ${DEST}"
open "${DEST}"
