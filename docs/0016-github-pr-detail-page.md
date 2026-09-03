# 0016 — GitHub module: PR detail page (review, comment, merge, close)

- **Date:** 2026-09-03
- **Depends on:** 0003 (§4.3 module-internal sub-pages), 0005 (GitHub module),
  0007 (snapshot + edge-freshness pattern), 0009 (repo view rows), 0010 (debug
  bridge, D10 id rule), 0013 (tracked fetches), 0014 (crumb prefix + suffix rule)
- **Closes the design need of:** owner request — in-app PR review page opened
  from a repo-view PR row ("PR title on the top, description … editable, list of
  commits which is scrollable, the conversation, a comment input, checks, and
  changed files"; tabs like github.com's PR view)

## 1. Context & problem

The repo view lists open PRs from the global snapshot (0007), but a PR row is a
dead end: clicking hands off to github.com in the browser (0012 D4 parity).
Reviewing means leaving the app. The owner wants the full review loop — read the
description and conversation, inspect commits/checks/diff, comment or submit a
review, and merge or close — in-app, on a dedicated page.

Two gaps make "just render it" insufficient:

1. **No detail data.** The snapshot carries only summary fields per PR
   (`title/number/draft/author/updatedAt/url/statusCheckRollup`, gql.rs
   `PullRequest`). Body, commits, reviews, comments, files, and mergeability
   are not in any payload we hold — they must be fetched when the page opens.
2. **No sub-page yet.** The GitHub module has exactly one screen (repo list +
   repo view). 0003 §4.3 prescribed the pattern (module-internal screen state +
   crumbs) and 0014 built the crumb machinery for it — the prefix
   reconciliation rule (0014 D3) is suffix-preserving by design, and the repo
   crumb's `on_click` was deferred to "the future subpages doc". This is that
   doc.

## 2. Scope

In:

- **Entry:** clicking a PR row in the repo view opens the in-app PR page (the
  browser hand-off moves to an explicit "open on GitHub" icon in the page
  header).
- **Full-content takeover:** while a PR is open, the PR page replaces the
  module's entire content (org Select + repo list hidden) — review wants width.
- **Header (always visible):** state badge, title, `#number`, author + branch
  meta line, refresh + open-on-GitHub icon buttons.
- **Four tabs** (gpui-component `TabBar`, `underline()` variant) with live
  counts: **Conversation** (default), **Commits**, **Checks**, **Files
  changed**. Only the active tab renders; each tab keeps its own scroll handle.
- **Conversation:** description card (body editable in place), timeline of
  comments + review events, and a comment box with a **full action selector**
  — Comment / Approve / Request changes.
- **Merge/close panel** in the conversation right rail: mergeability line,
  merge-method choice limited to **the methods the repo enables**, Merge and
  Close actions.
- **Commits tab:** scrollable commit rows.
- **Checks tab:** rollup summary + check rows, details link out.
- **Files changed tab:** summary line + per-file collapsible cards with a
  unified diff rendered from the REST patch.
- **Data:** one GraphQL detail query on open + refetch after each mutation and
  on manual refresh; one lazy REST fetch for file patches. Mutations:
  `addComment`, `addPullRequestReview`, `updateIssue` (body), `mergePullRequest`,
  `closePullRequest`.
- Crumbs `Home > GitHub > <repo> > #<number>`; repo crumb becomes clickable
  (back to repo root); `Esc` closes the page.
- Doc + README index row (this PR); implementation lands as staged PRs (§6).

Out (non-goals):

- **No comment edit/delete** on conversation comments — description editing
  only. Follow-up issue (§11).
- **No markdown rendering** — bodies render as plain text first cut. Follow-up.
- **No split diff view, no intra-line highlighting, no syntax highlighting** in
  the diff renderer. Follow-ups.
- **No in-app check-run log viewing** — check details link out to github.com.
- No draft reviews / line comments / review threads; no review dismissal.
- No PR creation, no label/assignee/reviewer management, no reactions.
- No pagination beyond the first 100 commits/reviews/comments (GraphQL
  `first: 100`) or the REST files cap (300 files, ~400 patches); a "+N more"
  hint is a follow-up (existing #87 covers list caps).
- No persistence: PR page state is session-only (nothing enters `shell.json`,
  the snapshot cache, or any new cache).
- No cadence change: the detail query runs on open/refresh/mutation only —
  background ticks do not touch the open page.

## 3. Verified facts

**Schema (docs.github.com GraphQL reference, verified 2026-09-03):**

- `repository(owner, name).pullRequest(number)` returns a single PR.
- `PullRequest` fields used: `id`, `title`, `body`, `state`, `isDraft`,
  `mergeable`, `mergeStateStatus` (`BEHIND/BLOCKED/CLEAN/DIRTY/DRAFT/
  HAS_HOOKS/UNKNOWN/UNSTABLE`), `baseRefName`, `headRefName`, `author`,
  `createdAt`, `commits(first: n) { totalCount nodes { commit { oid
  messageHeadline committedDate author { name avatarUrl user { login } } } } }`,
  `reviews(first: n) { nodes { author state body submittedAt } }`,
  `comments(first: n) { nodes { author body createdAt } }`,
  `reviews(first: n) { totalCount nodes { author state body submittedAt } }`,
  `comments(first: n) { totalCount nodes { author body createdAt } }`,
  `changedFiles` (top-level `PullRequest` field — corrected at stage-1
  implementation, bridge-verified against the live schema: `changedFiles`
  lives on the PR object, not the `files` connection, and the
  `…IfAvailable` variant the reference recommends is not in the public
  schema),
  `files(first: n) { totalCount }`, `statusCheckRollup { state
  contexts(first: n) { nodes … } }`. **The `files` connection carries no
  patch text** — patch hunks exist only on the REST endpoint.
- Mutations (input fields verified): `addComment { subjectId body }`;
  `addPullRequestReview { pullRequestId event body }` with
  `PullRequestReviewEvent ∈ { APPROVE, COMMENT, REQUEST_CHANGES, DISMISS }`
  (`body` optional — an approval may be empty); `updateIssue { id body }` (a PR
  *is* an issue; this edits the body); `mergePullRequest { pullRequestId
  mergeMethod ∈ { MERGE, SQUASH, REBASE } }`; `closePullRequest {
  pullRequestId }`.
- Per-repo merge-method switches: `Repository.mergeCommitAllowed`,
  `squashMergeAllowed`, `rebaseMergeAllowed` (all `Boolean!`; all three
  confirmed on the Repository reference page).
- REST (live check, 2026-09-03): `GET /repos/{o}/{r}/pulls/{n}/files` returns
  per-file `filename`, `additions`, `deletions`, `changes`, `status`, and
  `patch` (absent for binary/oversized files → render a placeholder row).
- **Union verified (stage 3, live 2026-09-03):** `statusCheckRollup.contexts`
  carries `CheckRun { name status conclusion detailsUrl startedAt completedAt }`
  and `StatusContext { context state targetUrl }`; `conclusion` is `null` while a
  run is queued/in progress (the phase lives in `status`), the rollup's own `state`
  is `SUCCESS|FAILURE|PENDING`, and PRs without checks return a `null` rollup
  (live-checked against `microsoft/vscode`, `grafana/grafana`, `noosxe/worker-mcp`).

**Upstream widgets (pinned gpui-component @ 0c746df):**

- `ui::tab::{Tab, TabBar}` with `.underline()` variant — the GitHub-style tab
  chrome; `selected_index` + `on_click` drive an enum in our state.
- `ui::input::textarea::{Textarea, TextareaState}` — multiline input with
  `.h/.appearance/.bordered/.disabled/.role/.aria_label`.
- `ui::{Badge, GroupBox, Skeleton, Button}` (Button has the `danger` variant
  used by settings.rs), `Select` (0009), `List` delegate infra.
- Theme: semantic `success/danger/warning` (+foregrounds) tokens; a
  `mono_font_family` token for SHAs/paths/diff text — no literal colors or
  ad-hoc fonts needed anywhere in this page (D10).

**Our tree (`main` @ 4c5429c):**

- `render_pr_row` (page.rs) is the click seam; rows already carry
  `.id("pr-{key}")` + `ListItem` role (0015 fix #82).
- `GithubPage::render` is a two-column `h_flex`; the takeover swaps the whole
  child for the PR page (D1).
- `run_specs`/`run_spec_tracked` (page.rs / service.rs, 0013 D4) is the fetch
  path a detail fetch plugs into; `FetchSpec` kinds extend.
- `reconcile_repo_crumb` (page.rs, 0014 D3) already preserves a sub-page
  suffix (`reconcile_preserves_subpage_suffix` test) and `Crumb.on_click`
  exists in core nav.rs — no core changes needed.

## 4. Owner decisions (recorded 2026-09-03)

| Q | Decision |
| --- | --- |
| Q1 — full takeover vs right column | **Full takeover** — the PR page replaces the module's whole content; the left repo rail hides while a PR is open. |
| Q2 — review actions | **Full support** — the comment box gets an action selector: Comment / Approve / Request changes (`addComment` / `addPullRequestReview`). |
| Q3 — merge methods | **All enabled options surfaced** — the method list comes from the repo's `mergeCommitAllowed`/`squashMergeAllowed`/`rebaseMergeAllowed`. |
| Q4 — comment edit/delete | **Out of scope** — filed as a follow-up issue; only the description is editable. |

## 5. Design

### D1 — Navigation: sub-page state + full takeover

```rust
// page.rs
pub struct GithubPage {
    // … existing fields …
    pr: Option<Entity<pr_page::PrDetailPage>>,   // Some = PR page owns the module content
}
```

- `render_pr_row`'s `on_click` replaces `cx.open_url` with
  `open_pr(repo_full_name, pr_number, window, cx)`: constructs `PrDetailPage`
  (which kicks the detail fetch, D2), stores it, notifies.
- `Render for GithubPage`: `if let Some(pr) = &self.pr { return pr.clone()
  .into_any_element(); }` — org Select and repo list unmount while a PR is
  open; they remount unchanged on close (their state lives in the page
  entity, so selection survives).
- Close paths: `Esc` (key binding on the PR page), the repo crumb `on_click`
  (0014 deferred this to the sub-pages doc — now wired to `self.pr = None`),
  and auto-close on sign-out (the connect panel owns the module again).
- Crumbs: entering appends the `#<number>` suffix via the existing
  `sync_repo_crumb` path — `Home > GitHub > <repo> > #482`. The repo crumb
  gets `on_click` = back-to-repo-root; the `#n` crumb is current-location
  (non-clickable, breadcrumb convention). 0014 D3's prefix rule keeps this
  stable under background ticks; leaving clears the suffix.

### D2 — Data: one detail query, one lazy patch fetch, no cache

- **`PR_DETAIL_QUERY`** (gql.rs): `repository(owner, name) { pullRequest(
  number) { …full §3 field set… } }` — one query, ~1–2 points, fetched **once
  on open** (paint skeleton immediately), on header refresh, and after every
  mutation. Same `Envelope` + per-node degradation conventions as 0007 D5/D3
  (one bad node degrades alone). Runs through `run_spec_tracked` (0013).
- **`PR_FILES` (REST)**: `GET /repos/{o}/{r}/pulls/{n}/files` fetched **the
  first time the Files changed tab is opened** (not on page open — the patch
  payload is the heavy part and most visits never open it). Response parsed
  into `{ path, additions, deletions, status, patch: Option<String> }`; refetch
  with the detail query after mutations that can change the diff (none in this
  scope — merge/close end the page's useful life anyway).
- **Wire structs** (gql.rs, mirroring existing serde style): `PrDetail` (owns
  everything the page renders), `PrCommit`, `PrReview`, `PrComment`,
  `PrCheckRow` (flattened CheckRun/StatusContext union), `PrFile`.
- **Mutations** (service.rs): a `run_mutation` helper on the same POST/auth/
  token-refresh path as queries (transport is identical — a different document
  string); tracked like fetches (0013). Each mutation completion triggers the
  detail refetch; failures surface inline (D4/D5), never as dialog modals.

### D3 — Page anatomy

```
┌──────────────────────────────────────────────────────────────┐
│ [Open●] Add retry budget to fetch layer #482        [↻] [↗]  │ header (fixed)
│ alice wants to merge 12 commits  main ← add-retry-budget      │
├──────────────────────────────────────────────────────────────┤
│  Conversation (12)  Commits (12)  Checks (8)  Files changed   │ TabBar underline
├───────────────────────────────────────────────┬──────────────┤
│  tab content: one scroll region per tab,      │ right rail   │
│  only the active tab renders                  │ (conversation│
│                                               │  tab only)   │
└───────────────────────────────────────────────┴──────────────┘
```

- **Header:** state badge (`Open` success / `Draft` muted / `Merged` accent /
  `Closed` danger), `text_lg` semibold title, muted `#number`, refresh +
  open-on-GitHub icon buttons (icon-only ⇒ tooltip, [DG]). Meta line: author
  avatar + login, "wants to merge N commits", `base ← head` (mono, `text_xs`).
- **Tabs:** `TabBar::new("pr-tabs").underline()`; labels carry counts
  (`Conversation (12)` … counts from `PrDetail` connections; hidden while
  loading). Tab state is `enum PrTab { Conversation, Commits, Checks, Files }`
  on `PrDetailPage`; per-tab `ScrollHandle`s so switching back preserves
  position.
- **Loading:** per-tab `Skeleton` rows (the page chrome + tabs render
  immediately). **Empty:** muted cells ("No reviews yet", "No checks
  reported", …). **Error:** inline retry row per section.

### D4 — Conversation tab (default)

Two columns: main timeline (`flex_1`) + fixed ~300px right rail (D5).

- **Description card** (first timeline item): author avatar, login, relative
  age, body as plain text (known limitation, §11). An **Edit** button (shown
  when the detail reports `viewerCanUpdate` — one extra GraphQL field, D2)
  swaps the body for a `Textarea` prefilled with the current body + Save /
  Cancel; Save runs `updateIssue { id body }` then refetches. One edit at a
  time; Save disabled while empty-body? (body may legitimately be emptied —
  no, GitHub rejects empty; Save disabled on empty), busy state while saving.
- **Timeline:** `comments` and `reviews` (with non-empty bodies) merged and
  sorted ascending by timestamp (stable merge — pure fn, table-tested). A
  review row renders its state as a badge (`APPROVED` success /
  `CHANGES_REQUESTED` danger / `COMMENTED` muted) beside the author + age,
  body below, same card visual as comments.
- **Comment box** (bottom, sticky end of the scroll region): `Textarea` +
  action `Select` (**Comment** / **Approve** / **Request changes**) + primary
  Submit button. Gating (pure fn, table-tested): Comment requires a non-empty
  body (`addComment`); Approve/Request-changes submit
  `addPullRequestReview { event, body }` — empty body allowed. Submit
  disabled + spinner while in flight; the box clears on success; failures
  show an inline error row above the box and keep the draft.

### D5 — Merge/close panel (conversation right rail)

A `GroupBox` at the rail top:

- **State line** from `mergeStateStatus` (pure mapping fn, table-tested).
  **Shipped copy (stage 2, 2026-09-03) — the full sentences, not the
  sketch below:** `CLEAN`/`HAS_HOOKS` → "This branch has no conflicts
  with the base branch" (success); `BEHIND` → "This branch is out-of-date
  with the base branch" (warning); `BLOCKED` → "Merging is blocked by
  required reviews or status checks" (warning); `DIRTY` → "This branch has
  conflicts that must be resolved" (danger); `DRAFT` → "Draft pull requests
  can't be merged until marked ready" (muted); `UNSTABLE` → "Mergeable,
  but some checks are not passing" (muted); `UNKNOWN`/absent → "GitHub is
  still calculating merge status" (muted). `DRAFT` and terminal `state`
  disable merging entirely.
- **Method selector**: a `Select` offering exactly the repo-enabled methods
  (`mergeCommitAllowed`/`squashMergeAllowed`/`rebaseMergeAllowed` — the three
  booleans ride along on the detail query via the `repository` wrapper).
  Default: MERGE when allowed, else the first allowed. One method = the Select
  hides (no choice to make), the button label carries the verb.
- **Actions:** primary `Merge` button (disabled while `mergeable ≠ MERGEABLE`,
  draft, or busy) → `mergePullRequest { pullRequestId, mergeMethod }`;
  secondary `Close` (`Button::danger`) → `closePullRequest`. While in flight
  both disable + spinner; on success the refetch repaints the header state
  badge and the panel swaps to a terminal note ("Merged ✓"/"Closed") — the
  next snapshot tick removes the row from the repo view list (no extra work).
- **Checks mini-summary** below the panel: rollup state + "View checks" link
  that switches to the Checks tab (no browser round-trip).

### D6 — Commits tab

Scrollable rows: author avatar/name (fall back to `author.name` when no user
login), `messageHeadline` (`text_sm`), short SHA (7-char, mono token, muted),
relative age. A muted caption when `commits.totalCount > nodes.len()`:
"Showing first 100 of N commits".

### D7 — Checks tab

- **Summary header** (outside the scroll): "8 checks · 7 passed · 1 failed",
  counts derived from the contexts list (pure fn).
- **Rows** from `statusCheckRollup.contexts(first: 100)`: name, state → icon +
  tone (`SUCCESS` success / `FAILURE` danger / `PENDING`/`QUEUED`/`IN_PROGRESS`
  warning or spinner / `NEUTRAL`/`SKIPPED` muted), and a details affordance
  that opens `detailsUrl`/`targetUrl` in the browser when present. CheckRun vs
  StatusContext union parsed per node (0007 D3 degradation).
  **Shipped (stage 3, 2026-09-03):** union shape verified live (§3);
  `CheckRun`'s `status`+`conclusion` pair unifies into
  `CheckState { Success Failure Pending Neutral }` — conclusion-bearing runs
  map terminal (`TIMED_OUT`/`ACTION_REQUIRED` → failure, `STALE`/`CANCELLED` →
  muted, an unknown future conclusion reads as failure, never silently muted),
  null-conclusion runs map pending; running rows use the `LoaderCircle` icon in
  warning tone, and pending/neutral buckets word as "running"/"skipped" in the
  summary line. The details affordance is a trailing external-link button; rows
  without a URL show none.

### D8 — Files changed tab

- **Summary line:** "23 files changed · +812 · −94" (additions success tone,
  deletions danger tone).
- **Per-file collapsible cards** (`Collapsible`): header = path (mono, `text_xs`,
  leading dir segments muted like github.com), `+adds/−dels` counts, change-type
  Badge (`added`/`modified`/`removed`/`renamed`); header click toggles.
- **Diff body:** the REST `patch` parsed (pure parser, table-tested: hunk
  headers `@@ … @@` muted; context lines plain; `+`/`−` lines tinted
  `success`/`danger` at low opacity for the row background with the sign glyph
  colored; malformed or missing patch → "Binary file or diff too large"
  placeholder row). Rendered in the theme mono token at `text_xs`, `flex_none`
  lines (no soft wrap) inside a horizontally scrollable region. No intra-line
  highlighting, no split view (§11).

### D9 — Wiring & file layout

```
crates/modules/github/src/
  pr_page.rs        new — PrDetailPage entity (PrTab, scroll handles, comment
                    draft state, edit state, busy flags), render_* per region,
                    pure helpers exported for tests
  gql.rs            PR_DETAIL_QUERY + PrDetail/PrCommit/PrReview/PrComment/
                    PrCheckRow/PrFile wire structs (+ mutation documents)
  service.rs        FetchSpec kinds { PrDetail, PrFiles }, run_mutation,
                    patch parse entry, state plumbing for the open PR
  page.rs           pr field + takeover + row click swap + crumb suffix +
                    Esc binding
docs/               this doc + README row (implementation PRs update 0009/0012
                    row-click wording when the behavior ships)
```

No core changes: `Crumb.on_click` (nav.rs) already exists; 0014's
reconciliation already tolerates suffixes.

### D10 — [DG] + token-discipline compliance

- Semantic colors only — state badges, diff tints, and count colors derive
  from `cx.theme()` tokens (`.opacity(..)` for tints); no literal color
  constructors; `clippy.toml` gates hold.
- Typography scale only: `text_xs` (diff/meta/SHA), `text_sm` (rows/body),
  `text_lg` (title). Mono text uses the theme's mono token, not a font name.
  Component sizing via `Size` tiers (buttons `Size::Small` in the rail) — no
  one-off pixel heights.
- Icon-only buttons (refresh, open-on-GitHub) carry tooltips; distinct
  hover/disabled/loading states on every action (Merge, Submit, Save).
- Designed loading (skeletons), empty, and error states for every tab surface.

### D11 — Bridge hygiene (0010 D10)

Upstream widgets (Tab, Select, Button, Textarea, Collapsible) are covered; our
custom interactive elements get `.id()` + role: timeline rows
(`Role::ListItem`), check rows, file-card headers (toggle = button role),
review-action rows. Verification plan in §8; a missing node in `tree` is a bug
in the element, not the bridge.

## 6. Implementation plan (staged — one issue + PR per stage, after this doc merges)

1. **Stage 1 — shell + conversation read path:** `pr_page.rs` skeleton,
   takeover + row click + crumbs + Esc, `PR_DETAIL_QUERY` + wire structs,
   header, tab chrome, description card (read-only), timeline render, comment
   box (read state), skeletons/empty/error.
2. **Stage 2 — write path:** `addComment`/`addPullRequestReview` action
   selector + gating, description edit (`updateIssue`), merge/close panel with
   allowed-method Select, `mergePullRequest`/`closePullRequest`, post-mutation
   refetch, terminal states.
3. **Stage 3 — commits + checks tabs** (union shape re-verified live first,
   §3 — done 2026-09-03).
4. **Stage 4 — files changed:** REST files fetch (lazy), patch parser,
   collapsible file cards + diff renderer.

Crates touched: `dashboard-module-github` only.

## 7. Testing

- **Pure fns, table-tested** (pr_page.rs / service.rs): mergeStateStatus →
  (copy, tone) mapping; allowed-method list from the three repo booleans
  (0–3 enabled, MERGE-not-allowed default); comment-action gating (empty body
  × three actions); timeline merge order/stability; patch parser (normal
  hunks, malformed, missing patch, huge hunk); checks summary counts; short
  SHA.
- **`#[gpui::test]` smoke:** PrDetailPage construct + tab switch keeps scroll
  state; action selector wiring mirrors the 0009 §4 pattern.
- Existing tests untouched except `render_pr_row` behavior references
  (0012 D4 wording) where the click target changed.

## 8. Debug-bridge verification (docs/0010)

With the app running, a repo selected, an open PR present:

1. `tree` → PR rows (`pr-{key}`) still present; `click pr-<key>` → the PR
   page mounts: header, `pr-tabs` with four tab nodes, crumb gains `#n`.
2. `key esc` → back to the repo view; crumb suffix cleared; repo selection
   intact.
3. Tab clicks via bridge → only the active tab's nodes in the dump; counts in
   labels.
4. Comment flow: `type` into the textarea, `click` the action Select + Submit
   → `logs` show the mutation POST and the refetch; the new comment appears.
5. Merge panel: disabled state on a draft/blocked PR; method Select lists
   exactly the enabled methods on a repo with a restricted set.
6. No `logs -f` left running; a11y flag reset after crashed runs (0010 rules).

## 9. Acceptance criteria

1. Clicking a PR row opens the in-app PR page as a full takeover; `Esc` and
   the repo crumb return to the repo view; breadcrumb reads
   `Home > GitHub > <repo> > #<number>` throughout.
2. Header shows state badge, title, number, author/branch meta; refresh and
   open-on-GitHub act.
3. Four tabs render with live counts; per-tab scroll positions survive tab
   switches; skeletons/empty/error states exist for each.
4. Comment submit posts and appears after refetch; Approve and Request-changes
   submit reviews; description edits save; failures keep the draft and show an
   inline error.
5. The merge panel reflects `mergeStateStatus`, offers exactly the repo-enabled
   merge methods, and Merge/Close round-trip to a terminal state; the repo
   list updates on the next snapshot tick.
6. Commits and Checks tabs render their rows with correct tones; check details
   open in the browser.
7. Files changed renders the REST patch as a unified diff with per-file
   collapse; binary/oversized files show the placeholder.
8. Gates: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`
   (zero warnings), `cargo test --workspace`; doc + README index row merged
   with this PR.

## 10. Human checks (owner)

- [ ] Open PRs of several shapes: draft, blocked-by-checks, conflicting,
      clean; confirm the merge panel copy/tone and button states match.
- [ ] Comment, Approve, and Request-changes on a scratch PR; verify on
      github.com that the events landed correctly.
- [ ] Merge via each enabled method on a sandbox repo (merge/squash/rebase as
      enabled); close another PR; confirm list updates.
- [ ] Edit a description; cancel an edit mid-way (draft discarded).
- [ ] Scroll the commits list, open Checks, open Files changed, collapse and
      expand a file card, switch tabs away and back (scroll kept).
- [ ] `Esc` and the repo crumb both return to the repo view with selection
      intact.

## 11. Follow-ups (GitHub issues, not this PR)

- Comment edit/delete on conversation comments (owner-approved deferral, Q4)
  — **filed with this PR**.
- Markdown rendering for PR bodies/comments (options: in-house minimal
  renderer vs a crate; separate doc if it grows).
- Split-view diff, intra-line diff highlighting, syntax highlighting via the
  upstream `highlighter`.
- In-app check-run log viewer.
- "+N more" pagination for commits/reviews/comments/files beyond the first-100
  / REST caps (adjacent to #87).
- Review threads (line comments), draft reviews, review dismissal.
- Delete-branch-after-merge checkbox in the merge panel.
