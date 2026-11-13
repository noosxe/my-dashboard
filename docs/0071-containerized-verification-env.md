# 0071 — Containerized verification environment

*Formalizes the ad-hoc Docker recipe used for the docs/0065 receipts in PR
#523 into a committed, reproducible environment: `docker/Dockerfile`,
`docker/entrypoint.sh` and `scripts/verify.sh`.*

> **Amended 2026-10-23** (D9): UI receipts are captured only here, never
> on the macOS host; signed-in surfaces use the docs/0081 §12.1 fixtures.

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

### D9 — UI receipts are container-only *(amendment, 2026-10-23)*

Owner policy: **no UI captures on the host.** Every UI receipt is
captured in this environment and nowhere else: `audit`, `tree
--tab-stops`, `assert-baseline` and structural goldens, `critique`
screenshots, and pixel goldens. The macOS host may build, run unit
tests and attach the bridge for logs, but it never produces a receipt or
a screenshot. Surfaces that need signed-in or external data render here
through the GitHub fixture mode (docs/0081 §12.1:
`DASHBOARD_GITHUB_FIXTURE` + `DASHBOARD_FIXED_NOW`, debug builds only),
not through a real account. A surface that can't be reached that way
gets a fixture extension, not a host capture.

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

Write flows run the same way: the fixture transport answers writes from
response files and records them (docs/0081 §12.1), and a receipt asserts
what a click sent with `assert-write` (docs/0010 D11). `pr-writes.txt`
(session `prwrites`) walks comment, Merge, Close and a gated-run approve:

```sh
scripts/verify.sh sh -c '
  set -e
  cargo build -p dashboard-app
  export DASHBOARD_GITHUB_FIXTURE=/work/crates/debug/verify/fixtures/github
  export DASHBOARD_FIXED_NOW=2026-10-23T12:00:00Z
  cargo run -p dashboard-debug -- session start --name prwrites --size 1280x800 \
    --binary /tmp/target/debug/my-dashboard
  cargo run -p dashboard-debug -- run crates/debug/verify/pr-writes.txt
  cargo run -p dashboard-debug -- session stop prwrites
'
```

`pr-esc-order.txt` (session `escorder`, same fixtures and clock) walks
the PR page's Esc ordering and the discard confirm (docs/0043 D7, #744):
Esc closes the rebase confirm before the page, and Esc over a queued
review draft asks "Discard 1 review draft?" first.

The usage surfaces (docs/0081 §12.1) run over the same fixtures, with
two opt-in extras. `DASHBOARD_GITHUB_USAGE_SEED=<days>` seeds a
deterministic ledger, so the usage page opens populated.
`DASHBOARD_GITHUB_FIXTURE_RATE=healthy|low|critical|exhausted|graphql-exhausted`
gives the footer chip a budget, one variant per docs/0082 D5 state. Both are anchored to the pinned clock, so their numbers
and countdowns are stable. One session per rate variant (the variant is
read at boot):

```sh
scripts/verify.sh sh -c '
  set -e
  cargo build -p dashboard-app
  export DASHBOARD_GITHUB_FIXTURE=/work/crates/debug/verify/fixtures/github
  export DASHBOARD_FIXED_NOW=2026-10-23T12:00:00Z
  export DASHBOARD_GITHUB_USAGE_SEED=7
  export DASHBOARD_GITHUB_FIXTURE_RATE=low
  cargo run -p dashboard-debug -- session start --name usage --size 1280x800 \
    --binary /tmp/target/debug/my-dashboard
  export DASHBOARD_DEBUG_SESSION=usage
  # The chip shows on every page (docs/0082 Q9), Home included.
  sleep 4
  cargo run -p dashboard-debug -- tree | grep "GitHub API"
  cargo run -p dashboard-debug -- session stop usage
'
```

Expected chip labels: `healthy` reads "GitHub API: REST: 4,812 of
5,000 requests remaining, resets in 41 minutes; GraphQL: 4,655 of
5,000 points …" (Normal, no state word); `low` reads 1,040 / 960,
each marked "Low"; `critical` reads 312 / 188, each marked "Critical";
`exhausted` reads "REST: paused, resumes at HH:MM in 37 minutes;
GraphQL: paused, …"; `graphql-exhausted` pauses GraphQL only and
leaves REST at 4,812 (docs/0082 Q10). The chip is the button that
opens the usage page on its Budget view, as of the pinned 12:00.

The usage page's committed receipts (docs/0082 §10) run over a 90-day
seed, one fresh session per script, because each walks the fresh-boot
ids; each session is named after its script (`seeded`, `freeze`,
`paused`). The 90-day seed (about 96k flights) is written by the store's
writer thread after `session start`, and the usage page reads the ledger
once when it opens, so a read that lands first stays at `0 requests · no
history yet`. Each script's first data label is therefore `wait-label
"1,071 requests · as of" --timeout 120 --poke <Refresh id>`: it clicks the
page's Refresh button each second until the count shows, so the recipe
needs no sleep and starts whenever the session does. `usage-seeded.txt` audits both views, walks their Tab cycles and
asserts the populated goldens. `usage-freeze.txt` is the range-flip
freeze receipt. `usage-chip-paused.txt` adds the `graphql-exhausted`
variant for the per-budget pause (Q10):

```sh
scripts/verify.sh sh -c '
  set -e
  cargo build -p dashboard-app
  export DASHBOARD_GITHUB_FIXTURE=/work/crates/debug/verify/fixtures/github
  export DASHBOARD_FIXED_NOW=2026-10-23T12:00:00Z
  export DASHBOARD_GITHUB_USAGE_SEED=90
  for s in usage-seeded usage-freeze; do
    cargo run -p dashboard-debug -- session start --name ${s#usage-} --size 1280x800 \
      --binary /tmp/target/debug/my-dashboard
    cargo run -p dashboard-debug -- run crates/debug/verify/$s.txt
    cargo run -p dashboard-debug -- session stop ${s#usage-}
  done
  export DASHBOARD_GITHUB_FIXTURE_RATE=graphql-exhausted
  cargo run -p dashboard-debug -- session start --name paused --size 1280x800 \
    --binary /tmp/target/debug/my-dashboard
  cargo run -p dashboard-debug -- run crates/debug/verify/usage-chip-paused.txt
  cargo run -p dashboard-debug -- session stop paused
'
```

The PR detail page (docs/0081 §12.1, #769) runs over the same fixtures.
`pr-page.txt` opens all eight scenarios from `fixture-user/beacon` in one
session, walks their tabs and asserts the Conversation goldens for the
populated, draft and merged states:

```sh
scripts/verify.sh sh -c '
  set -e
  cargo build -p dashboard-app
  export DASHBOARD_GITHUB_FIXTURE=/work/crates/debug/verify/fixtures/github
  export DASHBOARD_FIXED_NOW=2026-10-23T12:00:00Z
  cargo run -p dashboard-debug -- session start --name prpage --size 1280x800 \
    --binary /tmp/target/debug/my-dashboard
  cargo run -p dashboard-debug -- run crates/debug/verify/pr-page.txt
  cargo run -p dashboard-debug -- session stop prpage
'
```

The script leaves out `audit` and `tab-stops` until #768 and #733 land,
since both exit red at the first Conversation stop (its header says where
they go). A PR-page PR runs them as one-off verbs at the stop it touches.

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
