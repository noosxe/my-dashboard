# 0051 — GitHub view-driven minimal fetches

Issue #325. Eliminate the global snapshot query and replace its consumers
with minimal, view-driven fetches.

## 1. Motivation — measured receipts

GitHub's primary GraphQL limit charges **Σ connection instances at
declared `first`/`last` caps ÷ 100** per request (docs: "Predicting the
point value of a query"; minimum 1). `GLOBAL_SNAPSHOT_QUERY` nests
per-PR `labels(first:8)` + `reviewRequests(first:20)` inside
`pullRequests(first:100)` inside `repositories(first:100)` — ~30,202
units → **~302 points per page, ~604 per bootstrap** (119 repos = 2
pages). Steady state re-ran it every 5 minutes (~7,200 pts/hour),
rate-limiting the app against itself within ~40 minutes of open time,
with megabyte bodies ("initial seed takes too long"). Live receipts:
fresh window 5,000 → −606 in 21 s at launch; `ACCOUNT_PULL_REQUESTS`
page measured `rateLimit.cost = 2`.

docs/0050 §2's "flat 1 point per request" premise is wrong (issue #294
measured only connection-less queries) — corrected in §8 below.

The consumer sweep (#325) showed the big query feeds only: home card
totals, repo inventory metadata, and the module-home D6 union fallback.
Everything else already runs on its own cheap queries.

## 2. Design decisions

**D1 — Query set.** Three active lanes replace the snapshot:

| Lane | Query | Trigger | Cost |
|---|---|---|---|
| Home card | `CardTotals` (new) | home page shown | ~2 pts/page, ≤2 pages |
| Session | `SessionInventory` (new) | once per launch, first need | ~1 pt/page, ≤2 pages |
| Module home | `ACCOUNT_PULL_REQUESTS` (existing) | module home shown / org switch / retry | 2 pts/page (measured) |

`REPO_SNAPSHOT`, `PR_DETAIL`, `ISSUE_DETAIL`, notifications (REST), the
usage probe and pickers are untouched. `GLOBAL_SNAPSHOT_QUERY` and its
`Kind::Snapshot` plumbing are deleted.

**D2 — `CardTotals` query.**

```graphql
query CardTotals($cursor: String) {
  rateLimit { cost remaining resetAt }
  viewer {
    repositories(first: 100, after: $cursor,
                 ownerAffiliations: [OWNER, COLLABORATOR, ORGANIZATION_MEMBER],
                 orderBy: {field: PUSHED_AT, direction: DESC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        pullRequests(states: OPEN) { totalCount }
        issues(states: OPEN) { totalCount }
      }
    }
  }
}
```

No node fields — totals only. Pages to **exhaustion, hard cap 10 pages**
(D9). The card sums `totalCount` across pages and renders its three
numbers (notifications count still comes from the REST lane).

**D3 — `SessionInventory` query** — one fetch per app launch, armed on
first need (any consumer requiring login/orgs/inventory):

```graphql
query SessionInventory($cursor: String) {
  rateLimit { cost remaining resetAt }
  viewer {
    login
    avatarUrl
    organizations(first: 100) { nodes { login avatarUrl description } }
    repositories(first: 100, after: $cursor,
                 ownerAffiliations: [OWNER, COLLABORATOR, ORGANIZATION_MEMBER],
                 orderBy: {field: PUSHED_AT, direction: DESC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        databaseId id name nameWithOwner description
        isPrivate isArchived pushedAt hasIssuesEnabled url
        owner { login }
      }
    }
  }
}
```

Metadata scalars only; final field list trimmed at implementation to
what the consumers read (repo dropdown rows, crumb short name,
repo-header lookup, org filter by `owner.login`, new-issue/new-PR
creation gates: `node_id`, `has_issues_enabled`, `is_archived`).
Populates the existing `state.repos` / `state.orgs` / `state.user`
paths — `visible_repos`, `repo_crumb_label` and the repo-header lookup
keep working unchanged. `Repo.prs`/`Repo.issues` arrive **empty** and
are filled only by the lazy `REPO_SNAPSHOT` landing on repo open (the
existing docs/0007 D6 path); `last_activity()` degrades to `pushed_at`
until then. Logout needs no proactive check — failing requests surface
it through existing error paths.

**D4 — View-shown = fetch.** `Navigator` is an
`EventEmitter<NavEvent>`; `go()` emits on every navigation and
`Reentered` on re-clicking the current page. Both faces subscribe:

- **Overview card**: on nav → `Page::Home` active and auth connected →
  fire `CardTotals`. On first render with no landed totals yet, fire as
  well (app opens on home).
- **Module main page**: on nav → `Page::Module("github")` active → fire
  `fetch_home_prs` (module home) — alongside the existing org-switch
  and Retry triggers. Repo views keep their existing view-open fetch.

Repeats are absorbed by the docs/0031 inflight dedup — triggers are per
nav event, never per render frame.

**D5 — Skeletons, retention, D6 deletion.**

- The **D6 union fallback is deleted** (page.rs home_source union arm,
  the `partial` honesty marker, and its tests): its premise — eagerly
  prefetched rows — no longer exists. docs/0047 amended in §8.
- Surfaces with no landed data render **skeletons**
  (`gpui_component::skeleton::Skeleton`, available at pinned rev
  `94a313a`: animated, themed, fully `Styled`): the card's three
  numbers, module-home rows, repo rows before first landing. 1–2 s of
  skeleton on first open is accepted.
- **Retention**: lane data is kept in memory across view switches;
  re-showing a surface renders the previous data immediately, and the
  view-shown refresh replaces it in place on arrival (apply arms
  replace, they never clear first).

**D6 — Snapshot teardown, notifications untouched.** Delete
`GLOBAL_SNAPSHOT_QUERY`, `snapshot_spec`, the `Kind::Snapshot` fetch and
apply arms, and the `SNAPSHOT_EVERY` cycle. `needs_bootstrap` keeps its
**notifications** half (REST initial fetch on connect) and additionally
arms `SessionInventory` once on connect; the snapshot half is gone.

**D7 — Refresh policies stay as they are.** This doc does not touch the
existing age-gated heartbeat refreshes (home/repo-rows lanes, docs/0032
detail auto-refresh, docs/0035 list surfaces, docs/0038 realtime,
`focus_engaged` age gates) — their evaluation is issue #326. Only the
snapshot's own 5-minute cycle dies here.

**D8 — Usage calculator interplay.** Both new queries carry
`rateLimit { cost remaining resetAt }`, so the per-bucket book keeps
reconciling on every GraphQL response exactly as in docs/0050; the REST
probe, cooldown and chip are untouched. docs/0050 §2 is corrected to
the documented cost formula with the receipts above (the calculator's
display contract — server-reported, verbatim — is unchanged and was
never affected).

**D9 — Pagination contract.** Both new queries page to exhaustion with
a hard cap of **10 pages**; hitting the cap logs a warning (the totals
would be partial — same honesty as today's 3-page snapshot cap, at 10×
the coverage).

## 3. State and plumbing changes

- `Kind`: remove `Snapshot`; add `CardTotals`, `SessionInventory`.
- `gql.rs`: new query consts + response parsers (`CardTotalsPage`,
  `InventoryPage`); `GLOBAL_SNAPSHOT_QUERY` and its parser deleted.
- New state: `card_totals: Option<CardTotals>` (summed PRs, summed
  issues, landed clock) replacing `card_counts`'s read of
  `state.repos`. Inventory reuses `repos`/`orgs`/`user`.
- `service.rs`: specs + apply arms for the two new kinds; snapshot arms
  and `SNAPSHOT_EVERY` removed; bootstrap arm re-armed per D6.
- `card.rs` / `page.rs`: nav subscriptions per D4; skeleton states and
  retained-data rendering per D5; union fallback removed.

## 4. Testing

- Parsers: fixture JSON → `CardTotalsPage` / `InventoryPage` (fields,
  pagination cursors, cap-at-10 behavior).
- Lane arms: apply replaces in place (retention), landed clocks,
  `card_counts` from the new lane, inventory landing `repos`/`orgs`/
  `user` with empty summaries.
- Triggers: nav-event → spec fired (card on `Page::Home`, page on
  `Page::Module`), dedup absorbs repeats, first-render fire when
  nothing landed.
- Teardown: no `Kind::Snapshot` references remain; bootstrap still
  fetches notifications.
- Bridge verification: usage chip session counter ≈ ≤10 GraphQL points
  at open; aria labels correct; skeleton nodes present before first
  landing; previously fetched rows shown instantly on re-open.

## 5. Acceptance

- `GLOBAL_SNAPSHOT_QUERY` gone; no code path fetches it.
- App open on home bills ≤ ~10 GraphQL points total (chip + logs).
- Idle app on the home page (open, untouched) issues zero GraphQL
  requests; an *open module page* keeps its pre-existing age-gated
  heartbeats per D7 (their audit is #326).
- Card numbers, repo dropdown, crumbs, repo header, module home all
  correct; skeletons on first open; retained data on re-open.
- Notifications REST lane behavior unchanged.
- Gates: `cargo fmt --all --check`, `cargo clippy --workspace
  --all-targets` (zero warnings), `cargo test --workspace`.

## 6. Non-goals

- Refresh-policy audit of the surviving queries and focus-refresh
  mechanics — issue #326.
- Any datastore/cross-page cache layer (explicitly rejected).
- Notifications cadence, tick-loop re-evaluation beyond the snapshot
  arms, query changes outside the lanes listed in D1.

## 7. Cross-references

- #325 — implementation issue (facts, receipts, decisions 1–9).
- #326 — follow-up audit (costs, periodic refresh, fetch on focus).
- docs/0007 D6 (repo-scoped refresh on view open — unchanged, now the
  only filler of `Repo.prs`/`Repo.issues`), docs/0031 (inflight dedup),
  docs/0034/0037 (staleness marking — reused for the new lanes'
  clocks), docs/0042 (repo list filters — unchanged consumers),
  docs/0047 (module home; D6 amended), docs/0050 (usage calculator;
  §2 corrected).

## 8. Docs amendments carried by the implementation PR

- docs/0050 §2: replace the flat-cost premise with the documented
  formula + receipts (this doc §1).
- docs/0047: delete D6 (union fallback) and its `partial` marker;
  loading state = skeletons per D5.
- docs/0007 §5 / bootstrap references: snapshot bootstrap →
  `SessionInventory` + notifications (D6).
- docs/README.md: row for 0051.
