# GitHub Module: Repo-View List Filters & Sort

- **Issue:** #190 (gap audit: docs/0029 §3 "Navigation & discovery")
- **Status:** design — docs-only PR; implementation follows after merge + go-ahead
- **Depends on:** nothing; orthogonal to 0035 (freshness) and 0038 (realtime)

## 1. Context & problem

The repo view's right column renders three flat GroupBoxes — Open PRs,
Open Issues, Notifications (`page.rs:1713–1769`) — straight from the
snapshot node lists, in the order GitHub returned them (updated-desc over
the wire). Above ~20 rows users cannot find anything: there is no way to
separate their own PRs from others', no review-request view, no label
focus, no way to pull `review_requested` notifications out of the pile,
and no text search. Issue #190 asks for filter controls, author
mine-vs-others, updated-date sort, and (for notifications) reason and
read/unread filters.

## 2. Scope

- In: the three repo-view GroupBoxes (`prs-box`, `issues-box`,
  `notifs-box`) — per-box filter/sort toolbars, one shared quick-filter
  input, count titles, empty states; a pure filter/sort engine module;
  snapshot-query amendments for the two missing data dimensions
  (labels, review requests); passive label dots on rows.
- Out: the left repo list (already searchable via `RepoListDelegate`),
  detail pages, cross-repo global search (separate docs/0029 gap),
  notification **read/unread** browsing (needs an `all=true` fetch and
  changes inbox/home-card semantics — follow-up, §5), persistence across
  app restarts, row-level review glyph.

## 3. Verified facts

1. Repo rows come from the in-memory snapshot (docs/0007 D2): each
   `Repo` carries `prs: PrSummary` / `issues: IssueSummary` —
   `total_count` (exact) + up to 100 nodes (`gql.rs:311–357`). The
   queries already order nodes `UPDATED_AT DESC` server-side
   (`gql.rs:44, 60, 101, 117`). PR nodes carry `author { login }`,
   `updatedAt`, `statusCheckRollup { state }`; issue nodes carry
   `author`/`updatedAt`. **No labels, no review requests** anywhere.
2. The viewer login is already held: `state.user: Option<User>` set from
   `data.viewer.login` on every global snapshot apply
   (`service.rs:2008`) — a "mine" filter needs no new fetch.
3. Notifications are REST (`api::Notification`, `api.rs:16–27`): the
   payload carries `reason` ("comment", "review_requested", "mention",
   …) and `unread`. The inbox is **unread-only** by design
   (`GET /notifications` defaults to `all=false`; docs/0026 §2.1), and
   home-card counts (docs/0012) plus the repo box title derive from it —
   so a read/unread filter has nothing to filter on today.
4. GitHub GraphQL public schema IDL
   (`docs.github.com/public/fpt/schema.docs.graphql`, fetched
   2026-09-17): `PullRequest.reviewRequests(first): ReviewRequestConnection`
   (line 45021); `ReviewRequest { asCodeOwner: Boolean!,
   requestedReviewer: RequestedReviewer }` with
   `union RequestedReviewer = Bot | EnterpriseTeam | Mannequin | Team |
   User` (59711–59738, 59192); `PullRequest.reviewDecision`
   (45016); `PullRequest.labels(first): LabelConnection` (44650);
   `Issue.labels(first): LabelConnection` (20370). All plain-read
   fields — no token-scope change.
5. gpui-component `PopupMenu` + `DropdownMenu` and `InputState` are
   established in-tree: metadata pickers (`issue_page.rs:37, 2487, 4265`,
   docs/0022) and the diff file filter / composer editors
   (`pr_page.rs:31, 305`). No new component work.
6. Rows are stateful, testable nodes: PR/issue/notif rows carry
   `.id("pr-{key}")`-style ids (e.g. `page.rs:996`) — presence/absence
   under `debug_bounds` is the deterministic visibility probe
   (docs/0010 D10).
7. The refresh beat (docs/0035) re-lands snapshot rows without touching
   view state; filters/sort are render-time transforms over held nodes,
   so they compose with auto-refresh and the #87 `more_cell` cap hint
   (which stays total-based).

## 4. Design decisions

### D1 — Client-side over held nodes; no new fetch paths

≤100 nodes per box are already in memory (docs/0007 D2). Every control
change is an entity notify + re-render: filter → `nodes.iter().filter()`,
sort → stable sort of the borrowed slice. The snapshot's stored order is
never mutated. Rationale: server-side filtering means a fetch per click,
new spec kinds in the freshness beat, and cache-key questions — for data
that fits in one frame's iteration cost.

### D2 — Session view state on `GithubPage`

One `filters: RepoViewFilters` field: per-box author/label/reviews/
reason/kind/sort choices + the shared search text. Semantics:

- **Repo switch:** the *label* filter resets (names are repo-local);
  author/sort/reason/kind/search survive.
- **App restart:** not persisted (view state, not settings). Moving the
  choices into docs/0025 settings is a cheap follow-up if wanted.

### D3 — Control set: one toolbar row per box

Directly under each GroupBox title, before the row list — compact
ghost buttons opening the docs/0022-style `PopupMenu`:

| Box | Controls |
| --- | --- |
| PRs | Author (All / Mine / Others) · Reviews (All / Requested of me) · Label (All / ⟨names present⟩) · Sort (Updated ↓ *default* / Updated ↑ / Newest # / Oldest #) |
| Issues | Author · Label · Sort (same set) |
| Notifications | Reason (All / ⟨reasons present⟩) · Kind (All / ⟨`subject.type` values present⟩) · Sort (Updated ↓ / ↑) |

- Each button's label shows the active choice; a dot on the button marks
  non-default state so a forgotten filter is visible even collapsed.
- Menus whose option set is only the default are **hidden**, not
  rendered disabled (the docs/0024 D2 idiom).
- Notifications have no author in the REST payload — no author control
  there (fact 3); reason/kind come straight off the held rows.
- A **Clear** button appears in a box's toolbar only while that box has
  a non-default filter or sort.
- Element ids + debug_selectors on every control (`pr-filter-author`,
  …) — bridge-drivable per docs/0010 D10.

### D4 — Wire amendments (the only fetch changes)

Both snapshot queries (`GLOBAL_SNAPSHOT_QUERY`, `REPO_SNAPSHOT_QUERY`)
gain, per node:

- PRs: `labels(first: 8) { totalCount nodes { name color } }` and
  `reviewRequests(first: 20) { totalCount nodes { requestedReviewer {
  __typename … on User { login } } } }`
- Issues: `labels(first: 8) { totalCount nodes { name color } }`

Wire structs (serde, camelCase):

```rust
pub struct LabelSummary { pub total_count: i64, pub nodes: Vec<LabelRef> } // LabelRef { name, color }
pub struct ReviewRequestsSummary { pub total_count: i64, pub nodes: Vec<ReviewRequestRef> }
// ReviewRequestRef.requested_reviewer: Option<RequestedReviewer> enum { User { login }, Other }
```

`PullRequest.review_requests / labels: Option<…>` and
`Issue.labels: Option<…>` are `#[serde(default)]` — snapshots cached by
the old shape keep parsing as `None` (the docs/0031 continuity pattern;
`draft` precedent at `gql.rs:335`). `None` degrades gracefully: label
menu hidden, review filter falls back to All.

- "Requested of me" = any `requestedReviewer` is `User` with
  `login == viewer.login` (fact 4). Teams/mannequins requested do not
  match v1; `asCodeOwner` and `reviewDecision` are fetched-adjacent but
  unused for now (`reviewDecision` is the *viewer's own* review state —
  not the same question).
- Labels truncate at `first: 8` per node; the filter menu lists only
  names occurring among the held rows, so truncation cannot hide a
  filterable name that the rows actually show. Payload delta is
  per-node and bounded by the same first-100 caps; measured at impl and
  recorded in §10.

### D5 — One shared quick-filter input

A single small `InputState` ("Filter rows…") sits in the right column
under the actions row, above the three boxes. Live, case-insensitive
substring match on the row title, plus exact `#N` number match;
`InputEvent::Change` → notify (≤300 rows total — an in-memory scan per
keystroke is noise; no debounce). It applies to all three boxes at once.

*Open for owner review:* per-box inputs instead? One input means one
focus stop and one mental model ("what am I looking for"); per-box is
more precise but triples the chrome and focus order.

### D6 — Counts and empty states

- Title builder (pure fn): filtered → `"Open PRs (5 of 23)"`, unfiltered
  → `"Open PRs (23)"`; notifs likewise (`"Notifications (3 of 11)"`).
- Box fully filtered out: the existing `muted_cell` idiom with
  "No PRs match the current filters" — plus the toolbar's Clear button
  (D3) as the one-click way out.
- #87 `more_cell` unchanged (cap math is total-based, fact 7).

### D7 — Rows surface labels passively

Up to three 6 px colored dots (GitHub's own label colors, contrast-fixed
by the theme's muted treatment) before the author cluster on PR/issue
rows; tooltip lists names, `+n` when `total_count` exceeds what's
shown. Fixed-size dots keep the single-line row geometry unchanged —
no reflow risk, no wrap. Row-level review-request glyph stays out of
scope (the Reviews filter carries the feature in v1).

### D8 — Pure engine module

New `filters.rs` in the module — no gpui types:

```rust
pub(crate) enum AuthorFilter { All, Mine, Others }
pub(crate) enum ReviewsFilter { All, RequestedOfMe }
pub(crate) enum PrSort { UpdatedDesc, UpdatedAsc, NumberDesc, NumberAsc }
pub(crate) struct IssueFilter { /* author, label, sort */ }
pub(crate) struct NotifFilter { /* reason: Option<String>, kind: Option<String>, sort */ }
pub(crate) struct RepoViewFilters { prs: PrFilter, issues: IssueFilter, notifs: NotifFilter, search: String }

fn apply_prs<'a>(f: &PrFilter, nodes: &'a [gql::PullRequest], viewer: Option<&str>, search: &str) -> Vec<&'a gql::PullRequest>
fn apply_issues<'a>(…) -> Vec<&'a gql::Issue>
fn apply_notifs<'a>(…) -> Vec<&'a api::Notification>
fn pr_box_title(total: usize, visible: usize, filtered: bool) -> String
```

Sort ties break by number desc (stable, deterministic). Updated-at
parse failure sorts oldest (matches `relative_age`'s defensive
treatment). The page's render path becomes:
`let prs = apply_prs(&self.filters.prs, &repo.prs.nodes, viewer, &self.filters.search);`
then render as today.

## 5. Alternatives considered

- **Server-side filtering** (GraphQL `labels: ["x"]`, search query per
  filter state) — rejected: a fetch per control change, new spec kinds +
  freshness-beat wiring, cache-key explosion; the held 100 rows make
  client-side free and instant. Only >100-row repos feel the cap, and
  the existing `more_cell` already announces it.
- **Read/unread notifications in v1** — rejected: the inbox is
  unread-only (fact 3); showing read items needs an `all=true`
  secondary fetch and a count-semantics decision for the home cards and
  box titles (docs/0012/0026). Follow-up.
- **Persist filters in settings** — deferred (D2); session state first.
- **Per-author dropdown** (every author in the box) — deferred; the
  issue's asked minimum is mine-vs-others, and the shared search covers
  most precise lookups meanwhile.

## 6. Testing

Deterministic only — no live network, no screenshots:

- **Engine unit tests** (`filters.rs`): author mine/others (viewer
  login incl. `None`), label present/absent/None-nodes, requested-of-me
  (user match ✓, team-only ✗, viewer None ✗), all four sorts + tie
  behavior, search title substring / `#N` / no-match, combined
  filters, `pr_box_title` both modes.
- **Serde continuity** (`gql.rs`): old-shape snapshot JSON (no
  `labels`/`reviewRequests` keys) parses to `None`; new-shape parses
  names/colors/logins. Query-name test (`gql.rs:2419`) extended so both
  snapshot queries carry the new selections.
- **Page tests** (`page.rs`): snapshot fixture with mixed
  authors/labels/review-requests →
  - default render shows all rows (`debug_bounds` on row ids, fact 6);
  - `set` author=Others → my rows gone; Reviews=RequestedOfMe → only
    the requested row survives;
  - shared search input set → cross-box filtering + count titles;
  - label filter set → repo switch to a label-less repo resets it,
    author filter survives;
  - clear button returns to unfiltered (all rows back).
- **Perf:** none — ≤300 rows, single pass per render (D1).

## 7. Acceptance criteria

- PRs: Mine/Others split works on today's payload; "Requested of me"
  matches github.com's review-request state on the same PRs; label
  filter works once labels land in the snapshot.
- Issues: same author/label/sort bar.
- Notifications: reason + kind filters over the unread inbox.
- Shared quick filter narrows all three boxes; titles show `n of m`
  while filtered; Clear restores; no crash on empty nodes.
- Old cached snapshots keep loading (serde defaults) — feature
  degrades to author/sort only until the next snapshot lands.
- Gates green; docs/0029 §3 row annotated "(→ docs/0042)" at impl;
  docs/README index row added with this PR.

## 8. Human checks (owner)

- [ ] Toolbar readability at narrow tile widths (controls wrap or
      truncate gracefully inside each GroupBox)
- [ ] Menu feel matches the docs/0022 pickers; Esc/dismiss behavior
- [ ] Label dot colors read correctly against both bundled themes
- [ ] Filters survive an auto-refresh tick (rows re-land, view stays)
- [ ] Search feels instant on the busiest repo in the account
- [ ] "Requested of me" agrees with github.com on a real requested PR

## 9. Follow-ups (GitHub issues, not this PR)

- Read/unread notifications browsing (`all=true` fetch + count model)
- Row-level "review requested of me" glyph
- Persist filter choices in settings (docs/0025)
- Per-author filter dropdown, per-box search inputs (if D5's shared
  input disappoints)

## 10. Shipped (2026-09-17, #261)

Implemented as designed; the introspected wire facts held. Notes:

- Engine, wire additions, toolbars, shared quick filter, count titles
  and the repo-switch label reset all landed as specified; tests pin
  the engine tables, query shape, old-shape serde continuity and the
  page surfaces via `debug_bounds`.
- PR/issue rows gained `debug_selector`s (docs/0010 D10) — their plain
  `.id()`s were invisible to the dump and to tests.

### 2026-09-17 (owner feedback): D7 dots become named pills

The 6 px dots (D7) read as decoration, not labels. Rows now render up
to three **GitHub-style named chips right of the row title and `#N`** —
the website's treatment — reusing the 0022 Q2 / #119 pill styling
through a shared `page::label_pill` builder (the issue page's rail
delegates to it; one source of truth for the data-color pill).
`total_count` beyond three collapses into a muted `+n` pill whose
tooltip carries the hidden names; a label without a usable color still
names itself through the muted token pill. Chip containers keep the
`{pr,issue}-labels-{key}` probes; the row geometry stays single-line
(the left cluster wraps the title, number and chips).
