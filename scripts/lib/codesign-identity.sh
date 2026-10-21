# Shared codesign-identity helpers for the macOS bundling scripts
# (docs/0061 D1/D3). Sourced — never executed — by scripts/macos-app.sh
# and scripts/macos-package.sh so both sign under exactly the same rules.

# resolve_identity — print the codesign identity to sign with:
# SIGN_IDENTITY env (when set) → first "Apple Development" keychain
# identity (stable across rebuilds, so OS-notification authorization
# persists, docs/0060 D4) → ad-hoc "-" as the fallback (callers print
# the re-prompt note when they get "-").
resolve_identity() {
    local identity="${SIGN_IDENTITY:-}"
    if [ -z "${identity}" ]; then
        # The keychain may hold an expired duplicate under the same
        # subject (docs/0061 F5); `find-identity -v` lists valid ones
        # only, so "first match" is safe.
        identity="$(security find-identity -v -p codesigning \
            | grep '"Apple Development' | head -n1 \
            | sed -E 's/.*"([^"]+)".*/\1/' || true)"
    fi
    printf '%s' "${identity:--}"
}

# print_expiry <identity> — surface the identity certificate's notAfter
# date (docs/0061 D3): an expired certificate silently drops OS
# notifications, and the date is the only reliable diagnostic. Match by
# the SHA-1 the keychain reports for the identity — a plain -c lookup
# cannot distinguish same-named duplicate certificates. A failed read
# is a warning, never an error.
print_expiry() {
    local identity="$1"
    [ "${identity}" = "-" ] && return 0
    local certsha="" certdir="" fp="" expiry=""
    certsha="$(security find-identity -v -p codesigning \
        | grep "\"${identity}\"" | head -n1 | awk '{print $2}' || true)"
    if [ -z "${certsha}" ]; then
        echo "    warning: could not read the certificate expiry date"
        return 0
    fi
    certdir="$(mktemp -d)"
    security find-certificate -a -c "Apple Development" -p \
        | awk -v d="${certdir}" \
            '/BEGIN CERT/{n++; f=sprintf("%s/ac%d.pem", d, n)} {print > f}' || true
    for pem in "${certdir}"/ac*.pem; do
        [ -e "${pem}" ] || continue
        fp="$(openssl x509 -noout -fingerprint -sha1 -in "${pem}" 2>/dev/null \
            | sed -E 's/.*=//;s/://g' || true)"
        if [ "${fp}" = "${certsha}" ]; then
            expiry="$(openssl x509 -noout -enddate -in "${pem}" 2>/dev/null \
                | sed -E 's/^notAfter=//' || true)"
            break
        fi
    done
    rm -rf "${certdir}"
    if [ -n "${expiry}" ]; then
        echo "    certificate expires: ${expiry}"
    else
        echo "    warning: could not read the certificate expiry date"
    fi
}
