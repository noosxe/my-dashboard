# AGENTS.md

Working rules for AI agents in this repository. Follow precisely.

## Ground rules

- **`main` is protected.** All changes go through PRs; the product owner reviews, human-tests, and merges. Never push to `main`, never commit directly to it.
- **Stay focused.** Keep work scoped to the current task. If something unrelated needs fixing or you see an improvement opportunity, file a GitHub issue — do not fold it into the current PR.
- **Ask for help freely.** Do not guess where a decision is the product owner's to make. Blocking questions are cheap; wrong assumptions are not.
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
- Run the gates before pushing: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets` (zero warnings), `cargo test --workspace`. The clippy gate includes the token-discipline lints from `clippy.toml` (docs/0002 D5/D9/D10: no ad-hoc `text_size`/`line_height`, no literal color constructors) — a `disallowed_methods` warning means a styling rule was broken, not a false positive; exceptions need `#[allow(clippy::disallowed_methods)]` plus a justifying comment.

## Debugging & verification — the debug bridge

The app ships an in-process debug bridge (docs/0010; `debug-tools`
feature, on by default). The `dashboard-debug` CLI attaches to the
running app and inspects and drives it — no screenshots, no OS-level
input injection. It is the primary way to verify UI-facing changes.

### Connect

1. The a11y bus (needed only by `tree` / `click` / `focus`): start it once
   per session with `at-spi-bus-launcher --launch-immediately &` (shipped
   in the dev shell). `logs`, `key`, `type`, `mouse` work without it.
2. `cargo run` the app.
3. `cargo run -p dashboard-debug -- <verb>`. Token and socket are
   auto-discovered from the runtime dir; if the CLI reports no token or
   socket, the app isn't running.

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
- Node ids are the `accesskit_id` strings from `tree` output (stable for
  the run); the top-level `nodes` map keys are ephemeral and change per
  dump.
- The first `tree`/node command flips the session `ScreenReaderEnabled`
  flag; a graceful app exit restores it, a `kill` leaves it true — reset
  with `busctl --user set-property org.a11y.Bus /org/a11y/bus org.a11y.Status
  ScreenReaderEnabled b false` after crashed runs.
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
4. Remove any scratch files (e.g. under `/tmp`) used for issue bodies or smoke runs.
