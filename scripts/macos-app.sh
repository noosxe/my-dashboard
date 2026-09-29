#!/usr/bin/env bash
# Dev-only macOS bundling (docs/0060 D4). OS-notification delivery requires
# a bundled, trusted .app — plain `cargo run` posts are silently dropped by
# macOS. Builds the release binary, wraps it in "My Dashboard.app", signs
# it, installs into ~/Applications (trusted without admin), and opens it.
#
# Ad-hoc signatures change per build, and macOS keys notification
# authorization off the signature, so a rebuilt bundle may re-prompt.
# SIGN_IDENTITY=<keychain identity> signs with a stable identity instead.
set -euo pipefail

cd "$(dirname "$0")/.."

APP_NAME="My Dashboard"
BUNDLE_ID="com.noosxe.my-dashboard"
VERSION="0.1.0"
IDENTITY="${SIGN_IDENTITY:--}"
DEST="${HOME}/Applications/${APP_NAME}.app"

echo "==> release build"
cargo build --release -p dashboard-app

echo "==> assembling ${DEST}"
rm -rf "${DEST}"
mkdir -p "${DEST}/Contents/MacOS"
cp target/release/my-dashboard "${DEST}/Contents/MacOS/my-dashboard"
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
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

echo "==> codesign (identity: ${IDENTITY})"
codesign --force --sign "${IDENTITY}" "${DEST}"
if [ "${IDENTITY}" = "-" ]; then
    echo "    note: ad-hoc signing — a rebuild may re-trigger the macOS"
    echo "    notification authorization prompt (docs/0060 D4)."
fi

echo "==> opening ${DEST}"
open "${DEST}"
