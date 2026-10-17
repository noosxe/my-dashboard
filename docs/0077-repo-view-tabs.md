# 0077 — Repo View Tabs (per-tab lanes: Open PRs / Open issues / Notifications)

- **Issue:** (files with the implementation issue after this doc merges)
- **Amends:** docs/0007 D6 (repo-open fetch pair), docs/0035 D2 (heartbeat lanes)
- **Interacts with:** docs/0042 (repo-view filters), docs/0031 (in-flight dedup), docs/0036 (backoff), docs/0073 D3 (row derivation), docs/0065 (UI gate), docs/0071 (containerized verification)

## 1 Summary

The open-repo view's right column stacks three sections (Open pull requests,
Open issues, Notifications) and fetches all of their data on repo open —
one repo-scoped GraphQL snapshot carrying PRs **and** issues, plus the
per-repo notifications REST call. This doc restructures the view into a
three-tab layout and makes fetching view-relevant:

- **Tab 1 — Open PRs** (default). Opening the repo view fetches the repo's
  open PRs.
- **Tab 2 — Open issues.** Switching to it fetches the repo's open issues.
- **Tab 3 — Notifications.** Switching to it fetches the repo's
  notifications.

Each tab owns one fetch lane; only the visible tab's lane is ever fetched
or heartbeat-refreshed. The repo-scoped GraphQL snapshot (one query for both
collections) is retired in favor of two single-collection queries.

## 2 Current behavior (what changes and what doesn't)

**Fetches.** `select_repo_inner` fires `repo_open_specs` — the
`Kind::RepoSnapshot` GraphQL query (`query RepoSnapshot`, both
`pullRequests(states: OPEN, first: 100)` and `issues(states: OPEN,
first: 100)` plus totals in one round trip) and the `Kind::RepoNotifications`
REST pair — on every repo open. The docs/0035 D2 heartbeat re-fires the same
pair via `repo_rows_refresh_specs(FOCUS_MIN_AGE)` when `rows_landed[repo]`
is stale; docs/0036 backoff (`rows_fail_streak`) guards the lane; docs/0031
dedups in-flight.

**Render.** Three GroupBoxes (`prs-box`, `issues-box`, `notifs-box`) render
stacked, each with its docs/0042 filter toolbar, count title, cap hint, and
skeleton gate (`repo_rows_landed`). Rows derive per docs/0073 D3
(snapshot ⊗ documents, OPEN-filtered at read) — PR rows from `repo_pr_rows`,
issue rows from `repo_issue_rows`, notifications from the
`repo_notifications` map (the per-repo REST payload, re-derived from the
global bell sync on landing, docs/0007 D6).

**Unchanged by this doc:** the global snapshot (docs/0007) still seeds the
repo list (left column) and the paint-before-fetch rows; the home card's
totals lane; the bell's notifications lane; every detail page; the docs/0072
store and docs/0073 derivation; docs/0042 filters (each tab keeps its own
filter toolbar and the shared search box).

## 3 Goals

- G1 — One visible section per tab: Open PRs / Open issues / Notifications,
  Open PRs default.
- G2 — Fetch relevance: a tab's data is fetched when the tab is entered
  (open or switch), never for a hidden tab; the heartbeat refreshes only the
  active tab's lane.
- G3 — The established freshness substrate keeps working unchanged:
  docs/0031 in-flight dedup, docs/0034 staleness, docs/0036 backoff,
  ETag-conditional revalidation for the REST lane.
- G4 — No regression to paint-before-fetch: an empty, never-fetched tab
  shows skeleton rows; a landed lane paints from held state instantly
  (including the global snapshot's seed rows).

## 4 Decisions

### D1 — Tab state is page-owned and session-local

`RepoTab` enum on `GithubPage` (`Pulls | Issues | Notifications`), default
`Pulls`. It resets to `Pulls` on every repo *selection* (the "user opens the
repo view" boundary — the default tab is what the user asked for). It
survives detail-page round trips (open a PR, come back — the tab is where
you left it; the view-shown trigger re-offers the active lane, age-gated).
It does not persist across app restarts. The switch is a plain
`cx.listener` write + `cx.notify()` — no entity, no focus dance.

### D2 — Per-tab fetch lanes; RepoSnapshot retired

Two new GraphQL kinds replace `Kind::RepoSnapshot` everywhere:

- `Kind::RepoPulls` — `query RepoPulls($owner, $name)`: the repository's
  `pullRequests(states: OPEN, first: 100)` connection **and** its
  `pullRequests(states: OPEN) { totalCount }` scalar — the same fragments
  the RepoSnapshot query uses today for its PR half (row fields identical,
  so `land_summary` and docs/0073 derivation are untouched).
- `Kind::RepoIssues` — the issues half, symmetric.

Both land through the existing repo-snapshot apply path (merge the single
repo, replace only the landed collection, persist, land summaries, stamp the
lane clock). The `RepoSnapshot` kind, its query, and its test fixtures are
deleted — no dead code. The repo view's GroupBox count titles read
`repo.prs.total_count` / `repo.issues.total_count`, which each lane's query
carries; a lane's total can be the global snapshot's seed value until that
lane first lands (same semantics as today's seed-then-refresh).

GraphQL cost note, recorded deliberately: visiting both row tabs costs
~2×100 + 2 scalars ≈ two points, versus today's single ~201-unit query —
one point either way by GitHub's rounding; the split buys view-relevance,
which is the owner-set goal.

### D3 — The lane map (what fires when)

| Event | Fires |
| --- | --- |
| Repo open (`select_repo_inner`) | active tab's lane — with the D1 default, `RepoPulls` — for the newly selected repo. Nothing else. |
| Tab switch to `Issues` | `RepoIssues` for the selected repo |
| Tab switch to `Notifications` | `RepoNotifications` for the selected repo |
| Tab switch to `Pulls` | `RepoPulls` for the selected repo |
| View-shown after a detail page closes | active tab's lane (age-gated) |
| Mutation-refetch return (docs/0016 flows) | invalidates **all three** lanes' freshness for the repo (correctness over frugality after a write); the next view-shown/heartbeat then fetches the active one |
| Heartbeat beat (docs/0035) | active tab's lane only, at `FOCUS_MIN_AGE` |

"Switch fires the lane" means the switch *offers* the spec; the established
substrate decides whether a request goes out: docs/0031 in-flight dedup
absorbs a rapid toggle, and the lane's landed clock gates re-offers inside
the freshness floor (the same semantics as today's view-shown). A first
visit to a tab always fetches.

### D4 — Landed clocks and backoff go per-lane

`rows_landed: BTreeMap<String, Instant>` becomes lane-keyed — key
`"{full_name}\u{1}{lane-as-u8}"`-style composite or a `(String, RepoLane)`
map key; skeleton gates (`repo_rows_landed`) and the heartbeat read the
*active lane's* entry, so each tab's skeleton resolves on its own landing.
`rows_attempted_at` / `rows_fail_streak` stay **per-repo, shared across the
three lanes**: failures are connectivity-shaped, and one backoff clock keeps
the guard simple (docs/0036 D3's `bump_rows_backoff` call sites already sit
in the page's landing arm).

### D5 — Notifications tab behavior is today's behavior

The Notifications tab renders the existing notifs GroupBox unchanged:
per-repo REST fetch (ident-keyed ETags, 2 pages), the global bell sync's
re-derived slice, docs/0042 notif filters, row click → notification detail.
The only change is that it fetches on *tab entry* instead of on repo open —
the repo-open notifications fetch moves out of `repo_open_specs`.

### D6 — Tab strip render (docs/0017 / design-guides anchored)

`TabBar::new("repo-tabs").underline()` between the repo header and the right
column — the usage-analytics tab strip is the in-app precedent, the
gpui-component `story` gallery the widget anchor. Three tabs, plain text
labels ("Open PRs", "Open issues", "Notifications"); no count badges (the
GroupBox titles already carry counts — restraint over duplication). Each
`Tab` gets a debug id (`repo-tab-pulls` / `repo-tab-issues` /
`repo-tab-notifs`, docs/0010 D10) and the strip sets position/size-of-set so
assistive tech announces "tab 2 of 3"; the gpui-kit `Tab` primitive carries
the a11y plumbing (its docs note keyboard nav is still upstream — tab-strip
keyboard coverage follows the primitive, the 0065 audit records whatever it
does). The active tab's GroupBox renders below the strip; the two hidden
GroupBoxes render nothing (filters keep their state — `RepoViewFilters` is
untouched; option menus derive from held nodes as today, only the active
tab's options are computed).

### D7 — The left column and header are untouched

Repo list, breadcrumb, header actions (New issue / New PR, create gating),
search box (shared, filters the active tab's rows), sync indicator: all
exactly as today.

## 5 Knobs

- Lane freshness floor on re-offer: the existing `FOCUS_MIN_AGE` (no new
  constant; owner-tuned like today).
- Notifications pages: unchanged (`max_pages: 2`).
- Row cap: unchanged (100 per connection; cap hints as today).

## 6 Non-goals

- No tab persistence across restarts, no per-tab badges, no lazy pagination
  changes (the 100-cap and cap-hint stay).
- No change to the global snapshot's contents (it still carries PRs+issues
  for seeding; only the repo-view *refresh* lanes split).
- No notifications-list redesign — the tab is a relocation, not a rework.

## 7 Implementation plan

One PR after this doc merges:

1. **service.rs / gql.rs**: `RepoPulls`/`RepoIssues` queries + kinds; split
   apply path; lane-keyed landed clocks; `repo_open_specs` →
   `repo_lane_spec(repo, lane)`; heartbeat + mutation-refetch per D3/D4;
   delete `RepoSnapshot`.
2. **page.rs**: `RepoTab` + switch listener; tab strip render; per-lane
   skeleton gates; right column renders one GroupBox.
3. **Tests**: lane-map unit tests (open fires pulls; switch fires issues /
   notifs; fresh-lane toggle dedups; repo switch resets tab + fires pulls);
   split-apply landing (each lane replaces only its collection); heartbeat
   active-lane-only; filters/search still scoped per tab.
4. **Gate receipts (docs/0065)**: `dashboard-debug audit` over the repo-view
   surface + `tree --tab-stops`; the `github` structural golden **will
   drift** (tab strip added) — re-capture in the docs/0071 container in the
   same PR with the drift explained; `critique` before/after per D7's
   anchor rule.
