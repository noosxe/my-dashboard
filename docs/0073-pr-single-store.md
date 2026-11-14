# 0073 — PR single store: one copy of every PR fact

Date: 2026-10-14
Status: design (docs-only PR)
Follow-ups: implementation issue(s) to be filed after merge

## 1 — Problem

Three field reports in one week (#559 stuck pending, the row/page checks
divergence, #561 the open list ignoring a confirmed merge) shared one root:
**the same PR fact lived in more than one place, and every writer had to
remember to update all of them.** Each fix made one writer honest; none made
the next writer structurally unable to lie.

The copies that exist today (PR-scoped inventory):

| # | Holder | What it copies | Refreshed by |
|---|--------|----------------|--------------|
| 1 | `GithubState.pulls` (docs/0072) | the document: summary, detail, pending, books — **canonical** | landings + confirmations |
| 2 | `GithubState.home_prs[*].nodes` | full `gql::PullRequest` rows per lane | lane landings only (the #561 bug) |
| 3 | `PrDetailPage.summary` | a `gql::PullRequest` clone taken at page construction ("instant header paint") | never |
| 4 | repo-view PR row list | repo-scoped open-PR lane (exact field to be pinned by the stage-2 audit) | repo lane landings |

gpui gives us entity-granularity reactivity only (verified against the
pinned `gpui-pre 0.3.9`: no signals/memo/derived anywhere; the one
ecosystem candidate has 6 stars). Cross-surface consistency is therefore
**our job** — and the architecture must make it structural instead of
remembered.

## 2 — Goal

One store holds every PR ever fetched. Views **subscribe to the store** and
**derive** their presentation — filter/map over store contents at render
time — instead of holding their own copies. Fetch results and mutation
confirmations **enrich the store** through its methods; nothing outside the
store may hold a PR fact.

In scope: the PR domain (summaries, details, pending ops, lane indices).
Out of scope: §5.

## 3 — Design decisions

### D1 — One copy per fact

`pulls: BTreeMap<PrKey, PrEntry>` remains the only structure that stores
PR facts. A fact (a summary field, a detail, a pending op, a book stamp) is
written exactly once, at its landing site. No other structure may hold a
`gql::PullRequest` or `gql::PrDetail` value — including page fields and
lane rows. Enforced by the stage-2 audit + a lint-style test that greps the
module for fact-typed fields outside the store (see D8).

### D2 — Lanes become indices, not copies

`HomePullRequests { nodes: Vec<gql::PullRequest>, … }` loses `nodes`. A
lane keeps only what is *not* PR data:

```rust
pub(crate) struct HomeLane {
    /// Server order — the identity of "what this lane shows", in order.
    pub order: Vec<PrKey>,
    pub total_count: i64,
    pub complete: bool,
    pub cursor: Option<String>,
}
```

`apply()`'s lane landing still lands every row via `land_summary` (already
true since docs/0072 D2) and then records the **key order**. Replace vs
append ("Load more") semantics carry over unchanged, operating on keys.

### D3 — Views derive: filter/map over the store

The Open PRs view renders a **selector**: lane index ⊗ store entries, e.g.

```rust
impl GithubState {
    /// docs/0073 D3 — the open list derives from the lane's index and
    /// the documents: server order, filtered to OPEN at read time.
    pub(crate) fn lane_rows<'a>(&self, login: &str, scope: HomeScope) -> Vec<&'a PrEntry>
}
```

The `state == OPEN` filter (#561's local mirror of `is:open`) becomes the
**definition** of the view, not a patch — a confirmed merge drops the row
the moment the entry changes, because there is no second copy to go stale.
The quick-filter toolbar, hint bookkeeping (`shown` = filtered count), and
"Load more" all map over the same selector. Entries missing from the map
(evicted, sign-out) are skipped defensively at read time.

### D4 — Enrichment goes through the store

Every fetch that returns PR data lands it via a store method
(`land_summary`, `land_detail`, future lanes likewise). Page structs hold
no PR facts: `PrDetailPage.summary` dies — the page already paints the
header from the entry (docs/0072), and the entry always exists when the
page is opened from a row (the lane landed it). The snapshot's only job
("instant paint while the detail query flies") is subsumed: the summary is
already in the store before the page can exist.

*Summary-layer rules (#873, #874; `pr_summary.rs`):* the detail landing
refreshes every summary field the detail also carries (state, title,
draft, labels, review requests, `updatedAt`), so a merge or label edit seen
on the page reaches the list rows. A list landing no newer than the held
summary by `updatedAt` keeps the held descriptive fields and takes only the
rollup and URL (a state change always moves `updatedAt`, so an equal stamp
with another state is the older read; CI moves without touching it). A
synthesized row (the notification-subject open) seeds only the fields the
held summary lacks and never overwrites a landed one.

### D5 — Subscription is entity-observe + selectors

Views keep observing the store entity (`cx.observe`, the docs/0072
pattern); a change re-renders the view, whose selectors re-derive from the
store. Per-object subscriptions are not attempted — gpui has no
fine-grained reactivity to hang them on (§1), and at dashboard scale
(tens of rows per view) re-deriving a selector per frame is unmeasurable.
Escape hatch, out of scope here: if a surface ever measures hot, a
derived-snapshot helper (observe + compare) is additive and local.

### D6 — Confirmations write once

#561's rule ("confirmed state is a shared fact") becomes structural: the
confirmation mutates the entry, and because no other copy exists, every
surface that reads the entry — lane view, page header, merge rail — is
correct by construction. The lane-row patch from #561 is deleted in stage 1
(the thing it patched no longer exists).

### D7 — Existing books carry over

Detail TTL + `PR_STORE_DETAIL_CAP` eviction, `rollup_dirty` drift marks,
`PR_PENDING_MAX_AGE` expiry, token-guarded confirmations, sign-out clears:
all unchanged, all still living on the entry. The freshness clocks keep
their docs/0037/D2 contracts; the lane's `home_landed` stamp stays lane
bookkeeping.

### D8 — The journey is pinned

The seam that produced all three reports gets a permanent store-level
integration test: land a lane, open-derive the list, arm + confirm a
merge, assert the derived list drops the row **and** the document shows
MERGED — no landing in between. The fact-copy audit (D1) runs as a test
that fails when a PR-fact-typed field appears outside `service.rs`'s
store.

## 4 — Staged implementation

1. **Stage 1 — the lane loses its copies.** ✅ Shipped: `HomeLane` index
   (D2), the `lane_view` selector + Open PRs view switch (D3),
   `PrDetailPage.summary` removal (D4 — the page reads the store's
   entry; `open_pr` lands the row before the page exists), #561's
   lane-row patch and local `retain` filter deleted (D6), and the
   journey test (D8) pinning the merge→open-list seam.
2. **Stage 2 — the audit.** ✅ Shipped. Every holder enumerated, each
   folded into the store or dispositioned:

   | Holder | Disposition |
   |--------|-------------|
   | repo snapshot `prs.nodes` (`gql::Repo`) | **cache exception, documented** — the snapshot is the repo domain's persisted cache (docs/0007 D6); every landing now also lands its rows into `pulls` (D4), and the repo view + the agent `repo_prs` op read the **derived overlay** (`GithubState::repo_pr_rows`): entry wins, OPEN-filtered — a confirmed merge leaves the repo list on confirm |
   | `GithubPlace::Pr(String, PullRequest)` | **slimmed to `(String, i64)`** — the place carries identity; restore re-opens from the entry (summaries persist for the session) |
   | `open_pr`'s `pr: &gql::PullRequest` param | **kept** — an enrichment handoff (D4): the notification-subject path synthesizes a minimal row and lands it; messages are not holders |
   | `new_pr_page::Created(Box<PullRequest>)` event payload | **kept** — same rationale; landed by the handler |
   | agent `repo_prs` op | reads the derived overlay now (was raw snapshot rows) |

   The D1 enforcement test (`no_pr_fact_fields_outside_the_store`) fails
   the build when an owned `gql::PullRequest`/`gql::PrDetail` field
   appears outside the store's module — the regression this family
   shipped three times cannot re-enter silently.
3. **Stage 3 — selective follow-ons** (each judged on its own merits):
   derived-snapshot sugar if a hot path appears; extending the pattern to
   other domains via their own docs.

Each stage ships green gates, the receipt chain, and its docs/0072
supersession notes inline.

## 5 — Non-goals

- Notifications, issues, releases, repos: the same single-store pattern is
  likely right, but each domain gets its own design (its freshness and
  identity rules differ). Filed as deferred issues at implementation time.
- `app_db` persistence of the store (docs/0072 D8 / #556) — unaffected.
- Fine-grained reactivity (signals/memo) for gpui — a framework-level
  question (§1, D5); we adopt nothing until the ecosystem matures.
- Changing any GraphQL query, pagination behavior, or UX flow — this is a
  state-architecture refactor; the Open PRs view must look and behave
  identically to today (except the already-shipped #561 correctness).
