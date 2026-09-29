# 0019 — PR page: edit and delete conversation comments

| | |
| --- | --- |
| Issue | #89 (deferred from docs/0016 Q4, 2026-09-03) |
| Date | 2026-09-04 |
| Status | design — pending owner review |
| Depends on | docs/0016 (PR detail page, stages 1–2 shipped), docs/0017 (markdown bodies), docs/0010 (debug bridge) |

## 1. Context

docs/0016 Q4 deferred comment edit/delete out of the first cut of the PR
page — only the description was made editable. The page now renders the
merged timeline (comments + review summary bodies, 0016 D4) with markdown
bodies (0017) and writes through `Mutation` → detail refetch (0016 D4/D5).
This doc adds edit and delete for the two timeline row kinds the page
actually renders.

## 2. Goals and non-goals

Goals:

- **Edit** a conversation comment (IssueComment) or a review summary body
  (PullRequestReview) when the schema says the viewer may — inline editor,
  same feel as description editing (0016 D4).
- **Delete** either, behind an explicit confirmation.
- Bridge-visible ids and roles on every new interactive element (0010 D10).
- `(edited)` marker on rows whose `updatedAt` is newer than their stamp.

Non-goals (unchanged from 0016 D2 / its deferred list):

- Line comments and review threads — the timeline still shows only
  IssueComments and review **summary** bodies.
- Review dismissal (`dismissPullRequestReview`) — that is a maintainer
  action on others' reviews, not part of "edit my own conversation".
- Optimistic UI — writes stay request → refetch → repaint.
- Notifications, drafts, markdown toolbar, description editor changes.

## 3. Upstream facts (verified 2026-09-04 against the public GraphQL
## 3. Upstream facts (verified 2026-09-04)

Schema source: docs.github.com pages are JS-rendered, so the pinned static
mirror at bryanburgers.github.io/docql/github was used; selections
cross-checked with the docs.github.com reference where fetchable.

- `IssueComment` and `PullRequestReview` both expose:
  - `id: ID!` (Node identity — the mutation target),
  - `viewerCanUpdate: Boolean!` ("Check if the current viewer can update
    this object"),
  - `viewerCanDelete: Boolean!` (both implement the `Deletable`
    interface),
  - `createdAt` and `updatedAt` timestamp fields.
- Mutations (input objects verified field-by-field):
  - `updateIssueComment(input: { id: ID!, body: String! })`
  - `deleteIssueComment(input: { id: ID! })`
  - `updatePullRequestReview(input: { pullRequestReviewId: ID!, body:
    String! })` — "Updates the body of a pull request review".
  - `deletePullRequestReview(input: { pullRequestReviewId: ID! })`.
- Our detail query currently selects only `author body createdAt`
  (comments) and `author state body submittedAt` (reviews) — the id, the
  two viewer flags and `updatedAt` must be added to both node sets.
- In-app (pinned gpui-component `0c746df`, current pr_page.rs):
  > **Re-verified at the v0.6.0 pin `94a313a` (2026-09-09, #125):** the
  > rebrand moved the file (`crates/ui/src` → `crates/component/src`)
  > but `input/textarea.rs` is **byte-identical** across the bump — the
  > editor-swap semantics (prefill/focus/`auto_grow`) hold line-for-line.
  > The `(edited)` marker sources our own `updatedAt` field (D2), so it
  > is upstream-independent.
  >
  > **Re-verified at the v0.7.0 pin `0c830f4` (2026-09-29, #363):** 0.7.0
  > touched input/textarea (standard sizing, IME/undo/caret lifecycle
  > fixes) but the editor-swap semantics (prefill/focus/`auto_grow`)
  > hold — the 0019 probes pass on the new pin.
  - Timeline rows render through `body_card(...)`, whose header row has a
    `trailing` slot already carrying the review badge and the description
    card's `Edit` button — the affordance slot exists.
  - The markdown body cache (0017 D6) is rebuilt on every detail apply —
    an edited body repaints with zero cache work.
  - The write path is one `BusyOp` at a time, scoped inline errors
    (`ErrorScope::Timeline` for all conversation ops), post-mutation
    detail refetch (0016 D4/D5) — extended, not replaced.

## 4. Design decisions

### D1 — Scope: the two row kinds we render

IssueComment rows and PullRequestReview summary rows. Nothing else in the
timeline gains affordances. Deleting a review deletes the **whole review**
— including any APPROVED / CHANGES_REQUESTED state it contributes to the
PR — so its confirmation copy says exactly that (D4).

### D2 — Data model and per-node degrade

`gql::PrComment` and `gql::PrReview` gain:

```text
id: String                      // node id; mutation target
viewer_can_update: bool         // gates Edit
viewer_can_delete: bool         // gates Delete
updated_at: Option<String>      // the (edited) marker
```

All `#[serde(default)]`. Empty id or false flags simply hide the
affordances for that row — one malformed or unprivileged node never takes
the page down (0007 D3 per-node degrade, same as today's list rows).

### D3 — Gating is the schema's flags, nothing else

Edit ⇢ `viewerCanUpdate`; Delete ⇢ `viewerCanDelete`. No client-side
authorship heuristic: the server flag already encodes org roles and
permission realities (an org admin may delete a comment they did not
author — the flag is the contract, not `viewerDidAuthor`). Handlers
re-check the same flags as the honest guard, like every other action on
the page.

### D4 — Affordances: header buttons, inline editor, two-step delete

- **Header row** (`body_card`'s `trailing` slot): ghost small `Edit` and
  `Delete` buttons, exactly the `pr-description-edit` recipe. Review rows
  keep their state badge beside the buttons. Ids: `pr-comment-{ix}-edit`
  / `pr-comment-{ix}-delete` / `pr-review-{ix}-edit` / `pr-review-{ix}-delete`.
- **Edit**: the card body swaps to a prefilled Textarea + Save/Cancel —
  a mirror of `description_edit_card`. `auto_grow(4, 30)`: grows with the
  body, scrolls internally past 30 rows. Save is disabled on trimmed-empty
  bodies (`body: String!` rejects empties server-side; same client gate
  as the description editor).
- **One editor at a time**: `timeline_edit: Option<(String, Entity<
  TextareaState>)>` — (node id, editor). Opening a row editor closes the
  description editor, any other row editor, and a pending delete-confirm;
  each direction is symmetric.
- **Keyed by node id, not timeline index.** Indices shift on refetch;
  node ids survive. On detail apply, the editor/confirm is dropped when
  its node is gone (e.g. deleted from elsewhere).
- **Delete confirmation is inline, not a modal**: the card body swaps to
  the question + [Delete] (danger) / [Cancel]. Copy:
  - comment: `Delete this comment?`
  - review: `Delete this review? Its approval or request-changes state
    goes with it.`
  The app has no modal vocabulary; the confirmation stays inside the card
  the button lives in. Ids: `…-confirm` / `…-cancel` (the question text
  renders as a text node so `tree` reads it).
- **`(edited)` marker**: when `updated_at > created_at` (missing stamps
  count as never-edited, matching `timeline_order`'s stamp rule), the
  age cell renders `… (edited)`.

### D5 — Mutations: four new documents, transport unchanged

`service::Mutation` gains `UpdateComment`, `DeleteComment`, `UpdateReview`,
`DeleteReview`, mapping 1:1 to the four documents (`clientMutationId`-only
selections, matching the existing style). Inputs per the verified schema:
`{id, body}` / `{id}` / `{pullRequestReviewId, body}` / `{pullRequestReviewId}`.
`BusyOp` gains the matching four variants — one op in flight at a time is
unchanged — and all four map to `ErrorScope::Timeline`, so failures land
in the existing banner above the comment box.

### D6 — No optimistic writes

Save/Delete → busy → post-mutation detail refetch (the existing
`spawn_mutation` path) → repaint. The markdown cache rebuild makes the
new body appear; deletion drops the row. The page already round-trips
this way for five mutations; a second write-path style would double the
failure matrix for no visible win at timeline scale.

### D7 — Guards

- Save: disabled and handler-guarded on trimmed-empty body; a no-change
  body is allowed (GitHub accepts it; the refetch is the no-op).
- All row actions and the description editor respect the one-busy-op
  invariant (`busy.is_some()` blocks entry, as everywhere on the page).

### D8 — Bridge/a11y contract

Every new interactive element gets an explicit id plus its role
(0010 D10): edit, delete, save, confirm, cancel buttons across comment
and review rows; the confirm question renders as text so the dump shows
what is being confirmed. The index-scheme ids match the existing
`pr-comment-{ix}` card ids, stable within a run.

## 5. Testing

- **Parse/degrade (gql tests)**: a detail payload *with* the new fields
  parses them; a payload *without* them degrades to hidden affordances
  (empty id, false flags) without failing the node.
- **Pure fns**: the edited-marker rule (`updated > created`, missing
  stamps never count as edited) table-tested like `timeline_order`.
- **Probe (`#[gpui::test]`, the 0018 §8 measured pattern)**: fabricated
  detail — one comment and one review flagged viewer-can, one of each
  not — renders the real page; `debug_bounds` on the new ids asserts the
  buttons exist exactly on flagged rows, disappear in edit mode, and the
  confirm swap works. A scripted-transport round-trip asserts each of the
  four mutation POSTs carries the right document and variables
  (mirrors the 0016 stage-2 tests).
- **Gates**: fmt / clippy (0 warnings) / workspace tests as always.

## 6. Implementation plan

Single stage, one PR (code + this doc's shipped notes):

1. `crates/modules/github/src/gql.rs` — struct fields, detail-query
   selection, parse + degrade tests.
2. `crates/modules/github/src/service.rs` — `Mutation` variants +
   documents.
3. `crates/modules/github/src/pr_page.rs` — `BusyOp` variants +
   error scopes; `timeline_edit` / delete-confirm state; handlers
   (start/cancel/save edit, confirm/cancel delete); render (header
   buttons, edit card, confirm card, `(edited)` marker); tests.
4. `docs/README.md` — index row.

## 7. Open questions (owner)

- **Q1 — inline two-step delete confirm** (no modal): OK? The app has no
  modal vocabulary; this keeps the interaction inside the card.
- **Q2 — `(edited)` marker**: keep? One extra query field per node.
- **Q3 — review delete copy**: "Delete this review? Its approval or
  request-changes state goes with it." — OK?
- **Q4 — bodyless reviews**: a plain Approve (empty body) still carries
  `viewerCanUpdate`; we show Edit (schema-gated, saving adds a body and
  keeps the approval). GitHub's web UI hides the affordance there. Lean:
  keep the schema-gated rule (simpler, honest); flag if you want the
  GitHub parity exception.

## 8. Shipped (2026-09-04, issue #89)

Implemented as designed in one stage. The §7 open questions resolved to
the doc's leans (inline confirm, `(edited)` marker kept, review-delete
copy as written, no bodyless-review exception), per the owner's
go-ahead.

Implementation notes beyond the design text:

- **D2 (serde note)**: `PrComment`/`PrReview` are hand-parsed (no serde
  derives), so the degrade is manual — a shared `node_gate` closure reads
  `id` / `viewerCanUpdate` / `viewerCanDelete` / `updatedAt` identically
  for both node kinds, defaulting to empty/false/None.
- **Generalized editor card**: `description_edit_card` became
  `inline_edit_card(id, title, …)`; the Save-empty guard is derived
  inside the card so call sites can't drift from the gating. The
  description card id is now `pr-description` (the edit-mode card swaps
  with the read-only card, never both on screen) — the shipped
  `pr-description-save` / `-cancel` button ids are unchanged.
- **Probe mechanics (adds to §5)**: gpui `debug_bounds` only sees
  `.debug_selector()` keys, not element ids — the actions bars and swap
  cards carry test-only selector keys (`{prefix}-actions`, `{id}-card`,
  `{id}-confirm-card`); the D8 accesskit ids are untouched. Also:
  `pr_detail_from_value` takes the **repository wrapper** node, not the
  PR node — probe fixtures must wrap.
- **Mutation round-trip**: `run_pr_mutation` now has a scripted-transport
  test (`pr_row_mutations_post_their_documents`) asserting each of the
  four POSTs carries its operation name and the page-built variables
  verbatim — this coverage did not exist for the 0016 mutations.
