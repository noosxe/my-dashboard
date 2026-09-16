# 0030 — GitHub module: PR review threads (inline code review on the Files tab)

| | |
| --- | --- |
| Issue | #187 (docs/0029 §3 "the biggest gap") |
| Date | 2026-09-15 |
| Status | design — pending owner review |
| Depends on | docs/0016 (PR detail page), docs/0018 (Files tab sidebar + diff pane), docs/0017 (markdown bodies), docs/0019 (row-affordance conventions), docs/0010 (debug bridge) |

## 1. Context

docs/0029 names inline code review the single biggest feature gap: the app is
"a notification center, not a review tool." Today `reviewThreads` is not
queried anywhere; the Files tab (0018) renders each diff as inert text rows —
no line numbers, no anchors, nowhere to hang a threaded discussion.

This doc designs the read side (threads anchored to diff lines) and the write
side (line comments, replies, draft reviews, resolve) for the Files tab, on
the page's existing fetch/mutation rails.

## 2. Goals and non-goals

Goals:

- **Read** `reviewThreads` (with their comments) for the open PR and render
  them anchored to the correct diff lines in the Files tab diff pane.
- **Comment on a specific line** of a diff — including deleted lines
  (LEFT side) — and **reply** inside an existing thread.
- **"Start a review"**: batch several line comments locally, submit in one
  request as Comment / Approve / Request changes.
- **Resolve / unresolve** threads (schema-gated buttons).
- **Suggested changes** (```` ```suggestion ```` blocks) render as a distinct
  card inside thread comments.
- Diff rows gain a proper **line-number gutter** (both sides) — the anchor
  surface reviews need.
- Bridge-visible ids and roles on every new interactive element (0010 D10).

Non-goals (file as follow-ups if wanted):

- **Authoring multi-line comments** (`startLine`): multi-line *threads* still
  render (anchored at their end line), but v1 composes single-line only.
- **Applying suggested changes**: verified — there is **no public API** to
  apply a suggestion (the schema's `*Suggestion*` types are issue/assignee
  suggestions, unrelated; the web UI applies them via an internal endpoint).
  v1 renders + copies + offers the permalink out.
- **Editing/deleting own review comments** (`updatePullRequestReviewComment`
  / `deletePullRequestReviewComment`) — mechanical once threads exist; a
  0019-style follow-up.
- **Conversation-tab rendering of threads** — the conversation keeps review
  summary rows only; threads live in the Files tab (D1).
- **Server-side PENDING review lifecycle** — replaced by a local draft batch
  (D4).
- Optimistic writes — the page stays request → refetch → repaint (0019 D6).
- Check-run logs, syntax highlighting, split diff (0029 Tier-3 rows).

## 3. Upstream facts (verified 2026-09-15)

Schema source: the official SDL at
`https://docs.github.com/public/fpt/schema.docs.graphql` (fetched and parsed
2026-09-15; the docs.github.com reference pages are JS-rendered and useless
to a plain fetch — the SDL is the pinned static source, field-by-field).

Reads:

- `PullRequest.reviewThreads(after: String, before: String, first: Int,
  last: Int): PullRequestReviewThreadConnection!` — `totalCount` on the
  connection; **no filter args** (outdated/resolved filtering is ours).
- `PullRequestReviewThread`: `id: ID!`, `path: String!`, `line: Int` (the
  end line of the range), `startLine: Int` (multi-line only),
  `originalLine` / `originalStartLine`, `diffSide: DiffSide!` (LEFT | RIGHT),
  `startDiffSide: DiffSide`, `isCollapsed: Boolean!` (legacy name for
  resolved), `isResolved: Boolean!`, `isOutdated: Boolean!`,
  `resolvedBy: User`, `subjectType: PullRequestReviewThreadSubjectType!`
  (LINE | FILE), `viewerCanReply` / `viewerCanResolve` / `viewerCanUnresolve`,
  `comments(first: Int): PullRequestReviewCommentConnection!`.
- `PullRequestReviewComment`: `id`, `author { login avatarUrl }`, `body!`,
  `createdAt`, `updatedAt`, `lastEditedAt`, `replyTo { id }`,
  `pullRequestReview { id }`, `diffHunk: String!`, `path!`, `line`,
  `startLine`, `outdated: Boolean!`, `state: PENDING | SUBMITTED`,
  `subjectType`, `reactionGroups`, `url: URI!` (permalink),
  `viewerCanUpdate` / `viewerCanDelete` / `viewerCanReact`,
  `viewerDidAuthor`. (`databaseId` is deprecated → `fullDatabaseId`; we
  select `url` and skip database ids entirely.)
- **Naming asymmetry to encode once**: read side says `diffSide` /
  `startDiffSide`; the write inputs say `side` / `startSide`.

Writes (all payloads return the mutated node):

- `addPullRequestReviewThread(input)`: `body: String!`, `path`,
  `line: Int` ("required for line-level threads"), `startLine`,
  `side: DiffSide = RIGHT`, `startSide: DiffSide = RIGHT`,
  `subjectType = LINE`, and **exactly one of** `pullRequestId` (publishes
  immediately as its own `COMMENT` review) or `pullRequestReviewId` (attaches
  to that review — the PENDING flow we deliberately avoid, D4).
- `addPullRequestReviewComment(input)`: `body`, `inReplyTo: ID` (the comment
  node id replied to), plus legacy placement fields we do not use. The reply
  path: `{ body, inReplyTo }` — **replies publish immediately** and cannot
  ride a draft batch (no `replyTo` on draft threads), matching GitHub web.
- `addPullRequestReview(input)`: `pullRequestId: ID!`, `body`, `event`
  (omitted → review stays **PENDING**), `commitOID`,
  `threads: [DraftPullRequestReviewThread { body!, path, line, side,
  startLine, startSide }]`, `comments: [DraftPullRequestReviewComment
  { body!, path!, position! }]`. Passing `event` submits in the same request
  — a whole draft review is **one batched mutation**.
- `submitPullRequestReview(input)`: `event: PullRequestReviewEvent!`
  (APPROVE | COMMENT | REQUEST_CHANGES | DISMISS), `body`,
  `pullRequestReviewId` — only needed when a server-PENDING review exists,
  which D4 avoids.
- `resolveReviewThread(input: { threadId: ID!, resolutionReason })` /
  `unresolveReviewThread(input: { threadId: ID! })` — `resolutionReason`
  (ADDRESSED | INVALID | WONT_FIX) is optional; v1 omits it.
- `updatePullRequestReviewComment` / `deletePullRequestReviewComment` exist
  (follow-up scope, §2).
- `PullRequest.pendingReview` **does not exist** in the current schema (it
  was removed); draft detection would have to go through
  `reviews(states: [PENDING])`. D4 sidesteps the whole question.

Suggested changes are a **body convention**, not schema: a
```` ```suggestion ```` fenced code block whose content replaces the
commented lines. Rendering needs the fence's info string — which
`markdown.rs` currently drops (`Block::Code(String)`, 0017 D2).

## 4. Design decisions

### D1 — Threads are a lazy Files-tab fetch, not part of the detail query

New one-shot GraphQL `PR_THREADS_QUERY` (`repository(owner, name) {
pullRequest(number) { id reviewThreads(first: 100) { totalCount nodes {
…thread fields… comments(first: 100) { nodes { …comment fields… } } } } } }`),
fetched when the Files tab first opens and re-fetched after thread
mutations — the same page-owned convention as the detail query (0016 D2:
200 bodies parsed page-side; only non-200 results reach `GithubState::apply`
for the shared 401/rate-limit handling). New `Kind::PrThreads` on the
service lane, `FilesFetch`-style page state machine (`Threads: Idle /
Fetching / Ready / Failed`).

Rationale — 0016 D8 already established the pattern: the patch payload is
the heavy part and "most visits never open it". Threads anchor to patches,
serve the same audience, and keep the hot page-open query lean. The 100/100
caps match the page's other connection caps (0029 §2 pagination table gains
a row when this ships).

### D2 — Line anchoring: extend the patch walk to compute both sides

`parse_patch` (service.rs) already classifies rows but keeps no line
numbers. New pure fn `anchor_patch(patch) -> Vec<AnchoredLine>` walks the
same rows and seeds counters from each hunk header (`@@ -o,s +o,s @@`):
Context advances both sides, Add advances new, Del advances old. Each row
yields `(old: Option<u32>, new: Option<u32>)`.

The diff pane (0018 D8) gains a fixed two-column **gutter**: old-side number
(muted), new-side number; Hunk rows render the range, not a number. The
hover **+** comment affordance arms per row side: Add/Context →
`side: RIGHT` + new line; Del → `side: LEFT` + old line. Binary /
patch-less files (existing placeholder) host no rows and no gutter.

`PatchLine` and its consumers stay unchanged — the anchored walk is
additive (it can literally consume `parse_patch`'s output shape), so no
existing tone/strip behavior moves.

### D3 — Wire model: one thread struct, per-node degrade

```text
PrThread {
  id, path, line, start_line,
  side: DiffSide,                    // LEFT | RIGHT
  subject: Subject,                  // Line | File
  is_outdated, is_resolved: bool,
  resolved_by: Option<String>,
  viewer_can_reply, viewer_can_resolve, viewer_can_unresolve: bool,
  comments: Vec<PrThreadComment>,    // authored order preserved
}
PrThreadComment {
  id, author: Option<Author>, body: String,
  created_at, updated_at, last_edited_at: Option<String>,
  url: String,                       // permalink (D6)
  viewer_can_update, viewer_can_delete, viewer_can_react: bool,  // follow-up
  viewer_did_author: bool,
}
```

Everything `#[serde(default)]`-equivalent (hand-parsed like the rest of
gql.rs): a malformed thread node degrades alone — dropped threads never
fail the query (0007 D3). One editor/busy-op state rides node ids, not
indices (0019 D4 lesson).

**Placement rules** (pure fn, table-tested):

- LINE threads with `is_outdated = false` anchor at `(path, side, line)` —
  the thread card renders directly under its diff row.
- LINE threads that are outdated **or** whose line does not exist in the
  computed anchor map (patch truncated, force-push mid-session) render in a
  collapsed **"Outdated / not in current diff"** bucket at the file's end —
  expandable, never silently dropped.
- FILE-subject threads render under the file header row.

### D4 — Draft reviews are local and batched; no server-PENDING review

The per-line composer offers two actions:

- **Add single comment** → `addPullRequestReviewThread { pullRequestId,
  path, line, side, body }` — publishes immediately, refetch follows.
- **Start a review** → the comment queues into a page-local **draft list**
  (rendered inline under the anchored row with a *Pending* chip, like
  GitHub's). A sticky **Finish review** bar (visible while drafts exist)
  holds: summary Textarea (optional), event choice — Comment (default) /
  Approve / Request changes — and **Submit** running **one**
  `addPullRequestReview { pullRequestId, threads: […drafts…], event, body }`.

Rationale: server-PENDING reviews are the notorious stuck-review lifecycle
(web UI's own failure mode), the schema has removed the easy detection
field, drafts can't carry replies anyway, and one batch request beats N
round-trips. Cost: the draft list is session-only page state (0016 D2) —
closing the page discards it. That matches the composer-draft behavior
everywhere else in the app today (0029 files issue-template/draft
persistence as gaps, not this doc).

Replies to *existing* threads always post immediately via
`addPullRequestReviewComment { body, inReplyTo }` (schema: replies cannot
be batched) — same as GitHub, where replies during a draft review publish
at once. `viewerCanReply` gates the reply box.

### D5 — Resolve / unresolve on the thread header

Thread header row: `path:line` chip (or "File-level"), resolved ✓ chip with
`resolvedBy` login when resolved, and — schema-gated —
`viewerCanResolve` → Resolve, `viewerCanUnresolve` → Unresolve buttons
running `resolveReviewThread` / `unresolveReviewThread` → threads refetch.
`resolutionReason` stays unset (input is optional; the picker is a
follow-up if the owner wants it). One busy-op invariant and node-id-keyed
state as everywhere else (0019 D7).

### D6 — Suggested changes: render + copy + permalink, never apply

- `markdown.rs`: `Block::Code` becomes `Code { lang: Option<String>,
  code: String }` — the fence info string survives the parse (0017 D2
  amend, mechanical, table-tested; every non-suggestion fence renders
  exactly as before).
- Inside **thread comment bodies** only, a `suggestion`-tagged fence renders
  as a distinct "Suggested change" card: label, the replacement lines in a
  tinted mono block, and a **Copy** button (writes the raw block content to
  the clipboard).
- The thread header gains an **Open on GitHub** ghost button using the
  first comment's `url` — that is where *Apply suggestion* lives, since no
  API exists (§3). Opening external links is the established
  `shell.open` path (0017 links).

### D7 — Mutations: four new documents, transport unchanged

`service::Mutation` gains `ReviewThread`, `ReviewThreadReply`,
`ReviewBatch`, `ThreadResolve`, `ThreadUnresolve` (five variants, four
*new* documents — resolve/unresolve share a shape with different
operation names), mapping 1:1 in `run_pr_mutation`'s document match. The
page builds inputs verbatim per §3; `BusyOp` gains the variants; a new
`ErrorScope::Files` lands failures in the diff pane (inline, never modal),
mirroring the timeline banner pattern. Post-mutation: the threads refetch
(D1) re-renders; patches are not refetched (only merge/close change them,
which end the page's useful life anyway).

### D8 — Bridge/a11y contract

Every interactive element gets id + role (0010 D10), index-stable within a
run: `pr-line-{side}-{line}-comment` (hover + buttons),
`pr-thread-{n}` cards with `-reply`, `-resolve`, `-unresolve`,
`-open` buttons, `pr-thread-{n}-reply-box` + `-reply-send`,
`pr-draft-{n}` pending rows, `pr-finish-review` bar with
`-summary`, `-event-comment/-approve/-request-changes`, `-submit`,
`-discard`. Thread card text renders as real text nodes so `tree` reads
thread content.

## 5. Testing

- **Anchor walk (pure fn)**: table tests — hunk-header seeding, Context/Add/
  Del counter advances, LEFT-anchored deletions, multi-hunk files, missing
  `@@` tolerance (parse_patch's tolerant precedent).
- **Placement (pure fn)**: anchored / outdated / file-subject / unanchorable
  buckets, including the force-push case (line not in map).
- **Parse/degrade (gql tests)**: a threads payload with and without optional
  fields; a malformed thread node drops alone; the `diffSide` ↔ enum mapping
  incl. unknown values.
- **Markdown (0017 suite)**: `Code { lang }` carry-through; suggestion
  extraction (first fence, nested fences don't exist in GFM — plain scan of
  block list), non-suggestion fences untouched.
- **Probe (`#[gpui::test]`, the 0018 §8 pattern)**: fabricated files +
  threads render the real Files tab; `debug_bounds` asserts gutter numbers,
  per-row + buttons only on armable rows, thread cards under the right
  rows, the outdated bucket collapsed, the finish bar only while drafts
  exist. Scripted-transport round-trips assert each mutation POST carries
  its document and the page-built variables verbatim (the 0019 shipped
  pattern), including the batch `threads` array.
- **Gates**: fmt / clippy (0 warnings) / workspace tests as always.

## 6. Implementation plan

Two stages, one per implementation issue (split of #187):

1. **Stage 1 — read side**: `gql.rs` (`PR_THREADS_QUERY`, wire structs,
   parse + degrade tests), `service.rs` (`anchor_patch`, `Kind::PrThreads`),
   `markdown.rs` (`Code { lang }`), `files.rs` (gutter, thread cards,
   outdated bucket, suggestion cards), `pr_page.rs` (threads state machine,
   Files-tab wiring), `docs/README.md` row.
2. **Stage 2 — write side**: `service.rs` (`Mutation` variants + documents),
   `files.rs` (hover + composer, thread reply box, draft list + finish bar,
   resolve buttons), `pr_page.rs` (busy ops, `ErrorScope::Files`, handlers),
   bridge ids + probe tests.

Stage 1 alone already turns the Files tab into a review *reader*; stage 2
makes it a review *surface*. Both keep the gates and the bridge contract.

## 7. Open questions (owner)

- **Q1 — local-draft reviews (D4)**: draft list lives in page state and dies
  with the page; server-PENDING is never used. Acceptable? (Lean: yes —
  simplest correct thing; host-held retention like new_issue_page D7 is a
  small follow-up if wanted.)
- **Q2 — threads only in the Files tab (D1)**: the conversation tab keeps
  review summaries only. GitHub also nests threads under their review in
  the conversation — wanted eventually? (Lean: follow-up, not v1.)
- **Q3 — two-stage split (§6)**: read side first, write side second — or
  one combined implementation PR? (Lean: two issues; each stage is a
  reviewable, testable unit.)
- **Q4 — resolve without a reason picker (D5)**: omit `resolutionReason` in
  v1? (Lean: yes; server records its default.)

## 8. Shipped — stage 1, read side (#207)

Implemented as designed in one stage. Implementation notes beyond the
design text:

- **D2 (parse_patch removed)**: `anchor_patch` fully superseded
  `parse_patch` — same tolerant classification, plus both-side line
  numbers — so `parse_patch` and its table were deleted rather than kept
  as dead code. The old classification table re-runs through the anchored
  walk (`anchor_walk_preserves_parse_patch_classification`). The walk also
  handles single-count hunk headers (`@@ -1 +1 @@`, common on one-line
  diffs) and `\ No newline` markers (no anchor, no advance).
- **Placement (D3)**: the pure core is `place_threads(threads, path,
  rows)`; `thread_placement(page, file)` is the render-side wrapper.
  Placement ignores zero/absent line numbers (file-subject and unanchorable
  LINE threads never match a diff row by accident).
- **Probe mechanics (adds to §5)**: `debug_bounds` sees only
  `.debug_selector()` keys, so the test anchors are
  `pr-thread-{n}-card`, `pr-suggestion-{n}-{m}-{b}-card`,
  `pr-outdated-toggle-key` (a wrapper div around the toggle Button), and
  `pr-diff-scroll-key` on the scroll container; accesskit ids (D8) are
  unchanged. The bucket-expand probe drives the same page state the
  toggle's handler writes (`outdated_open`).
- **Suggestion cards (D6)**: the Copy button writes the raw fence content
  to the clipboard; suggestion code renders one non-wrapping mono row per
  line (the diff-pane convention; gpui has no pre-wrap mode).

### Stage 2 — write side (#208)

- **D7 (variant count)**: five `Mutation` variants, four new documents —
  `ReviewBatch` reuses the existing `addPullRequestReview` document (the
  batch is the same input plus its `threads` array), so `PR_ADD_REVIEW_MUTATION`
  gained a second caller.
- **D4 (reply target)**: a reply posts against the thread's **latest**
  comment (the GitHub-web convention); the API threads it into the whole
  thread either way. A thread vanishing under an open reply box (refetch
  race) closes the box instead of firing.
- **D4 (composer invariants)**: one row composer and one reply box at a
  time; opening another row discards the previous unsubmitted text — drafts
  are explicit via **Start a review**. On failure every composer stays open
  with its text (the 0019 pattern); success closes the surface that fired.
- **Plus affordance**: the per-row comment button is always visible (muted
  ghost icon), not hover-revealed — the 0019 row-actions precedent; hover
  reveal also risks hover-only discovery for bridge/AT users.
- **Busy handling**: while any write is in flight every Files-tab button
  disables; Submit reads **Submitting…**. Post-mutation the threads refetch
  re-renders the pane — the detail query (and patches) are not refetched.

### Follow-up — resolve-reason picker (#213)

- **Live-verified schema (2026-09-16 introspection)**:
  `ResolveReviewThreadInput.resolutionReason: PullRequestReviewThreadResolutionReason`
  (optional enum, `ADDRESSED | WONT_FIX | INVALID`) — §3's record holds.
  The **read** side `PullRequestReviewThread` has **no `resolutionReason`
  field**, so the resolved chip stays generic ("Resolved by {login}") —
  only the input carries the reason.
- **Interaction**: the Resolve button no longer fires immediately; it
  opens an always-visible picker row under the thread card (0019 house
  style): **Resolve (no reason)** — field omitted, today's default —,
  **Addressed**, **Won't fix**, **Invalid**, **Cancel**. One picker at a
  time; it closes on any choice (success or failure — on failure the
  Files-scope error row shows and the picker can be reopened).
- **Bridge ids (D8)**: `pr-thread-{n}-resolve-picker`,
  `-resolve-plain`, `-resolve-addressed`, `-resolve-wont-fix`,
  `-resolve-invalid`, `-resolve-cancel`; probe selector
  `pr-resolve-picker-key`.

### Follow-up — review-comment edit/delete (#212)

- **Live-verified schema (2026-09-16 introspection)**:
  `UpdatePullRequestReviewCommentInput { pullRequestReviewCommentId: ID!,
  body }` — the id field is **`pullRequestReviewCommentId`**, unlike the
  timeline edits' `id` / `pullRequestReviewId`;
  `DeletePullRequestReviewCommentInput { id: ID! }`.
- **The 0019 D4/D5 row pattern applied to thread comments**: Edit/Delete
  ghost buttons on the viewer's own comments (`viewerCanUpdate` /
  `viewerCanDelete`, parsed since stage 1); the comment swaps to the
  shared `inline_edit_card` / `delete_confirm_card` while armed. One
  editor anywhere on the page — thread surfaces yield to the timeline
  editor/description/confirm and vice versa, and clear the resolve
  picker. Save gated on a non-blank body (derived in `inline_edit_card`,
  re-checked in the handler).
- Failures land in `ErrorScope::Files` (inline in the diff pane, D7);
  the surface that fired stays open with its text. Deleting a thread's
  root comment removes the whole thread card after the threads refetch.
- The comment's meta row now shows **(edited)** via the shared
  `row_is_edited` stamp rule (updatedAt strictly newer than createdAt).
- **Bridge ids (D8)**: `pr-thread-{i}-comment-{m}-edit` / `-delete`;
  the shared cards bring `{key}-save`/`{key}-cancel` and
  `{key}-confirm`/`{key}-cancel`; probe keys
  `pr-thread-{i}-comment-{m}-edit-card` and
  `pr-thread-{i}-comment-{m}-delete-confirm-card`.
