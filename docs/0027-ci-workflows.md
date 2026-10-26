# 0027 — CI: GitHub Actions Gates (fmt / clippy / test, self-sufficient, no Nix)

## 1. Summary

Enforce the AGENTS.md quality gates — `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets` at **zero warnings** (including the
`clippy.toml` token-discipline lints), and `cargo test --workspace` — on every
PR to `main` that touches build-affecting paths (documentation- and
repo-meta-only changes skip the runners — D7). Pushes to `main` re-run CI
only when the dependency cache-key inputs change (D9).

The CI is **nix-free** (owner decision, §4): the
workflow installs its own toolchain (rustup, pinned) and everything
GitHub's ubuntu images do **not** ship (the GUI dev stack + fonts),
assuming a hosted-image-parity baseline on the runner image (§2, §4, §5) —
GitHub-hosted ubuntu runners (`ubuntu-24.04`) ship it by construction.
Two gate jobs (`lint`, `test`) plus a `deny` dependency audit
(`cargo-deny` against the committed `deny.toml`, also run weekly — D8),
SHA-pinned third-party actions only, no secrets.

The runner platform is **GitHub-hosted** (`ubuntu-24.04` standard hosted
runners, both jobs — switched from the owner's self-hosted docker runners
after they went down, §4): hosted VMs are just as ephemeral (fresh VM per
run, torn down after). Build artifacts are cached with `rust-cache`
(D2, #158) and the apt `.debs` with `actions/cache` (D10); both caches
are saved only from `main`, and CI builds carry no debug info (D10).

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
  named explicitly in `flake.nix`, 1.98.0 — D4; gpui dynamic libs on `LD_LIBRARY_PATH`,
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
   The downloaded `.debs` are cached (D10): apt is pointed at a restored
   archive directory, so a warm run downloads nothing while `apt-get
   update`, dependency resolution and every dpkg maintainer script and
   trigger still run exactly as in a cold install.
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
see D2 for the bigger cold-build cost and #158. *(2026-10-26, D10)* Measured
on hosted runners: apt median ~27 s but a long tail up to 949 s (a slow
mirror served the 43.5 MB of `.debs` at 47 kB/s); with the `.deb` cache the
step is ~16–17 s and its download is 0 B. rustup is ~8–10 s. (The **interim parity-gap
state** — runs failing on missing `cc`/`xvfb-run` until the self-hosted
images reached parity — is history: the hosted `ubuntu-24.04` image ships
the baseline by construction, §4.)

### D2 — Ephemeral runners: cold by design, caching deferred (#158 landed it)

*(original decision, superseded below)* On the ephemeral platform, the
out-of-workspace `CARGO_TARGET_DIR=$HOME/.cache/my-dashboard/target`
export was a **no-op perf-wise** (fresh VM per run), kept only as
future-proofing; every run paid the full cold build (~10–15 min/job
observed). Build-artifact caching was deliberately deferred to #158.

*(landed, #158)* **`Swatinem/rust-cache` v2** now caches `~/.cargo`
(registry + git deps) and `./target` on both jobs — keyed automatically
by job id (separate lint/test caches), rustc version, and the
Cargo.lock/Toml hashes, plus every `CARGO*`/`RUST*`/`CC*`/`CMAKE*` env
var in the job (upstream-verified at the pinned v2.9.2 commit — so
`RUST_TOOLCHAIN` and D10's `CARGO_PROFILE_DEV_DEBUG` are key inputs, and
changing either starts a fresh cache instead of restoring an
incompatible one); placed *after* the toolchain bootstrap so the
rustc hash is correct; `cache-on-failure: true` so a red first run on a
branch still warms the cache. The out-of-workspace target dir export was
**removed**: rust-cache's default `workspaces` mapping is `. -> target`,
and a custom target dir outside the workspace would silently miss
target-artifact caching (upstream-verified) while offering nothing the
action does not already handle. Quota/eviction stays GitHub-side (10 GB
repo cache cap, LRU eviction; the action prunes non-dependency and
week-old artifacts before save).

*(amended 2026-10-26, D10)* **Saved from `main` only**
(`save-if: ${{ github.ref == 'refs/heads/main' }}`): PR runs restore
`main`'s entries and never save their own.

### D3 — Two jobs: `lint`, `test`

- `lint` — `cargo fmt --all --check`; `cargo clippy --workspace --all-targets
  -- -D warnings`; `cargo check -p dashboard-app --no-default-features`
  (bridge-less variant stays compilable).
- `test` — `cargo test --workspace` under `xvfb-run --auto-servernum` with a
  fresh temp `XDG_RUNTIME_DIR` (§2 explains why both are kept despite the
  headless-green probe).

A third job, `deny`, audits the dependency graph (D8).

Jobs are independent (no `needs`); the platform may run them concurrently or
queued. `concurrency: ci-<ref>` with `cancel-in-progress` kills superseded
runs. `timeout-minutes` (30/45) bounds a hung job.

### D4 — Toolchain pinned, in lockstep with the flake

The dev-shell flake is the reference toolchain and **names its release
explicitly** (`rust-bin.stable."1.98.0"`), so a `nix flake update` can
never move it. CI stays nix-free (§4) and pins the same release as a
single workflow-level env, `RUST_TOOLCHAIN: "1.98.0"`. A floating `stable`
on either side would let it diverge from the local gates and fail
green-local PRs mid-flight.

Four pins, one release, bumped **deliberately, in one PR**:

| Pin | Where |
|---|---|
| Dev shell | `flake.nix` — `rust-bin.stable."<x.y.z>"` |
| CI | `.github/workflows/ci.yml` — `RUST_TOOLCHAIN` |
| Verification image | `docker/Dockerfile` — `FROM rust:<x.y.z>@sha256:…` (docs/0071 D2, §5) |
| Workspace | `Cargo.toml` — `[workspace.package] rust-version` |

`scripts/check-toolchain-pins.sh` fails when they disagree; it is the
first step of the CI `lint` job and runs locally with no toolchain.

`rust-version` is the pinned toolchain, **not a support floor**: this is
an app built only with the toolchain above, never a library consumed
under older compilers. Keeping it equal means cargo's MSRV-aware
resolver (resolver 3) never holds back a release that toolchain can
build — a stale 1.85 had made security bumps need `--precise` (#641).
The bump needs a `flake.lock` that knows the new release (`nix flake
update rust-overlay`), and `rust-version` may only rise.

### D5 — Supply chain & hygiene

Exactly one third-party action, `actions/checkout`, pinned by commit SHA
(`3d3c42e…` = v7.0.1) with `persist-credentials: false`.
`dependabot.yml` gains a `github-actions` ecosystem so the pin gets weekly
update PRs; each ecosystem's updates are grouped into a single PR
(`groups` with an all-matching pattern). No other actions, no cache service,
no secrets — the workflow is
fork-safe by construction. Since then two more SHA-pinned actions joined
under the same rule and the same dependabot coverage: `Swatinem/rust-cache`
(#158) and `EmbarkStudios/cargo-deny-action` (D8), and
`actions/cache/restore` + `actions/cache/save` (v6.1.0, D10).

### D7 — Path-based filtering: docs-only changes don't build anything

The `pull_request` trigger carries a `paths-ignore`
**denylist** (the `push` trigger has its own, narrower allowlist — D9): `docs/**` and every Markdown file (`**/*.md` plus `*.md` for
the repo root — README, AGENTS.md), plus the agent-skill/tooling files
`.agents/**` and `skills-lock.json` (added 2026-10-12 after a skills-only
PR paid two full cold builds for shell/markdown changes), and the
repo-meta files `.pi/**` (agent working artifacts, untracked by the
same day's hygiene PR), `.gitignore` and `LICENSE*` (added 2026-10-12
after a `.gitignore`-only hygiene PR paid a full run). First added
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

### D9 — Pushes to `main`: re-run only when the cache inputs change

Every merge used to re-run `lint` and `test` on `main` in full, re-checking
code its PR run had just passed. In October 2026 that was ~48% of all
billable minutes (~1,045 of ~2,170). The `main` runs still have one job
nobody else can do: GitHub scopes caches by ref, and PR runs restore only
from their own ref or the base branch, so `main`'s rust-cache entries are
what keeps every new PR's build warm (D2). Without them PR jobs go back
to the cold ~10–15 min.

rust-cache saves only dependency artifacts (workspace crates are pruned
before save). Its key is the job id, rustc version and the
`Cargo.toml`/`Cargo.lock` hashes. A code-only merge therefore re-saves
an identical cache. So the `push` trigger carries a **`paths` allowlist**
of exactly the inputs that change what `main` should hold:

- `Cargo.lock` and `**/Cargo.toml`: the dependency set (the cache key);
- `deny.toml`: the audit policy (D8);
- `.github/workflows/ci.yml`: the workflow itself, which carries
  `RUST_TOOLCHAIN` (the rustc part of the key).

D7's denylist reasoning ("future paths keep CI by default") is about
*gating*. Gating happens on the PR. This list exists for *cache
freshness*. It names what the run is for, so an allowlist is the right
shape. Consequences:

- **No post-merge re-check of code-only merges.** A semantic conflict
  between two PRs that each passed against an older `main` now shows up
  on the next PR's run, not on `main`. That's acceptable for a
  single-maintainer repo with linear (rebase) merges. If it starts to
  bite, the fix is the merge queue or "require branches up to date" (a
  D6 repo-settings action), not a `main` re-run.
- **Cache longevity.** GitHub evicts entries untouched for 7 days, but a
  PR restore counts as a touch, so active development keeps `main`'s
  entries alive. After a quiet week, the first PR pays one cold build.
  *(amended 2026-10-26, D10)* Since PRs no longer save, that cold build
  repeats on every PR push until the next allowlisted push to `main`
  re-warms it — see D10 for the numbers and the trade-off.
- **The weekly `deny` schedule is unchanged** (D8). New advisories still
  surface on `main` without a push.

### D10 — Cheaper caches: `.deb` cache, no debug info, saves from `main` only

Profiled over the 69 runs from 2026-10-13 to 2026-10-23: apt install
time had a median of 27 s but 11 runs took over 2 minutes (173 min in
total); cache restore + save took 108 min. The test cache was ~1.7 GB and
the lint cache ~1 GB, and every PR saved its own copy (scoped to
`refs/pull/N/merge`) against the 10 GB repo cap, where it could evict the
`main` entries every PR restores from. A cold rebuild of the whole gpui
family (#674) also ran the runner out of disk (rust-lld: bus error, "No
space left on device"). Three changes, `lint` and `test` stay separate
jobs (D3):

1. **apt `.debs` are cached, not installed files.** Before the install
   step, `actions/cache/restore` restores `/tmp/apt-archives/*.deb`; the
   install step passes `-o Dir::Cache::Archives=/tmp/apt-archives -o
   APT::Keep-Downloaded-Packages=true` to `apt-get install`, then runs
   `apt-get autoclean` with the same options so superseded `.debs` don't
   pile up. apt reuses each cached `.deb` whose checksum matches the
   freshly updated (signed) index and downloads only the rest, so a new
   upstream version or a changed runner image costs one partial download,
   never a broken install. The directory sits under `/tmp` so apt's
   `_apt` sandbox user can reach it (under `$HOME` apt falls back to an
   unsandboxed download, with a warning). The key is
   `apt-<job>-<os>-<arch>-<hash of ci.yml>` (the package list lives in
   the workflow), with the prefix up to the hash as restore key. Like
   rust-cache, the `.debs` are saved only on `main` and only when the
   restore wasn't an exact hit. The D1 root/sudo guard is unchanged.

   *Rejected: `awalsh128/cache-apt-pkgs-action`* (read at v1.6.3,
   `553a35b`). On a cache miss its install script runs
   `curl …/ilikenwf/apt-fast/master/quick-install.sh | bash`, an unpinned
   remote script, which breaks D5. On a hit it untars the files that
   `dpkg -L` listed over `/` and runs no dpkg triggers (fontconfig, ldconfig,
   …); maintainer scripts run only with `execute_install_scripts: true`.
   The packages are never registered with dpkg, and the key covers only
   the package names, not their versions or the image. It also always
   calls `sudo`, which drops D1's root-container path. Caching the `.debs`
   keeps apt and dpkg fully in charge and needs only `actions/cache`.

2. **No debug info**: workflow-level `CARGO_PROFILE_DEV_DEBUG: "0"`
   overrides the workspace's `debug = "limited"`. Nothing in CI runs a
   debugger. The `test` profile inherits `dev` (Cargo reference,
   Profiles), so `cargo test` follows; checked with a probe crate: `cargo
   test -v` drops `-C debuginfo=1` under the variable, so no separate
   `CARGO_PROFILE_TEST_DEBUG` is needed. rust-cache hashes `CARGO*` env
   into its key (D2), so the first run after this change starts a new
   cache, and the old one is never restored into a different-profile
   build.

3. **Caches are saved from `main` only**: rust-cache's
   `save-if: ${{ github.ref == 'refs/heads/main' }}` (input verified at
   the pinned v2.9.2 source: `save.ts` returns before saving unless it is
   `"true"`), and the `.deb` save step carries the same condition. PRs
   restore `main`'s entries (GitHub lets a PR read its base branch's
   caches) and never save.

**Interaction with D9.** `main` runs only when `Cargo.lock`, a
`Cargo.toml`, `deny.toml` or `ci.yml` changes. Those are exactly the
inputs of the rust-cache and `.deb` keys, so every change that would make
`main`'s cache stale also re-saves it: a dependency change re-warms the
cache on its merge, and so does this PR's own merge (it changes `ci.yml`
and the env part of the key). Code-only merges leave the dependency set,
and so the cache, unchanged. The weekly schedule still runs only `deny`
(D8) and saves nothing.

**Trade-offs, accepted:**

- **A dependency PR rebuilds its changed crates on every push.** It
  restores `main`'s older entry by prefix and can't save its own, so a
  big bump (the gpui family in #674) pays its rebuild on each push until
  it merges. After that, `main` holds the new graph.
- **Quiet weeks.** GitHub evicts entries untouched for 7 days, and a PR
  restore counts as a touch. With no PR runs for 7 days, `main`'s entries
  are evicted and nothing re-creates them until the next allowlisted push
  to `main`. Until then every PR push is a cold build (~6–7 min `lint`,
  ~10 min `test`, measured below). In practice the weekly dependabot cargo
  PR's merge changes `Cargo.lock` and re-warms `main`. A manual re-warm
  path (a `workflow_dispatch` trigger, or `lint`/`test` on the weekly
  schedule) is deliberately not added; that is the owner's call.

**Measured (2026-10-26).** "Before" is historical runs with
`debug = "limited"`: warm = PR runs restoring an exact `main` hit (runs
39891076007, 39887822010); cold = runs with no rust-cache hit (39511003531
and 39523605469 for `test`, 39499372740 for `lint`). "After" is #675 run
40390032550 (attempt 1 cold, attempt 2 warm, with PR saves temporarily
enabled to measure).

| | Before | After |
|---|---|---|
| `test` cache size | 1712 MB | 831 MB (−51%) |
| `lint` cache size | 975 MB | 563 MB (−42%) |
| apt step | median 27 s, tail to 949 s | 16–17 s warm (0 B downloaded); 22–26 s cold |
| rust-cache restore, warm (`test` / `lint`) | 56–60 s / 23–42 s | 24 s / 15 s |
| `cargo test`, warm | 103–106 s | 85 s |
| `cargo clippy`, warm | 16–26 s | 19 s |
| `test` job, warm | 193–210 s | 143 s |
| `lint` job, warm | 96–100 s | 78 s |
| `cargo test`, cold | 612–622 s | 535 s |
| `clippy` + bridge-less `check`, cold | 370 + 43 s | 303 + 33 s |
| `test` / `lint` job, cold | 680–698 s / 482 s | 598 s / 401 s |
| rust-cache save (cold, `test` / `lint`) | 33 s / 23 s | 25 s / 20 s |

Link time is not broken out on its own; the warm `cargo test` step
(compile workspace crates, link test binaries, run) is where it shows.
Also not counted above: PR runs no longer save their own 1–2.7 GB entries,
and the apt long tail is gone because the warm path downloads nothing.

### D6 — What deliberately stays out

- **Nix package/theme builds**: `nix build .#my-dashboard` and the
  Catppuccin Whiskers/jq gates are nix derivations by construction; with a
  nix-free CI they run **local-only**. Porting the theme gates to a shell
  step is follow-up #156.
- **Release-mode build**: dev-profile clippy + test compile every crate;
  a release build would add ~10 min/run for little signal.
- **Maintenance/scheduled jobs**: the platform is ephemeral, so there is
  nothing on the runner to prune (the persistent-host rationale is void);
  #155 stays open only until closed as obsolete. The weekly `deny` run
  (D8) is the one scheduled job — it watches the advisory database, not
  the runner.
- **Build-artifact caching**: #158.
- **Required status checks**: enabling `lint`/`test` in branch protection is
  a repo-settings action for the owner (§6), not workflow code.
- A shared composite action for the duplicated bootstrap steps — two copies
  are fine; refactor if a third job ever needs it.

### D8 — Dependency audit: `cargo-deny` (#639)

A third job, `deny`, runs `cargo deny check` against the committed
`deny.toml` at the repo root through `EmbarkStudios/cargo-deny-action`,
pinned to its peeled v2.1.1 commit (bundles cargo-deny 0.20.2). The action
runs in its own container with its own cargo, so the job needs no
toolchain bootstrap, apt step or cache, and finishes in about a minute.

**Policy** (owner decisions 2026-10-22, §4):

| Check | Fails the job on | Notes |
|---|---|---|
| `advisories` | vulnerabilities, and unmaintained / unsound / yanked crates, direct **and** transitive | Exceptions go in `ignore`, each with a reason and an issue |
| `licenses` | any license outside the explicit allow-list | Allow-list built from what the graph uses; workspace crates (`publish = false`) are skipped |
| `sources` | any registry other than crates.io; any git source other than the pinned gpui-component rev; any git dep without `rev = …` | Bumping the gpui-component rev in `Cargo.toml` means bumping `allow-git` too |
| `bans` | wildcard version requirements | Duplicate versions only **warn** (the gpui stack brings many — `getrandom`, `itertools`, `hashbrown`, …); promote to `deny` when the graph allows |

The graph is audited for the shipped platforms only (`[graph] targets`:
x86_64/aarch64 Linux and macOS) with all features enabled.

**Triggers.** The `deny` job rides the workflow's normal triggers (PRs to
`main` under the D7 path filter; pushes to `main` under the D9 allowlist,
which names `Cargo.lock`, every `Cargo.toml` and `deny.toml`, so a
dependency change always runs it). It also runs on a weekly `schedule`
(Mondays 06:00 UTC), because new advisories land in the RustSec database
without any change here. `lint` and `test` skip the scheduled event
(`if: github.event_name != 'schedule'`), and scheduled runs take their own
concurrency group so they never cancel a push run on `main`. A red
scheduled run means a new advisory: triage it the same way as below.

**Run it locally** before pushing a change to `Cargo.toml`, `Cargo.lock`
or `deny.toml` (it is not an AGENTS.md gate; CI enforces it):

```bash
cargo install --locked cargo-deny@0.20.2   # once; matches the CI action
cargo deny check                            # all four checks
cargo deny check advisories                 # or one at a time
```

**Adding an exception.** First try to remove the finding: for an advisory,
`cargo update -p <crate>` (or `--precise <version>`) often pulls in the
patched release without touching any manifest. When it can't be fixed
here (upstream-only, needs a semver-major bump), file a GitHub issue,
then add the narrowest exception with a comment that names the issue:

- advisory → `[advisories] ignore = [{ id = "RUSTSEC-…", reason = "… (#NNN)" }]`;
- license → add it to `[licenses] allow` if it is acceptable for the whole
  graph, otherwise a per-crate `[licenses] exceptions` entry;
- source → an `allow-git` entry with the exact URL and `?rev=`.

cargo-deny warns when an `ignore` entry no longer matches anything; delete
it then, and close the issue.

Initial triage (#639): six vulnerabilities (rustls, rustls-webpki,
webpki-roots, quick-xml via zbus_xml) were fixed by lockfile bumps. Eight
unmaintained advisories have no patched release and come in through the
upstream GPUI stack, so they are ignored and tracked in #640.

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
- *(2026-10-22, #639)* **Dependency audit** (D8): advisories **fail** the
  build (not warn-only), licenses and sources fail, bans warn on
  duplicates. The audit runs on PRs/pushes **and** weekly on a schedule.
- *(2026-10-22, #641)* **Explicit toolchain everywhere** (D4): the flake
  names its release instead of `stable.latest`, `rust-version` rises from
  the stale 1.85 to the pinned 1.98.0, and a pin-agreement check in the
  `lint` job enforces the four-way lockstep. The dev shell is the only
  local toolchain (no Homebrew/rustup rust, docs/0001 §4).
- *(2026-10-23)* **Push-to-`main` runs trimmed** (D9): pushes to `main`
  re-run CI only for `Cargo.lock`/`Cargo.toml`/`deny.toml`/`ci.yml`
  changes, keeping the PR-shared caches warm without re-checking every
  merge (~48% of October's minutes). The weekly audit stays.
- *(2026-10-26)* **Cheaper caches** (D10): the apt `.debs` are cached
  (`actions/cache`, not `awalsh128/cache-apt-pkgs-action`, which runs an
  unpinned remote script), CI builds drop debug info, and rust-cache plus
  the `.deb` cache save from `main` only. `lint`/`test` stay separate
  jobs. Measured: caches −42 to −51%, warm `test` job 193–210 s → 143 s,
  warm `lint` 96–100 s → 78 s, apt 0 B downloaded when warm.

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
- **#640** — unmaintained transitive crates ignored in `deny.toml`
  (upstream GPUI stack); drop each ignore as upstream moves off it.
- **#158** — build-artifact caching for the ephemeral runners
  (`rust-cache`/`sccache`, or accept cold permanently).
- (Informal, no issue yet: a shared composite action if a third job needs
  the bootstrap; release/packaging CI per the `docs/README.md` standing
  note.)
