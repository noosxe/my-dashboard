# 0044 — GitHub pull request creation flow

Part of the GitHub module (docs/0005, 0014, 0020 line; the 0024 creation
pattern applied to PRs). Implements #191 — a docs/0029 §3 PR write gap.

## 1. Summary

A "New pull request" affordance in the repo view opens a full-content
creator sub-page: base/head branch selects (branch list fetched on
open), title + body (markdown), preview via the 0017 renderer, a draft
toggle, one `createPullRequest` mutation, then the in-app PR page for
the new number. Same-repo PRs only in v1 (forks are a §9 follow-up).
Drafts survive cancel/Esc for the session; refetch-on-create; probes on
every interactive element.

The 0024 prerequisite carries over unchanged: the mutation takes the
repository's **GraphQL node id**, which `Repo.node_id` has carried since
0024 D1 shipped. No snapshot growth, no cache-version bump this time.

## 2. Upstream facts (live introspection 2026-09-18, pinned IDL
`/tmp/gh-schema.graphql`)

- `Mutation.createPullRequest(input: CreatePullRequestInput!)` →
  `CreatePullRequestPayload { pullRequest: PullRequest, clientMutationId }`.
- `CreatePullRequestInput`: **`repositoryId: ID!`**,
  **`baseRefName: String!`**, **`headRefName: String!`**,
  **`title: String!`**, then optional fields — `body: String`,
  `draft: Boolean = false`, `maintainerCanModify: Boolean = true`,
  `headRepositoryId: ID` (the fork story: cross-repo heads are
  namespaced `username:branch` and set `headRepositoryId`),
  `clientMutationId`. Ref names are **short names** (`"main"`, not
  `refs/heads/main`).
- Branch data: `Repository.refs(first:, refPrefix:, orderBy:, query:)`
  → `RefConnection { nodes: [Ref], totalCount }` with
  `Ref.name: String!`. `RefOrder` = `{ field: RefOrderField!,
  direction: OrderDirection! }`; `RefOrderField` = `ALPHABETICAL |
  TAG_COMMIT_DATE` — i.e. **name-alphabetical is the only sane branch
  ordering** (no updated/recency option). `Repository.defaultBranchRef:
  Ref` (nullable) names the natural base.
- Branch counts are unbounded upstream; `RefConnection.totalCount`
  reports the true size so a `first: 100` page can announce truncation.
  A `query:` filter arg exists for a future filter box (§9).
- No `viewerCanCreatePullRequest`-style flag exists on `Repository`, and
  there is no `hasPullRequestsEnabled` analog of 0024's
  `hasIssuesEnabled`. `Repository.isArchived: Boolean!` exists but the
  snapshot does not carry it (0024 precedent: archived repos already
  fail issue creation as a scoped post error; see §9).
- Auth: the OAuth token requests `repo read:org notifications`
  (auth.rs, docs/0011 §3); `repo` covers PR creation. No elevation.
- Permissions: reading the repo suffices to *attempt* creation (push
  rights govern whether you have a head branch at all — the failure
  mode is GitHub's own user-error, surfaced on the form).

## 3. Design

- **D1 — zero snapshot growth.** `Repo.node_id` (0024 D1) is the
  mutation key and ships already. Branches are *not* snapshot data —
  they are big, per-repo, and only needed inside the creator — so they
  load as a stateless lane fetch on creator open (the PickerFetch
  pattern, docs/0022 §5/§11), exactly like labels/users/milestones.
- **D2 — entry point: the repo header's actions row.** The row carries
  "New issue" (0024 D2 as amended); it gains a ghost **"New pull
  request"** button beside it, same right-aligned cluster. Visibility
  gate, hidden-not-disabled (docs/0005 gate discipline):
  connected ∧ repo selected ∧ `node_id.is_some()` ∧ `!is_archived`
  *(the archived term added by #275 — it moved out of the "dies as a
  scoped form error" set and into the gate: the button now hides
  instead of failing at submit)*. No upstream creation flag gates PR
  creation beyond that; a repo where it can't succeed (no differing
  branches) dies as a scoped form error or an empty head select,
  never a silent post.
- **D3 — surface: full-content sub-page (the 0016 D1 pattern).** Pushed
  like the issue creator, with full mutual exclusion: opening it closes
  any open issue/PR page, issue creator, or notification page, and
  vice versa. Crumb suffix **`#new-pr`**, reconciled through the 0014
  D3 compare-first guard. Esc asks the host to close it (the
  `NewIssuePageEvent` subscription pattern, docs/0024 D3).
- **D4 — the form.** Two branch selects (**Base**, **Head**) above the
  0024 D4 title/body chrome: title input (required), body textarea,
  Preview toggle (parse-once 0017 blocks, no per-keystroke reparse), a
  **Draft** checkbox, Cancel/Create.
  - Branch lists arrive from a one-page fetch —
    `refs(first: 100, refPrefix: "refs/heads/", orderBy: {field:
    ALPHABETICAL, direction: ASC})` plus `defaultBranchRef { name }` in
    the same document. Open-immediately draft state (0022 §11): the
    selects render a loading row, then the names; failure renders a
    retryable in-form error (fetch errors are retryable by design —
    never autofail). *(Amended by
    §9: each select's search box also fires a repo-wide `refs(query:)`
    page — see below.)*
  - Defaults once loaded: base = `defaultBranchRef.name` (fallback:
    first branch), head = first branch that differs from base.
  - Create is disabled until the title is non-empty **and** base ≠
    head — input validity, not a permission gate
    (disabled-with-reason is correct; 0024 D4).
  *(Amended by docs/0046 §2: the body textarea opens pre-filled with
  the repo's PR template body — one stateless `RepoPrTemplates` GraphQL
  lane fetch on open, body only, title never templated. A restored
  draft beats the template (0046 D4 rule) and typed content is never
  clobbered by a re-open; the prefill parks nothing — it stays a
  default, not a draft. Lane failure leaves the creator empty; creation
  never blocks.)*
  - Truncation: when `totalCount > 100` the select footer reads "first
    100 of N branches (alphabetical)" — a hint, not a blocker.
    *(Amended by §9: the search box covers the rest of the repo —
    see below.)*
  - Failure of the mutation renders a scoped danger error on the form
    (0018 failure-copy pattern); the draft and branch choices are
    untouched.
- **D5 — the mutation sends the minimum.**
  `service::Mutation::CreatePullRequest { repo_key, node_id, base,
  head, title, body, draft }` →
  `createPullRequest(repositoryId, baseRefName, headRefName, title,
  body, draft)` only. `maintainerCanModify` rides the upstream default
  (true); `headRepositoryId`/namespaced heads (forks) deferred to §9.
  Reviewers/labels/assignees/milestone at creation — #191 marks them
  "Optional" — are deferred per the 0024 D5 precedent (GitHub-web
  sets them after creation too); unlike 0024, the in-app post-create
  story already exists: the docs/0043 rail pickers.
- **D6 — success: in-app, by number.** The payload's
  `pullRequest { number title url state isDraft }` synthesizes the
  minimal `gql::PullRequest` row (the detail page re-fetches its own
  truth; the row is initial paint only — the 0024 D6 row-click path).
  The creator closes, the PR page opens for the new number, and the
  repo view refetches so the new PR appears in the list
  (refetch-on-create — the house D6; no optimistic insertion).
- **D7 — draft retention: session-scoped, per open.** The 0024 D7
  mechanism verbatim: the creator entity stays alive once opened
  (visibility flag + crumb reconcile); Cancel/Esc hide it, reopening
  restores title/body/draft-flag. Dropped on successful creation (D6)
  and on repo switch. Unlike 0024, branch lists are *not* retained —
  each open re-fetches (fresh branch state beats a stale list; the
  fetch is one cheap page). No disk persistence.
- **D8 — probes (docs/0010 D10).** Every interactive element gets
  `.id(...)` + role + `.debug_selector(...)`: `new-pr-open`,
  `new-pr-base`, `new-pr-head`, `new-pr-title`, `new-pr-body`,
  `new-pr-preview`, `new-pr-draft`, `new-pr-cancel`, `new-pr-create`,
  error slot `new-pr-error`, branch slot `new-pr-branches` (loading /
  error / truncated footer). The probe harness (`status::init` +
  `ProbeHttp`) drives open → type → create → busy/error per the 0023
  §10 pattern.

## 4. Implementation plan (one PR, after this doc merges)

1. gql.rs: `REPO_BRANCHES_QUERY` (refs page + `defaultBranchRef`);
   `CREATE_PR_MUTATION`; `branches_from_value` parse (+ totalCount);
   `created_pr_from_body` payload parse; both new documents registered
   in the hash-comments test.
2. service.rs: `Kind::RepoBranches` (stateless GraphQL lane, mirror of
   `Kind::RepoLabels`) and `Mutation::CreatePullRequest` arm.
3. page.rs: gate fn `create_pr_allowed(&Repo, connected)`; header
   button beside "New issue"; creator plumbing — takeover-sibling
   field, `open_creator_pr`/`close_creator_pr`, mutual exclusion with
   the issue creator and detail pages, crumb `#new-pr`, `Created`
   handling (drop creator, `repo_open_specs` refetch, `open_pr`).
4. New `new_pr_page.rs`: the creator sub-page (0024 chrome + branch
   selects + draft toggle), `NewPrPageEvent::{Created(gql::PullRequest),
   Dismissed}`.
5. Tests + docs/0029 §3 row annotation ("PR metadata editing" row
   already points here for writes; the creation gap row flips).

## 5. Testing

- Unit: `create_pr_allowed` gate table (connected × repo × node_id);
  branches parse (names, totalCount, missing `defaultBranchRef`
  degrades to first-branch base, empty branch list); default
  base/head choice table (default branch present/absent, no differing
  head); payload JSON const (exact input keys — no
  `headRepositoryId`, no `maintainerCanModify`, draft present);
  minimal-row synthesis from a payload; GraphQL-error body → form
  error.
- Probe test: open creator, selects show loading then branches
  (`ProbeHttp` serves a fixture body for the read, fails the mutation),
  title empty → Create disabled; base == head → Create disabled; type
  title, pick differing head, Create → `busy == Some(CreatePullRequest)`
  → mutation fails → `new-pr-error` carries the message, draft and
  branch choices survive, busy clears.
- Draft test: cancel → reopen → title/body/draft intact, branches
  re-fetched; successful create (error path asserts the inverse) →
  dropped; repo switch → dropped.
- Mutual exclusion test: creator open → opening an issue/PR page closes
  it (and the reverse), crumb reconciles.

## 6. Debug-bridge verification (docs/0010)

`dashboard-debug tree` shows the button only on a gated-in repo; `click
new-pr-open` opens the creator; the branch selects populate after the
fetch; `type` into title/body; `click new-pr-create` posts; `logs -f`
shows the `repo-branches` fetch line, the `pr-mutation create` line and
the repo refetch. No OS-level input injection, per the bridge rules.

## 7. Acceptance criteria

- [ ] New pull request button appears only on repos where the node id
      is known and the viewer is connected; hidden otherwise.
- [ ] Creator opens as a full-content sub-page with crumb `#new-pr`;
      Esc and Cancel return to the list with the draft intact.
- [ ] Branch selects load on open, default to the repository's default
      base and the first differing head; base == head or empty title
      disables Create.
- [ ] Preview toggle renders the 0017 blocks of the current body.
- [ ] Create posts once, shows busy, and on success lands on the
      in-app PR page for the new number; the repo list refetches and
      the PR is in "Open PRs".
- [ ] A failed create (no commits between branches, archived repo, …)
      renders GitHub's message on the form and keeps the draft.
- [ ] Draft toggle round-trips into the payload (`draft: true`).
- [ ] Zero clippy warnings; token discipline holds (theme tokens only —
      no data colors here).

## 8. Human checks (owner)

- [ ] Create a PR on a real repo (a scratch fork-branch pair); verify
      on github.com (title, body, draft flag, branches).
- [ ] Draft survives cancel/reopen and Esc; dies on repo switch and on
      successful create.
- [ ] Branch select feels right on a repo with many branches
      (alphabetical order is the only upstream option — confirm it
      reads fine).
- [ ] The "first 100 of N branches" hint shows on a >100-branch repo
      (if you have one handy).
- [ ] Emoji/UTF-8 in title and body round-trip.

## 9. Follow-ups (GitHub issues, not this PR)

**Branch search beyond the first 100 (implemented, issue #274).** Each
picker's search box now covers the whole repo: keystrokes (300 ms
debounce, superseded timers dropped by generation) fetch
`refs(first: 100, refPrefix: "refs/heads/", query: $query)` for that
select alone — verified live as a substring filter on the ref name,
with `totalCount` reflecting the query. The filtered page swaps into
that select only; the sibling select keeps the default page. When the
filtered page truncates, the slot shows "first N of M branches
matching \"q\""; an empty search restores the in-memory default page
(no refetch). Failures log-and-swallow — the select keeps the local
filter's view and the default page stays authoritative (never autofail,
D6). Mechanism: the select delegate is a SearchableVec wrapper
(BranchSearchDelegate) whose `perform_search` pings the page; server
substring semantics are a superset of the client's case-insensitive
`contains`, so the two filters never disagree. Cursor pagination was
rejected: a 10k-branch repo would pay 100 sequential round trips
before the picker is usable.

- Fork / cross-repo PRs (`headRepositoryId` + `username:branch`
  heads) — needs a fork-aware head repo picker.
- Branch filter box (the `query:` arg) or richer pagination for
  >100-branch repos; today the list truncates with a hint.
- `isArchived` in the repo snapshot to hide the creation buttons on
  archived repos (both creators — 0024 shares the characteristic).
- Metadata at creation time (labels/reviewers at post time) — only if
  GitHub-web parity ever demands it; the docs/0043 rail pickers cover
  post-create.
---
## 10. Shipped (2026-09-18, issue #191)

Implemented as designed, in one PR. Nothing diverged from the design
text; the §9 valves all stayed shut. Notes beyond it:

- **Selections are read, not mirrored** (D4/D5): the creator never
  keeps duplicate `base`/`head` strings — the submit path and the
  render-time Create gate both read `selected_value()` straight off the
  `SelectState` widgets, so there is no mirror to desynchronize. The
  defaults land via `set_items` + `set_selected_value` when the branch
  page lands.
- **Branch fetches re-arm per open through the host** (D7): the host's
  `open_creator_pr` calls `refetch_branches` on a reused entity; a
  fresh entity fetches in its constructor. The fetch uses `spawn_in`
  and hands the window back into `apply_branches`, because select item
  updates need it (the 0024 submit shape, applied to a read).
- **The event payload is boxed** (D6): `NewPrPageEvent::Created(
  Box<gql::PullRequest>)` — the synthesized row is fat (docs/0042 D4
  shapes) and the event fires exactly once; clippy's
  `large_enum_variant` flagged the unboxed shape.
- **The D5 minimum is probe-asserted, not just const-tested**: the
  probe harness's HTTP fixture records the mutation variables it sees;
  the test asserts the exact input key list (`repositoryId`,
  `baseRefName`, `headRefName`, `title`, `body`, `draft`) — no
  `headRepositoryId`, no `maintainerCanModify` — plus the posted
  values, including the draft flag flipped through the row toggle.
- **Auth-connected probe state**: the creator's fetch gates on
  `auth.connected()`, so the probe harness force-connects the state
  entity (`AuthStatus::Connected`, the docs/0026 §4.3 pattern) before
  constructing the page.

Workspace: 428 tests green (was 419); clippy/fmt clean. Bridge smoke:
launch + snapshot clean; the actions row sits behind a repo selection,
where the known deep-navigation a11y gap (#97) starts — interactive
verification rode the `debug_bounds` probe tests instead, per the PR
body's disclosure.
