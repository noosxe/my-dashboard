# 0027 — CI: GitHub Actions Gates on Self-Hosted Runners (fmt / clippy / test, no Nix)

## 1. Summary

Enforce the AGENTS.md quality gates — `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets` at **zero warnings** (including the
`clippy.toml` token-discipline lints), and `cargo test --workspace` — on every
PR to `main` and every push to `main`, running on the owner's self-hosted
runners (labels: `self-hosted, linux, amd64`).

The CI is **nix-free** (owner decision, §4): the toolchain comes from rustup,
system libraries from apt. Two jobs (`lint`, `test`), one pinned third-party
action (`actions/checkout`), no secrets, no `actions/cache` — warm builds come
from a workspace-external target directory on the persistent runner hosts.

Release/packaging CI (binary builds, caches, tag automation) is explicitly out
of scope (see the standing note in `docs/README.md`; it takes its own doc when
written).

## 2. Facts & constraints

- **Gates are codified** in AGENTS.md ("Run the gates before pushing"); CI must
  run exactly those three commands, plus guard the `--no-default-features`
  (bridge-less) build variant that AGENTS.md calls out as easy to break.
- **The test suite is headless-green today** (verified 2026-09-07: full
  `cargo test --workspace` passes with `DISPLAY`/`WAYLAND_DISPLAY`/
  `DBUS_SESSION_BUS_ADDRESS` unset and a fresh temp `XDG_RUNTIME_DIR`; exit 0,
  0 failures). The flake's `doCheck = false` comment ("the GUI harness wants
  display/runtime-dir") is honored conservatively anyway: the test step keeps
  an `xvfb-run` wrapper and sets `XDG_RUNTIME_DIR` to a fresh temp dir, so a
  future gpui rev that starts opening real windows fails loudly instead of
  mysteriously.
- **Runner behavior** (verified): self-hosted workspaces persist between jobs,
  but `actions/checkout`'s default clean step runs `git clean -ffdx`, which
  deletes `target/` (and everything else untracked/ignored) inside the
  workspace. A runner executes **one job at a time**, so a shared target dir
  needs no locking.
- **Dev-shell parity**: locally the gates run inside `nix develop` (toolchain
  frozen by `flake.lock` at 1.98.0, gpui dynamic libs on `LD_LIBRARY_PATH`,
  hermetic IBM Plex via `FONTCONFIG_FILE`). CI cannot replicate the nix parts —
  parity is therefore *approximate* (D1, D4) and the drift surface is recorded
  here deliberately.
- **No secrets** are needed by any gate; fork PRs are safe.

## 3. Design

### D1 — Plain rustup + apt, no Nix on the runners

Each job bootstraps Rust into `~/.cargo` (user-local, no sudo): installs
rustup if missing, then `rustup toolchain install $RUST_TOOLCHAIN` with
`rustfmt`/`clippy` components (idempotent; ~seconds warm on a persistent
host). The gpui system libraries come from a **one-time, documented apt
provisioning command** (§5) that mirrors the flake's `dynamicLibraries` list
plus compiler/font/test support. No `LD_LIBRARY_PATH` is needed: apt installs
into the default loader path (that env var exists in the dev shell only
because nix stores libs elsewhere).

Drift surface vs the dev shell, accepted: apt lib versions float (deviation
from `flake.lock`), fonts resolve through system fontconfig rather than the
hermetic `FONTCONFIG_FILE` (`fonts-ibm-plex` keeps the typeface identical;
metric-sensitive tests would surface any residue). Mitigation if that ever
bites: ship a tiny in-repo `fontconfig.conf` and export `FONTCONFIG_FILE`.

### D2 — Cargo cache outside the workspace

`CARGO_TARGET_DIR=$HOME/.cache/my-dashboard/target`, exported via
`$GITHUB_ENV` in the bootstrap step. It survives checkout's `git clean -ffdx`,
stays warm across runs and branches (cargo's fingerprinting handles
invalidation), and needs no `actions/cache` archiving. `~/.cargo` (registry +
git checkouts of the pinned gpui revs) persists on the host the same way.
Disk hygiene is deferred to a follow-up workflow (§7, #155).

### D3 — Two jobs: `lint`, `test`

- `lint` — `cargo fmt --all --check`; `cargo clippy --workspace --all-targets
  -- -D warnings`; `cargo check -p my-dashboard --no-default-features`
  (bridge-less variant stays compilable).
- `test` — `cargo test --workspace` under `xvfb-run --auto-servernum` with a
  fresh temp `XDG_RUNTIME_DIR` (§2 explains why both are kept despite the
  headless-green probe).

Jobs are independent (no `needs`); with one runner they queue, with several
they parallelize. `concurrency: ci-<ref>` with `cancel-in-progress` kills
superseded runs and frees the shared hosts. `timeout-minutes` (30/45) bounds a
hung job so it cannot wedge a shared runner indefinitely.

### D4 — Toolchain pinned, in lockstep with the flake

`RUST_TOOLCHAIN: "1.98.0"` as a single workflow-level env, matching the
dev-shell toolchain the flake currently freezes. Bumped **deliberately,
together with** `flake.lock` toolchain moves (rust-overlay stable bump). A
floating `stable` on CI would let it diverge from the local gates and fail
green-local PRs mid-flight.

### D5 — Supply chain & hygiene

Exactly one third-party action, `actions/checkout`, pinned by commit SHA
(`3d3c42e…` = v7.0.1) with `persist-credentials: false`.
`dependabot.yml` gains a `github-actions` ecosystem so the pin gets weekly
update PRs. No other actions, no cache service, no secrets — the workflow is
fork-safe by construction.

### D6 — What deliberately stays out

- **Nix package/theme builds**: `nix build .#my-dashboard` and the
  Catppuccin Whiskers/jq gates are nix derivations by construction; with a
  nix-free CI they run **local-only**. Porting the theme gates to a shell
  step is follow-up #156.
- **Release-mode build**: dev-profile clippy + test compile every crate;
  a release build would add ~10 min/run for little signal.
- **Maintenance/scheduled jobs**: follow-up #155.
- **Required status checks**: enabling `lint`/`test` in branch protection is
  a repo-settings action for the owner (§6), not workflow code.
- A shared composite action for the duplicated bootstrap step — two copies
  are fine; refactor if a third job ever needs it.

## 4. Owner decisions (2026-09-07)

- **No Nix on CI** — D1/D6 reworked from the original nix-based proposal
  (rustup + apt instead of `nix develop`; nix jobs dropped).
- **Single PR** for this feature (docs + workflows + dependabot tweak
  together) — the small-feature flow of AGENTS.md.
- **Maintenance workflow** deferred to its own issue (#155).
- **Theme gates** stay local-only for now; nix-free port filed (#156).

## 5. Implementation plan (this PR)

Files:

- `.github/workflows/ci.yml` — new (listing below).
- `.github/dependabot.yml` — add the `github-actions` ecosystem.
- `docs/0027-ci-workflows.md` — this doc; `docs/README.md` index row.

One-time runner-host provisioning (per host, sudo, ~once):

```bash
sudo apt-get update && sudo apt-get install -y \
  build-essential pkg-config curl git ca-certificates \
  libwayland-dev libvulkan-dev libxcb1-dev libxkbcommon-dev \
  libatk1.0-dev libfontconfig1-dev libglib2.0-dev libgtk-3-dev \
  xvfb fonts-ibm-plex
```

(the lib set mirrors the flake `dynamicLibraries`; `xvfb`/`fonts-ibm-plex`
per §2/D1; no dbus/at-spi needed — probe in §2).

`ci.yml`:

```yaml
name: CI

on:
  pull_request:
    branches: [main]
  push:
    branches: [main]

# One in-flight run per ref: superseded pushes cancel their queued run
# and free the shared runners sooner.
concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: true

# Pinned in lockstep with the dev shell's toolchain (flake.lock freezes
# rust-bin.stable.latest; 1.98.0 today). Bump both together, deliberately
# (docs/0027 D4).
env:
  RUST_TOOLCHAIN: "1.98.0"

jobs:
  lint:
    runs-on: [self-hosted, linux, amd64]
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Bootstrap Rust toolchain
        run: |
          command -v rustup >/dev/null 2>&1 || \
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
              sh -s -- -y --profile minimal --default-toolchain none
          echo "$HOME/.cargo/bin" >> "$GITHUB_PATH"
          export PATH="$HOME/.cargo/bin:$PATH"
          rustup toolchain install "$RUST_TOOLCHAIN" --profile minimal \
            --component rustfmt --component clippy
          rustup default "$RUST_TOOLCHAIN"
          # Workspace-external target cache (docs/0027 D2): survives
          # checkout's `git clean -ffdx`, warm across runs.
          echo "CARGO_TARGET_DIR=$HOME/.cache/my-dashboard/target" >> "$GITHUB_ENV"
      - name: cargo fmt --all --check
        run: cargo fmt --all --check
      - name: cargo clippy --workspace --all-targets (zero warnings)
        run: cargo clippy --workspace --all-targets -- -D warnings
      - name: cargo check -p my-dashboard --no-default-features (bridge-less)
        run: cargo check -p my-dashboard --no-default-features

  test:
    runs-on: [self-hosted, linux, amd64]
    timeout-minutes: 45
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Bootstrap Rust toolchain
        run: |
          command -v rustup >/dev/null 2>&1 || \
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
              sh -s -- -y --profile minimal --default-toolchain none
          echo "$HOME/.cargo/bin" >> "$GITHUB_PATH"
          export PATH="$HOME/.cargo/bin:$PATH"
          rustup toolchain install "$RUST_TOOLCHAIN" --profile minimal
          rustup default "$RUST_TOOLCHAIN"
          echo "CARGO_TARGET_DIR=$HOME/.cache/my-dashboard/target" >> "$GITHUB_ENV"
      - name: cargo test --workspace (Xvfb, fresh runtime dir)
        run: |
          export XDG_RUNTIME_DIR="$(mktemp -d)"
          xvfb-run --auto-servernum cargo test --workspace
```

## 6. Testing & human checks

- **Local gates**: no Rust code changes in this PR; `cargo fmt --all
  --check`, `cargo clippy --workspace --all-targets` (zero warnings) and
  `cargo test --workspace` re-run before pushing (AGENTS.md discipline).
- **Headless probe** (§2) already reproduced the CI conditions locally:
  fresh `XDG_RUNTIME_DIR`, no display, no session bus — suite green.
- **Owner, on the PR**: provision the runner host (§5), watch this PR's CI
  run go green (cold run: toolchain download + full workspace build +
  gpui git deps — expect a long first run; warm runs should drop to a few
  minutes), then re-run once to feel the warm timing.
- **Owner, after merge**: add `lint` and `test` as **required status
  checks** for `main` (repo settings → Branches → protect `main`). This is
  what turns AGENTS.md's "run the gates before pushing" from convention
  into enforcement at merge time.

## 7. Follow-ups (GitHub issues, not this PR)

- **#155** — scheduled maintenance workflow pruning the D2 target cache.
- **#156** — port the Catppuccin theme jq/Whiskers gates to a nix-free CI
  step.
- (Informal, no issue yet: a shared composite action if a third job needs
  the bootstrap; release/packaging CI per the `docs/README.md` standing
  note.)
