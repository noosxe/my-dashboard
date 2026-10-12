#!/usr/bin/env bash
# macOS dev-launch signer: cargo's documented `target.<cfg>.runner` hook
# (.cargo/config.toml) invokes this wrapper with the freshly built binary
# before executing it. Signing with a stable identity gives the Keychain a
# stable designated requirement, so item grants ("Always Allow") stick
# across rebuilds instead of re-prompting on every new binary.
#
# Identity resolution: $RUST_DEV_CODESIGN_IDENTITY wins, else the first
# "Apple Development" identity in the keychain. Without one, off macOS, or
# for an already-signed binary, this is a pass-through. (cargo has no
# post-build hook — rust-lang/cargo#1894 — so bare `cargo build` leaves
# unsigned artifacts; the signature lands on the first runner-mediated
# launch after each rebuild.)
#
# Rename the env var per project if you like (e.g. MYAPP_CODESIGN_IDENTITY).
set -eu

exe="$1"
shift

if [ "$(uname)" != "Darwin" ] || ! command -v codesign >/dev/null 2>&1; then
    exec "$exe" "$@"
fi

# Already signed -> the file is unchanged since the last launch; re-signing
# would rewrite it for nothing.
if codesign --verify "$exe" >/dev/null 2>&1; then
    exec "$exe" "$@"
fi

identity="${RUST_DEV_CODESIGN_IDENTITY:-}"
if [ -z "$identity" ]; then
    # First Apple Development identity, e.g.
    #   1) ABCDEF… "Apple Development: name (TEAMID)"
    identity="$(security find-identity -v -p codesigning \
        | grep '"Apple Development' | head -n1 \
        | sed -E 's/.*"([^"]+)".*/\1/' || true)"
fi
if [ -z "$identity" ]; then
    exec "$exe" "$@"
fi

codesign --force --sign "$identity" "$exe" || true
exec "$exe" "$@"
