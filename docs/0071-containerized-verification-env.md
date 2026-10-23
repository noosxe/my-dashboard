# 0071 — Containerized verification environment

*Formalizes the ad-hoc Docker recipe used for the docs/0065 receipts in PR
#523 into a committed, reproducible environment: `docker/Dockerfile`,
`docker/entrypoint.sh` and `scripts/verify.sh`.*

## 1. Context

docs/0065 receipts (audit, tab-stops, critique) require a Linux session:
headless sway + the debug bridge + the a11y bus. The product owner has no
Linux box; the sanctioned substitute is a Docker container (decision
recorded on #523). Until now that container existed only as an
unversioned `docker run` invocation pasted into the #523 PR body — every
receipt run re-assembled it from memory, and each lesson (the at-spi
shim, the `--binary` flag, the osxfs tax) lived in a session transcript.
This doc commits the environment so the recipe is reviewable, pinned and
reusable by anyone.

## 2. Decisions

### D1 — Env-only image; the repo is mounted, never COPY'd

The image carries tooling and system libraries only. `scripts/verify.sh`
bind-mounts the working tree at `/work`, so every run verifies the tree
exactly as it stands — no stale baked-in sources, no rebuild of the image
per change. Consequence: the image changes rarely (D2/D3 only), and
`docker build` in the wrapper is a near-instant cached no-op.

### D2 — Base pinned by digest, bumped in lockstep

`FROM rust:1.98.0@sha256:620dbcd1…` — tag *and* digest, matching
the dev-shell flake's explicit release, `RUST_TOOLCHAIN` in
`.github/workflows/ci.yml` and `rust-version` in `Cargo.toml` (docs/0027
D4's four-way lockstep, enforced by `scripts/check-toolchain-pins.sh`). The
digest is the Docker analogue of CI's SHA-pinned actions: re-pulling the
same tag months later must not silently change the verification env.

### D3 — apt set: CI's list plus the verification stack

The GUI dev libs mirror CI's apt list (docs/0027 D1), with a lockstep
comment in both files — deliberately minus `fonts-ibm-plex`: that is an
Ubuntu package name, and the rust base is Debian trixie, which ships no
equivalent. IBM Plex is vendored from upstream instead (below).
The additions are exactly what the #523 runs proved necessary:

- `sway` + `dbus` + `at-spi2-core` — headless compositor, session bus,
  a11y bus for dashboard-debug sessions;
- `grim` — Wayland screenshots for the docs/0065 critique ritual
  (screenshot + tree snapshot, never pixels alone);
- `xvfb` + `cmake` — hosted-runner build-baseline parity (docs/0027 §2;
  the hosted image ships them by construction, the rust base does not);
- `fonts-dejavu-core` — glyph fallback coverage under the pixman
  renderer;
- IBM Plex Sans + Mono — vendored from upstream `v6.4.0` (the version
  Ubuntu noble packages for CI) since trixie ships no equivalent
  package; both zips sha256-pinned in the Dockerfile, TTFs only,
  `fc-cache`d into `/usr/local/share/fonts/ibm-plex`;
- `jq` — post-processing of `--json` tree/audit dumps.

### D4 — The at-spi shim is baked, and fails loudly

On bookworm, `at-spi-bus-launcher` lives in `/usr/libexec` but is exec'd
by bare name — discovered by failure during #523. The Dockerfile symlinks
it into `/usr/local/bin` behind a `test -x` guard: if the path ever
moves, the *image build* fails, instead of shipping an env whose a11y
silently breaks. Same fail-safe shape as CI's sudo guard.

### D5 — The entrypoint is the session bootstrap

`docker/entrypoint.sh` creates `XDG_RUNTIME_DIR` (0700), sets
`WLR_BACKENDS=headless`, `WLR_LIBINPUT_NO_DEVICES=1`,
`WLR_RENDERER=pixman`, and `exec`s `dbus-run-session -- "$@"`. Every
`docker run` therefore lands receipt-ready — builds, tests and session
verbs need zero per-run setup — and the wrapped command's exit code
becomes the container's.

Consequence: the debug session record and the a11y bus live in the
container's `XDG_RUNTIME_DIR`, so **a receipt chain is one wrapper
invocation**, `session start` through `session stop` (§4). Session
state deliberately does not survive across `docker run`s — the same
isolated-and-disposable contract AGENTS prescribes for sessions.

### D6 — Warm caches ride named volumes; the volume shadows CARGO_HOME

`scripts/verify.sh` mounts `gpui-diff-cargo-cache` at `/usr/local/cargo`
and `gpui-diff-target` at `/tmp/target` (names retained from #523 so the
populated caches carry over: ~3 min warm builds vs ~25 min cold).
`CARGO_TARGET_DIR=/tmp/target` also keeps build output off the macOS
bind mount, whose write tax the #523 runs paid. Two consequences are
part of the deal:

- on first use, a named volume initialized over a non-empty image
  directory *copies* that directory — so the volume shadows
  `CARGO_HOME`, including the `cargo`/rustup shims;
- therefore a base bump (D2) must recreate both volumes
  (`docker volume rm gpui-diff-cargo-cache gpui-diff-target`), or the
  stale shims shadow the new toolchain. §5 bakes this into the bump
  ritual.

Related lesson: `session start` must get
`--binary /tmp/target/debug/my-dashboard` — the default discovery would
find `./target/debug/my-dashboard` on the bind mount, a *macOS* binary.

### D7 — `scripts/verify.sh` is the only sanctioned invocation

One file owns the image name, build, mounts, volume names and env. No
bare `docker run` invocations in PR bodies or session transcripts
anymore; the wrapper is small enough to read, and changes to it are
reviewed like any code.

### D8 — Trust boundary

Root inside the container is fine; the isolation boundary is the
container itself. Only the repo is mounted — never `$HOME`, never
credential stores, never the Docker socket.

## 3. The pieces

| File | Role |
|---|---|
| `docker/Dockerfile` | The environment: digest-pinned base, apt set (D3), shim (D4), env (D6) |
| `docker/entrypoint.sh` | Session bootstrap (D5) |
| `scripts/verify.sh` | Host-side canonical entry (D7): build-if-stale, run with mounts |

## 4. Canonical recipes

The AGENTS gates, warm (~3 min each after the first):

```sh
scripts/verify.sh cargo fmt --all --check
scripts/verify.sh cargo clippy --workspace --all-targets -- -D warnings
scripts/verify.sh cargo test --workspace
```

A docs/0065 receipt session (docs/0010 §3 is the full protocol) — one
chain, one container, `set -e` so any failing verb aborts honestly:

```sh
scripts/verify.sh sh -c '
  set -e
  cargo build -p dashboard-app
  cargo run -p dashboard-debug -- session start --name receipts --size 1440x900 \
    --binary /tmp/target/debug/my-dashboard
  DASHBOARD_DEBUG_SESSION=receipts cargo run -p dashboard-debug -- doctor
  DASHBOARD_DEBUG_SESSION=receipts cargo run -p dashboard-debug -- audit <surface>
  DASHBOARD_DEBUG_SESSION=receipts cargo run -p dashboard-debug -- tree --tab-stops
  cargo run -p dashboard-debug -- session stop receipts
'
```

The committed receipts run the same way — `cargo run -p
dashboard-debug -- run crates/debug/verify/audit.txt` (the 0065 D3
audit with the D5 structural-golden asserts after every stop; session
`audit` at 1280x800). Golden refresh is this same container with a
one-shot capture receipt that walks audit.txt's exact step order,
running `baseline <surface> --update` at each stop — the committed
receipts only ever assert, never write (0065 D5).

Signed-in GitHub surfaces run over the committed fixtures (docs/0081
§12.1) — debug builds only, no network, no keyring:

```sh
scripts/verify.sh sh -c '
  set -e
  cargo build -p dashboard-app
  export DASHBOARD_GITHUB_FIXTURE=/work/crates/debug/verify/fixtures/github
  export DASHBOARD_FIXED_NOW=2026-10-23T12:00:00Z
  cargo run -p dashboard-debug -- session start --name commits --size 1280x800 \
    --binary /tmp/target/debug/my-dashboard
  cargo run -p dashboard-debug -- run crates/debug/verify/repo-commits.txt
  cargo run -p dashboard-debug -- session stop commits
'
```

Interactive inspection: `scripts/verify.sh bash`.

Freshness caution: cargo tracks host edits by mtime, and macOS bind
mounts (osxfs) have coarse, sometimes stale mtimes — an in-container
build right after host edits can silently reuse a stale binary from the
volume. When receipts must reflect exactly what the working tree holds,
start the chain with `cargo clean -q -p <touched crates>` (deps stay
cached; the rebuild is short).

## 5. Base-image bump ritual

One PR changes all four toolchain pins together (docs/0027 D4): the
tag + digest in `docker/Dockerfile`, the release in `flake.nix`,
`RUST_TOOLCHAIN` in `.github/workflows/ci.yml` and `rust-version` in
`Cargo.toml`; `scripts/check-toolchain-pins.sh` must pass. After
merging, recreate the caches and pay the one cold build:

```sh
docker volume rm gpui-diff-cargo-cache gpui-diff-target
```

## 6. Non-goals

- **Not a dev shell.** macOS remains the daily driver; no GUI/audio
  passthrough, no macOS-targeting builds in the container.
- **Not CI.** GitHub-hosted runners stay (docs/0027). The apt-list
  lockstep is enforced by comment contract and review, not automation.
- **No registry.** The image is built locally on demand; the digest pin
  is the reproducibility contract.
- **No cross-compilation.** The container verifies on aarch64 Linux
  (Apple-Silicon Docker resolves the digest pin's arm64 variant); other
  targets stay out of scope.
- **No tooling beyond the lessons.** Every package above traces to a
  concrete #523 finding or a docs/0027 parity requirement; additions
  need the same evidence.
