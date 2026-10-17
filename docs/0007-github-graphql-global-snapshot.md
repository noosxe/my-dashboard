# 0007 — GitHub Module: GraphQL Global Snapshot (repos, PRs, issues)

- **Date:** 2026-09-01
- **Depends on:** 0005 (module contract, GitHub service layer), 0004 (paths,
  persistence primitives, debounced saves)
- **Closes the design need of:** #28 (pr-count walk → GraphQL aggregation),
  expanded by owner direction to a full global-snapshot architecture
- **Superseded in part (docs/0051):** the global snapshot query and its
  5-minute cycle were removed — GitHub's connection-cap cost formula
  billed it ~302 points/page (~604 per bootstrap, self rate-limiting
  within ~40 minutes of open time). The survivor is the repo-scoped
  refresh on view open (§6/D6); the global query's consumers moved to
  the totals-only card query and the once-per-launch session inventory.

## 1. Context & problem

The GitHub module talks REST exclusively today (0005 §3). Three structural
costs, all rooted in REST's one-resource-per-request model:

1. **The PR-count walk is O(N) requests** — `pr_walk_specs()` issues
   `GET /repos/{owner}/{repo}/pulls?state=open&per_page=100&page=1` per repo
   (cap `PR_WALK_REPO_CAP = 100`), every 5th tick. Accurate but heavy: at full
   cadence ≈ 1,200 REST requests/hour of the 5,000 budget.
2. **The walk runs serially** (`spawn_tick_loop` awaits specs in order), so
   fresh counts arrive after ~N×RTT; the card repaints once per tick, after
   the slowest repo (#28 owner note — lever 2).
3. **Silent caps**: PR counts cap at 100 (`prs.len()` of page 1), the repos
   list stops at page 1 (100 repos, `MAX_PAGES` override `None`), and repo
   views wait for the next tick after selection before showing PRs.

GitHub's GraphQL API collapses all of this into **one round trip**. Verified
facts (GitHub docs, 2026-09-01; schema cross-checked against the
`@octokit/graphql-schema` SDL mirror v15.26.1 — see §5):

- Endpoint `POST https://api.github.com/graphql`, `Authorization: bearer
  <token>` — the existing device-flow OAuth token works as-is; no new scopes.
- **Notifications have no GraphQL API** (GitHub community discussion #13056)
  → notifications MUST stay REST. Everything else the module fetches
  (`/user`, `/user/orgs`, `/user/repos`, per-repo `/pulls`) CAN move.
- Rate-limit points are computed from *connections*, not fields: the full
  snapshot query costs ≈ 2 points per call (of 5,000/hour) regardless of how
  many fields ride along.
- GraphQL has **no ETag/304** — conditional revalidation disappears for
  migrated endpoints (acceptable at 1 call / 5 min; notifications keep ETags).

## 2. Scope

**In scope:**

- A minimal GraphQL client path in the module's HTTP layer (POST + JSON
  envelope + `errors[]` handling; no ETag machinery).
- One **global snapshot query**: viewer account, orgs, and every repo with
  open-PR data, open-issue data, and exact counts.
- One **repo-scoped snapshot query** (same node shape, single repo) fired on
  repo-view open for edge freshness.
- Cache-first bootstrap: paint from disk cache instantly, refresh UI when
  the snapshot lands, persist to disk.
- Tick-loop restructure: snapshot replaces the walk + repo/org/user refresh
  kinds; per-repo notification refetching stops after open (derived from the
  global REST fetch instead).

**Out of scope (filed separately, not opportunistically done):**

- Issues **UI** — issue data is gathered now (groundwork), displayed later
  under its own doc/PR.
- Notifications leaving REST (impossible upstream).
- Mark-as-read / any mutation.
- UI/layout changes beyond data accuracy (counts stop capping).

## 3. Decisions

### D1 — Root query shape: `viewer.repositories` with explicit affiliations

`User.repositories` **defaults `ownerAffiliations` to `[OWNER,
COLLABORATOR]`** (schema SDL) — org-member repos would silently vanish
compared to REST `/user/repos` (default affiliation `owner, collaborator,
organization-member`). The query MUST pass all three:

```graphql
ownerAffiliations: [OWNER, COLLABORATOR, ORGANIZATION_MEMBER]
```

The sketch in #28 (`repositoryOwner(login:).repositories(ownerAffiliations:
OWNER)`) is rejected for the same reason — owner-only is a subset of what
the module shows today. `viewer` is the right root (no login variable
needed; it is also the `/user` replacement).

### D2 — Per-repo subselection: full PR + issue data, not just counts

Node fields are free on rate-limit points (§5); the user-directed behavior
is "instant, up-to-date repo views". Each repo node carries:

```graphql
pullRequests(states: OPEN, first: 100,
             orderBy: {field: UPDATED_AT, direction: DESC}) {
  totalCount
  nodes { databaseId number title isDraft state
          author { login avatarUrl }
          updatedAt url
          statusCheckRollup { state } }
}
issues(states: OPEN, first: 100,
       orderBy: {field: UPDATED_AT, direction: DESC}) {
  totalCount
  nodes { databaseId number title
          author { login }
          updatedAt url }
}
```

- `totalCount` fixes the >100 silent cap for card counts; `first: 100`
  node lists match today's REST page-1 depth per repo.
- `statusCheckRollup { state }` is the CI-rollup signal 0005 §3 explicitly
  deferred ("one extra call per PR" under REST); here it rides along at zero
  marginal point cost. Surfaced in the repo-view PR rows since #67 — glyph
  + tooltip per row, absent for a null rollup.
- **Row ordering changes**: REST `/pulls` defaults `sort=created, desc`;
  the GraphQL query pins `UPDATED_AT DESC` (activity-first). Accepted
  behavior change — the freshest PRs surface first.
- Issue nodes are gathered for all repos (groundwork for the future issues
  view) even though no UI consumes them yet. If payload size ever matters
  (§5), dropping issue nodes from the *global* query is the designated
  trim — the repo-scoped query keeps them.

### D3 — Identity fallback: `databaseId` is nullable

GraphQL `id` is a base64 Node ID (string); `databaseId: Int` matches today's
REST `id: u64` but is nullable. Row identity MUST fall back to the composite
`"{nameWithOwner}#{number}"` when `databaseId` is null. The composite is
also the cache/BTreeMap key for PRs (per-repo lists are already keyed by
repo slug; PR entries move from `id` to this stable key).

### D4 — One snapshot, one atomic apply

The snapshot lands as a unit: account + orgs + repos (+ PRs/issues/counts)
replace their state fields in a single `apply`, then one window refresh.
The card never shows a half-walked state (#28's "atomic apply" goal).
Per-node degradation is per-repo: a repo whose subselection returns null or
lands in `errors[]` keeps its last-known data (stale-but-present) and logs
to stderr — [DG] silent degradation, never a dialog, never a dropped card.

List ordering is part of the apply, not the connection: after every merge
the repo list is sorted client-side by **composite last-activity** — per
repo, the newest of `pushedAt`, the latest open-PR `updatedAt`, and the
latest open-issue `updatedAt` (nodes arrive `UPDATED_AT DESC` already; the
max over nodes is taken so correctness never depends on that). The
connection's `PUSHED_AT DESC` ordering alone ignores PR/issue activity,
and the repo-scoped refresh that fires on selection must never move the
row (#35). Ties break by key for a stable order.

### D5 — HTTP layer addition (no ETag path)

`http.rs` gains a `post_graphql(query, variables)` method: `POST` to
`https://api.github.com/graphql`, bearer token, JSON body
`{"query": ..., "variables": ...}`, response parsed into an envelope:

```rust
struct GraphQLEnvelope<T> {
    data: Option<T>,
    errors: Option<Vec<GraphQLError>>, // { message, path?, type? }
}
```

HTTP-level status handling mirrors REST: 401 → existing token-rejected
flow (Q9); 403/429 → rate-limit gate (`rate_limited_until`). Point budget
comes from the query's own `rateLimit { remaining resetAt }` field (the
authoritative per-request cost for GraphQL; header parsing stays for REST).
The `Http` trait grows one method; the mock-test pattern (0005 §5) extends
naturally.

### D6 — Cadence and tick restructure

`TICK` stays 60 s. `plan_tick` becomes:

| When | Work |
| --- | --- |
| bootstrap | snapshot + global notifications (REST, MAX_PAGES — same walk as every tick, #170) |
| every tick | global notifications (REST, ETag, MAX_PAGES — unchanged) |
| every 5th tick | snapshot |
| repo view opened | the active tab's lane only (docs/0077 D3: `RepoPulls` GraphQL by default; a tab switch fetches its lane) — the `RepoSnapshot` both-collections query is retired |

- The snapshot cadence replaces `PR_WALK_EVERY` (5 ticks) **and**
  `REPO_LIST_EVERY` (10 ticks): repos/orgs now refresh at 5 min instead of
  10, at ~2 points/call — total ≈ 24 points/hour of 5,000. The REST walk,
  `Kind::User`, `Kind::Orgs`, `Kind::Repos`, `Kind::SelectedPrs`, and
  `Kind::PrCount` are deleted.
- Per-repo notifications: fetched **once** when the repo view opens; after
  that the GroupBox derives from the global notification list (filter by
  `repository.full_name`). No per-tick repo-scoped notification fetch.
- On repo open the view paints **immediately from the in-memory snapshot**
  (all repos' PRs are already there), then the repo-scoped GraphQL call
  refreshes just that repo ("edge freshness"). Selection no longer waits
  for the next tick.

### D7 — Persistence: one versioned snapshot key

Today's granular keys (`user`, `orgs`, `repos`, `pr-counts`, `prs-{slug}`,
`notifs-{slug}`) collapse into **`snapshot-v2`** *(bumped to
**`snapshot-v3`** by #275: `Repo` gained `is_archived`, and the
schema-version discipline is the key itself — old files are ignored
wholesale and every repo refetches with the field)*:

```json
{ "schema": 2,
  "user":  { "login": "..." },
  "orgs":  [{ "login": "..." }],
  "repos": [{ "databaseId": 42, "name": "...", "nameWithOwner": "...",
              "description": null, "isPrivate": false, "pushedAt": "...",
              "owner": { "login": "..." },
              "prs":  { "totalCount": 3,
                        "nodes": [ { "key": "o/r#12", "number": 12, ... } ] },
              "issues": { "totalCount": 7, "nodes": [ ... ] } }] }
```

- Written through the existing `store.rs` primitives (atomic writes,
  corrupt-file quarantine, debounced saves — 0004 §4.2). Old cache files are
  simply ignored (fresh keys); sign-out already wipes the module cache root.
- Bootstrap seeds UI state from `snapshot-v2` + `notifications` exactly as
  today's seeds work (0005 §3.4), fires the snapshot immediately.
- `notifications` and `notifs-{slug}` keys keep their REST shapes.

## 4. File layout & shape

```
crates/modules/github/src/
  http.rs    + post_graphql() (D5), GraphQLEnvelope
  gql.rs     NEW — query constants (GLOBAL_SNAPSHOT_QUERY,
             REPO_SNAPSHOT_QUERY), response structs, node→domain mapping
  api.rs     reduced to REST notifications (builders + Notification shape)
  service.rs Kind::{Snapshot, RepoSnapshot}; plan_tick per D6;
             apply_snapshot() (D4); selection fires immediate repo call
  card.rs    unchanged (reads Σ totalCount — now exact)
  page.rs    unchanged (PR rows gain nothing new to render; counts exact)
```

Domain types stay close to today's (serde renames where GraphQL names
differ): `Repo` gains `prs: PrSummary`, `issues: IssueSummary`;
`PullRequest` moves to the GraphQL field set (`isDraft`, `author`,
`updatedAt`, `url`, `statusCheckRollup.state`). The REST `PullRequest`
struct is deleted alongside the walks it served.

## 5. Quota, limits, and payload (verified)

- **Points**: `Σ connection-requests ÷ 100`, minimum 1 (GitHub rate-limit
  doc). The snapshot's connections: `repositories` (1) + per-repo
  `pullRequests` (N) + per-repo `issues` (N) + `organizations` (1) →
  2N + 2 → **2 points** for a 100-repo page. Node fields (`author`,
  `statusCheckRollup { state }`, scalars) add connections nowhere.
- **Node limit** 500,000: worst case ≈ 100 repos × (100 PR nodes + 100
  issue nodes + author nodes) ≈ 30k — far under.
- **Connection page cap**: `first`/`last` ∈ 1–100 enforced. Repos list:
  `first: 100` + `endCursor` follow-up while `hasNextPage`, capped at
  3 pages (300 repos — strictly better than today's single REST page);
  beyond the cap, later pages arrive on the next snapshot tick (cursor
  persisted in memory only).
- **Secondary limits**: ≤ 2,000 points/min (we use ~2 per 5 min), ≤ 100
  concurrent requests (we send 1–2).
- **Payload**: worst case (100 repos × 100 PRs + 100 issues with titles)
  ≈ 1–2 MB JSON; typical accounts are tens of KB. Trim order if it ever
  matters: issue nodes out of the global query (D2), then PR nodes (counts
  and repo views still correct via repo-scoped calls).

## 6. Error & degradation mapping

| Failure | Behavior |
| --- | --- |
| network error | stale data stays; stderr note (0005 Q9 pattern) |
| HTTP 401 | existing token-rejected flow |
| HTTP 403/429 or `rateLimit.remaining == 0` | `rate_limited_until = resetAt`; gate all fetches |
| `errors[]` with partial `data` | apply the `data` part; affected repo nodes keep last-known values; stderr per path |
| `data: null` | treat like network failure (stale + stderr) |
| repo node null / subselection null | that repo keeps last-known PRs/counts; card total uses last-known |
| `databaseId` null | composite key fallback (D3) |

## 7. Acceptance criteria

- [ ] Opening the app paints cached repos/PRs/counts instantly, then
      refreshes once when the snapshot lands (single repaint, no
      half-populated states).
- [ ] Card PR total equals Σ `pullRequests.totalCount` (repo with >100
      open PRs shows the true number, not 100).
- [ ] Opening a repo view paints PRs immediately from memory and issues a
      repo-scoped GraphQL call; a second open within the same tick window
      still triggers the edge call.
- [ ] Repo-scoped notifications are fetched exactly once per open; later
      ticks update the GroupBox from the global list only.
- [ ] Org-member repos appear (affiliation catch, D1).
- [ ] PR rows ordered by `updatedAt` desc.
- [ ] All gates: `cargo fmt --all --check`, `cargo clippy --workspace
      --all-targets` (zero warnings), `cargo test --workspace`.
- [ ] New unit tests: envelope parsing (partial errors), node→domain
      mapping incl. `databaseId`-null fallback, `plan_tick` cadence table,
      snapshot apply atomicity, cache round-trip `snapshot-v2`.

## 8. Human checks

- [ ] Fresh connect (empty cache): card populates after first snapshot.
- [ ] Second launch: instant cache paint, then refresh if data moved.
- [ ] Repo with >100 open PRs shows the real count.
- [ ] Open a repo view: instant rows, no spinner; notifications group
      populated; verify in devtools/logs only one repo-scoped call per open.
- [ ] Org repos present in the sidebar list.
- [ ] Disconnect network: stale data remains, stderr notes, no dialogs.

## 9. Follow-ups (GitHub issues, not this PR)

- Issues UI on top of the gathered data (needs its own design note).
- ~~Surface `statusCheckRollup.state` in PR rows~~ — shipped with #67.
- #24 (walk URL attribution) — the walk disappears; close as superseded by
  this design if the fix hasn't landed by implementation time.
- Global notifications REST `per_page` is server-clamped to 50 (today's
  `per_page=100` request is accepted but returns ≤50/page) — pagination
  already handles it; no action, recorded for accuracy.

## 10. Shipped — the CI glyph in PR rows (2026-09-07, issue #67)

The §9 follow-up: `statusCheckRollup.state` now renders in the repo-view
PR rows. `gql::rollup_state` maps the five real rollup states to the
unified tone (SUCCESS → success green, FAILURE/ERROR → danger red,
PENDING/EXPECTED → warning amber); `null` and unrecognized values render
no glyph at all. Each glyph is icon-only with a text tooltip ("CI:
passing" / "CI: failing" / "CI: pending"), `CircleCheck`/`CircleX`/
`LoaderCircle` per the icon family, probe key `pr-ci-{key}` (0010 D10).
Same tone contract as the PR-detail page's `check_visual` (0016 D7).
Snapshot-only — no query, service, or cache changes.

**2026-09-29, #397**: the pending glyph above now spins — same
`LoaderCircle`, same warning amber, rendered through the upstream
`Spinner` (reduced-motion: static). Decoration only; the tooltip keeps
the "CI: pending" semantics and the probe row/bounds are unchanged.
