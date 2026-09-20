# 0047 — GitHub module home view: account-wide open PRs

- **Issue:** #295
- **Status:** design (docs-only PR)
- **Depends on:** 0009 (module structure, two-column layout), 0031/0035/0037 (fetch, list auto-refresh, freshness), 0042 (list filters), 0044 §9 (truncation-hint idiom)

## 1. Summary

The GitHub module's home state — the right column with no repository
selected — is a static "Select a repository" placeholder. This doc
replaces it with the module's home view: **all open pull requests
across the currently selected account's repositories** (personal
account or org, per the left-column selector), rendered with the same
PR rows and the 0042 filter engine as a repo's PR list. Pick an
account on the left → see every open PR on the right, immediately.
Selecting a repo still takes the column over; deselecting returns
home.

## 2. Upstream facts (live-verified 2026-09-20, user token)

Measured against the production API; probe table and method in the
#294 comment on the usage-calculator issue.

- **GraphQL costs 1 point per request, flat.** Verified from 10 to
  10,100 max-node queries with the server's own `rateLimit.used`
  delta around each request. Both the old
  `1 + (maxNodeCount + 1) / 100` rule and the docs' "predicting point
  value" section (connection-pages ÷ 100) overstate live accounting.
  All budget math below is therefore request counts.
- **Search is the mechanism.** GraphQL `search(query: …, type: ISSUE,
  first: 100)` with `is:pr is:open` plus an account qualifier:
  `org:LOGIN` scopes to the org's repositories, `user:LOGIN` to
  repositories owned by the personal account — symmetric with the
  selector's two account kinds (0009). `issueCount` reports the full
  match count regardless of paging (vercel: 6,090).
- **Search caps at 1,000 results per query.** Verified end-to-end by
  walking vercel's cursor chain: pages advance to offset 1,000, then
  `pageInfo.hasNextPage` goes false while `issueCount` still says
  6,090. Truncation arrives as a clean end-of-pages, not an error —
  the client sees a normal `complete` page and a truthful count.
- **Result nodes carry the row's fields.** `type: ISSUE` search
  returns `SearchResultItem` union nodes; `... on PullRequest`
  fragments expose the same fields the repo-view PR rows render
  (number, title, draft, author, updatedAt, url,
  statusCheckRollup, labels, reviewRequests). Field-for-field parity
  with the repo PR selection, checked against the pinned IDL, is an
  implementation gate so `gql::PullRequest` parses unchanged.
- **Cost envelope.** A first page = 1 request = 1 point. Walking a
  6,090-PR org to the 1,000 cap = 10 points ≈ 0.2% of the 5,000/hr
  budget. A steady-state refresh (first page) = 1 point.

## 3. Design

### D1 — Home replaces the placeholder

`render_right_column`'s `None` (no repo selected) arm renders the
home view instead of the "Select a repository" placeholder; the
placeholder string disappears from the module. Repo selection keeps
taking the column over (existing behavior, untouched); deselection
returns home. Left column (org select + repo list) unchanged.

### D2 — One account-scoped lane, snapshot on the state

New `Kind::AccountPullRequests`: a GraphQL POST search lane, `ident`
= account login, variables `{ login, scope: "org" | "user" }`.
Results land on `GithubState` as a snapshot keyed by
`(login, scope)` — the repo-list pattern (0009 D6):

```text
home_prs: Option<HomePullRequests {
    login, scope,
    nodes: Vec<gql::PullRequest>,   // search order (updated desc)
    total_count: i64,               // issueCount — full match count
    complete: bool,                 // no further pages (cap or end)
}>
```

with an `updated_ms` stamp for the freshness cluster (0033/0037).
The home view shows its own cluster when active — one cluster per
page rule holds; the repo view keeps its own.

### D3 — Fetch policy

Fetch fires when the module opens (after orgs resolve and the
effective account is known — 0009 D6) and on every account switch.
Auto-refresh follows the list-surface policy (0035: engaged-list
interval, idle pause). GraphQL POSTs don't etag — a refresh is a full
first-page refetch, deduped by the 0031 inflight machinery
(kind + ident); "Load more" pages (D4) are separate one-shot requests
outside the refresh cycle.

Failure semantics: a failing refresh keeps the last snapshot and
surfaces the retryable in-form error idiom (fetch errors are
retryable by design, never autofail). A failing first-ever fetch
shows error + Retry in place of the list. No fetch ever blocks the
left column.

### D4 — Paging to the cap

Open fetches the first page (100). A "Load more" button appends the
next page (100/request) while `!complete`. The hint footer uses the
0044 §9 idiom, in three states:

- `fetched < total_count`, under the cap: "first N of M open pull
  requests";
- at the 1,000 cap with more matching: "first 1,000 of M — search
  caps at 1,000" (the cap is GitHub's, the copy must say so);
- `complete`: no hint.

`complete` = server said no next page (which at a full cap is the
verified end-of-pages behavior, §2).

### D5 — Rows and filters reuse

Rows render through the existing `render_pr_row` and filter through
`filters::apply_prs` (0042 engine) over the held snapshot nodes — no
new row type, no new filter plumbing. One render difference: repo-view
rows omit the repository (it's the page's context); home rows span
repos and must show an `owner/name` prefix chip parsed from the node
key (`owner/name#number`), the same idiom the notification rows use
for key parsing. Filter-menu options derive from the present nodes
(0042 D3), so a truncated home list only offers filters it can apply.

The PR filter section (`filters.prs`) and the search box apply to
whichever PR list is visible — home and repo view share the control
set and the state. A repo's rows and the home rows never show at once
(D1 exclusivity), so a shared filter state cannot act on two lists
at once, and a filter set while looking at one surface still applies
when the other surfaces — consistent with 0042 treating filters as
page-level, not surface-level.

### D6 — Degraded mode: the snapshot union

While the search lane has never landed (loading or first-fetch
failure) but the repo snapshot holds data, the home view renders the
**union of the repo snapshot's PR nodes** for the effective account's
repos, re-sorted by `updated_at` — zero extra requests, the same
`PullRequest` shape, marked honestly:

- the freshness cluster shows the *repo snapshot's* age (the union's
  true provenance);
- when any contributing repo's `total_count` exceeds its node count,
  or the repo list itself is truncated, the hint reads "partial —
  from repo snapshots".

Once the search lane lands (or later succeeds), the real snapshot
replaces the union. This keeps home from ever showing a blank column
when good data is already on disk (the D6 conservatism shared with
0004 §4.7 and 0044 §9: degradation must never invent a failure state
that the data doesn't justify).

The union is a fallback only, not the primary mechanism: it can't
give account-wide totals, its ordering is inherited from per-repo
caching, and its freshness would be coupled to repo-list refreshes —
each of those alone justifies the search lane.

### D7 — Row behavior parity

Click behavior identical to repo-view PR rows (browser; the deep-nav
gap #97 applies equally here — no new navigation invented). No
compose/review actions beyond what repo rows already carry. Labels,
CI signal, draft marking, review-request chips: inherited by
construction (same node type → same renderer).

### D8 — Empty and loading states

- No open PRs for the account: empty state ("No open pull
  requests") + freshness cluster — a real result, not a loading row.
- First-ever fetch in flight: spinner row, the repo list's loading
  idiom.
- The `None`-placeholder arm no longer exists, so there is no state
  in which the right column says nothing.

### D9 — Account-switch semantics

Switching the selector shows the target account's cached snapshot
immediately if present (stale-while-revalidate marking per
0032/0034) and refetches; snapshots are keyed by `(login, scope)`,
so alternating between an org and the personal account doesn't
thrash. Pending "Load more" pages belong to the account that issued
them — a switch drops the button, not the landed nodes.

## 4. Implementation plan (one PR, after this doc merges)

1. `gql.rs`: `ACCOUNT_PULL_REQUESTS_QUERY` (search + PullRequest
   fragment + `rateLimit` selector, `$scope` discriminator),
   parser reusing the `PullRequest` node shape + `total_count`,
   `complete` from `pageInfo`; shape-test entry in the
   no-JS-comment sweep.
2. `service.rs`: `Kind::AccountPullRequests` run arm (GraphQL POST,
   ident, inflight dedup), snapshot landing + 0035 list auto-refresh
   wiring, no etag.
3. `page.rs`: home arm in `render_right_column`; `load_home_prs` /
   `load_more_home_prs`; union fallback (D6); hint footer (D4);
   `owner/name` chip in the home-row path; freshness cluster.
4. Tests: fixture search payloads (two pages + a capped account),
   probes — render-on-open, account switch, load-more + cap hint,
   empty, first-fetch failure → Retry, refresh failure keeps
   snapshot, union fallback renders and is replaced; parse tests.
5. Docs: README index row (same PR), 0029 §3 amendment pointer.

## 5. Testing

- gql parse: union-fragment nodes land as `PullRequest`;
  `issueCount` → `total_count`; `pageInfo` → `complete`.
- Probes (TestAppContext, injectable/zero delays where timers
  apply): home renders on open with orgs resolved; account switch
  swaps snapshots without refetch flicker; load-more appends and
  stops at the cap with the cap hint; empty account shows the empty
  state; first-fetch failure shows Retry and recovers; refresh
  failure keeps the last snapshot (in-form error); union fallback
  renders from repo snapshots with the partial hint and is replaced
  when the lane lands; repo selection hides home, deselection
  restores it.
- Filters: `apply_prs` narrows home rows; menu options derive from
  present nodes.

## 6. Debug-bridge verification (docs/0010)

Exercise through the bridge before pushing: open the module with no
repo selected → `tree` shows home rows and the hint; `click` a row →
browser opens the PR (manual); switch accounts in the left select →
rows swap; select a repo → home hides; deselect → home returns with
its cached snapshot; exercise a filter preset against home rows.

## 7. Acceptance criteria

- With orgs loaded and no repo selected, the right column lists the
  account's open PRs after one page-fetch; rows are
  renderer-identical to repo PR rows plus the `owner/name` chip.
- Filters and the search box narrow the home list exactly as they
  narrow a repo PR list.
- "Load more" appends to the 1,000 cap; the hint is truthful in all
  three states (D4).
- Freshness cluster + auto-refresh behave per 0033/0035/0037; a
  steady-state refresh costs 1 point.
- Account switches render cached snapshots immediately (D9); repo
  selection/deselection toggles home without refetch on every toggle.
- The union fallback (D6) covers lane failure with honest provenance
  marking; the right column is never blank and never says
  "Select a repository".

## 8. Human checks (owner)

- [ ] Row density of the home list next to the left repo list feels
      right; the `owner/name` chip reads clearly.
- [ ] Hint copy in the truncated and capped states reads honestly.
- [ ] Empty account state and union-fallback state look right.
- [ ] Filters and search visibly narrow the home list.

## 9. Follow-ups (GitHub issues, not this PR)

- Beyond-cap coverage for very large accounts (query splitting by
  repository or date windows) — the 1,000 cap is real (§2); v1 is
  honest about truncation, not complete. Filed if this doc merges.
- Grouping by repository and sort overrides beyond the 0042 engine
  defaults.
- A symmetric issues view on home ("all open issues").
- Review-queue presets (requested-changes/awaiting-review filters)
  — pairs with the #281 composer arc.
