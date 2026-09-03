# 0012 — GitHub Module: Open Issues (home-card count + per-repo section)

- **Date:** 2026-09-02
- **Depends on:** 0007 (GraphQL global snapshot — issue data already fetched),
  0005 (module contract, card/page surfaces)
- **Closes the design need of:** the issues-UI follow-up recorded in
  0007 §9 ("Issues UI on top of the gathered data — needs its own design note")

## 1. Context & problem

Doc 0007 put open-issue data in every snapshot: both the global query and the
repo-scoped query fetch, per repo, `issues(states: OPEN, first: 100,
orderBy: UPDATED_AT DESC)` with `totalCount` plus node fields (`databaseId`,
`number`, `title`, `author { login }`, `updatedAt`, `url`). The domain types
(`IssueSummary`, `Issue`) and the persisted snapshot carry the data, but no UI
consumes it — every issue field is annotated
`#[allow(dead_code)] // consumed by the issues UI (follow-up)` in `gql.rs`.
0007 explicitly recorded this as groundwork.

Owner request (this design):

1. **Home card** — a third count row next to `Notifications {n}` /
   `Open PRs {m}`: open issues.
2. **Repo view** — an **Open Issues** section between the existing
   **Open PRs** and **Notifications** sections (work items of the repo before
   the notification feed, mirroring the PR surface).

The data path already exists, so this is a pure-UI feature with exactly one
data-path tweak (issue-author avatar, D1). The design effort goes to count
semantics, placement, and row treatment — the places where silent divergence
from the PR surfaces would creep in.

## 2. Scope

In:

- Home card: third connected-state row `Open Issues {n}`.
- Repo view: `Open Issues` GroupBox between `prs-box` and `notifs-box`,
  with issue rows.
- One GraphQL field addition: issue `author { login avatarUrl }` (D1).
- `#[allow(dead_code)]` cleanup for the fields this feature consumes (D5).
- Tests + docs: this doc, README index row; older docs touched by the
  implementation PR only where shipped behavior diverges.

Out (non-goals):

- No issue detail view/page — row click opens github.com in the browser
  (PR parity).
- No new fetches: no cadence change, no issue-scoped API, no pagination
  beyond the existing first-100-nodes-per-repo page.
- No labels / assignees / milestones / comment counts on rows.
- No cross-repo "all issues" list.
- No filtering or sorting UX on the section.

## 3. Verified upstream facts

- `Issue.author` is `Actor` (nullable) in the GitHub GraphQL schema — the same
  type `PullRequest.author` returns; the `Actor` interface carries `login` and
  `avatarUrl` (docs.github.com GraphQL reference, `objects#issue` /
  `interfaces#actor`). The repo's two snapshot queries already fetch exactly
  `author { login avatarUrl }` on the PR side, so the issue-side addition is
  the same subselection on the same interface.
- Everything else — the `issues(states: OPEN, first: 100, orderBy: …)`
  connection shape, `totalCount`, `UPDATED_AT DESC` ordering — is already in
  the production queries per 0007 D2 and is not re-derived here.

## 4. Decisions

### D1 — Data path unchanged; one exception: issue author gains `avatarUrl`

Zero service changes. The snapshot cadence (0007 D6) already refreshes issues
for all repos every 5th tick and re-fetches the selected repo on open; the
persisted snapshot (`snapshot-v2`, 0007 D7) already serializes `issues` —
existing caches render issues immediately, no cache-key bump, no migration.

The one query change: issue nodes fetch `author { login avatarUrl }` in **both**
`GLOBAL_SNAPSHOT_QUERY` and `REPO_SNAPSHOT_QUERY`. Rationale: node fields are
free on rate-limit points (0007 §5); the shared `Author` struct then carries a
real avatar for issues; the row component (D4) reuses the PR row's author
treatment instead of growing an avatar-less special case. Wire/domain shape is
unchanged — `Author.avatar_url` already exists with `#[serde(default)]`.

### D2 — Home card: third row `Open Issues {n}`, summed `total_count`

Connected-state rows become:

```
Notifications {n}
Open PRs {m}        m = Σ r.prs.total_count      (existing, unchanged)
Open Issues {k}     k = Σ r.issues.total_count   (new, same semantics)
```

- Summed over the cached repos via `totalCount` — exact per-repo counts
  (>100 open issues in one repo does not truncate the card number; 0007 D2),
  subject to the same recorded repo-list limitation as Open PRs
  (0005 §6.1).
- `service::GithubState::card_counts()` grows to return the third count;
  a negative `total_count` clamps to 0 exactly like the PR sum (defensive
  parity).
- All non-connected card states are untouched except one copy string:
  "Connect to see notifications and pull requests." →
  "Connect to see notifications, pull requests, and issues."

### D3 — Repo view: `Open Issues` GroupBox between Open PRs and Notifications

Right-column section order (0005 §6.3) becomes: header (name/lock,
description, Refresh) → **Open PRs** → **Open Issues** → **Notifications**.
The section is `GroupBox::new().id("issues-box")`, mirroring `prs-box`.

- **Title count:** `Open Issues ({issues.len()})` — the nodes-list length,
  mirroring the Open PRs box (`prs.len()`), i.e. the number of rendered rows.
  Consequence: a repo with >100 open issues shows "(100)" while the card shows
  the exact sum — identical to how PRs behave today. Keeping the two adjacent
  boxes consistent beats mixing count semantics; exact totals in box titles
  are a follow-up for both boxes together (§10).
- **Rows:** first 100 open issues, `UPDATED_AT DESC` (pinned by the query —
  activity-first, same as PRs), rendered in arrival order. The repo-scoped
  refresh on view open repaints the section exactly like the PRs one
  (0007 D6 edge freshness).
- **Empty:** muted cell "No open issues" (mirror of "No open PRs").

### D4 — Issue rows: the PR row treatment minus the draft badge

`render_issue_row()` mirrors `render_pr_row()`:

- element id `issue-{key}` (`key` = `owner/name#number`, the 0007 D3 fallback
  identity) **plus a role** — `.role(accesskit::Role::ListItem)` (docs/0010
  D10; without the role gpui drops the node from the a11y tree entirely,
  see 0015 D1);
- layout: `px_2 / py_1p5 / rounded_sm / text_sm`, hover accent background,
  cursor pointer;
- left: title (medium weight) + `#{number}` muted;
- right: author avatar + login (when author present) + relative age
  (`relative_age(updated_at)`);
- click → `cx.open_url(issue.url)` — browser, not an in-app view (PR parity).

No `Draft` badge — issues have no draft state in our field set. There is no
`statusCheckRollup` for issues; nothing extra to surface.

Per docs/0010 D10 these rows are interactive elements carrying an id **and a
role** — only the pair produces an a11y node — so they appear in
`dashboard-debug tree` with stable `accesskit_id`s and are click-targetable
through the bridge. The `ListItem` role was added in the follow-up fix
(#82) after the 0015 D1 discovery; before it, both row families rendered
but were invisible to the dump and to bridge `click`.

### D5 — dead_code cleanup

Consumed by this feature → drop the attributes and their "consumed by the
issues UI (follow-up)" comments from: `IssueSummary.total_count`,
`Issue.key`, `Issue.number`, `Issue.title`, `Issue.author`, `Issue.url`.
Kept: `Issue.database_id` (still unconsumed; comment reworded to match its PR
twin — REST-parity field kept for cache continuity).

### D6 — Render plumbing: derive from the existing snapshot, no new state

The right column already reads the selected repo from the in-memory snapshot
(0007 D6 paint-immediately path); the issues slice flows to the new section
the same way the PR slice does. No new `GithubPage` fields, no new
`GithubState` fields, no new entities.

## 5. File layout & shape

```
crates/modules/github/src/
  gql.rs     both queries: issue `author { login avatarUrl }`; drop the
             consumed-field dead_code allows (D5); test fixtures extended
  service.rs card_counts() → three counts (unread, open PRs, open issues)
  card.rs    third row in the connected arm; disconnected copy gains "issues"
  page.rs    `issues-box` GroupBox between `prs-box` and `notifs-box`;
             `render_issue_row()`; empty state; plumbing per D6
  api.rs, http.rs, auth.rs, settings.rs — untouched
docs/
  README.md                       index row for 0012 (this PR)
  0005-github-module.md           §6.1/§6.3 wording updated by the
                                  implementation PR where shipped behavior
                                  diverges (docs stay source-of-truth)
```

## 6. Testing

- `gql.rs`: fixture update — an issue author with `avatarUrl` parses into
  `Author { login, avatar_url }`; extends
  `maps_repo_nodes_with_prs_and_issues`.
- `service.rs`: `card_counts` — issues sum across repos; clamp parity;
  exact totals for a >100-per-repo repo.
- `page.rs`: the `repo()` test helper already builds `issues: IssueSummary`;
  right-column layout is not unit-testable in isolation and is verified
  through the bridge (§7).
- Gates before push: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets` (zero warnings — token-discipline
  lints apply: theme colors via `cx.theme()`, token text sizes),
  `cargo test --workspace`.

## 7. Verification through the debug bridge (implementation PR)

Per AGENTS.md PR discipline, exercised before pushing:

1. `cargo run` + `dashboard-debug tree` — `issues-box` present between
   `prs-box` and `notifs-box`; `issue-*` row nodes with bounds; the home card
   shows the three count rows.
2. `dashboard-debug click issue-<owner/name#N>` — the issue opens in the
   browser.
3. `dashboard-debug logs` — repo open produces no new fetches beyond the
   existing repo-scoped snapshot.

## 8. Acceptance criteria

1. Connected card: Notifications / Open PRs / Open Issues rows; Open Issues
   = Σ `issues.total_count` (exact per repo; same repo-list cap as PRs).
2. Repo view: Open Issues between Open PRs and Notifications; title
   `Open Issues ({len})`; rows show title, `#number`, author avatar + login,
   relative age; `UPDATED_AT DESC`; first 100.
3. Row click opens the issue on github.com; hover affordance matches PR rows.
4. Repo with no open issues: "No open issues" muted cell. No repo selected:
   unchanged.
5. No new requests, no cadence change; `snapshot-v2` caches load with issues
   visible immediately.
6. `#[allow(dead_code)]` removed from the consumed fields (D5); clippy clean.
7. All gates green; docs updated (this doc + index; 0005 wording where
   shipped behavior diverges).

## 9. Human checks (owner)

- Card visual: three count rows fit the 320×200 card comfortably; alignment
  matches the existing rows.
- Issue rows vs PR rows: avatar, muted `#number`, age — eyeball parity.
- Click-through: browser opens the issue, app keeps focus/geometry.
- If a >100-open-issues repo is available: card number exact, section title
  shows "(100)".

## 10. Follow-ups (GitHub issues, not this PR)

- Exact totals in repo-view box titles — `total_count` is already cached;
  applies to the PRs and Issues boxes together, one issue covers both.
- Issues depth (labels, assignees, comment counts, section filtering) — only
  on owner request.
- Pre-existing, untouched: `statusCheckRollup` surfacing in PR rows
  (0007 §9).
