# 0027 — CI: GitHub Actions Gates (fmt / clippy / test, self-sufficient, no Nix)

## 1. Summary

Enforce the AGENTS.md quality gates — `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets` at **zero warnings** (including the
`clippy.toml` token-discipline lints), and `cargo test --workspace` — on every
PR to `main` and every push to `main` that touches build-affecting paths
(documentation-only changes skip the runners — D7).

The CI is **nix-free** (owner decision, §4): the
workflow installs its own toolchain (rustup, pinned) and everything
GitHub's ubuntu images do **not** ship (the GUI dev stack + fonts),
assuming a hosted-image-parity baseline on the runner image (§2, §4, §5) —
GitHub-hosted ubuntu runners (`ubuntu-24.04`) ship it by construction.
Two jobs (`lint`, `test`),
one pinned third-party action (`actions/checkout`), no secrets.

The runner platform is **GitHub-hosted** (`ubuntu-24.04` standard hosted
runners, both jobs — switched from the owner's self-hosted docker runners
after they went down, §4): hosted VMs are just as ephemeral (fresh VM per
run, torn down after), so every run is still a full cold build; build
caching is deliberately deferred (#158).

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
- **No hosted image ships a GUI dev stack — but it does ship the build
  baseline.** Verified against the published `ubuntu-24.04` hosted-image
  manifest: `gcc`/`g++` 13.2, `make`, `binutils`, CMake, `pkg-config`,
  `xvfb`, `curl`, `git`, base fonts are preinstalled — and zero matches for
  `libgtk-3-dev`/`libwayland-dev`/`libxkbcommon-dev`/IBM Plex. GUI projects
  install those per job on GitHub's own runners; ours does the same. The
  GitHub's `ubuntu-24.04` hosted image ships that baseline by construction
  (§4), so the workflow assumes it and installs only the remainder (D1).
- **The runner platform is ephemeral**: one fresh VM per run, torn
  down after. Nothing persists between runs — no warm `target/`, no `~/.cargo`,
  no installed packages. CI runs on the platform confirmed the gaps
  progressively: missing `cc`/`xvfb` (first run), a non-root job user
  with no `sudo` — `apt-get` died on the lists lock with `Permission
  denied` (second run) — and after parity landed, a link-only gap,
  `-lxkbcommon-x11` (third run): clippy/check don't link, so only the
  test job's final binary link exposed the missing apt package.
  §5 provisioning list was wrong and is superseded by the in-workflow
  install plus the parity contract below (all three gaps were on the
  retired self-hosted platform — §4).
- **Dev-shell parity**: locally the gates run inside `nix develop` (toolchain
  frozen by `flake.lock` at 1.98.0, gpui dynamic libs on `LD_LIBRARY_PATH`,
  hermetic IBM Plex via `FONTCONFIG_FILE`). CI cannot replicate the nix parts —
  parity is therefore *approximate* (D1, D4) and the drift surface is recorded
  here deliberately.
- **No secrets** are needed by any gate; fork PRs are safe.

## 3. Design

### D1 — Jobs install only what hosted images lack

The runner-image contract is "GitHub ubuntu image parity" (§4): the build
baseline verified in §2 — gcc/g++/make/binutils/CMake/pkg-config/xvfb/git/
curl — is the **image's** job. The workflow installs only the remainder:

1. **System dependencies** — `apt-get install` of the gpui link libraries
   (mirroring the flake's `dynamicLibraries`: wayland, vulkan, libxcb,
   libxkbcommon (+ its `-x11` variant — a separate apt package, link-only:
   surfaced by `cargo test` because clippy/check never link), atk,
   fontconfig, glib, gtk3) plus `fonts-ibm-plex`
   (dev-shell font parity — not on hosted images either). The step is
   sudo-shimmed (`sudo` if present, else plain) so it works as root in the
   docker containers and as the sudo-capable `runner` user on hosted images.
2. **Rust toolchain** — rustup into `~/.cargo`, pinned `$RUST_TOOLCHAIN`
   with `rustfmt`/`clippy` components.

No `LD_LIBRARY_PATH` is needed: apt installs into the default loader path
(that env var exists in the dev shell only because nix stores libs elsewhere).

Drift surface vs the dev shell, accepted: apt lib versions float (deviation
from `flake.lock`), fonts resolve through system fontconfig rather than the
hermetic `FONTCONFIG_FILE` (`fonts-ibm-plex` keeps the typeface identical;
metric-sensitive tests would surface any residue). Mitigation if that ever
bites: ship a tiny in-repo `fontconfig.conf` and export `FONTCONFIG_FILE`.

Per-job cost on the ephemeral platform: ~30 s apt + ~40 s rustup — accepted;
see D2 for the bigger cold-build cost and #158. (The **interim parity-gap
state** — runs failing on missing `cc`/`xvfb-run` until the self-hosted
images reached parity — is history: the hosted `ubuntu-24.04` image ships
the baseline by construction, §4.)

### D2 — Ephemeral runners: cold by design, caching deferred

`CARGO_TARGET_DIR=$HOME/.cache/my-dashboard/target` is still exported, but on
the ephemeral platform this is a **no-op perf-wise** (fresh VM per run);
it is kept because it is harmless and a future persistent host gets warm
builds for free — and because an out-of-workspace target dir never interacts
badly with `actions/checkout`'s `git clean -ffdx`. Every run pays the full
cold build (cargo fetch + full workspace compile incl. the gpui git deps,
~10–15 min/job observed). Build-artifact caching (`rust-cache`/`sccache`,
quota/eviction questions included) is deliberately out of scope: follow-up
#158.

### D3 — Two jobs: `lint`, `test`

- `lint` — `cargo fmt --all --check`; `cargo clippy --workspace --all-targets
  -- -D warnings`; `cargo check -p dashboard-app --no-default-features`
  (bridge-less variant stays compilable).
- `test` — `cargo test --workspace` under `xvfb-run --auto-servernum` with a
  fresh temp `XDG_RUNTIME_DIR` (§2 explains why both are kept despite the
  headless-green probe).

Jobs are independent (no `needs`); the platform may run them concurrently or
queued. `concurrency: ci-<ref>` with `cancel-in-progress` kills superseded
runs. `timeout-minutes` (30/45) bounds a hung job.

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
update PRs; each ecosystem's updates are grouped into a single PR
(`groups` with an all-matching pattern). No other actions, no cache service,
no secrets — the workflow is
fork-safe by construction.

### D7 — Path-based filtering: docs-only changes don't build anything

Both triggers (`pull_request` and `push` to `main`) carry a `paths-ignore`
**denylist**: `docs/**` and every Markdown file (`**/*.md` plus `*.md` for
the repo root — README, AGENTS.md), plus the agent-skill/tooling files
`.agents/**` and `skills-lock.json` (added 2026-10-12 after a skills-only
PR paid two full cold builds for shell/markdown changes). First added
2026-09-17 after a docs-only
design PR paid two full cold builds (~25 min of runner time) for changes
that cannot affect a build.

- **Denylist, not an allowlist, deliberately.** The repo's substance is
  Rust, so an allowlist (`paths: crates/** …`) would silently drop CI for
  any *future* build-affecting path — a new top-level directory, a new
  config file — until someone remembers to extend the filter. A denylist
  fails safe: only paths that provably cannot affect a build skip it;
  everything else, known or future, runs both jobs.
- **Per-event semantics.** Filters re-evaluate on every event: a PR mixing
  docs and code runs; a PR that starts docs-only and later gains code runs
  from the push that adds it.
- **Required-check interaction.** A path-skipped run surfaces as a
  "Skipped" check run; if `lint`/`test` are later enabled as required
  status checks (a D6 repo-settings action), GitHub reports skipped checks
  as satisfying the requirement on path-filtered events, so docs-only PRs
  stay mergeable.
- **The gates themselves are unchanged.** AGENTS.md's "run the gates
  before pushing" stays the local contract; D7 only stops CI from
  re-enforcing them where they cannot matter.

### D6 — What deliberately stays out

- **Nix package/theme builds**: `nix build .#my-dashboard` and the
  Catppuccin Whiskers/jq gates are nix derivations by construction; with a
  nix-free CI they run **local-only**. Porting the theme gates to a shell
  step is follow-up #156.
- **Release-mode build**: dev-profile clippy + test compile every crate;
  a release build would add ~10 min/run for little signal.
- **Maintenance/scheduled jobs**: the platform is ephemeral, so there is
  nothing on the runner to prune (the persistent-host rationale is void);
  #155 stays open only until closed as obsolete.
- **Build-artifact caching**: #158.
- **Required status checks**: enabling `lint`/`test` in branch protection is
  a repo-settings action for the owner (§6), not workflow code.
- A shared composite action for the duplicated bootstrap steps — two copies
  are fine; refactor if a third job ever needs it.

## 4. Owner decisions (2026-09-07)

- **No Nix on CI** — D1/D6 reworked from the original nix-based proposal
  (rustup + apt instead of `nix develop`; nix jobs dropped).
- **Single PR** for this feature (docs + workflows + dependabot tweak
  together) — the small-feature flow of AGENTS.md.
- **Maintenance workflow** deferred to its own issue (#155).
- **Theme gates** stay local-only for now; nix-free port filed (#156).
- *(same day, after the first CI run)* **Runner platform is the owner's
  ephemeral docker product** (one container per run, teardown): the §5
  host-provisioning requirement is dropped in favor of the self-sufficient
  in-workflow apt step (D1); D2's warm-cache rationale is void — cold runs
  accepted, caching filed as #158.
- *(follow-up, same day)* **Workflow installs only what GitHub's ubuntu
  images lack** — the runner images will be updated to full hosted-image
  parity (gcc/make/cmake/pkg-config/xvfb/git/curl, …). Until that lands,
  CI on the platform is expected to fail on missing `cc`/`xvfb-run`;
  accepted in exchange for the lean, portable workflow.
- *(2026-09-24)* **Runner platform switched to GitHub-hosted ubuntu
  runners** — both jobs run on `ubuntu-24.04` standard hosted runners:
  the owner's self-hosted runners are down, and the hosted image meets
  the workflow's parity contract by construction (§2), so the interim
  parity-gap caveats are void. Workflow logic is unchanged — only the
  `runs-on` labels, surrounding comments and this doc move; the §5
  listing stays the original-PR snapshot (as D7 left it).

## 5. Implementation plan (this PR)

Files:

- `.github/workflows/ci.yml` — new (listing below).
- `.github/dependabot.yml` — add the `github-actions` ecosystem.
- `docs/0027-ci-workflows.md` — this doc; `docs/README.md` index row.

Runner-image contract: **GitHub-ubuntu-image parity** — git (checkout),
network (apt/crates.io/github/rustup), the §2 build baseline (gcc/g++/make/
binutils/CMake/pkg-config/xvfb/curl + base fonts), **and apt privilege**
(root containers or passwordless sudo for the job user — what hosted
images give their `runner` user). The workflow installs the GUI dev stack +
`fonts-ibm-plex` itself, guarded by an explicit contract error when neither
privilege path exists. (Written when CI ran on the owner's self-hosted
docker runners; since the 2026-09-24 hosted-runner switch (§4), GitHub's
`ubuntu-24.04` image satisfies this contract by construction.)

`ci.yml`:

```yaml
name: CI

on:
  pull_request:
    branches: [main]
  push:
    branches: [main]

# One in-flight run per ref: superseded pushes cancel their queued run
# and free the runners sooner.
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
      - name: Install system dependencies (GUI dev stack + fonts)
        # Installs ONLY what GitHub's ubuntu images do not ship
        # (docs/0027 D1): the gpui link libs + IBM Plex for font parity
        # with the dev shell. The hosted baseline — gcc/make/cmake/
        # pkg-config/xvfb/git/curl — is the runner image's job (parity
        # decision, docs/0027 §4; until the images reach it, runs fail
        # on missing cc/xvfb by design).
        run: |
          if [ "$(id -u)" -ne 0 ] && ! command -v sudo >/dev/null 2>&1; then
            echo "::error::cannot install packages: non-root job user and no sudo on this runner image."
            echo "::error::hosted-image parity requires root containers or passwordless sudo for the job user (docs/0027 §5)."
            exit 1
          fi
          SUDO="$(command -v sudo >/dev/null 2>&1 && echo sudo || true)"
          $SUDO apt-get update
          $SUDO env DEBIAN_FRONTEND=noninteractive apt-get install -y \
            libwayland-dev libvulkan-dev libxcb1-dev libxkbcommon-dev \
            libxkbcommon-x11-dev \
            libatk1.0-dev libfontconfig1-dev libglib2.0-dev libgtk-3-dev \
            fonts-ibm-plex
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
          # Out-of-workspace target dir (docs/0027 D2): a no-op perf-wise on
          # the ephemeral docker runners (fresh container per run); kept so
          # a future persistent host gets warm builds for free.
          echo "CARGO_TARGET_DIR=$HOME/.cache/my-dashboard/target" >> "$GITHUB_ENV"
      - name: cargo fmt --all --check
        run: cargo fmt --all --check
      - name: cargo clippy --workspace --all-targets (zero warnings)
        run: cargo clippy --workspace --all-targets -- -D warnings
      - name: cargo check -p dashboard-app --no-default-features (bridge-less)
        run: cargo check -p dashboard-app --no-default-features

  test:
    runs-on: [self-hosted, linux, amd64]
    timeout-minutes: 45
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Install system dependencies (GUI dev stack + fonts)
        # Installs ONLY what GitHub's ubuntu images do not ship
        # (docs/0027 D1): the gpui link libs + IBM Plex for font parity
        # with the dev shell. The hosted baseline — gcc/make/cmake/
        # pkg-config/xvfb/git/curl — is the runner image's job (parity
        # decision, docs/0027 §4; until the images reach it, runs fail
        # on missing cc/xvfb by design).
        run: |
          if [ "$(id -u)" -ne 0 ] && ! command -v sudo >/dev/null 2>&1; then
            echo "::error::cannot install packages: non-root job user and no sudo on this runner image."
            echo "::error::hosted-image parity requires root containers or passwordless sudo for the job user (docs/0027 §5)."
            exit 1
          fi
          SUDO="$(command -v sudo >/dev/null 2>&1 && echo sudo || true)"
          $SUDO apt-get update
          $SUDO env DEBIAN_FRONTEND=noninteractive apt-get install -y \
            libwayland-dev libvulkan-dev libxcb1-dev libxkbcommon-dev \
            libxkbcommon-x11-dev \
            libatk1.0-dev libfontconfig1-dev libglib2.0-dev libgtk-3-dev \
            fonts-ibm-plex
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
- **Owner, on the PR**: CI turns green once the runner images carry the
  hosted baseline (§4/§5) — until then the runs are **expected to fail** on
  missing `cc`/`xvfb-run`; that is the image-parity gap being surfaced, not
  a workflow bug. Every run is a cold build (~10–15 min/job) either way.
- **Owner, after merge**: add `lint` and `test` as **required status
  checks** for `main` (repo settings → Branches → protect `main`). This is
  what turns AGENTS.md's "run the gates before pushing" from convention
  into enforcement at merge time.

## 7. Follow-ups (GitHub issues, not this PR)

- **#155** — maintenance workflow (persistent-host rationale; likely to be
  closed obsolete given the ephemeral platform).
- **#156** — port the Catppuccin theme jq/Whiskers gates to a nix-free CI
  step.
- **#158** — build-artifact caching for the ephemeral runners
  (`rust-cache`/`sccache`, or accept cold permanently).
- (Informal, no issue yet: a shared composite action if a third job needs
  the bootstrap; release/packaging CI per the `docs/README.md` standing
  note.)
