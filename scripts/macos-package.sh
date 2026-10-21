#!/usr/bin/env bash
# Produce a drag-installable "My Dashboard.app" in the current working
# directory (docs/0061 D6): the release bundle, assembled and signed, but
# NOT installed to ~/Applications and NOT opened — use when you want the
# artifact itself (drag it to /Applications, hand it to another Mac,
# inspect the bundle). The install-and-run dev loop stays
# scripts/macos-app.sh (docs/0060 D4).
#
# Release-only: a draggable app is the real app — same
# com.noosxe.my-dashboard bundle id and (per the shared signing lib)
# usually the same Apple Development identity, so it stays the *same*
# app to macOS and notification authorization carries over (docs/0061
# F3). Still not distribution-shaped (docs/0061 D4): no Developer ID,
# no notarization, no hardened runtime — other Macs will Gate-keep this
# app; that conversation remains parked (docs/0061 §6).
set -euo pipefail

APP_NAME="My Dashboard"
BUNDLE_ID="com.noosxe.my-dashboard"
VERSION="0.1.0"
BIN="target/release/my-dashboard"

# The .app lands in the caller's cwd; sources resolve from the repo.
OUT="$PWD"
cd "$(dirname "$0")/.."
source scripts/lib/codesign-identity.sh
IDENTITY="$(resolve_identity)"

echo "==> build (release)"
cargo build --release -p dashboard-app

DEST="${OUT}/${APP_NAME}.app"
echo "==> assembling ${DEST}"
rm -rf "${DEST}"
mkdir -p "${DEST}/Contents/MacOS" "${DEST}/Contents/Resources"
cp "${BIN}" "${DEST}/Contents/MacOS/my-dashboard"
# App icon (docs/0079): generated from crates/app/assets/macos/AppIcon.svg
# by scripts/app-icon.sh; CFBundleIconFile (no extension) points at it.
cp crates/app/assets/macos/AppIcon.icns "${DEST}/Contents/Resources/AppIcon.icns"
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

echo "==> produced ${DEST}"
echo "    not installed, not opened — drag it to /Applications yourself."
