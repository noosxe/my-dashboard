# 0061 — Development-profile signing for macOS dev builds

**Status:** shipped with this doc (small-feature flow, one PR).
**Owner:** product owner. **Date:** 2026-09-30.

## 1. Problem

Every build of the app on macOS is ad-hoc signed (the linker does it
implicitly for debug binaries; `scripts/macos-app.sh` did it explicitly
for bundles). Ad-hoc signatures change per build, and macOS keys
OS-notification authorization off the code signature — so a rebuilt
bundle is a *new* app as far as macOS is concerned, and the
notification permission can re-prompt or silently drop posts
(docs/0060 D4). Release/distribution signing (Developer ID,
notarization) is explicitly a later conversation; this doc covers
signing **dev builds with the owner's personal Apple Development
certificate** so the identity is stable across rebuilds.

## 2. Verified facts (F)

- **F1 Keychain baseline.** Before this work, `security find-identity
  -v -p codesigning` found **zero** identities on the owner's machine.
  The owner created one via Xcode 27 (Settings → Accounts → personal
  team → Manage Certificates → **Apple Development**):
  `Apple Development: <name> (<TEAMID>)`. Apple: development
  certificates "belong to individuals"; Xcode-based creation requires
  no paid program membership ("membership isn't required" to develop
  and test on personal devices).
- **F2 Debug binaries today.** `codesign -dvv target/debug/my-dashboard`
  shows `Signature=adhoc … (adhoc, linker-signed)`,
  `Identifier=my_dashboard-<hash>` (hash-derived → changes every
  rebuild), `TeamIdentifier=not set`.
- **F3 What macOS keys notifications to.** Authorization follows the
  code signature; delivery additionally requires a bundled, trusted
  `.app` (docs/0060 F5). `App::set_app_identity` is the **Windows**
  path only (docs/0060 §4) — the macOS identity comes from the bundle
  id + signature, so a separate dev bundle id cannot collide with the
  hard-coded identity string.
- **F4 Signing surface.** `/usr/bin/codesign` and `/usr/bin/security`
  work from both a plain shell and `nix develop`. Entitlements are not
  required for local notifications; the hardened runtime and secure
  timestamps (`--timestamp`) are distribution-era options and stay off.
- **F5 Certificates expire.** Apple Development certificates are
  short-lived (exact dates are per-certificate; Xcode's manage view or
  the keychain shows them). An expired certificate returns the app to
  the silent-drop behavior — the only reliable diagnostic is the
  certificate's own expiry date.
  In practice this machine's keychain holds an **expired duplicate**
  with the identical subject name (valid until 2026-08-25; the current
  one runs until 2027-09-30) — so the script's expiry lookup matches
  certificates by the SHA-1 the keychain reports for the identity, not
  by name.

## 3. Decisions (D)

- **D1 — Identity resolution order.** `SIGN_IDENTITY` env var (if
  set) → otherwise the **first `Apple Development` identity** found by
  `security find-identity -v -p codesigning` → otherwise ad-hoc `-`
  with the existing note. The personal profile is the default, not
  something to remember to pass.
- **D2 — Dev profile, own bundle id.** `scripts/macos-app.sh --dev`
  bundles the **debug** binary as `My Dashboard Dev.app` with
  `CFBundleIdentifier=com.noosxe.my-dashboard.dev`, so dev and release
  bundles hold independent notification authorizations (re-signing one
  never disturbs the other's prompt history). Default (no flag)
  behavior is unchanged: release binary, `My Dashboard.app`,
  `com.noosxe.my-dashboard`.
- **D3 — Expiry surfaced.** Every real-identity run prints the
  certificate's `notAfter` date (via `security find-certificate` +
  `openssl x509`); a failed read is a warning, never an error.
- **D4 — Nothing distribution-shaped.** No entitlements, no hardened
  runtime, no secure timestamp, no provisioning profile. Those belong
  to the release-signing conversation (owner: later).
- **D5 — Plain `cargo run` stays ad-hoc.** The signed loop is
  bundle-and-sign; there is no cargo post-build hook, and an in-place
  re-signed bare binary still could not post notifications (F3), so it
  buys nothing that matters.

## 4. Testing

- Script-level only (no Rust changes): `bash -n` clean; identity
  auto-discovery parses `security find-identity` output; both profiles
  assemble, sign, and open.
- Machine-verified at ship time: dev bundle signature shows
  `Authority=Apple Development: …` and the `TeamIdentifier`; the
  expiry line prints; re-running `--dev` rebuilds and re-signs under
  the **same** identity (that is the fix).

## 5. Human checks (owner)

- [ ] `./scripts/macos-app.sh --dev` twice in a row (with a rebuild
      between): the second run must **not** re-trigger the macOS
      notification authorization prompt.
- [ ] Unfocus the dev app, raise a hub notification: it lands in the
      macOS notification center.
- [ ] First signing run: the keychain access prompt (if it appears)
      is answered **Always Allow** — one-time for this terminal.
- [ ] Release mode (`./scripts/macos-app.sh`) still works and keeps
      its own authorization history.

## 6. Non-goals (parked, not done here)

- Release/distribution signing: Developer ID, notarization, DMG
  packaging, secure timestamps, hardened runtime — the owner deferred
  this conversation.
- nix-built darwin package signing (the flake package is Linux-only
  today).
- CI signing (no secrets in CI by policy; also release-shaped).
- Re-signing the bare debug binary for the plain `cargo run` loop (D5).
