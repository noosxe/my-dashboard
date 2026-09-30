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

if [ -z "${SIGN_IDENTITY:-}" ]; then
    # First Apple Development identity, e.g.
    #   1) ABCDEF… "Apple Development: name (TEAMID)"
    SIGN_IDENTITY="$(security find-identity -v -p codesigning \
        | grep '"Apple Development' | head -n1 \
        | sed -E 's/.*"([^"]+)".*/\1/' || true)"
fi
IDENTITY="${SIGN_IDENTITY:--}"

echo "==> build ($([ "${DEV}" = 1 ] && echo debug || echo release))"
"${BUILD[@]}"

echo "==> assembling ${DEST}"
rm -rf "${DEST}"
mkdir -p "${DEST}/Contents/MacOS"
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
    # Development certificates are short-lived; an expired one drops OS
    # posts silently again. Surface the date of the certificate that
    # signing actually used (docs/0061 D3), matched by the SHA-1 the
    # keychain reports for this identity — a plain -c lookup cannot
    # distinguish same-named duplicate certificates.
    CERTSHA="$(security find-identity -v -p codesigning \
        | grep "\"${IDENTITY}\"" | head -n1 | awk '{print $2}' || true)"
    EXPIRY=""
    if [ -n "${CERTSHA}" ]; then
        CERTDIR="$(mktemp -d)"
        security find-certificate -a -c "Apple Development" -p \
            | awk -v d="${CERTDIR}" \
                '/BEGIN CERT/{n++; f=sprintf("%s/ac%d.pem", d, n)} {print > f}' || true
        for pem in "${CERTDIR}"/ac*.pem; do
            [ -e "${pem}" ] || continue
            FP="$(openssl x509 -noout -fingerprint -sha1 -in "${pem}" 2>/dev/null \
                | sed -E 's/.*=//;s/://g' || true)"
            if [ "${FP}" = "${CERTSHA}" ]; then
                EXPIRY="$(openssl x509 -noout -enddate -in "${pem}" 2>/dev/null \
                    | sed -E 's/^notAfter=//' || true)"
                break
            fi
        done
        rm -rf "${CERTDIR}"
    fi
    if [ -n "${EXPIRY}" ]; then
        echo "    certificate expires: ${EXPIRY}"
    else
        echo "    warning: could not read the certificate expiry date"
    fi
fi

echo "==> opening ${DEST}"
open "${DEST}"
