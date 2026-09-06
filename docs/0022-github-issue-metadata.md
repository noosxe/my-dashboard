# 0022 — GitHub Module: Issue Page Metadata Editing (title, labels, assignees, milestone)

- **Status**: design — implementation tracked in #120
- **Prerequisites**: docs/0020 (issue page), docs/0019 (inline editors),
  docs/0016 §6.2 (mutation transport), docs/0010 (debug bridge)
- **Follows**: #120 (issue-page follow-up; grouped by #117's §11 because all
  four edits share the viewer-gate + refetch machinery)

## 1. Summary

The issue page's rail is display-only today: title, labels, and assignees can
be read but not edited, and the milestone is invisible. This doc designs the
metadata write path:

- **Title editing** — header Edit affordance swapping to an inline editor.
- **Labels + assignees management** — rail pickers (checklists) that add and
  remove entries.
- **Milestone** — a rail display cell plus a set/clear picker (same pattern;
  see §9 for the pre-authorized split if the picker work balloons).

The unifying upstream fact (verified live, §2): **one mutation covers all
four** — `UpdateIssueInput` carries `title`, `labelIds`, `assigneeIds`, and
`milestoneId` — and the page already posts that mutation for body edits and
close/reopen. No new GraphQL documents are needed.

## 2. Upstream facts (live introspection 2026-09-06, `gh api graphql`)

### 2.1 Write side — `UpdateIssueInput` carries everything

Relevant `UpdateIssueInput` inputFields:

| field | type | note |
| --- | --- | --- |
| `id` | `ID!` | the issue node id (already fetched) |
| `title` | `String` | title edit |
| `body` | `String` | already used (0017) |
| `labelIds` | `[ID!]` | **replaces the whole label set** |
| `assigneeIds` | `[ID!]` | **replaces the whole assignee set** |
| `milestoneId` | `ID` (nullable) | set; `null` clears |
| `state` | `IssueState` | already used (close/reopen) |

Consequences:

- The dedicated delta mutations (`addLabelsToLabelable`,
  `removeLabelsFromLabelable`, `addAssigneesToAssignable`,
  `removeAssigneesFromAssignable` — all verified present) are **unnecessary**:
  they would add four documents and payload parsers to avoid one
  client-side list computation.
- `labelIds` / `assigneeIds` are **replace, not delta**. The client must send
  the complete target list. See D3 for the safety interactions this forces.
- The mutation document is byte-identical to the PR description editor's
  (`updateIssue(input: $input) { issue { id } }`, gql.rs) — transport is
  already wired end to end.

### 2.2 Read side — gates and milestone

`Issue` fields verified present:

- `milestone: Milestone` — `Milestone` carries `id`, `title`, `state`
  (`MilestoneState` enum: `OPEN | CLOSED`), `number`.
- Granular gates: `viewerCanUpdate` (title/body — already fetched),
  **`viewerCanLabel`**, **`viewerCanAssign`**, **`viewerCanSetMilestone`**
  (all verified). Each affordance gates independently — no umbrella gate
  needed (and `viewerCanUpdateMetadata` exists but is coarser than these).

### 2.3 Picker sources (repository-level)

- `repository.labels(first:, query:, orderBy: {field: LabelOrderField,
  direction: OrderDirection})` — `LabelOrderField` = `NAME | CREATED_AT |
  ISSUE_COUNT`. `Label` nodes carry `id name color` (plus more).
- `repository.assignableUsers(first:, query:)` — nodes `{ id login
  avatarUrl }`.
- `repository.milestones(first:, states:)` — `states` takes `MilestoneState`
  values; nodes are the §2.2 Milestone shape.
- The `query:` filter args exist but are **deferred** (D6): v1 pickers show a
  plain first page; this repo's label/assignee sets are small.

## 3. Current gaps (what the fetch must grow)

`ISSUE_DETAIL_QUERY` today: labels nodes `{ name color }`, assignees nodes
`{ login avatarUrl }`, `viewerCanUpdate/Close/Reopen`. For this doc:

1. labels nodes gain **`id`** (mutation targets),
2. assignees nodes gain **`id`**,
3. `milestone { id title state }` (nullable),
4. `viewerCanLabel`, `viewerCanAssign`, `viewerCanSetMilestone`.

`totalCount` is already fetched on both connections — it becomes load-bearing
(D3).

## 4. Design decisions

### D1 — one mutation, four operation identities

Every metadata write posts `updateIssue`. New `Mutation` variants —
`IssueSetTitle`, `IssueSetLabels`, `IssueSetAssignees`, `IssueSetMilestone` —
all mapping to the same document text the PR/issue body editors use. The
variant's job is operation identity (busy label, log line, gating), not a new
document. The page builds each input exactly as it does today.

### D2 — no optimistic writes; refetch after every mutation

Carried unchanged from 0020 D6/D7: the mutation resolves, the page refetches
`IssueDetail`, the rail re-renders from the new payload. Failures keep the
editor open with an `ErrorScope::Rail` message (0019 pattern).

### D3 — replace-semantics safety: edit arms only on complete, fully-identified state

Because `labelIds` / `assigneeIds` replace the full set, a save computed from
partial state would silently drop entries the page never saw. A picker's Edit
affordance therefore arms only when **all** of:

- the viewer gate holds (`viewerCanLabel` / `viewerCanAssign` /
  `viewerCanSetMilestone`),
- every currently-applied node parsed with an `id`, and
- the connection is complete: `totalCount == nodes.len()` (`first: 20` may
  truncate).

When any check fails, the affordance stays hidden (not disabled — 0020 D5)
and the panel keeps its display-only look. This is a degradation interaction,
not a test fixture: long-running issues with >20 labels genuinely hit it.

Concurrency: the replace window is render → Save. Like the 0017 body edit,
the last save wins; the window is kept short (save on picker Save, immediate
refetch). Accepted and documented — same trade-off the body editor shipped
with.

### D4 — title editing reuses the 0019 editor card

Header title gains an Edit affordance (gated `viewerCanUpdate`). Swapping to
an `inline_edit_card` ("Edit title"), Save gated on non-empty (same pure
submit-gate pattern as comments), Cancel/Esc restores. The title editor
participates in the one-editor rule (D7).

### D5 — pickers are checklists in the rail

Each rail panel (labels, assignees, milestone) gains a small Edit affordance.
Activated, the panel body swaps to a checklist card:

- one `Checkbox` row per candidate (checked = currently applied); label rows
  show the name chip, assignee rows the login (avatar), milestone rows the
  title with an `OPEN`/`CLOSED` tag;
- the milestone picker is single-select with a leading **"No milestone"** row
  (checking it posts `milestoneId: null` — clearing);
- **Save** posts the complete new id list (D1/D3); **Cancel/Esc** restores the
  display panel without writing;
- rows are display-only (no per-row affordances); the row set is the picker.

Picker source data is fetched lazily on first open (the files-sidebar
`PrFiles` pattern): new `Kind::RepoLabels`, `Kind::RepoAssignableUsers`,
`Kind::RepoMilestones` specs — GraphQL queries against `repository.*`
(§2.3), parsed page-side, cached in session-only page state, re-fetched on
refresh while open. Ordering: labels by `NAME` ascending; assignees by login
client-side; milestones open only (`states: [OPEN]` — closed milestones are
rarely assignment targets; a closed milestone already on the issue still
renders from the issue payload and stays checkable in its list).

### D6 — search deferred

The upstream `query:` filter args (§2.3) make type-ahead trivial later; v1
ships the plain first page (50). A search box would drag in focus handling
and debounce — follow-up material, not this doc.

### D7 — one editor at a time, now across five surfaces

The 0019 one-editor rule extends: title editor, labels picker, assignees
picker, milestone picker, and any timeline-row editor are mutually exclusive.
Opening one closes the others; `busy` blocks arming any of them (0019 D7).

### D8 — milestone display cell

When `milestone` parses, the rail meta shows a cell: milestone title +
muted `OPEN`/`CLOSED` tag (id `issue-milestone`). Absent milestone → no cell
(no "No milestone" noise in the display state; the picker carries the empty
state).

### D9 — ids, roles, probe keys (0010 D10)

Interactive elements get ids + roles; probe selector keys ship from day one
(the 0019 §8 lesson: `debug_bounds` sees only selector keys):

`issue-title-edit`, `issue-title-card` (editor card), `issue-labels-edit`,
`issue-labels-card`, `issue-labels-row-{ix}`, `issue-assignees-edit`,
`issue-assignees-card`, `issue-assignees-row-{ix}`, `issue-milestones-edit`,
`issue-milestones-card`, `issue-milestones-row-{ix}`, `issue-milestone`
(display cell). Save/Cancel ride the shared `inline_edit_card` ids.

## 5. Query and payload shapes

```
# ISSUE_DETAIL_QUERY additions
labels(first: 20) { totalCount nodes { id name color } }
assignees(first: 20) { totalCount nodes { id login avatarUrl } }
milestone { id title state }
viewerCanLabel viewerCanAssign viewerCanSetMilestone
```

Picker queries (new consts, fetched lazily):

```
repository.labels(first: 50, orderBy: {field: NAME, direction: ASC})
repository.assignableUsers(first: 50)
repository.milestones(first: 50, states: [OPEN])
```

Mutation inputs (all `updateIssue`): `{ id, title }` / `{ id, labelIds }` /
`{ id, assigneeIds }` / `{ id, milestoneId }` — exactly one field-group per
post (the op identity in D1), never a combined write.

## 6. UI layout (no change to the split)

The rail's labels/assignees panels keep their look; each header row gains the
Edit affordance at the trailing edge (where the 0019 rows put theirs). The
title Edit sits beside the state badge in the header. Editors swap in place —
no dialogs, no new windows, consistent with 0019/0020.

## 7. Testing

- **gql parse tests**: milestone present/absent; label/assignee `id` parsing;
  new viewer flags; degradation (missing id → flagged on the parsed struct).
- **payload tests**: each op posts exactly its one field-group (`title` only;
  `labelIds` only; `assigneeIds` only; `milestoneId` set vs `null` clear);
  the computed label list equals the full target set (add AND remove cases).
- **gating tables**: `viewerCanUpdate/Label/Assign/SetMilestone` false →
  affordance hidden; D3 guard (missing id, truncated connection) → Edit
  hidden.
- **probe tests**: header edit swap; panel → checklist swap and back;
  one-editor rule across title/pickers/timeline rows (D7); busy blocking;
  refetch re-renders the rail with the new set; cap/regression probes reuse
  the selector keys above.
- existing 0020/0021 suites must stay green — no key renames.

## 8. Explicit non-goals

- Label/assignee creation (new upstream entities) — pickers select existing
  ones only.
- Type-ahead search in pickers (D6).
- Locking + reactions (ride #121); data-colored chips (#119).
- Bulk editing across issues; projectV2 fields (`projectIds`,
  `issueFieldUpdates` — verified present, deliberately untouched).

## 9. Scope valve

If the milestone picker's lazy-fetch machinery grows beyond a copy of the
labels pattern, **split milestone-setting into its own follow-up** and ship
D8 (display) with title/labels/assignees. The issue (#120) pre-authorizes
this split; decide at implementation, not by growing this design.

## 10. Acceptance criteria

1. Title edits a scratch issue; empty title is refused; Esc cancels.
2. Labels/assignees add and remove via the pickers; Save posts once and the
   rail reflects the refetch; an issue with >20 labels hides the label Edit
   (D3) instead of offering a destructive save.
3. Milestone set and cleared; the display cell tracks the refetch.
4. All failure paths surface in the rail and keep their editor open.
5. Workspace gates stay green; new probes cover every new key in D9.
