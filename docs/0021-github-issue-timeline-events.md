# 0021 — GitHub issue page: timeline event rows

Date: 2026-09-06 · Status: design (implementation after this doc merges, against #118)
Upstream pins: gpui-component `94a313a` (v0.6.0) · GitHub GraphQL verified live 2026-09-06

## 1. Context & problem

The issue page (docs/0020, shipped in #117) renders the description and the
`comments` connection — nothing else. Every real issue page interleaves the
event stream with the comments: who added which label, who was assigned, when
the issue was closed (and as what), when the title changed. Without events the
conversation reads as a comment list and loses the "what happened" narrative;
a closed issue's closing comment appears with no visible close above it.

docs/0020 §11 deferred this (Q1): render timeline event rows via
`IssueTimelineItems(itemTypes: …)`. This doc designs exactly that.

## 2. Scope

**In:** assigned / unassigned, labeled / unlabeled, closed (with reason) /
reopened, renamed-title, milestoned / demilestoned — merged into the existing
conversation in chronological order, comments first-class as today.

**Out (follow-ups, not silently dropped):** cross-referenced events
(shipped later, docs/0085 — see "Cross-reference rows" at the end),
referenced events (commit mentions), locked/unlocked (ride #121 locking), project/sub-issue/
connected/pinned events, reactions (see docs/0020 §11 / #121).

## 3. Verified facts (live `gh api graphql` introspection, 2026-09-06)

- `Issue.timelineItems(...)` → **`IssueTimelineItemsConnection!`** with
  `totalCount Int!`, `filteredCount Int!`, `pageCount Int!`, `pageInfo`,
  `nodes: [IssueTimelineItems]`, `updatedAt DateTime!`.
- `IssueTimelineItems` is a **UNION** of ~50 types (`IssueComment` + events).
  Nodes must be read with `... on X` inline fragments.
- Filter arg `itemTypes: [IssueTimelineItemsItemType!]` — enum values are
  per-type: `ASSIGNED_EVENT`, `UNASSIGNED_EVENT`, `LABELED_EVENT`,
  `UNLABELED_EVENT`, `CLOSED_EVENT`, `REOPENED_EVENT`, `RENAMED_TITLE_EVENT`,
  `MILESTONED_EVENT`, `DEMILESTONED_EVENT`, … (also `ISSUE_COMMENT`, which we
  deliberately do not request — see D2). `ISSUE_COMMENT` was verified present
  in the enum; omitting it keeps comments out of this connection.
- Per-type fields verified:
  - `AssignedEvent`/`UnassignedEvent`: `actor`, `createdAt`, `id`,
    **`assignee` (singular `Assignee` union: User/Bot/Organization/Mannequin
    — all have `login`; union selection needs inline fragments)**,
    `assignable`, `intent`.
  - `LabeledEvent`/`UnlabeledEvent`: `actor`, `createdAt`, `id`,
    `label { name color … }` (Label!).
  - `ClosedEvent`: `actor`, `createdAt`, `id`,
    `stateReason (IssueStateReason)`, `closer` (union), `duplicateOf` (union).
  - `ReopenedEvent`: `actor`, `createdAt`, `id`.
  - `RenamedTitleEvent`: `actor`, `createdAt`, `id`,
    `previousTitle String!`, **`currentTitle`** (not "newTitle").
  - `MilestonedEvent`/`DemilestonedEvent`: `actor`, `createdAt`, `id`,
    `milestoneTitle String!`.
- `actor` is the `Actor` interface — `login` selects directly (no fragments).

## 4. Owner questions (lean recorded; doc updated on answer)

**Q1 — which event set for v1?** Lean: the §2 set (assigned/unassigned,
labeled/unlabeled, closed/reopened, renamed, milestoned/demilestoned = 9
`itemTypes` values). Cross-referenced/referenced defer — their rows are
interactive (open the source) and need a second data shape; folding them in
doubles this PR's surface for the least common rows.

**Q2 — how do comments and events interleave, given two capped pages?**
Lean: fetch events-only via the filter (`first: 100`, no `ISSUE_COMMENT`),
merge with the existing gated `comments(first: 100)` in memory, sort by
`createdAt` (stable: same timestamp → comment before event). When either
connection is capped (`filteredCount > nodes.len()` / comments capped), render
one muted "+N more on github.com" row at the bottom of the conversation. This
is the #123 pattern applied to events; #123 remains open for the comments
side.

**Q3 — row affordance?** Lean: pure muted text rows (icon + sentence),
non-interactive, `issue-event-{ix}` ids (roles per 0010 D10 apply only to
interactive elements; these still get ids for the dump). Label names render
as the same muted chip style as the rail (0020 Q2 lean). Actor = `login`
bolded via the existing emphasis token, not a new color.

## 5. Design

### D1 — Query: extend `ISSUE_DETAIL_QUERY`, not a second fetch

One round trip per page view remains (0020 D2). `ISSUE_DETAIL_QUERY` gains:

```graphql
timelineItems(first: 100, itemTypes: [ASSIGNED_EVENT, UNASSIGNED_EVENT,
    LABELED_EVENT, UNLABELED_EVENT, CLOSED_EVENT, REOPENED_EVENT,
    RENAMED_TITLE_EVENT, MILESTONED_EVENT, DEMILESTONED_EVENT]) {
  totalCount filteredCount
  nodes { __typename id createdAt actor { login } ... per-kind fields }
}
```

Per-kind selections: label `{ name }`; `assignee` via four login fragments
(User/Bot/Organization/Mannequin); ClosedEvent `stateReason`; RenamedTitle
`previousTitle currentTitle`; (De)Milestoned `milestoneTitle`. No `ISSUE_COMMENT`
in the filter — comments stay on their gated connection (0019 gates cannot
come from timeline nodes).

### D2 — Wire structs: hand-parsed, one enum, per-node degrade (0007 D3)

```rust
pub(crate) enum IssueEventKind {
    Assigned { login: String },  Unassigned { login: String },
    Labeled { label: String },   Unlabeled { label: String },
    Closed { reason: Option<String> },  Reopened,
    Renamed { from: String, to: String },
    Milestoned { title: String },       Demilestoned { title: String },
}
pub(crate) struct IssueEvent {
    pub(crate) id: String,
    pub(crate) actor_login: Option<String>,
    pub(crate) created_at: Option<String>,
    pub(crate) kind: IssueEventKind,
}
```

Parse: match `__typename`; unknown names skip (defensive — the filter should
make them impossible, but degrade beats panic, 0007 D3). `actor` may be null
(ghost/deleted) → row copy degrades to "Someone". `IssueDetail` gains
`events: Vec<IssueEvent>` + `events_capped: bool`.

### D3 — Conversation: merged, sorted, single scroll

`IssueDetailPage::timeline()` builds `Vec<TimelineRow>` =
`Comment(&PrComment) | Event(&IssueEvent)`, sorted by `createdAt` ascending,
stable tiebreak comment-before-event (both strings RFC3339; missing stamps
sort first, same as 0016's tolerance). This reverses 0020's "no sort fn"
simplification — the sort fn returns, with table tests. Cap rows render once
at the end: "+N more events on github.com" (opens the issue URL, id
`issue-events-more`).

Row copy (muted, sentence-per-kind): "alice added the `bug` label · 3d ago",
"closed as not planned · 2d ago" (`state_reason_word` reuse), "bob assigned
carol", "title changed from A to B", "added to milestone M". Closed copy
reuses 0020's badge vocabulary — one vocabulary per concept.

### D4 — No editor/cap interactions

Events are read-only decoration on the conversation: they never carry Edit/
Delete (no viewer gates exist), never participate in the one-editor rule, and
`apply_detail_result`'s vanished-node editor drop keeps keying on comment ids
only. Refetch-after-mutation unchanged — events arrive with the same payload.

### D5 — Token discipline + bridge hygiene (0020 D6, 0010 D10)

Muted-foreground text rows, `text_sm`; label chips = the rail's chip style
(semantic tokens; no data-colored hex — that decision is #119). Ids:
`issue-event-{ix}`, `issue-events-more`. No new interactive elements → no new
roles beyond the existing container. Probe selector key shipped from day one
(0019 §8 lesson): conversation container already dumped; rows get ids only.

## 6. Implementation plan (one PR, after this doc merges)

1. gql.rs: extend `ISSUE_DETAIL_QUERY` (D1); `IssueEvent`/`IssueEventKind` +
   parse into `issue_detail_from_value` (D2); degrade + unknown-typename tests
   with full envelope wrap.
2. issue_page.rs: `TimelineRow` merge/sort fn (D3) + row rendering between
   description and comments; cap row; `issue-events-more` click → `open_url`.
3. page.rs / service.rs: no changes (payload-only extension; transport untouched).
4. docs/0020 §11 note ("timeline rows: see 0021"), this doc §8 shipped notes.

## 7. Testing

- gql: events parse from a full envelope; unknown `__typename` skipped; ghost
  actor (null) degrades; `assignee` fragments read logins for all four union
  member types; capped flag from `filteredCount > nodes.len()`.
- Table tests: sort/interleave order incl. tiebreak and missing-stamp cases;
  row-copy table per kind (incl. "closed as not planned"/"Someone" degrades).
- Probe tests (real page): rows render in order between description and
  comments (ids `issue-event-{0..n}` measurable via `debug_bounds`); cap row
  appears iff capped; events never break the one-editor rule (edit a comment
  adjacent to events; `issue-comment-0-card` still swaps).

## 8. Debug-bridge verification (docs/0010)

Same posture as 0020 §8: the a11y-dump regression (unfiled, environmental)
still blocks `tree`/`click`, so interactive checks run as manual human checks
in the PR body; `logs` correlation (`issue-detail applied:` line gains an
events count) and the probe tests carry the automated share.

## 9. Acceptance criteria

1. An issue with labels/assignees/title-changes/close/reopen shows those
   events interleaved in order among the comments, muted and non-interactive.
2. ClosedEvent copy names the reason ("closed as not planned") consistent with
   the header badge.
3. Busy issues (>100 events or >100 comments) show one "+N more on github.com"
   row instead of truncating silently.
4. Comment Edit/Delete/confirm flows behave exactly as shipped in 0019/0020
   with events present.
5. Gates green; new tests per §7.

## 10. Human checks (owner)

- [ ] Own a scratch issue: label it, assign, rename, close (both reasons),
      reopen — rows appear in order, copy reads right, ghost-actor case (if
      producible) says "Someone".
- [ ] A busy upstream issue (find one with 100+ events): cap row present, opens github.com.
- [ ] Comment on the issue — new comment lands below the latest event after refetch.

## 11. Follow-ups (GitHub issues, not this PR)

- ~~Cross-referenced event rows~~ shipped with docs/0085 (#918, #923); referenced (commit) events stay a follow-up there.
- Locked/unlocked rows (with #121).
- ~~Unify the "+N more" cap-row component with #123's comments cap row.~~
  Done with #123 (2026-09-06): the row is now the shared
  `pr_page::cap_row`, and the PR conversation grew the same row
  (0016 §12).

---

## 8. Shipped (2026-09-06, issue #118)

Implemented as designed (D1–D5). Implementation notes beyond the design
text:

- **`events_total` added to `IssueDetail`** (a small D2 extension): D3's
  cap-row copy needs the missing-row count, and the D2 bool alone cannot
  produce it. `events_capped` stays (derived: `events_total >` page len).
- **Probe selector keys, again (D5 amendment)**: D5 said "ids only", but
  §7's plan measures rows via `debug_bounds`, which sees only
  `.debug_selector()` keys (the 0019 §8 lesson this repo keeps
  re-learning). Event rows and the cap row carry test-only selector keys
  (`issue-event-{ix}`, `issue-events-more`); the accesskit ids are
  unchanged.
- **The cap row is the one interactive addition** — a ghost `Button` with
  id + role (`issue-events-more`), opening the issue URL. `missing` sums
  both shortfalls (hidden events + hidden comments); events and comments
  are disjoint node sets, so the sum is the honest hidden-row count.
- **Comment keys unchanged**: in the merged loop, comments keep their
  comments-connection index (`issue-comment-{ix}`) — the md cache and the
  0019 editor swap key on it; only the render order changes.
- **`sort_by_key`**: the merge sort keys on `(Option<i64> stamp, u8 rank)`;
  `Option` ordering puts missing stamps first, rank 0 comments before
  rank 1 events on equal stamps (D3 tiebreak).
- **Testing (adds to §7)**: gql union parse/degrade/cap test (14 nodes:
  every kind, all four Assignee union members, unknown `__typename`,
  assignee-less node); sentence + interleave tables; a probe test asserting
  render order across the merge (event rows and comment action bars
  measured by y), cap-row arming only on truncation, and the 0019 editor
  swap surviving events. Workspace totals after this PR: 234 tests.

- **2026-09-07 live-API fix**: the shipped query selected `id`,
  `createdAt`, and `actor { login }` **directly on the
  `IssueTimelineItems` union** — GitHub rejects any bare selection on a
  union, so *every* issue-detail fetch failed server-side ("Selections
  can't be made directly on unions"), the page fell back to its
  error/empty state, and no events ever rendered in production despite
  the fixture suites passing (they never hit the wire). Fix: the three
  shared fields now ride each member's inline fragment (the response
  merges them into the node object, so the parser and fixtures are
  unchanged); a document-level tripwire test asserts every member
  fragment carries them. Found via the app log during the owner's
  human-check pass — the debug-bridge `logs` channel doing exactly its
  job (docs/0010).

---

## 9. Cross-reference rows (docs/0085, #923)

The issue page shows `CrossReferencedEvent` rows ("octo-dev mentioned this
issue in Fix loader #42") through the row, wire type and parser shared with
the PR page (`xref_row/`, `gql/xref_events.rs`); the page-specific wiring is
`issue_page/xref.rs`.

- **Query (amends D1):** `ISSUE_DETAIL_QUERY` gains the aliased
  `references: timelineItems(last: 100, itemTypes: [CROSS_REFERENCED_EVENT])`
  window, apart from the `first: 100` events window so a burst of mentions
  cannot push state events out, and the repository's `nameWithOwner` (a source
  that is the page itself drops). `IssueDetail.references` carries the parsed
  rows (`#[serde(default)]`, so persisted payloads still load). A response
  with `errors` and a null list element (a source the viewer cannot read)
  still lands its data: the unreadable row drops.
- **Merge (amends D3):** `TimelineRow::Xref(ix, event)` joins the merged
  timeline at the same rank as events: by time, the comment first at an equal
  stamp, missing stamps first. The row ids are
  `issue-event-{events.len() + ix}`, after the event rows, so the ids stay
  unique. Exact repeats (same source, same actor) fold at parse time.
- **Cap row:** the references the window cut (`filteredCount` minus the
  carried rows, beyond what the parser dropped) join the "+N more on
  github.com" count with the hidden events and comments.
- **Wording:** page kind "issue"; a PR source with `willCloseTarget` reads
  "{actor} linked a pull request that will close this issue", every other
  source "{actor} mentioned this issue in"; no actor reads "Someone".
- **Activation:** the whole row is one Button (tab stop). A source in a
  repository the module lists emits `IssuePageEvent::OpenSubject` with the
  title; the host opens the in-app page through its `open_subject` (the
  notification page's hand-off), which records a trail place, so Back returns
  to the issue. Any other source opens in the browser and the page stays.
- **No leave guard:** the issue page has none. Esc closes it at once, and it
  holds nothing that opening another page could lose: the comment draft
  saves to the draft store 500 ms after the last keystroke (docs/0045) and
  is restored when the page reopens (verified: an unsent draft survives
  the round trip and is sent from the restored box). So the row emits
  straight away, where the PR page runs its unsent-review-draft guard first.
- **Fixtures and receipts:** `IssueDetail.201` (open, a PR that will close
  it, a mention from a listed repo, an `atlas` PR, an untracked source, a
  deleted actor), `.198` (closed, locked, seen by a reader) and `.202`
  (100 of 130 references), listed by `RepoIssues.beacon`; the issue page's
  first structural golden is `github-issue-populated`; `timeline-xref.txt`
  walks the issue page (docs/0071 §4). Known gap: the issue page's comment
  box traps Tab (#931), so the keyboard walk runs on the locked issue, which
  has none. The close-reason select gained an accessible name, which the
  first issue-page audit asked for.
