# AGENTS.md

Working rules for AI agents in this repository. Follow precisely.

## Ground rules

- **`main` is protected.** All changes go through PRs; the product owner reviews, human-tests, and merges. Never push to `main`, never commit directly to it.
- **Stay focused.** Keep work scoped to the current task. If something unrelated needs fixing or you see an improvement opportunity, file a GitHub issue — do not fold it into the current PR.
- **Ask for help freely.** Do not guess where a decision is the product owner's to make. Blocking questions are cheap; wrong assumptions are not.
- **MCP servers are available** (not connected by default, but discoverable) and may be used when needed — including web search and context7 for verifying upstream sources. Never design or implement against remembered API details; verify against pinned upstream code/docs.

## Documentation

- **Every feature must be reflected in the docs.** The design docs in `docs/` are the source of truth and must always be up to date with the shipped behavior when a PR lands.
- Design docs are numbered (`docs/NNNN-slug.md`) and follow the lifecycle Draft → Accepted → Implemented.

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
- PR bodies carry a human-check list — interactive/visual verification is the product owner's job.
- Run the gates before pushing: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets` (zero warnings), `cargo test --workspace`.

## After merge — cleanup procedure

1. Hard-reset local `main` to `origin/main` (`git fetch && git reset --hard origin/main`) — rebased merges make `git pull` report divergent branches.
2. Delete merged branches: remote branches are usually auto-deleted; local branches need manual `-d` (use `-D` after a rebase-merge, the squashed commit SHA will not match).
3. Verify clean gates on `main`.
4. Remove any scratch files (e.g. under `/tmp`) used for issue bodies or smoke runs.
