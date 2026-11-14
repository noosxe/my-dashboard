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

> **Re-verified at the v0.6.0 pin `94a313a` (2026-09-09, #125):** the
> GPUI Kit rebrand moved the crate tree (`crates/ui/src` →
> `crates/component/src`; crate name and public paths unchanged).
> `select.rs`, `input/textarea.rs` and `theme/theme_color.rs` are
> byte-identical across the bump; `tab/tab.rs`'s only diff is a rustfmt
> reformat of one `child()` call — no behavior change. Every fact below
> holds line-for-line.
>
> **Re-verified at the v0.7.0 pin `0c830f4` (2026-09-29, #363):** the
> `size_full` trap survives — `select.rs` still carries exactly two
> `size_full` sites (:526, :857 at 0.7.0). 0.7.0 reworked window startup
> (Base-owned Root) and notification delivery (`Root::push_notification` →
> `WindowExt`), not Select layout; the 0016 probes pass unchanged on the
> new pin.
>
> **Re-verified at the v0.7.3 pin `67265a3` (2026-10-12):** `select.rs` is
> byte-identical to the v0.7.1 pin `87d10ae` — the `size_full` trap
> stands, still exactly two sites (:527, :858). The v0.7.2/v0.7.3
> releases carry no Select-layout changes.

> **Re-verified at the v0.8.0 pin `1676826` (2026-10-19):** `select.rs`
> still carries exactly two `size_full` sites — the trap stands. The
> v0.8.0 release notes list no Select-layout change (Select: keyboard
> confirmation and item reconciliation only, #3446).

> **Re-verified at the v0.8.1 pin `99576db` (2026-10-26):**
> `component/src/select.rs` is byte-identical to the v0.8.0 pin — the
> two `size_full` sites (:554/:896) and the trap stand. The tab bar and
> list changes in v0.8.1 are API ports to gpui-pre 0.3.10 only
> (`ScrollEvent` rename, OS-aware `cmd-enter`/`ctrl-enter` binding, the
> `secondary(cx)` modifier helper); the tab/timeline split behavior is
> untouched.
>
> **Re-verified at the v0.9.0 pin `7591a35` (2026-11-06):** `select.rs`,
> `tab/`, `sidebar/`, `list/` and `input/state.rs` are byte-identical to
> v0.8.1 (`git diff 99576db 7591a35`); the `size_full` trap and the
> tab/timeline split behavior stand.
>
> **Re-verified at the v0.9.1 pin `39ab849` (2026-11-11):** `select.rs`
> and `sidebar/` are byte-identical to v0.9.0. `tab/tab.rs` changed only
> to give icon-only tabs a minimum width, and `list/list_item.rs` only
> in the selected row's text colour; `input/state.rs` gained fold and IME
> additions. The `size_full` trap and the tab/timeline split stand.

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
  Esc and the crumbs ask first when unsent review drafts would be lost
  (0043 D7, #744).
- Crumbs: entering appends the `#<number>` suffix via the existing
  `sync_repo_crumb` path — `Home > GitHub > <repo> > #482`. The repo crumb
  gets `on_click` = back-to-repo-root; the `#n` crumb is current-location
  (non-clickable, breadcrumb convention). 0014 D3's prefix rule keeps this
  stable under background ticks; leaving clears the suffix.

### D2 — Data: one detail query, one lazy patch fetch, no cache


> **Superseded in part (2026-10-14, docs/0072):** the no-cache clause
> above is overturned — a shared PR document store now holds landed
> details, warm opens paint stale-while-revalidate (the one-query shape
> stays; the revalidate is the background refetch), and merge/close/
> update-branch confirmations patch the document instead of blocking on
> a refetch. Rationale trail: docs/0072 §8. Everything else in this
> decision (one query, lazy files, 0013 tracking) stands.
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
  avatar + login, "wants to merge N commits", `base ← head` (mono, `text_xs`),
  then a labelled "updated 1d ago" (#764). The age is always the PR's
  `updatedAt` — the store summary's before the detail lands, the newer of the
  two after — never `createdAt`, so landing the detail never swaps the figure.
  The header never claims what it doesn't know (#763): with neither detail nor
  summary (opened from a notification) the state badge is absent until one of
  them provides a state, and the open-on-GitHub button uses the known URL or,
  failing that, `https://github.com/<repo>/pull/<n>`, so it never opens an
  empty URL. Logic lives in `pr_page/header_meta.rs`.
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
  age, body as plain text (known limitation, §11). **Shipped (2026-09-04):
  bodies render markdown** (description, comments, reviews — one
  `body_card` seam) per docs/0017. An **Edit** button (shown
  when the detail reports `viewerCanUpdate` — one extra GraphQL field, D2)
  swaps the body for a `Textarea` prefilled with the current body + Save /
  Cancel; Save runs `updateIssue { id body }` then refetches. One edit at a
  time; Save disabled while empty-body? (body may legitimately be emptied —
  no, GitHub rejects empty; Save disabled on empty), busy state while saving.

  Shipped (2026-09-04, editor height fix): the editor textarea renders one
  line tall — upstream's plain multi-line layout ignores `rows` (it is
  honored only in auto-grow mode, and the card's auto-height container
  collapses the `relative(1.)` height to the 1-line minimum). The editor
  state now uses `auto_grow(8, 40)`: grows with the body from 8 rows, caps
  at 40 with internal scrolling, matching GitHub's edit box behavior.
- **Timeline:** `comments` and `reviews` (with non-empty bodies) merged and
  sorted ascending by timestamp (stable merge — pure fn, table-tested). A
  review row renders its state as a badge (`APPROVED` success /
  `CHANGES_REQUESTED` danger / `COMMENTED` muted) beside the author + age,
  body below, same card visual as comments.
- **Comment box** (bottom, sticky end of the scroll region): `Textarea` +
  action `Select` (**Comment** / **Approve** / **Request changes**) + primary
  Submit button. Gating (pure fn, table-tested): Comment requires a non-empty
  body (`addComment`); so does Request-changes — GitHub rejects an empty
  body for `REQUEST_CHANGES` server-side (REST docs: `body` required for
  `REQUEST_CHANGES`/`COMMENT`; only `APPROVE` may go empty, #171). Reviews submit
  `addPullRequestReview { event, body }`.
  Submit disabled + spinner while in flight; the box clears on success; failures
  show an inline error row above the box and keep the draft.

### D5 — Merge/close panel (conversation right rail)

> **Redesigned (docs/0083, #798).** The panel becomes the rail's pinned
> **status block**: a verdict (docs/0083 D2) replaces the state line below,
> gate rows for reviews, checks and mergeability (D3) replace the checks
> mini-summary, and one action row (D4) carries a split Merge button whose
> caret replaces the method `Select`. Close moves into a `⋯` menu (D5).
> Merged and closed PRs show who and when instead of a terminal note (D6).
> The **gating** and **confirm** rules below stay in force unchanged. The
> state-line sentences remain the verdict's tooltip and a11y description.
> Where this section and docs/0083 disagree on layout, docs/0083 wins.

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
  *(Amended by docs/0083 D4: the method moves into the Merge split
  button's caret, and the default becomes `Repository.viewerDefaultMergeMethod`
  when allowed, else the first allowed (#776). Shipped: the detail query
  selects the field, the page picks the method once per open page, and a
  missing, unknown or no-longer-allowed value falls back to the rule above,
  merge commit preferred.)*
- **Actions:** primary `Merge` button → `mergePullRequest { pullRequestId,
  mergeMethod }`; secondary `Close` (`Button::danger`) → `closePullRequest`.
  While in flight both disable + spinner; on success the refetch repaints the
  header state badge and the panel swaps to a terminal note ("Merged ✓"/
  "Closed") — the next snapshot tick removes the row from the repo view list
  (no extra work).

  **Confirm (amended 2026-11-06, #738; `pr_page/merge_confirm.rs`).**
  Neither button sends on one click: *Merge pull request* and *Close pull
  request* open an inline confirm in the rail, in place of the button row
  (docs/0065 Appendix A items 3 and 9 — destructive actions confirm, and
  the user can back out). The Merge confirm names the selected method and
  `head → base` ("Squash and merge feature/x into main?") with *Confirm
  merge*; the Close confirm says it closes the PR without merging, with a
  danger *Close without merging*. Both have *Cancel*, and `Esc` cancels
  (topmost surface). Only the confirming button sends `mergePullRequest` /
  `closePullRequest`. One confirm at a time, the update-branch rebase
  confirm (docs/0067 D4) included: opening one closes the other. The
  gates below stay authoritative at both steps — a confirm opens only
  through an open gate, re-checks it when confirmed, and closes by itself
  when the gate shuts (refetch, state change, busy op). The state machine
  is independent of where the buttons sit, so Close keeps the confirm when
  it moves into a `⋯` menu (#798). *Convert to draft* (#772) is the third
  confirm kind: "Convert to draft? Reviewers won't be notified of new
  pushes until it is ready again." with a *Convert to draft* button, gated
  by `viewerCanUpdate` on an open, non-draft PR. Ready for review (#771)
  and Reopen (#770) don't confirm, since each is undone in one click.
  *Delete branch* (#773) is the fourth confirm kind, "Delete branch
  `<name>`?", gated by write access on the head repository (docs/0083 §2).
  No commit-message editing or prefill
  (`viewerMergeHeadlineText`/`viewerMergeBodyText`): deferred.

  **Gating (amended 2026-11-06, #708 + #737; `pr_page/merge_gate.rs`).**
  Two separate questions. *Visibility* is about the viewer and **hides** the
  control (the sidebar's rule for edit controls); *enablement* is about the
  PR and **disables** it, with the state line as the tooltip.

  - **Visible:** Close shows when `PullRequest.viewerCanClose` is true.
    Merge shows when `Repository.viewerPermission` is `WRITE`, `MAINTAIN` or
    `ADMIN`. `MODERATE`, `TRIAGE`, `READ`, an unknown value, or an absent
    field hide it. The schema has no `viewerCanMerge`; `viewerPermission` is
    the nearest signal and is a floor, not a guarantee (branch rulesets can
    still refuse a writer — the server stays the authority and the failure
    surfaces inline). A degraded payload without `viewerCanClose` parses as
    false, so Close hides rather than guesses. The panel itself (state line,
    update-branch row) stays for every viewer; the method selector hides with
    Merge.
  - **Enabled (Merge):** open, not draft, idle, at least one allowed method,
    `mergeable == MERGEABLE` **and** `mergeStateStatus` in the allowlist
    `CLEAN`, `HAS_HOOKS`, `UNSTABLE`. `mergeable` reports only merge
    conflicts; branch protection (required reviews and checks, strict
    up-to-date) lives in `mergeStateStatus`. Verified against the live
    GraphQL `MergeStateStatus` enum (descriptions): `CLEAN` "Mergeable and
    passing commit status", `HAS_HOOKS` "Mergeable with passing commit status
    and pre-receive hooks", `UNSTABLE` "Mergeable with non-passing commit
    status" — all mergeable. `BLOCKED` ("The merge is blocked"), `BEHIND`
    ("The head ref is out of date"), `DIRTY` ("The merge commit cannot be
    cleanly created") and `UNKNOWN` ("The state cannot currently be
    determined") disable it, as does any absent or unrecognised value (an
    allowlist, so a value GitHub adds later fails closed). The live enum has
    no `DRAFT`; the state-line mapping keeps it as harmless legacy and draft
    is gated on `isDraft`.
  - **Not handled:** bypassing protection as an admin. The schema's
    `viewerCanMergeAsAdmin` ("bypass branch protections and merge the pull
    request immediately") would support it; it is tracked in #821.
  - Close is also enabled only while `state == OPEN` and idle.
- **Checks mini-summary** below the panel: rollup state + "View checks" link
  that switches to the Checks tab (no browser round-trip).
  *(Superseded by docs/0083 D3: the Checks gate row in the status block
  carries the rollup, exact counts and the waiting runs, and opens the
  Checks tab.)*

  *(The layout note below is superseded by docs/0083 D1: the rail is
  `h_full` with a pinned status block and its own scrolling metadata
  region, so it no longer hugs its content; the split keeps
  `items_start`.)*

  **Shipped (2026-09-04, layout fix):** the conversation split is
  `items_start` + the timeline carries an explicit `h_full()` — the
  timeline owns the scroll bounded to the split's height, the rail hugs
  its content at the top and never rides along while the left column
  scrolls (owner report: the rail used to move with the page). Plain
  cross-axis stretch also bounds the timeline, but it stretches the
  rail's v_flex and pushes its panels down the column — probe-verified
  on the live build, so `items_start` + `h_full` is the pinned
  mechanism. Re-verified on the v0.6.0 build (2026-09-09, #125): the
  mechanism is gpui layout semantics, untouched by the component bump —
  the layout probes (`files_split_has_real_layout`, the 0018 §8 sidebar
  probe) and the #154 timeline anchor tests all assert real measured
  bounds and pass on `94a313a`.
>
> Re-verified on the v0.7.0 build (2026-09-29, #363): gpui layout
> semantics, untouched again; the probes and the #154 anchor tests pass
> on `0c830f4`.

### D6 — Commits tab

Scrollable rows: author avatar/name (fall back to `author.name` when no user
login), `messageHeadline` (`text_sm`), short SHA (7-char, mono token, muted),
relative age. A muted caption when `commits.totalCount > nodes.len()`:
"Showing first 100 of N commits".

### D7 — Checks tab

- **Summary header** (outside the scroll): "1 failed · 8 checks · 7 passed",
  counts derived from the contexts list (pure fn). Failure leads, in danger tone
  and semibold; the total follows in the foreground colour; the other buckets
  are regular weight and muted (#792).
- **The counter includes runs waiting for approval** (#783): the tab reads
  `contexts.totalCount` plus the gated runs (docs/0066 D3), and the header's
  total is the same number, so a PR whose only CI is gated reads "Checks (2)"
  and "2 checks · 2 waiting", never "Checks (0)". The tab stays bare while the
  sweep is out and nothing has been found, rather than flash a wrong 0 (D3).
- **Over the cap** (#747, `contexts.totalCount` > rows): the rows can't speak
  for the rollup, so a capped list never reads as complete. The header leads
  with the server's total and the `statusCheckRollup.state` verdict and scopes
  the buckets to the rows: "137 checks · in progress · first 100: 90 passed ·
  10 running". A failing verdict leads in tone, as a failure count does:
  "some failed · 137 checks · first 100: 10 failed · 90 passed" (verdict words: all passed, some failed, some errored, in
  progress, queued, status unknown). A row pinned under the header, sharing its rule, captions "Showing
  first 100 of 137 checks" (", plus 2 waiting" when runs are held: the cap
  limits rollup rows only, and the total above includes those runs) with a "View all on GitHub" button to the PR's
  `/checks` page. It's pinned rather than last in the list, as the Commits
  caption is, because on a 100-row list the end is off screen. Rows beyond the
  first 100 aren't paginated. Nodes dropped from a degraded payload count as
  capped too: they're equally unaccounted for.
- **Row order** (#792): failed first, then running, then everything else, so
  the failure is the first row in view. Within a rank, rows cluster by workflow
  (unlabelled rows last), then sort by name, then by check run id, so a refresh
  doesn't reorder them. The view sorts; the list stays in the server's order.
  Over the cap this orders the listed rows only. docs/0066 D3's waiting section
  still sits above the rows: it is what the PR is held on, and it needs a person.
- **Row anatomy** (#792): chevron, state icon, name, workflow (muted), time
  (muted, right-aligned), external link. Same-named rows are told apart by the
  workflow, `checkSuite.workflowRun.workflow.name`. Third-party checks and status
  contexts have none. That field is nullable, so a token without Actions access
  gets a partial error that stops there (logged, docs/0007 D5): the detail still
  lands and the rows go unlabelled. The time is `completedAt − startedAt`
  ("42s", "3m 05s", "1h 02m") on a finished run, and "started 3m ago" on a running
  one rather than a counter that would freeze between refreshes (docs/0052). A
  skipped run reports the same start and end, and shows nothing, as do status
  contexts and queued runs.
- **The whole row toggles the log** (#792, docs/0039 D3) on a row with one: the
  row is one real button, so it stays in the tab order, and the chevron moves to
  the leading edge, away from the external link. Its accessible name says what
  the row shows: "View log for build in Release, failed, 1m 42s". Rows without a
  log keep the chevron column empty so names align, and gated rows do the same.
- **Rows** from `statusCheckRollup.contexts(first: 100)`: name, state → icon +
  tone (`SUCCESS` success / `FAILURE` danger / `PENDING`/`QUEUED`/`IN_PROGRESS`
  warning or spinner / `NEUTRAL`/`SKIPPED`/`CANCELLED` muted), and a details affordance
  that opens `detailsUrl`/`targetUrl` in the browser when present. CheckRun vs
  StatusContext union parsed per node (0007 D3 degradation).
  **Shipped (stage 3, 2026-09-03):** union shape verified live (§3);
  `CheckRun`'s `status`+`conclusion` pair unifies into
  `CheckState { Success Failure Pending Neutral Cancelled }` — conclusion-bearing
  runs map terminal (`TIMED_OUT`/`ACTION_REQUIRED` → failure, `STALE`/`NEUTRAL`/
  `SKIPPED` → muted "skipped", `CANCELLED` → muted too but its own state, #817;
  an unknown future conclusion reads as failure, never silently muted),
  null-conclusion runs map pending; running rows use the `LoaderCircle` icon in
  warning tone, and pending/neutral/cancelled buckets word as "running"/"skipped"/"cancelled"
  in the summary line (e.g. "3 checks · 1 passed · 1 skipped · 1 cancelled"),
  and the row's accessible name says "cancelled". A cancelled run keeps the
  muted tone and the "rest" sort rank but gets the `Ban` glyph, not the
  skipped dash: it started and did not finish (#817). The details affordance is a trailing external-link button; rows
  without a URL show none.
  StatusContext rows map their `StatusState` the same way: SUCCESS →
  success, ERROR/FAILURE → danger, PENDING and EXPECTED (a required
  status not yet reported) → pending — matching the rollup glyph mapping
  (0007 §6, #174); null/unknown → muted.

### D8 — Files changed tab

> **Amended (2026-10-12, docs/0070):** the diff body now renders through
> the gpui-kit `Diff` (docs/0070 D1) — the `parse_patch`/`diff_row`
> mechanics below are historical; badge and +/− count semantics moved
> into upstream header chrome.

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

  **Shipped (stage 4, 2026-09-03):** fetch is lazy — the REST walk
  (`Kind::PrFiles` through the shared `run_rest` page-stitcher, no
  ETag) fires on the first Files-tab selection, and the header refresh
  refetches a loaded list. 30 pages cap the walk (GitHub's own
  3000-file limit); the first short page ends it long before. Cards
  default open (github.com parity); the header is a ghost `Button`
  (`pr-file-{ix}`, chevron flips `ChevronRight`/`ChevronDown`) and the
  collapse set is index-keyed, cleared on refetch so stale indexes
  cannot leak into a new list. Diff rows are `whitespace_nowrap` +
  `flex_none` inside an `overflow_x_scroll` region; hunk headers carry
  an `accent` 15% tint, +/- rows `success`/`danger` 12% tints with the
  whole row's text in the tone color. Unknown `status` strings degrade
  to a muted badge with the raw label (0007 D3).

  **Superseded (2026-09-04, docs/0018):** the accordion was replaced by
  the master–detail sidebar (view-mode Select, shared filter input,
  List/Tree views, single-file diff pane). The summary line, the
  `parse_patch` diff rendering and the status-badge semantics above
  survive unchanged; the per-file `Collapsible` cards and the
  index-keyed collapse set are deleted. See docs/0018 D1–D10.

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
   collapsible file cards + diff renderer — done 2026-09-03; §6.3 D8
   carries the shipped notes.

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
- ~~"+N more" pagination for commits/reviews/comments/files beyond the
  first-100 / REST caps (adjacent to #87).~~ Comments and reviews now
  surface the shared cap row (2026-09-06, #123: muted "+N more on
  github.com", id `pr-comments-more`, opens the PR upstream; commits
  and files keep their own caps). In-app pagination remains a possible
  later upgrade.
- Review threads (line comments), draft reviews, review dismissal.
- Delete-branch-after-merge checkbox in the merge panel.

---

## 12. Shipped (2026-09-06, issue #123 — conversation cap row)

The PR conversation surfaces its connection caps: when `comments` or
`reviews` truncate at `first: 100`, a muted "+N more on github.com" row
closes the timeline (since #742 the query reads the newest 100 and the
row opens the timeline as "N older on github.com", §14). Notes beyond §11's follow-up line:

- **Shared component** (`pr_page::cap_row`): one builder for every
  capped connection — the issue page's events row (docs/0021 D3) now
  renders through it too, so the two pages cannot drift. `missing` is
  the caller's sum of shortfalls (comments + reviews here; disjoint
  node sets, so the sum is the honest hidden count).
- **Reviews counted too**: §11 named comments, but the reviews
  connection caps identically and its rows live in the same timeline —
  hiding them silently would be the same bug.
- **Row identity**: id `pr-comments-more` (0010 D10), ghost button,
  `open_url` to the PR upstream. In-app pagination (Load-more) stays a
  possible later upgrade; the hint row is the #87-cheap pattern the
  issue blessed at pick-up.
- **No query change** (as first shipped): `totalCount` was already
  fetched for both connections (0016 D1); #742 later changed the window,
  §14.
- **Testing**: probe test — absent when every node is on the page,
  present after a refetch that truncates both connections; the issue
  page gains a comments-only-truncation arm on its existing cap test.

## 13. Shipped (2026-09-29, issue #397 — spinning pending glyph)

The D7 `running` row glyph animates: pending checks render through the
upstream `Spinner` (`IconName::LoaderCircle` in the same warning amber
via `.color()`, Medium = the `size_4` the static arms keep) instead of a
static `Icon`. The tone contract itself is untouched — amber in, amber
out. Animation is decoration only: the row text and details carry the
semantics, upstream handles reduced-motion (static, zero frame
requests), and the animation stays scoped to the glyph's box so rows
repaint only their icon area. The repo-view PR rows spin through the
same seam (0007 §10).

## 14. Shipped (#742, #746 — newest-100 window, pending review)

- **The window is the newest 100.** `comments` and `reviews` read
  `last: 100` (still ascending, so the timeline stays chronological).
  A long PR now cuts its *oldest* items: the user's own new comment
  always shows, and a notification jump to a recent comment finds its
  `databaseId` in range (the anchor still gives up quietly for older
  ones, docs/0026 D9).
- **The cap row leads the timeline**, right under the description, and
  reads "N older on github.com" (id `pr-comments-more`, opens the PR
  upstream). N is still the sum of both shortfalls
  (`totalCount − nodes`), disjoint sets. The shared `cap_row` keeps its
  "+N more" wording for the capped lists that end a pane.
- **The viewer's PENDING review is not a timeline entry.** A review
  started on github.com has no `submittedAt`, no age, and Edit/Delete
  would act on a draft this page does not own (its own review flow
  submits in one batch, docs/0030 D4, so a server-PENDING review never
  comes from here). It is dropped from the timeline and the
  Conversation count, and a quiet line above the composer says "You have
  a pending review on GitHub" with an Open on GitHub button (ids
  `pr-pending-review`, `pr-pending-review-open`). Edit and Delete are
  never offered on it.
- **Code**: `pr_page/conversation_window.rs` (pure window/pending
  helpers, the cap row, the notice).

## 15. Shipped (#752, #807 — head change, UNKNOWN re-poll, page-modal writes)

- **Head change fans out.** A detail landing with a new `headRefOid`
  refetches the open Files and threads lanes once; the diff and the
  check logs follow. A line comment or draft started on the old head is
  refused with an inline Files error until it is redone on the new diff.
- **UNKNOWN re-polls.** The merge rail's "GitHub is still calculating
  merge status" (D5) re-polls the detail up to 3 times, 4 s apart, and
  stops as soon as it resolves or the page closes.
- **Rail metadata and reactions** confirm on the store, so a write
  finishing after the page closed still voids the cached detail.
- Rules and reasons: docs/0072 D3/D4. **Code**: `pr_page/head_change.rs`,
  `pr_page/mergeable_poll.rs`, `pr_page/page_write_confirm.rs`.
