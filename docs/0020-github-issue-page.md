# 0020 — GitHub module: issue detail page (comment, close, reopen)

- **Date:** 2026-09-05
- **Depends on:** 0003 (§4.3 module-internal sub-pages), 0005 (GitHub module),
  0007 (snapshot + edge-freshness pattern), 0009 (repo view rows), 0010
  (debug bridge, D10 id rule), 0013 (tracked fetches), 0014 (crumb prefix +
  suffix rule), 0016 (PR detail page — the takeover/write-path template),
  0017 (markdown bodies), 0019 (per-node comment gates + inline edit/delete)
- **Closes the design need of:** owner request — an issue detail page
  "similar to PR in whichever ways necessary"

## 1. Context & problem

The repo view lists open issues from the global snapshot (0007), but an issue
row is a dead end: `render_issue_row`'s `on_click` hands off to the browser
(0012 D4 parity). Reading or commenting on an issue means leaving the app.

The PR page (0016) already built almost everything an issue page needs:

1. **The sub-page pattern** — module takeover, crumb suffix, Esc close,
   module-crumb re-entry (#100), tracked detail fetch + refetch-on-mutation
   (0013), the mutation POST path (0016 §D2).
2. **The conversation internals** — markdown bodies (0017), inline editors,
   and per-comment `viewerCanUpdate`/`viewerCanDelete`-gated Edit/Delete
   (0019). Conversation comments on issues **are** `IssueComment` objects —
   the same node type the PR timeline renders — so 0019's affordances,
   gating, one-editor rule, and node-id keying apply verbatim.
3. **Reuse of three of the four write documents** — `addComment`
   (`subjectId` is a bare Node ID; an issue qualifies), `updateIssue` (the PR
   description edit already goes through it), and 0019's
   `updateIssueComment`/`deleteIssueComment`.

What is genuinely new: an **issue detail query** (no payload we hold carries
bodies or comments), a **close/reopen write path**, and a **metadata rail**
(labels, assignees, close controls) where the PR page has its merge panel.

## 2. Scope

In:

- **Entry:** clicking an issue row in the repo view opens the in-app issue
  page (the browser hand-off moves to the header's "open on GitHub" ↗ icon,
  0016 D1 parity).
- **Full-content takeover**, mutually exclusive with the PR page — opening
  one closes the other; while either is open the org Select + repo list are
  unmounted (0016 D1 mechanics, shared).
- **Header:** state badge (`Open` success / `Closed` danger, with the close
  reason where GitHub shows it), title, `#number`, author + age meta line,
  refresh + open-on-GitHub icon buttons.
- **Single conversation view** — no tab bar (issues have no commits, checks,
  or files): description card (markdown per 0017; Edit via `updateIssue`
  when `viewerCanUpdate`), comment timeline (markdown; 0019 Edit/Delete
  affordances gated by the node's own flags), comment box (`addComment`).
- **Right rail** (conversation-only, ~300px like 0016 D5): Labels group,
  Assignees group, and a State panel — **Close** (with a close-reason
  choice) when open, **Reopen** when closed, gated by `viewerCanClose` /
  `viewerCanReopen`.
- **Data:** one GraphQL detail query on open + refetch after every mutation
  and on manual refresh; no cache; tracked fetches (0013). New mutations:
  `closeIssue { issueId stateReason }`, `reopenIssue { issueId }`.
- Crumbs `Home > GitHub > <repo> > #<number>`; `Esc` closes; module-crumb
  re-entry closes the issue page too (#100 pattern).
- Doc + README index row (this PR); implementation lands as one PR (§6).

Out (non-goals):

- **No timeline event rows** (labeled / assigned / cross-referenced /
  renamed / referenced …) — the `IssueTimelineItems` union is deferred; the
  header badge + rail State panel carry the state spine. Follow-up.
- No label / assignee / milestone **management** — display only. Labels +
  assignees render; milestone display is deferred.
- No title editing (body only — PR page parity). Follow-up.
- No reactions, locking, issue creation, pinning, or transfer.
- No data-colored label chips in v1 (muted name chips; see §4 Q2).
- No pagination beyond the first 100 comments (GraphQL `first: 100`);
  truncation now surfaces as the "+N more on github.com" row (2026-09-06,
  #123 — the events cap row sums the comment shortfall, and the PR page
  mirrors it in 0016 §12). In-app paging stays with #87.
- Session-only state (nothing enters `shell.json`, the snapshot cache, or a
  new cache). No cadence change: the detail query runs on open, refresh, and
  post-mutation only — background ticks do not touch the open page.

## 3. Verified facts

**Schema (live introspection via `gh api graphql`, 2026-09-05; the static
docql mirror is stale for `Issue` — it lacks the viewer-close fields — so
introspection is the source of record here):**

- `repository(owner, name).issue(number)` returns a single Issue.
- `Issue` fields used: `id`, `number`, `title`, `body`, `state`
  (`IssueState ∈ { OPEN, CLOSED }`), `stateReason`
  (`IssueStateReason ∈ { REOPENED, NOT_PLANNED, COMPLETED, DUPLICATE }`,
  null while open), `closedAt`, `author { login avatarUrl }`, `createdAt`,
  `viewerCanUpdate`, `viewerCanClose`, `viewerCanReopen`,
  `labels(first: n) { totalCount nodes { name color } }`,
  `assignees(first: n) { nodes { login avatarUrl } }`,
  `comments(first: 100) { totalCount nodes { … } }`.
- `Issue.comments.nodes` are **`IssueComment`** — the same object the PR
  conversation renders; the 0019 gate fields (`id`, `viewerCanUpdate`,
  `viewerCanDelete`, `updatedAt`) are verified on it (0019 §3).
- Mutations (input fields verified live): `closeIssue { issueId stateReason
  duplicateIssueId rationale isSuggestion confidence }` — we send only
  `issueId` + optional `stateReason`; `reopenIssue { issueId }`;
  `addComment { subjectId body }` — `subjectId` is a Node ID, so the
  document the PR comment box already posts serves issues unchanged;
  `updateIssue { id title body … }` — the PR description edit document, used
  for the issue body; `updateIssueComment { id body }` /
  `deleteIssueComment { id }` — 0019 documents, unchanged.

**Upstream widgets (pinned gpui-component @ 0c746df):** no new widgets —
`GroupBox`, `Badge`, `Button` (danger), `Textarea` (`auto_grow`), the 0017
markdown renderer, `Select` (close-reason). Theme semantic
`success/danger/warning` tokens cover the badge tones.

**Our tree (`main` @ `66fd7dc`):**

- `render_issue_row` (page.rs ~526) is the click seam; rows already carry
  `.id("issue-{key}")` + `ListItem` role.
- `GithubPage.pr: Option<Entity<PrDetailPage>>` + the render-time takeover
  guard (page.rs ~810); `open_pr` / `close_pr` / `return_to_root` are the
  lifecycle trio to mirror; `PrPageEvent` subscription is the Esc-close
  channel.
- `sync_repo_crumb` derives the `#number` suffix from `self.pr` (page.rs
  ~230) — the issue page's number must feed the same derivation.
- `run_specs` / `run_spec_tracked` (0013) is the fetch path; `FetchSpec`
  kinds extend; `run_mutation` (0016) is the POST path.
- 0019 §8 lessons hold: `debug_bounds` only sees `.debug_selector()` keys —
  new cards/action bars get test-only selector keys from day one; fixture
  payloads must wrap in the envelope the parser expects.

## 4. Owner questions (lean recorded; doc updated on answer)

| Q | Question | Lean |
| --- | --- | --- |
| Q1 | Timeline event rows (labeled/assigned/closed-by) now or follow-up? | **Follow-up** — header badge + rail carry state; the union is the biggest new parse surface and most issues read fine without it. |
| Q2 | Label chips: muted name-only, or data-colored from `Label.color`? | **Muted name-only** — hex is user data, and a data-driven color needs a token-discipline exception ([DG]); not worth it for v1. |
| Q3 | Close flow: a close-reason Select (Completed / Not planned) beside the Close button, or plain Close (always `COMPLETED`)? | **Select**, github.com parity; one extra `Select`, defaulting to Completed. |
| Q4 | Title editing? | **Out** — body only, PR parity; file later if wanted. |
| Q5 | Crumb label: `#<number>` (PR parity) or `issue #<number>`? | **`#<number>`** — only one sub-page is ever open, so the trail is unambiguous in practice. |

## 5. Design

### D1 — Navigation: sibling sub-page, one takeover

```rust
// page.rs
pub struct GithubPage {
    // …
    pr: Option<Entity<pr_page::PrDetailPage>>,
    issue: Option<Entity<issue_page::IssueDetailPage>>,   // new, mutually exclusive with pr
}
```

- `render_issue_row`'s `on_click` swaps `cx.open_url` for
  `open_issue(repo_full_name, issue, window, cx)`: constructs
  `IssueDetailPage` (which kicks the detail fetch, D2), clears `self.pr`
  (mutual exclusion), stores the entity, syncs crumbs, focuses.
- Symmetrically, `open_pr` clears `self.issue`.
- `Render for GithubPage`: the takeover guard checks `pr` then `issue` —
  whichever is `Some` replaces the module content (same unmount/remount
  semantics as 0016 D1; repo selection survives, per #100).
- Close paths mirror 0016: `Esc` on the issue page emits `IssuePageEvent::Close`
  → the host's subscription calls `close_issue`; the repo crumb `on_click`
  (back to repo root, selection survives) calls both closers;
  `return_to_root` (module crumb) also closes the issue page; sign-out
  auto-closes (the connect panel owns the module).
- Crumbs: `sync_repo_crumb` extends its derivation to
  `pr_number.or(issue_number)` — entering appends `#<number>` (Q5), leaving
  clears it. 0014 D3's prefix rule keeps this stable under background ticks.

### D2 — Data: one detail query, shared wire structs, no cache

- **`ISSUE_DETAIL_QUERY`** (gql.rs): `repository(owner, name) { issue(number)
  { …§3 field set… } }` — fetched once on open (paint skeleton immediately),
  on header refresh, and after every mutation; same `Envelope` + per-node
  degradation conventions (0007 D3: one bad node degrades alone); runs
  through `run_spec_tracked` (0013) as a new `Kind::IssueDetail`.
- **Wire structs** (gql.rs): `IssueDetail` owns everything the page renders
  (header fields + `labels: Vec<IssueLabel>` + `assignees: Vec<Author>` +
  `comments: IssueCommentSummary`-shaped connection). **Timeline comments
  reuse the existing `PrComment` struct as-is** — it is `IssueComment`
  shaped (0019 added the gate fields to exactly this type); a doc comment
  records that it serves both pages (renaming it would churn PR code and
  tests for zero behavior). `IssueLabel { name, color: Option<String> }`
  (color carried but unused while Q2 stands).
- **Parsing:** `issue_detail_from_value(&Value)` mirrors
  `pr_detail_from_value` (wrapper-node convention, hand-rolled field reads,
  per-node degrade). The comment rows parse through the same node-gate
  closure shape 0019 introduced.
- **Mutations** (service.rs, `run_mutation`): two new documents —
  `ISSUE_CLOSE_MUTATION` (`closeIssue(input: $input)`, variables
  `{ issueId, stateReason? }`) and `ISSUE_REOPEN_MUTATION`
  (`reopenIssue(input: $input)`, `{ issueId }`). Every mutation completion
  triggers the detail refetch; **no optimistic writes** (0019 D6 parity —
  the refetch repaints through the 0017 cache rebuild). Failures surface in
  the timeline-scoped error banner (`ErrorScope::Timeline`), never modals.

### D3 — Page anatomy

```
┌──────────────────────────────────────────────────────────────┐
│ [Open●] Race in retry budget when token expires #487  [↻][↗] │ header (fixed)
│ alice opened this issue 3 days ago · 12 comments             │
├───────────────────────────────────────────────┬──────────────┤
│ description card → comment timeline (scroll)  │ right rail   │
│ → comment box (sticky end)                    │  labels      │
│                                               │  assignees   │
│                                               │  state panel │
└───────────────────────────────────────────────┴──────────────┘
```

- **Header:** state badge (`Open` success / `Closed` danger; when closed
  with `NOT_PLANNED`/`DUPLICATE` the badge copy appends "· not planned" /
  "· duplicate" — `COMPLETED` and open-badge stay bare), `text_lg` semibold
  title, muted `#number`, author + "opened N ago" meta, refresh + ↗ icon
  buttons (tooltips, [DG]). Ids `issue-refresh`, `issue-open-github`.
- **No tabs.** One scroll region (timeline) + fixed rail; the split uses the
  pinned 0016 mechanism (`items_start` + timeline `h_full()` — the rail
  hugs its content and never rides the scroll).
- **Loading:** skeleton rows (header chrome renders immediately).
  **Empty:** muted cells ("No comments yet", "No labels", "No one
  assigned"). **Error:** inline retry row per section; fetch failures keep
  the last good payload (0007 D3).

### D4 — Conversation

- **Description card** first: author avatar, login, age, body as markdown
  (0017 `body_card` seam). **Edit** when `viewerCanUpdate` — the 0019
  `inline_edit_card` with id `issue-description` (auto-grow `4–30`
  Textarea, Save disabled on empty, one editor at a time page-wide).
  Save → `updateIssue { id body }` → refetch.
- **Timeline:** the comment list ascending by `createdAt` (pure sort fn,
  table-tested — trivially the PR merge minus reviews). Each row: the 0019
  comment card — markdown body, age, `(edited)` marker when
  `updatedAt > createdAt`, and Edit/Delete affordances gated by the node's
  own `viewerCan*` flags. **The 0019 state pattern transfers wholesale:**
  `timeline_edit: Option<(String, Entity<TextareaState>)>` +
  `delete_confirm: Option<String>` keyed by node id, one editor or confirm
  at a time, editors dropped on refetch when their node vanished, inline
  two-step delete confirm, ids `issue-comment-{ix}` (+ the 0019 §8
  `debug_selector` probe keys on cards/bars).
- **Comment box** (sticky end): Textarea (`issue-comment-input`) + primary
  Submit (`issue-comment-submit`). Gating (pure fn, table-tested):
  non-empty body, not busy. Submit → `addComment { subjectId: issue node
  id, body }` → refetch; box clears on success; failures keep the draft and
  show the inline error row.

### D5 — Right rail (conversation only)

Three `GroupBox` panels, stacked, content-hugging:

- **Labels:** muted chips (name only, Q2), wrap; empty → muted cell.
- **Assignees:** login rows (avatar + login); empty → muted cell.
- **State:**
  - Open: **Close** button (`Button::danger`, `issue-close`) + — per Q3 — a
    close-reason `Select` (`issue-close-reason`: Completed / Not planned,
    default Completed). Gated by `viewerCanClose` (button hidden, not
    disabled, when the viewer lacks it — affordance honesty per 0019 D3).
  - Closed: **Reopen** button (`issue-reopen`, default variant) gated by
    `viewerCanReopen`; below it a muted line "closed N ago · <reason>".
  - In-flight: busy disables the panel's buttons + spinner; success refetch
    repaints the header badge and swaps the panel state. Failure → timeline
    banner, panel state unchanged.
- Rail panel headers use the 0016 D5 visual language (no new widget work).

### D6 — Token discipline + bridge hygiene ([DG], 0010 D10)

- Semantic colors only (`success`/`danger`/`muted_foreground` + opacity
  tints); no literal color constructors — including label chips (Q2).
- Typography scale only (`text_xs` meta, `text_sm` body, `text_lg` title);
  no one-off pixel sizes.
- Every new interactive element carries `.id()` + role: issue rows already
  do; new ones — `issue-refresh`, `issue-open-github`, `issue-close`,
  `issue-close-reason`, `issue-reopen`, `issue-comment-input`,
  `issue-comment-submit`, plus the 0019-derived
  `issue-comment-{ix}-edit/-delete/-save/-cancel/-confirm` set; timeline
  rows are `Role::ListItem`. A missing node in `tree` is a bug in the
  element, not the bridge.

## 6. Implementation plan (one PR, after this doc merges)

Smaller than 0016 (no commits/checks/files stages): gql.rs (`ISSUE_DETAIL_QUERY`,
`IssueDetail`/`IssueLabel` structs, close/reopen documents, parser + degrade
tests) → service.rs (`Kind::IssueDetail`, two `Mutation` variants, scripted
transport test) → `issue_page.rs` (page entity, rail, conversation, handlers,
probe tests) → page.rs (field, takeover guard, open/close/lifecycle trio,
crumb derivation, row click swap) → docs §8 shipped notes. Reused unchanged:
markdown renderer, `inline_edit_card`, `delete_confirm_card`, comment-gate
parsing, mutation transport, takeover mechanics.

Crates touched: `dashboard-module-github` only. (If the owner prefers it
split — read-path PR, then write-path PR — the seam is D4's comment box: the
read path lands complete without it.)

## 7. Testing

- **Pure fns, table-tested** (issue_page.rs / service.rs): state-badge
  mapping (state × stateReason → copy + tone); comment sort order/stability;
  comment-box gating (empty × busy); close gating truth table
  (`viewerCanClose`/`viewerCanReopen`/busy × panel state); close-reason →
  input mapping.
- **Parse/degrade tests** (gql.rs): full payload parses; per-node degrade
  (comment missing gate fields → affordances off, node intact; missing
  labels/assignees → empty groups, page intact); reason-absent closed issue.
- **`#[gpui::test]` probes** (0018/0019 pattern, real page + scripted
  `MockHttp`): affordances follow viewer flags via `debug_selector` keys;
  edit/confirm swaps + one-editor rule + vanished-node drop; comment box
  clears on success, keeps draft on failure; close/reopen round-trip
  repaints the badge. Fixture payloads wrap in the envelope (§3 note).
- **Scripted transport** (0019 pattern): close/reopen POSTs carry their
  operation names and variables verbatim.
- Existing tests untouched except any 0012 D4 wording that references the
  issue-row browser hand-off.

## 8. Debug-bridge verification (docs/0010)

With the app running, a repo selected, an open issue present:

1. `tree` → issue rows (`issue-<key>`) present; `click issue-<key>` → the
   issue page mounts: header, rail groups, crumb gains `#n`.
2. `key esc` → back to the repo view; crumb suffix cleared; selection
   intact. Module crumb → repo list, page closed (#100).
3. Comment flow: `type` into the textarea, `click issue-comment-submit` →
   `logs` show the POST + refetch; the comment appears.
4. Close → badge flips to Closed; Reopen → back to Open; both in `logs`.

(The a11y-dump regression noted in #113 still blocks `tree`/`click` on this
machine — until it's fixed these run as manual human checks in the PR body.)

## 9. Acceptance criteria

1. Clicking an issue row opens the in-app issue page as a full takeover;
   `Esc`, the repo crumb, and the module crumb all return correctly;
   breadcrumb reads `Home > GitHub > <repo> > #<number>` throughout.
2. Opening an issue while a PR page is open (or vice versa) swaps pages
   cleanly — never both mounted.
3. Header shows the state badge (with reason copy when closed), title,
   number, author/age meta; refresh and open-on-GitHub act.
4. Description edits save (when permitted) and repaint; comment submit
   posts and appears after refetch; failures keep the draft and show the
   inline error.
5. Own comments show Edit/Delete per the 0019 gates; editing, deleting, and
   the `(edited)` marker behave exactly as on the PR page.
6. Labels and assignees render; the State panel offers Close (with reason)
   / Reopen strictly per `viewerCanClose`/`viewerCanReopen`; round-trips
   repaint the header; the repo list updates on the next snapshot tick.
7. Gates: `cargo fmt --all --check`, `cargo clippy --workspace
   --all-targets` (zero warnings), `cargo test --workspace`; doc + README
   index row merged with this PR.

## 10. Human checks (owner)

- [ ] Open issues of several shapes: open, closed-completed,
      closed-not-planned, locked; confirm badge copy and rail states.
- [ ] Comment on an issue; verify on github.com it landed.
- [ ] Close as Completed, then as Not planned on a scratch issue; Reopen
      one; confirm states on github.com.
- [ ] Edit the body of an issue you authored; cancel mid-edit.
- [ ] Edit + delete one of your comments; confirm the `(edited)` marker and
      the removal.
- [ ] Open an issue on a repo where you lack close rights — no Close
      button, not a disabled one.
- [ ] With a PR page open, click an issue row (and the reverse) — clean
      swap, no stale header/rail.

## 11. Follow-ups (GitHub issues, not this PR)

- ~~Timeline event rows (labeled / assigned / closed-by / cross-referenced)
  via `IssueTimelineItems(itemTypes: …)` (Q1 deferral).~~ → designed and
  shipped in docs/0021 (#118); cross-referenced/referenced rows remain a
  follow-up there.
- Data-colored label chips (needs a token-discipline exception decision).
- ~~Title editing; milestone display; label/assignee management.~~ →
  designed and shipped in docs/0022 (#120).
- ~~Reactions; locking.~~ → designed and shipped in docs/0023 (#121);
  issue creation remains (docs/0022 §9-adjacent, #122).
- "+N more" pagination for comments beyond the first 100 (#87 adjacent).

---

## 12. Shipped (2026-09-05, issue #116)

Implemented as designed in one PR (one stage; the §6 read/write split
wasn't needed). The §4 questions resolved to the doc's leans per the
owner's merge of this doc.

Implementation notes beyond the design text:

- **D2 (transport)**: `run_pr_detail` generalized into a shared
  `run_graphql_fetch(http, spec, doc, name)` — the PR and issue detail
  fetches are the same call with different documents, per D2's
  "transport is identical". `Kind::IssueDetail` is the query kind; the
  two write documents ride the existing `Kind::PrMutation` kind with new
  `Mutation::IssueClose`/`IssueReopen` variants (the page passes the
  shared `Comment`/`UpdateDescription`/`UpdateComment`/`DeleteComment`
  variants unchanged).
- **D2 (parse)**: 0019's inline `node_gate` closure lifted to a module
  fn and a `comment_from_node` helper — the PR parser and the issue
  parser now share the exact same `IssueComment` node read (the PR
  parser's behavior is unchanged; its 12 tests cover the refactor).
- **Shared cards**: `body_card` / `inline_edit_card` /
  `delete_confirm_card` (and the small cell builders) take `&App` now —
  `Context<T>` derefs, so every PR call site is unchanged while the
  issue page reuses them verbatim.
- **ErrorScope**: the shared enum gained a `Rail` variant (the issue
  state panel) beside the PR page's `Panel` — each page produces only
  its own variant.
- **Textarea id (D6 deviation)**: the comment Textarea carries no
  explicit element id — the PR page's established pattern (the upstream
  Textarea owns its focus/a11y node); all buttons and panels carry the
  D6 ids as designed.
- **No timeline sort fn (D4 simplification)**: the PR page sorts a
  merged comments+reviews list; the issue page renders the single
  comments connection in the API's chronological order — nothing to
  sort, so no sort fn (the §7 table-tests for it dropped with it).
- **Probe mechanics (0019 §8 lesson applied)**: probe-selector keys
  shipped from day one (`issue-comment-{ix}-actions`, `{id}-card`,
  `{id}-confirm-card`, `issue-state-panel-body`); the envelope-wrap
  convention followed in fixtures (`issue_detail_from_value` takes the
  repository wrapper node).
- **Testing (adds to §7)**: `issue_mutations_post_their_documents`
  extends the scripted-transport pattern to `closeIssue`/`reopenIssue`
  (close always carries the panel's `stateReason`). Workspace totals
  after this PR: 230 tests.
