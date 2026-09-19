# 0024 — GitHub issue creation flow

Part of the GitHub module (docs/0005, 0014, 0020 line). Implements #122 —
the last docs/0020 §11 follow-up that is a *new surface* rather than an
addition to the existing issue page.

## 1. Summary

A "New issue" affordance in the repo view opens a full-content creator
sub-page: title + body (markdown), preview via the 0017 renderer, one
`createIssue` mutation, then the in-app issue page for the new number.
Drafts survive cancel/Esc for the session. Refetch-on-create; no
optimistic writes; probes on every interactive element.

The one non-obvious prerequisite: the mutation takes the repository's
**GraphQL node id**, not the `databaseId` our snapshots already carry —
so the repo snapshot grows two fields (`id`, `hasIssuesEnabled`) and the
creation gate reads them.

## 2. Upstream facts (live introspection 2026-09-07, `gh api graphql`)

- `Mutation.createIssue(input: CreateIssueInput!)` →
  `CreateIssuePayload { issue: Issue, clientMutationId }`.
- `CreateIssueInput`: **`repositoryId: ID!`**, **`title: String!`**,
  `body: String`, then triage fields we do not send —
  `assigneeIds: [ID!]`, `milestoneId: ID`, `labelIds: [ID!]`,
  `projectIds`, `projectV2Ids`, `issueTemplate: String`,
  `issueTypeId: ID`, `parentIssueId: ID`, `issueFields`,
  `clientMutationId: String`.
- `Repository.id: ID!` (GraphQL node id) and
  `Repository.hasIssuesEnabled: Boolean!` both exist. The GraphQL node id
  (`R_kgDO…`-shaped) is **not** the numeric `databaseId` — a snapshot
  carrying only `databaseId` cannot drive the mutation.
- Auth: the OAuth token requests `repo read:org notifications`
  (auth.rs, docs/0011 §3). The `repo` scope already covers issue
  creation; no elevation, no new scope.
- Permissions: any authenticated viewer who can read a repository can
  file issues on it — creation is *not* gated on `viewerPermission`
  (contrast docs/0023 D5, where locking needs triage-and-above).

## 3. Design

- **D1 — the snapshot grows two fields.** Both the global snapshot and
  `REPO_SNAPSHOT_QUERY` (same node shape, doc 0007 D6) add `id` and
  `hasIssuesEnabled` on the repository node. `Repo` grows
  `node_id: Option<String>` and `has_issues_enabled: bool`
  (`#[serde(default = "true")]` — an old cached snapshot that lacks the
  field degrades to *assumed enabled*, and a wrong assumption dies as a
  scoped mutation error, never a silently hidden button; `node_id`
  degrades to `None` = creation unavailable for that repo until the
  next fetch).
- **D2 — entry point: the repo header's actions row.** The row that
  right-aligns the Refresh button gains a ghost **"New issue"** button
  (crumb-adjacent, one obvious place — no per-list duplication).
  *(Amended by docs/0038's freshness work: the textual Refresh button is
  gone — the repo header's freshness cluster icon is the surface's only
  refresh affordance, and the actions row carries just "New issue",
  right-aligned.)*
  Visibility gate, hidden-not-disabled (docs/0005 D-gate discipline):
  connected ∧ repo selected ∧ `node_id.is_some()` ∧
  `has_issues_enabled`. Issues disabled or node id missing → button
  absent; never a dead button.
- **D3 — surface: full-content sub-page (the 0016 D1 pattern).** The
  creator is pushed like the issue/PR detail pages, with full mutual
  exclusion: opening it closes any open issue/PR page, and opening an
  issue/PR page closes it. Crumb suffix **`#new`**, reconciled through
  the 0014 D3 compare-first guard. Esc asks the host to close it (the
  `IssuePageEvent` subscription pattern).
- **D4 — the form.** Title input (required) and body textarea (the
  comment-box `TextareaState` pattern, 0020 D6 chrome). A **Preview**
  toggle swaps the textarea for 0017-parsed blocks of the current body
  (parse on toggle, like the existing `md` caches — no live reparse per
  keystroke). Create is disabled until the title is non-empty — that is
  input validity, not a permission gate, so disabled-with-reason is
  correct here. Failure renders a scoped danger error on the form
  (0018 failure-copy pattern); the draft is untouched.
- **D5 — the mutation sends the minimum.**
  `service::Mutation::CreateIssue { repo_key, node_id, title, body }`
  → `createIssue(repositoryId, title, body)` only. Labels/assignees/
  milestone at creation are a GitHub-web *non*-feature (they are set
  after creation, which our 0022 pickers already do better in-app);
  `issueTemplate`/`issueTypeId` likewise deferred.
- **D6 — success: in-app, by number.** The payload's
  `issue { number url }` synthesize the minimal snapshot row
  (`Issue { key, number, title, … }` — the detail page re-fetches its
  own truth; the row is initial paint only, exactly the row-click
  path). The creator closes, the issue page opens for the new number,
  and the repo view refetches so the new OPEN issue appears in the
  list (refetch-on-create — the house D6, same one-refetch path as
  every other mutation; no optimistic insertion).
- **D7 — draft retention: session-scoped, per open.** Once opened, the
  creator *entity stays alive* — Cancel/Esc only hide it (visibility
  flag + crumb reconcile), so reopening restores title and body.
  The draft is dropped on successful creation (D6) and on repo switch
  (the draft belongs to the repo it was opened for). No disk
  persistence — the cache holds snapshots, not drafts; losing a draft
  to an app restart is acceptable for v1 and filed as a follow-up if
  it bites.

  > **Amended by docs/0045 (#199).** Drafts now persist to
  > `<data>/modules/github/drafts/` (the data root — the "cache holds
  > snapshots" rationale stands; only this sentence's conclusion is
  > reversed), restoring on page open and clearing on submit. Repo
  > switch now *parks* the draft per repo instead of destroying it.
- **D8 — probes (docs/0010 D10).** Every interactive element gets
  `.id(...)` + role + `.debug_selector(...)`: `new-issue-open`,
  `new-issue-title`, `new-issue-body`, `new-issue-preview`,
  `new-issue-cancel`, `new-issue-create`, and the error slot
  `new-issue-error`. The probe harness (`status::init` + `ProbeHttp`)
  drives open → type → create → busy/error, per the 0023 §10 pattern.

## 4. Implementation plan (one PR, after this doc merges)

1. gql.rs: extend both repo queries (`id`, `hasIssuesEnabled`); add
   `CREATE_ISSUE_MUTATION`; `Repo` fields + parse (cache-version bump
   per doc 0007 if the snapshot shape is versioned).
2. service.rs: `Mutation::CreateIssue` arm → `run_mutation`.
3. page.rs: gate fn `create_allowed(&Repo, connected)`; header button;
   creator push/close/mutual-exclusion/crumb plumbing.
4. New `new_issue_page.rs` (or an `issue_page.rs` module-local section
   if it stays small): the creator sub-page.
5. Tests + docs/0020 §11 pointer flip.

## 5. Testing

- Unit: `create_allowed` gate table (connected × repo × node_id ×
  has_issues_enabled); payload JSON const test (exact input keys, no
  triage fields); minimal-row synthesis from a payload.
- Probe test: open creator, type into title/body via simulated input,
  Create → `busy == Some(CreateIssue)`; with `ProbeHttp` the mutation
  fails → the `new-issue-error` slot carries the message, the draft
  survives, busy clears.
- Draft test: cancel → reopen → text intact; successful create (error
  path asserts the inverse) → dropped; repo switch → dropped.

## 6. Debug-bridge verification (docs/0010)

`dashboard-debug tree` shows the button only on a gated-in repo; `click
new-issue-open` opens the creator; `type` into title/body; `click
new-issue-create` posts; `logs -f` shows `issue-mutation create` lines
and the refetch. No OS-level input injection, per the bridge rules.

## 7. Acceptance criteria

- [ ] New issue button appears only on repos where creation is possible
      (issues enabled, node id known, connected), hidden otherwise.
- [ ] Creator opens as a full-content sub-page with crumb `#new`; Esc
      and Cancel return to the list with the draft intact.
- [ ] Preview toggle renders the 0017 blocks of the current body.
- [ ] Create with empty title is disabled; with a title it posts, shows
      busy, and on success lands on the in-app issue page for the new
      number.
- [ ] The repo list refetches — the new issue is in "Open Issues".
- [ ] A failed create renders its error on the form and keeps the
      draft.
- [ ] Zero clippy warnings; token discipline holds (the creator uses
      theme tokens only — no data colors here).

## 8. Human checks (owner)

- [ ] Create an issue on a real repo; verify it on github.com (title,
      body, markdown rendering).
- [ ] Draft survives cancel/reopen and Esc; dies on repo switch and on
      successful create.
- [ ] Button hidden on a repo with issues disabled.
- [ ] Preview toggle feels right (no flicker/reparse cost).
- [ ] Emoji/UTF-8 in title and body round-trip.

## 9. Follow-ups (GitHub issues, not this PR)

- Draft persistence across restarts (only if it bites).
- Issue-template awareness (`issueTemplate`) — read the repo's
  templates and offer a picker.
- Labels/assignees/milestone at creation time (only if GitHub-web
  parity ever demands it; 0022 pickers cover post-create).

## 10. Shipped (2026-09-07, issue #122)

Implemented as designed in one PR. The introspected facts (§2) held at
implementation; nothing diverged from the design. Notes beyond the text:

- **Snapshot extension is tolerant on both sides** (D1): the parse test
  covers present fields, missing fields (old cache → `node_id: None`,
  assumed-enabled), and an explicit `hasIssuesEnabled: false`. The two
  test-fixture `Repo` constructors (page.rs, service.rs) now carry
  defaults so the fixtures match the wire reality.
- **The creator owns its write, the host owns the aftermath** (D6): the
  page parses the payload via `gql::created_issue_from_body` (GraphQL
  errors and malformed bodies both degrade to a visible form error) and
  emits `Created(row)`; the host drops the creator, fires
  `repo_open_specs` (the house one-refetch path), and opens the issue
  page for the synthesized minimal row.
- **`title` is `pub(crate)`** so the host-level retention test can type
  into it — ownership never leaves the creator; the host only reaches
  through it in tests.
- **`status::init` in the probe harness** (0023 §10 lesson applied
  eagerly): Create really spawns through `run_mutation`, so the creator's
  probe harness installs the in-flight tracker global.
- **Preview bodies are `textarea`-free** — the toggle swaps the textarea
  for `markdown::render_blocks`, same parse-once cache rule as the
  0017 bodies (no reparse per keystroke).

Workspace: 265 tests green (was 257); clippy/fmt clean. Human checks
(§8) ride the PR body.
