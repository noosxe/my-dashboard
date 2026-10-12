---
name: macos-keychain-dev-signing
description: Stop macOS from re-prompting for the keychain password on every rebuild of a Rust app. Use when a dev binary asks for the login keychain password after each cargo build/run, when a keychain "Always Allow" grant doesn't stick, when setting up macOS code signing for local Rust development, or when storing credentials via keyring/Security.framework in a Rust app.
---

# macOS keychain dev signing for Rust apps

Fix the "signing wants to access your keychain" / keychain password prompt that reappears after every rebuild of a locally-built Rust binary.

## Root cause (understand before applying)

Keychain item ACLs record the accessing binary's **code-signing requirement** (designated requirement). `cargo` dev builds are unsigned or ad-hoc-signed; ad-hoc signatures are effectively a hash of the binary, so **every rebuild is a new identity** and an "Always Allow" grant can never match again. Adding the binary path to the item ACL (`security add-generic-password -T …`) does not help — without a stable signature the identity is path + changing hash.

The fix is not scripting around the prompt: it is giving the binary a **stable signing identity**, so the designated requirement is stable and the grant sticks across rebuilds.

## Why cargo can't do this natively (don't try these)

- `build.rs` runs **before** linking (Cargo Book → Build Scripts) — no binary exists yet to sign.
- Post-build hooks were requested and **declined** upstream (rust-lang/cargo#1894 — codesigning was the motivating example). There is no `post-build` script mechanism and none is coming.
- The one documented cargo surface that executes anything around a build is `target.<cfg>.runner` (Cargo Book → Configuration): the runner is invoked with the fresh binary as its first argument, for `cargo run` / `cargo test` / `cargo bench`. That is the hook this skill uses.

## Apply (3 steps)

1. **Copy the wrapper** from `templates/sign-and-run.sh` into the project at `.cargo/sign-and-run` and make it executable (`chmod +x`). It: passes through off macOS / without `codesign` / without any identity / when the binary is already signed (verify fast-path); signs with `$RUST_DEV_CODESIGN_IDENTITY` if set, else the first "Apple Development" identity (`security find-identity -v -p codesigning`); never lets a sign failure block the launch (`|| true` — worst case is the old behavior).
2. **Wire the runner**: append `templates/cargo-config.snippet.toml` to the project's `.cargo/config.toml` (create it if missing):
   ```toml
   [target.'cfg(target_os = "macos")']
   runner = [".cargo/sign-and-run"]
   ```
   On Linux/CI the `cfg` never matches and nothing changes.
3. **Identity**: most dev machines already have an Apple Development certificate (Xcode) — auto-discovered. If none exists, mint a stable self-signed codesigning cert once: `rcodesign generate-self-signed-certificate` (github.com/indygreg/apple-platform-rs, pure Rust, scriptable, no GUI) or Keychain Access → Certificate Assistant. A per-project env override (`RUST_DEV_CODESIGN_IDENTITY="Apple Development: …"`) wins over discovery. Self-signed is fine for local dev; release builds keep their real pipeline.

## Verify (without launching the app)

```console
$ bash -n .cargo/sign-and-run                    # syntax
$ cargo metadata --no-deps --format-version 1 >/dev/null   # config parses
$ cp /bin/echo /tmp/t && chmod +x /tmp/t && codesign --remove-signature /tmp/t
$ .cargo/sign-and-run /tmp/t hello               # → prints "hello", binary now signed
$ codesign -dv /tmp/t                            # → shows the signature
$ .cargo/sign-and-run /tmp/t again               # → verify fast-path, no re-sign
$ rm /tmp/t
```

Final human check: rebuild + `cargo run` twice — the keychain prompt appears **at most once** ("Always Allow"), then silence across rebuilds.

## Caveats (state these, don't paper over them)

- Bare `cargo build` still produces unsigned artifacts — cargo has no post-build hook. The signature lands on the first `cargo run`/`test` after each rebuild. Tooling that spawns the raw binary directly (debug bridges, app launchers) inherits the signature already baked into the file, so one runner-mediated launch after a rebuild covers those too.
- `codesign --force` rewrites the binary in place; the wrapper skips it when a valid signature is already present.
- A self-signed identity is trusted only on machines that have it in their keychain — this is a developer-machine technique, not distribution signing.
- Test binaries also go through the runner (harmless; they usually touch no keychain).
- Wrapper failures are silent by design (`|| true`): if prompts ever return, check `security find-identity -v -p codesigning` first — usually the identity expired or vanished.
