# 0043 — GitHub Module: PR Page Metadata Editing (reviewers, labels, assignees, milestone)

- **Status**: design — implementation tracked in #189
- **Prerequisites**: docs/0022 (issue metadata editing — the pattern this
  ports), docs/0016 (PR detail page), docs/0019 (inline editors),
  docs/0031/0032 (fetch-record, detail auto-refresh), docs/0010 (debug bridge)
- **Follows**: #189 (PR write operations gap, docs/0029 §3)

## 1. Summary

The issue page has full metadata editing — labels, assignees, milestone —
via rail pickers (docs/0022). The PR page's rail carries only the Merge
and Checks panels: reviewers, labels, assignees and milestone are
display-less and uneditable, so adding a reviewer means opening GitHub in
a browser. This doc ports the 0022 machinery and adds the one PR-specific
picker GitHub has and issues don't: **review requests**.

- **Reviewers** — a rail panel plus a checklist picker posting
  `requestReviews`.
- **Labels, assignees, milestone** — the 0022 rail panels and pickers,
  riding `updatePullRequest` instead of `updateIssue`.

No title/body editing here — description editing already exists (docs/0016
D4) and title editing is a separate follow-up.

## 2. Upstream facts (live introspection 2026-09-17, pinned schema IDL)

### 2.1 Write side — `UpdatePullRequestInput` carries the metadata trio

Relevant `UpdatePullRequestInput` inputFields (verified):

| field | type | note |
| --- | --- | --- |
| `pullRequestId` | `ID!` | the PR node id (already fetched) |
| `labelIds` | `[ID!]` | **replaces the whole label set** |
| `assigneeIds` | `[ID!]` | **replaces the whole assignee set** |
| `milestoneId` | `ID` (nullable) | set; `null` clears |
| `title` / `body` | `String` | already used via the sibling paths |

Same shape as `UpdateIssueInput` (0022 §2.1) — one mutation covers the
trio, replace-not-delta on `labelIds` / `assigneeIds`. The dedicated
delta mutations stay unnecessary, same reasoning as 0022.

Note the description editor (docs/0016) posts `updateIssue` against the
PR node id — a quirk that predates this doc and stays untouched. Metadata
gets the PR-honest document (`updatePullRequest`), not a fourth user of
the quirk.

### 2.2 Write side — review requests ride `requestReviews`

`RequestReviewsInput` (verified):

| field | type | note |
| --- | --- | --- |
| `pullRequestId` | `ID!` | |
| `userIds` | `[ID!]` | user node ids |
| `botIds` / `teamIds` | `[ID!]` | verified present, **out of scope** (§8) |
| `union` | `Boolean` (default `false`) | **`true` replaces the set** — "Add users to the set rather than replace" |

Replace semantics again: the picker always posts `union: true` with the
complete target user-id list; an empty list clears all requests. There is
no remove-reviewers mutation in the schema — `requestReviews` +
`union: true` is the only write, so clear-all goes through it too.

### 2.3 Read side — gates on `PullRequest`

Verified on `PullRequest`:

- `viewerCanLabel: Boolean!`, `viewerCanAssign: Boolean!` — the same
  granular gates the issue page uses.
- **No `viewerCanSetMilestone`** — that flag exists on `Issue` only. The
  milestone gate needs a substitute: `repository.viewerPermission`
  (`RepositoryPermission`, verified on `Repository`), values `ADMIN |
  MAINTAIN | WRITE | TRIAGE | READ`. Triage-or-above is the upstream
  requirement for milestone edits.
- **No review-request gate at all** — nothing like
  `viewerCanRequestReview` exists. Upstream rule: the PR author or
  triage-or-above may request reviews. Gate = `viewerPermission`
  triage-or-above **or** viewer login == PR author login (viewer login
  is already on `GithubState.user` from the global snapshot).

The `repository` node of `PR_DETAIL_QUERY` stays raw
(`serde_json::Value`) by design, so adding
`viewerPermission` to the envelope is a query-text change plus a raw
read — the same path `mergeCommitAllowed` &c. already take (merge
methods, docs/0016 D5).

### 2.4 Picker sources — already shipped

`repository.labels`, `repository.assignableUsers`,
`repository.milestones` fetch specs (`Kind::RepoLabels`,
`Kind::RepoAssignableUsers`, `Kind::RepoMilestones`) and their query
consts exist since docs/0022 — service-level, repo-scoped, **reused
as-is**. Reviewers select from the same `assignableUsers` page the
assignee picker uses: one fetch, two consumers.

### 2.5 Read side — what `PR_DETAIL_QUERY` grows

`PullRequest` fields verified present: `labels`, `assignees`,
`milestone`, `reviewRequests` (`PullRequestReviewRequest.requestedReviewer`
is the `RequestedReviewer` union — `Bot | EnterpriseTeam | Mannequin |
Team | User`). Today the query selects none of these (the repo-view list
query carries login-only review requests for docs/0042 filters — that
shape is untouched; the detail page needs node **ids** to write back).

## 3. Current gaps (what the fetch must grow)

`PR_DETAIL_QUERY` additions, all on the `pullRequest` node:

1. `labels(first: 20) { totalCount nodes { id name color } }`,
2. `assignees(first: 20) { totalCount nodes { id login avatarUrl } }`,
3. `milestone { id title state }` (nullable),
4. `reviewRequests(first: 20) { totalCount nodes { requestedReviewer {
   __typename ... on User { id login } } } }` — with `id`, unlike the
   list query's login-only selection (§2.5),
5. `viewerCanLabel viewerCanAssign`,
6. envelope: `viewerPermission` on the raw `repository` node.

`totalCount` is load-bearing again (D3), same as 0022.

## 4. Design decisions

### D1 — one mutation, three operation identities (+ one for reviewers)

Labels, assignees and milestone post `updatePullRequest` via a new
`PR_UPDATE_META_MUTATION` document. New `BusyOp` variants — `PrSetLabels`,
`PrSetAssignees`, `PrSetMilestone` — share the document; the variant is
operation identity (busy label, log line, gating), never a new document.
The input carries exactly one field-group per post, never a combined
write. Reviewers post `requestReviews` via a new
`PR_REQUEST_REVIEWS_MUTATION` (`BusyOp::PrSetReviewers`).

### D2 — no optimistic writes; refetch after every mutation

Carried unchanged from 0022 D2 and the PR page's own Merge flow: the
mutation resolves, `fetch_detail` refetches, the rail re-renders from the
new payload. Every metadata failure surfaces in the rail
(`ErrorScope::Rail` — one slot, 0022's single-rail-error precedent) and
keeps the picker open.

### D3 — replace-semantics safety: same arming rule, now with a reviewer clause

The 0022 D3 rule carries verbatim: a picker's Edit affordance arms only
when **all** of —

- the viewer gate holds (D4),
- every currently-applied node parsed with an `id`, and
- the connection is complete (`totalCount == nodes.len()`).

— and stays hidden, not disabled, when any check fails. The reviewer
picker adds its own clause: every current review request must parse as a
`User` **with an id**. A requested team, bot or mannequin can't be
reproduced by a user-id replace write, so their presence forces the Edit
hidden (display-only panel) instead of offering a save that would drop
them. Same degradation interaction as truncated label pages — real
fixtures hit it (any repo with a CODEOWNERS team request).

The merge-beyond-the-guard behavior ships too (0022 §11): applied
entries the source page missed are appended to the checklist as checked
rows — dropping them would be exactly the silent-loss bug D3 exists to
prevent.

### D4 — gates: two real flags, two documented substitutions

| panel | gate | source |
| --- | --- | --- |
| Labels | `viewerCanLabel` | verified on `PullRequest` |
| Assignees | `viewerCanAssign` | verified on `PullRequest` |
| Milestone | permission ∈ {TRIAGE, WRITE, MAINTAIN, ADMIN} | `repository.viewerPermission` — no PR-level flag exists (§2.3) |
| Reviewers | same permission test **or** viewer is the author | no flag exists (§2.3); author identity from `GithubState.user` |

`viewerPermission` may be `null` (implicit read access on public repos) —
`null`/`READ` disarms the two substituted gates. That matches github.com,
which hides the sidebar Edit affordances from non-collaborators.

### D5 — rail: metadata panels join Merge and Checks

Rail order: **Merge → Reviewers → Labels → Assignees → Milestone →
Checks**. Each panel is the 0022 shape — display state with a trailing
Edit affordance; activated, the body swaps to the checklist card; Save
posts the complete id set, Cancel/Esc restores. Panel specifics:

- **Reviewers** — display: login rows (avatar where present) + muted
  "Review requested" styling; candidates: `assignableUsers` minus the PR
  author row (GitHub's own picker excludes them). Empty display state:
  the muted "None" cell carrying the Edit affordance when armed (0022
  §11 panel-home note), nothing at all when disarmed.
- **Labels** — display reuses `page::label_pill` (the single data-color
  pill builder, docs/0002 D5 exception via `data_color`) — the reuse
  docs/0022 §12 D5 anticipated. Checklist rows show the name chip.
- **Assignees** — display: login + avatar rows; checklist rows likewise.
- **Milestone** — display cell with the muted `OPEN`/`CLOSED` tag; the
  single-select picker has the leading "No milestone" row posting
  `milestoneId: null`; closed milestones on the PR still render and stay
  checkable (0022 D8).

Source fetches are lazy on first open, cached in session-only page
state, re-fetched on refresh while open — the 0022 D5 lifecycle verbatim,
with `Kind::*` reused and one shared `assignableUsers` page behind the
assignee and reviewer pickers (one fetch, two consumers, same repo).

### D6 — shared machinery moves to `pickers.rs`, renderers stay page-local

The pure picker machinery — `PickerRow`, `PickerDraft`,
`PickerFetch<T>`, the source→kind mapping, and the save computations
(current set → target id list, per operation) — moves from
`issue_page.rs` to a shared `pickers.rs` module; the issue page delegates.
Rationale: the replace-semantics save computation is exactly the code
that must not fork between the two pages. The panel/checklist renderers
and `panel_title` stay page-local — they are thin, and their probe ids,
gates and listeners are page-specific.

### D7 — one editor at a time; busy blocks arming; Esc ordering

The PR page's existing editor set (description editor) extends with the
four pickers under the 0019 one-editor rule: mutually exclusive, opening
one closes the others, `busy` blocks arming. Esc cancels the topmost
surface and only closes the page when nothing is open (0022 §11 Esc
note). `BusyOp::*` failures render in the rail error slot (D2).

The surfaces Esc cancels, topmost first (#744): the discard confirm
(below), the Merge/Close confirm, the Update-branch rebase confirm, the
gated-runs review panel, the delete confirms, the row and thread-comment
editors, the line and reply composers, the resolve-reason picker, the
description editor, the rail pickers. A surface added to the page joins
this list in `cancel_top_editor`.

When nothing is open, Esc closes the page — unless it holds unsent review
drafts (queued review comments, the finish-review summary, or text in the
line composer). Then an inline confirm asks "Discard N review drafts?"
with **Discard** and **Keep editing**; Esc on the confirm is Keep
editing. The same confirm guards the repo crumb and the module crumb.
Navigation the shell commits before the module hears of it (a sidebar
jump to another module, history Back/Forward) and sign-out cannot ask
first and drop the drafts. The drafts stay session-only; persisting them
is #281's scope.

### D8 — ids, roles, probe keys (0010 D10)

Every new interactive element gets an id + role; probe selector keys ship
from day one, mirroring the issue page's:

`pr-reviewers-edit`, `pr-reviewers-card`, `pr-reviewers-row-{ix}`,
`pr-labels-edit`, `pr-labels-card`, `pr-labels-row-{ix}`,
`pr-assignees-edit`, `pr-assignees-card`, `pr-assignees-row-{ix}`,
`pr-milestones-edit`, `pr-milestones-card`, `pr-milestones-row-{ix}`,
`pr-milestone` (display cell). Save/Cancel ride the shared
`inline_edit_card` ids. The label display keeps the row-level
`pr-labels-{key}` container probe untouched (docs/0042 probe contract).

## 5. Query and payload shapes

```
# PR_DETAIL_QUERY additions (pullRequest node)
labels(first: 20) { totalCount nodes { id name color } }
assignees(first: 20) { totalCount nodes { id login avatarUrl } }
milestone { id title state }
reviewRequests(first: 20) {
  totalCount nodes { requestedReviewer { __typename ... on User { id login } } }
}
viewerCanLabel viewerCanAssign

# envelope (raw repository node)
viewerPermission
```

New mutation documents:

```
mutation PrUpdateMetadata($input: UpdatePullRequestInput!) {
  updatePullRequest(input: $input) { pullRequest { id } }
}
mutation PrRequestReviews($input: RequestReviewsInput!) {
  requestReviews(input: $input) { pullRequest { id } }
}
```

Inputs (one field-group per post): `{ pullRequestId, labelIds }` /
`{ pullRequestId, assigneeIds }` / `{ pullRequestId, milestoneId }` (set
or `null` clear) / `{ pullRequestId, userIds, union: true }` (empty list
clears).

Parsed page types (session-only, `PrDetail` grows):
`PrLabel {id name color}`, `PrAssignee {id login avatar_url}`,
`PrMilestone {id title state}`, `PrReviewRequest {id login}` — `id` is
`Option<String>`; `None` flags a degraded node for D3. The detail-level
review-request type is distinct from the list query's login-only
`ReviewRequestsSummary` (docs/0042 D4 stays untouched).

## 6. UI layout (no change to the split)

The rail widens by up to four panels between Merge and Checks (§4 D5);
the conversation column is untouched. Editors swap in place in the rail
— no dialogs, consistent with 0019/0022. The header gains nothing.

## 7. Testing

- **gql parse tests**: new `PrDetail` fields parse; per-node degradation
  (missing id → flagged); a team/bot review request parses to the
  D3-blocking shape; envelope `viewerPermission` reads off the raw node,
  absent → `None`.
- **query-shape tests**: `PR_DETAIL_QUERY` selects the six §5 additions
  (the 0022/0042-style containment checks); the list query's login-only
  selection is asserted unchanged.
- **payload tests**: each op posts exactly its one field-group on
  `updatePullRequest`; `requestReviews` posts `union: true` with the
  complete target set; empty set posts the clear.
- **gating tables**: `viewerCanLabel/Assign` false → affordance hidden;
  permission `READ`/`null` → milestone + reviewers hidden (labels/
  assignees still per flag); author login match arms the reviewer picker
  without permission; D3 guards (missing id, truncated connection,
  team-requested) hide the respective Edit.
- **save computations**: add AND remove cases produce the full target
  list; reviewers exclude the author; unchanged milestone closes without
  posting (0022 §11).
- **probe tests**: panel → checklist swap and back; one-editor rule
  across description + four pickers (D7); busy blocking; refetch
  re-renders the rail; Esc ordering. Cap/regression probes reuse the D8
  keys.

## 8. Explicit non-goals

- Team / bot review requests (display-only when requested by others;
  the app never requests them).
- Type-ahead search in pickers (0022 D6 deferral carries). *(→ shipped
  in #197: filter inputs on the labels/assignees/reviewers checklists on
  both pages — see the 0022 D6 amendment.)*
- Suggested reviewers, review-request waves/notifications.
- PR title editing (separate follow-up; #189 scopes the four metadata
  surfaces only).
- ProjectV2 fields, label/assignee/milestone creation.

## 9. Scope valve

If the `pickers.rs` extraction (D6) turns out to churn the shipped issue
page beyond a mechanical move, land the PR panels with the machinery
copied and file the extraction as its own issue — decided at
implementation, not by growing this design. The reviewer panel may
similarly split off alone (it is the only genuinely new picker); the
other three are straight ports.

## 10. Acceptance criteria

1. Reviewer added and removed via the picker; the panel tracks the
   refetch; clearing all requests posts the empty set.
2. Labels/assignees add and remove; Save posts once; an >20-label PR
   hides the label Edit instead of offering a destructive save (D3).
3. Milestone set and cleared on a repo where the viewer holds triage+;
   a triage-less viewer sees no Edit on milestone or reviewers, but the
   author still sees the reviewer Edit (D4).
4. A PR with a team review request shows the panel display-only (D3).
5. All failure paths surface in the rail and keep their picker open.
6. Workspace gates stay green; new probes cover every D8 key; the
   issue-page suites stay green — no key renames.

---
## 11. Shipped (2026-09-17, issue #189)

*(#197 later added the 0022-D6 type-ahead filter to the checklist
pickers this doc's rail shipped — page-local renderers on both pages,
shared `picker_row_matches` predicate in `pickers.rs`.)*

Implemented as designed, in one PR. The §9 scope valve stayed shut: the
`pickers.rs` extraction was a mechanical move (the issue page's
table-tested pure helpers relocated verbatim, plus the arming core and
the shared `triage_gate`), and the reviewer panel shipped with the rest.

Implementation notes beyond the design text:

- **Reuse, not parallel types**: the PR detail's labels/assignees/
  milestone parse straight into the 0022 node shapes (`IssueLabel`,
  `IssueAssignee`, `IssueMilestone`) — the row builders take one type
  per surface and the pages can't drift.
- **D3, reviewer clause mechanics**: a team/bot/mannequin request parses
  to `id: None, login: ""` ("team or bot" display row); a *null*
  `requestedReviewer` ref skips entirely — the node count then sits
  below `totalCount`, which disarms the picker the same way. Both
  shapes are live fixtures.
- **D7, the 0022 refusal model**: a second picker cannot open while one
  is up (the Edit affordances hide and `start_*` refuses) — the probe
  test pins the refusal, not a close.
- **Esc extends the PR page**: `DismissPrPage` now cancels the topmost
  surface (delete confirms → row editors → composers → description →
  picker) before closing the page — the 0022 §11 ordering.
- **Realign on auto-refresh** (docs/0032 interplay): a picker open
  across a detail refetch rebuilds by id, choices preserved.
- **`pr_meta_mutation`** is the single pure (mutation, variables)
  builder for all four saves — the payload tests assert the
  one-field-group rule and the `union: true` replace/clear semantics
  against it.

Workspace: 419 tests green (was 407); clippy/fmt clean; probe keys for
every new interactive element shipped per D8.
