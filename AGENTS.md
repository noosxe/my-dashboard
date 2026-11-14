# AGENTS.md

Working rules for AI agents in this repository. Follow precisely.

## Ground rules

- **`main` is protected.** All changes go through PRs; the product owner reviews, human-tests, and merges. Never push to `main`, never commit directly to it.
- **Scratch files go in `.scratch/`, never `/tmp`.** `/tmp` may be a RAM-backed mount that is wiped on every restart, and the machine can lose power without warning. Working files that must survive a restart but don't belong in a commit (issue and PR body drafts, mockups in progress, review notes, receipt output being triaged) go in `.scratch/` at the repo root, which is gitignored. Use a subdirectory per task. Anything worth keeping long-term moves out of `.scratch/` when the task ends: into the repo if it belongs there (docs, goldens), otherwise to the project's private notes.
- **Stay focused.** Keep work scoped to the current task. If something unrelated needs fixing or you see an improvement opportunity, file a GitHub issue — do not fold it into the current PR.
- **Focused modules, not huge files.** New code goes into a module tree where each file has one clear concern (a few hundred lines is the target), with tests next to the code they cover. Don't keep growing the existing multi-thousand-line files (for example `pr_page.rs`, `service.rs`, `page.rs`). When a feature touches one of them, put the new code in a new submodule. Splitting an existing large file is its own focused PR, filed as an issue, never folded into a feature PR.
- **Ask for help freely.** Do not guess where a decision is the product owner's to make. Blocking questions are cheap; wrong assumptions are not.
- **Everything you write is public.** The repo is public: source, commit messages, issues, PR bodies, comments and the wiki can all be read by anyone. Never write any of the following into them:
  - names of the owner's private repositories;
  - machine hostnames;
  - the owner's personal or system-config repos and their paths;
  - signing identities (Apple TeamIdentifier, certificate expiry dates);
  - real data that the app fetched from private accounts.

  Use neutral placeholders instead: `<private-repo>`, `<host>`, "a downstream flake consumer", `<redacted>`. Use the synthetic GitHub fixtures (docs/0081 §12.1) for examples. This applies equally to pasted logs, receipts and screenshots. If something slips through, tell the product owner: editing the text is not enough, because GitHub keeps edit history and old revisions must be deleted by hand.
- **MCP servers are available** (not connected by default, but discoverable) and may be used when needed — including web search and context7 for verifying upstream sources. Never design or implement against remembered API details; verify against pinned upstream code/docs.

## Documentation

- **Every feature must be reflected in the docs.** The design docs in `docs/` are the source of truth and must always be up to date with the shipped behavior when a PR lands.
- Design docs are numbered (`docs/NNNN-slug.md`).
- **No docs lifecycle.** There is no draft/accepted/implemented state tracking: docs are written and sent with the PR that carries them (the design-doc PR for big features, the implementation PR for small ones), and **a merged PR means the docs are accepted**. Never open separate status-flip PRs after implementation.

## Feature workflow

### Big features — design first

1. **Design doc first, as its own PR.** No draft marker needed — a PR with a proper summary description is enough. The PR contains the design doc and nothing else (docs-only).
2. The product owner reviews and merges the design-doc PR.
3. After the design-doc PR is merged, the agent must:
   - clean up related stale branches (remote and local);
   - file a GitHub issue for the implementation — split into multiple issues if the scope is too big for a single issue;
   - **ask for the go-ahead**;
   - wait for the product owner's go before making any code changes.

### Small requested features — one PR

- No separate doc PR and no GitHub issue: documentation and code changes go into the **same PR**.
- Like all feature PRs, the product owner is responsible for human tests and merging.

### PR discipline (both flows)

- **PRs stay focused.** Any issue that does not directly interfere with the current feature implementation must be filed on GitHub for later pickup. Suggested improvements and follow-ups must also be filed, not implemented opportunistically.
- **Cut work is filed, not memorized.** Anything a design doc deliberately excludes (its *Non-goals* section) or a PR consciously defers gets a GitHub issue before the doc's implementation merges — the non-goal section records the decision, the issue keeps the work actionable. Tag those issues `deferred` so consciously postponed work is one label filter away; pure roadmap items (docs/0029 §5) that were never in anyone's scope stay untagged.
- PR bodies carry a human-check list — interactive/visual verification is the product owner's job. For UI-facing changes, exercise the change through the debug bridge first (see below) and say so in the PR body.
- **UI-facing PRs additionally carry the 0065 gate receipts**: run `dashboard-debug audit` over every touched surface (and `tree --tab-stops` for keyboard coverage), paste the outcome — FAILs fixed before review, findings/incomplete triaged in the PR body against the docs/0065 Appendix A rubric, never silently. If a committed structural golden exists for the surface (`crates/debug/verify/snapshots/`), run `assert-baseline` too and explain any drift. New surfaces get a golden (captured in the containerized verification
env, docs/0071) in the same PR.
- **Visual critique ritual (0065 D7)** for UI-facing PRs: in the containerized verification env (docs/0071), `critique <surface>` before and after the change (screenshot + tree snapshot, never pixels alone), run a multimodal critique against docs/0065 Appendix A, fix named findings, re-capture — **max 3 iterations**; findings still standing after the cap ship as known-issues listed in the PR body, never silent debt. The PR carries the before/after pair and the findings list.
  - **Who judges:** Claude models judge the frames with their own vision: read the screenshot and the tree snapshot directly and critique against Appendix A. They do not run the local judge panel (`critique --panel`, docs/0065 D7). Other models use the panel as documented there.
- **Name the anchor (0065 D8)**: every new UI surface names its reference anchor from `docs/references/` in the PR body and composes toward it (rhythm, hierarchy, restraint — not pixel copies); the gpui-component `story` gallery is the widget-level secondary anchor. Deviations from the named anchor are noted in the PR body, never smuggled.
- Run the gates before pushing: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets` (zero warnings), `cargo test --workspace`. The clippy gate includes the token-discipline lints from `clippy.toml` (docs/0002 D5/D9/D10: no ad-hoc `text_size`/`line_height`, no literal color constructors) — a `disallowed_methods` warning means a styling rule was broken, not a false positive; exceptions need `#[allow(clippy::disallowed_methods)]` plus a justifying comment.

## Debugging & verification — the debug bridge

The app ships an in-process debug bridge (docs/0010; `debug-tools`
feature, on by default). The `dashboard-debug` CLI attaches to the
running app and inspects and drives it — no screenshots, no OS-level
input injection. It is the primary way to verify UI-facing changes.
Sway sessions and the a11y stack need Linux; on this macOS-only setup
they run inside the containerized verification env (docs/0071):
`scripts/verify.sh <cmd…>` mounts the working tree in the pinned
image with every session verb receipt-ready — see docs/0071 §4 for
the canonical gate and receipt recipes (one receipt chain = one
container).

### Connect

The default flow is a **live debug session** (docs/0055): isolated,
headless, disposable.

1. `cargo run -p dashboard-debug -- session start --name <n>` — spawns a
   headless sway compositor plus the app, owns the a11y bus step, and
   flips/records `ScreenReaderEnabled` itself. `--size WxH` fixes the
   window (receipt coordinates assume it).
2. Drive it with `dashboard-debug --session <n> <verb>` (or
   `DASHBOARD_DEBUG_SESSION=<n>`). `doctor [--session <n>]` when anything
   feels off — it names the failure class, not just the symptom.
3. `session stop <n>` when done — graceful `app.quit` first, restores the
   flag, purges the session dir. `--keep` preserves artifacts for
   inspection.

Attaching to an app you started yourself (`cargo run` + default
discovery) still works for quick reads; the a11y bus step then is
`at-spi-bus-launcher --launch-immediately &` (dev shell). Committed
receipt scripts (`run crates/debug/verify/*.txt`) replay a whole
verification in one command.

### Verify and drive

- `dashboard-debug tree` — the component tree with roles, labels,
  window-relative bounds and focus states; `--json` prints the raw dump
  (adds source locations in dev builds and the `a11y_merge` drift
  counters).
- `dashboard-debug logs -f` — live app logs while you exercise the UI;
  correlate actions with events instead of guessing from code.
- `click <accesskit_id>`, `focus <id>`, `key tab`, `type "text"`,
  `mouse move|down|up|scroll` — full UI control. None of it moves the
  OS pointer or steals the keyboard: the user's session is untouched.

### Rules

- **Never reach for OS-level input injection** (xdotool, ydotool, enigo,
  …). The bridge is the sanctioned path and exists precisely so agents
  don't disturb the owner's machine.
- **No UI captures on the host** (docs/0071 D9). Every UI receipt —
  `audit`, `tree --tab-stops`, `assert-baseline`/goldens, `critique`
  screenshots — is captured only in the containerized verification env
  (docs/0071), never on the macOS host. Surfaces that need signed-in or
  external data render there through the GitHub fixture mode (docs/0081
  §12.1); a surface it can't reach gets a fixture extension, not a host
  capture.
- Node ids are the `accesskit_id` strings from `tree` output (stable for
  the run); the top-level `nodes` map keys are ephemeral and change per
  dump.
- `ScreenReaderEnabled` flips when the bridge activates a11y. Sessions
  record the starting value and `session stop` restores it (verified even
  after a SIGKILLed app); a crashed attach outside a session leaves it
  true — `doctor` names the drift and prints the restore command.
- New interactive elements need `.id(...)` plus a role (docs/0010 D10), or
  they are invisible to the dump and to `click` — treat a missing node in
  `tree` as a bug in the element, not in the bridge.
- After a `--no-default-features` build, rebuild with default features
  before launching the app (the bridge-less binary overwrites
  `target/debug/my-dashboard`).
- Don't leave `logs -f` running when you're done.

The full protocol — all commands, parameters and error codes — is
docs/0010 §3.

## After merge — cleanup procedure

1. Hard-reset local `main` to `origin/main` (`git fetch && git reset --hard origin/main`) — rebased merges make `git pull` report divergent branches.
2. Delete merged branches: remote branches are usually auto-deleted; local branches need manual `-d` (use `-D` after a rebase-merge, the squashed commit SHA will not match).
3. Verify clean gates on `main`.
4. Remove the task's `.scratch/` subdirectory once nothing in it is still needed (issue bodies, smoke runs).
