# 0072 — GitHub data store: one shared PR/issue cache, stale-while-revalidate reads, optimistic mutations

- **Status:** design PR (docs-only)
- **Tracked by:** implementation issue (filed at design merge)
- **Trigger case:** owner request — the module's UX trails github.com, mostly
  because a slow GraphQL round-trip sits between every intent and every pixel.
  The owner wants the module to behave like github.com: one shared store that
  every fetch and every client-side write updates, mutations that run in the
  background with a toast on completion, and screens that repaint from the
  store instantly — including going back to a list that already reflects a
  mutation confirmed seconds ago, before any refetch could land.

## 1. Summary

Three moves, one direction: data stops living in pages and starts living in
the store.

1. **A normalized PR/issue document store** inside `GithubState` (the module's
   one gpui entity, docs/0005) keyed by `(repo, kind, number)`. Home rows
   (summaries) and PR-page payloads (details) become two freshness layers of
   the same document; every fetch lands in it and every page reads from it.
2. **Stale-while-revalidate reads.** Opening a PR paints the cached summary —
   and cached detail, when one exists — instantly, then revalidates in the
   background. Skeletons only for cold documents.
3. **Optimistic, store-scoped mutations.** Clicking merge flips the document
   to a pending state — home row and PR header repaint immediately,
   everywhere; the GraphQL call runs in the background; success applies the
   confirmed state and raises a toast; failure keeps the previous state and
   surfaces the error inline plus a toast. The user can navigate anywhere the
   instant they click.

This design **supersedes docs/0016 D2's "no cache" non-goal** (§2.1, §8) and
generalizes the one instant-paint trick it did ship — handing the clicked
summary row to the page — into the store itself.

## 2. Facts & constraints

### 2.1 Current data flow — verified in tree 2026-10-14

- `GithubState` (service.rs) is already the module's central entity: home
  summaries, notifications, repo inventory, freshness bookkeeping. It is
  **not** page-local — pages hold `Entity<GithubState>` and read it.
- Fetches run through the `FetchSpec` pipeline: `run_spec` /
  `run_spec_tracked` / `run_read_deduped` → results land via
  `GithubState::apply(SpecResult)` (service.rs), which stamps the per-lane
  freshness (`snapshot_age`, `repo_rows_landed`, …). In-flight spec runs are
  the docs/0013 status-bar indicators; reads are deduped
  (`run_read_deduped`).
- `PrDetailPage` (pr_page.rs) holds `summary: gql::PullRequest` — "the
  snapshot row that opened the page — instant header paint while the detail
  query is in flight" — plus `detail: Fetch<gql::PrDetail>`, which is
  **page-local**: fetched fresh on every open, dropped when the page closes.
  Reopening the same PR 40 seconds later pays the full GraphQL round-trip
  again behind a skeleton.
- Mutations are modal: `merge_pr`/`close_pr`/… set `busy: Option<BusyOp>`
  ("one at a time; every write surface disables while set"), await the
  GraphQL confirmation, then trigger the detail refetch; only then does the
  header state change. `mutation_error` carries failures to the surface that
  raised them (docs/0016 D4/D5).
- Home list rows are `gql::PullRequest` summaries (`HomePrsPage`); docs/0016
  §3 records that detail fields "are not in any payload we hold — they must
  be fetched when the page opens."
- docs/0016 D2 (shipped 2026-09-03) decided: one detail query on open +
  refetch after each mutation, **"no cache — the snapshot cache, or any new
  cache"**, and no cadence change. Correct for a stage-4 page; it is the
  ceiling this design removes (§8).

### 2.2 GPUI doctrine — gpui-kit coding guides, verified 2026-10-14

- State ownership: "Put each state in the narrowest owner that can keep it
  correct … **shared application services may be stored as GPUI globals**."
  The named failure mode is "one entity containing the entire application's
  **unrelated** state" — a domain-scoped store is the sanctioned shape, and
  this one is module-scoped and domain-pure.
- Async work: "Represent async operations with explicit states … **Preserve
  usable previous data during refresh when possible** … attach a revision or
  identity and reject stale work rather than applying it to new state."
- async.md pitfalls: never update entities from inside a background task;
  spawn a foreground `cx.spawn`, await the background work, and mutate
  through the entity context on landing.

### 2.3 Infrastructure that already exists — build on it, don't rebuild it

- `run_read_deduped` single-flight reads; docs/0013 in-flight tracking (one
  spec run = one indicator).
- `GithubCache` — disk JSON + ETag map (snapshot/notifications lanes).
- Toast hub (toasts.rs): `raise/info/success/warning`, `dedupe_key`,
  `source`, persisted notification log, spill for pre-hub toasts.
- `app_db` (docs/0063): SQLite, WAL, dedicated writer thread, additive
  `user_version` migrations, degraded mode — the *phase-2* landing spot (D8).
- `AuthGate` single-flight 401/refresh with FIFO retry (docs/0028) —
  mutations already survive token refresh transparently.

## 3. Decisions

### D1 — Store shape: a document map in `GithubState`

```rust
struct PrKey   { repo: String, number: i64 }        // Kind reserved: Kind::Pr | Kind::Issue
struct PrEntry {
    summary: gql::PullRequest,                      // home-list layer, always present
    detail:  Option<gql::PrDetail>,                 // page layer, lands on first open
    summary_at: Instant,
    detail_at:  Option<Instant>,
    pending: Option<PendingOp>,                     // D4
}
struct PendingOp { kind: PendingKind, token: u64, started_at: Instant }
```

`pulls: BTreeMap<PrKey, PrEntry>` on `GithubState`. The key *is* the
identity the guides demand: results can only ever land on their own
document. `BTreeMap` keeps renders cheap and ordered; nothing here is
persisted (D8). Issues join via `Kind::Issue` in stage 4 with the same
machinery.

### D2 — One write path, stamped once

Every landing goes through new `GithubState` methods — `land_summary`,
`land_detail`, `begin_pending`, `resolve_pending`, `fail_pending` — each
stamping its timestamp and notifying **once**. The home lane's
`apply(SpecResult)` lands summaries; the PR page's detail continuation lands
details into the same document. The page-local `Fetch<gql::PrDetail>` dies:
`PrDetailPage` reads its entry from the store and subscribes to
`GithubState` for repaints. Freshness bookkeeping exists in exactly one
place, and the dedup/in-flight instrumentation keeps working because the
spec runners are unchanged.

### D3 — Reads are stale-while-revalidate

`open_pr` paints whatever the entry has — summary always (home landed it),
detail when cached — and kicks the detail spec in the background only when
the document is cold or stale (D9). `run_read_deduped` already collapses a
double-open. Skeletons appear only on cold documents. The lazy lanes keep
their own behavior (docs/0016 stage 4/5: Files REST walk, checks pane) —
they render from the landed detail as today, just out of the store instead
of out of the page.

### D4 — Optimistic, store-scoped mutations

Merge click: server-truth gates checked as today (`mergeable`, `draft`,
state), then `begin_pending(entry, Merge)` — and the UI *everywhere* that
renders that entry repaints from the store: the home row grows a muted
"merging…" pill, the PR header button reads "working…" (the docs/0066 D5
wording) with a spinner. The GraphQL call proceeds through the existing
`spawn_mutation` continuation, and the continuation resolves **against the
state entity, never the page that armed the op** — the resolver is
session-scoped, so closing the PR page or navigating away mid-flight
cannot orphan a pending document (the resolution is exactly what dies
when a page-routed continuation loses its entity):

- **success** → apply the confirmed fields to the document (`state = MERGED`,
  merged timestamp, …), clear `pending`, raise `toasts::success` with
  `dedupe_key: "pr-merged:{repo}#{n}"`, `source: "github"`;
- **failure** → keep the previous document exactly as it was, clear
  `pending`, record `MutationError` on the entry (the inline error surface
  stays where it is — docs/0016 D5) and raise `toasts::warning`.

`BusyOp`/`MutationError` move to store scope (the page keeps only
presentation). Because pending lives on the *entry*, not the page, the user
can navigate anywhere the instant they click — exactly the owner's scenario:
merge, jump home, see the row flip when the confirmation lands, with no
home refetch in between.

**Hardening (shipped with the implementation, after a field report):** a
pending op whose confirmation never lands — the arming page dropped
mid-flight, or GitHub went silent — unblocks itself: the tick loop runs
`expire_stale_pending` each beat, and a document pending longer than
`PR_PENDING_MAX_AGE` (120 s) returns to its prior state with the inline
surface explaining on next visit. **No toast** — an expiry does not know
that the op failed upstream, and a "failed" toast would lie; a late
confirmation is refused by its gone token (D6), so a slow-but-alive
GitHub merge can never apply into an expired document uninvited. The
success path's background revalidate (below) only flies while the arming
page is still alive; a dead page's document revalidates on its own
freshness gates at next open (D3).

### D5 — Concurrency: one pending op per entry, many entries in flight

The single-flight constraint narrows from "the whole page" to "the one
document" — the guides' duplicate-destructive-submission guard, kept.
Different PRs mutate concurrently (the status bar already renders `×n`).
A second mutation on a pending entry renders as the disabled state it
already does.

### D6 — Stale-work rejection

Every `PendingOp` carries a store-issued `token`; a confirmation applies
only if the entry's pending token matches. Fetch landings apply only to a
living entry (key identity). Entries with pending ops are pinned against
eviction (D9) so a slow mutation can never confirm into a void — the token
check stays as belt-and-braces.

### D7 — Toasts are the away-channel, raised always

Completion and failure of every mutation raises a toast, even when the user
never left the page. Rationale: the inline surface remains the detailed
story (docs/0016 D5 unchanged); the toast is cheap, dedupe-keyed, and
consistent — "merge finished" should not depend on where you were standing.
Level `success`/`warning` per D4.

### D8 — Persistence is deferred, not forgotten

Entries are session-scoped memory. The literal "local in-app DB" — an
`app_db` table via the additive `user_version` chain, seeded at startup the
way `GithubCache` seeds snapshots — is the phase-2 extension and gets its
own issue. Deliberate: persistence drags in staleness-on-restore, pruning,
and migration questions this design does not need solved to ship the UX
win, and `GithubCache` already covers the cold-start lanes that matter
today.

### D9 — Bounds, TTLs, eviction

- Detail bodies are the heavy part: cap the map at 64 documents with
  detail; evict oldest-landed details first (summaries track the home list
  and are naturally bounded by it). Pending entries are pinned (D6).
- SWR staleness for a detail: 5 minutes to start, tuned on receipts — the
  tick loop (docs/0013: 60 s ticks) keeps *lists* fresh; this TTL only
  decides when a *reopen* revalidates in the background.
- `sign_out` clears the map (it already clears session state).

### D10 — Lanes unchanged

Notifications lane, tick cadence, rate-limit pause, docs/0013 tracking, and
the snapshot/ETag lanes keep their behavior. The store is a new consumer of
the existing fetch pipeline, not a new pipeline.

## 4. UI spec

- **Home PR row** — existing states plus a pending pill when the entry has
  one: "merging…" / "closing…" / "updating branch…" (label from
  `PendingKind`), muted tone, non-interactive (click still opens the page,
  which shows the same pending state).
- **PR header** — while pending: primary button label "working…" + spinner
  (docs/0066 D5 wording), sibling write buttons disabled; tabs, links, and
  navigation all stay live — pending never blocks leaving.
- **Inline errors** — unchanged placement and copy (docs/0016 D5); the
  failure toast carries a one-line summary and points back at the surface.
- **Toast copy** — success: "Merged #482 · owner/repo"; failure:
  "Merge failed · #482 owner/repo — {error}".

## 5. Verification

- **Unit** (service.rs test pattern): land_summary/land_detail freshness
  stamping; begin/resolve/fail pending round-trip; token mismatch rejected;
  eviction skips pinned entries; sign_out clears.
- **UI integration** (gpui tests, page.rs/pr_page.rs pattern): reopen with a
  cached detail paints no skeleton; merge flips the home row instantly on
  store notify, before any list refetch; confirmation applies MERGED with
  no refetch; failure restores the prior state and records the inline
  error.
- **Bridge receipts** (implementation PR): live session — click merge,
  navigate home within a second, assert the pending pill in `tree`; assert
  the completion toast in `logs`. Human merge test on a scratch repository.
- **Gates** as always: `cargo fmt --all --check`, `cargo clippy --workspace
  --all-targets`, `cargo test --workspace`. No layout change is intended;
  if the golden drifts structurally, that's a bug in the implementation.

## 6. Stages

1. **Store + SWR reads** — D1–D3, D9: document map, land paths, pr_page
   reading the store, no skeleton on warm opens. Owner-visible win #1:
   reopening PRs is instant.
2. **Optimistic merge / close / update-branch** — D4–D7: pending pills,
   background completion, toasts. Owner-visible win #2: mutations stop
   being modal.
3. **Remaining mutations** — comment, review submit, description edit,
   timeline row writes, line comments/replies onto the same machinery
   (append-style confirmations land via the detail refetch as today).
4. **Issues** — `Kind::Issue` documents for issue_page/new_issue_page.

Stages 1–2 are the design's core; 3–4 are mechanical follow-ons on the
same rules and can split into their own issues.

## 7. Non-goals

- **Persistence** to `app_db` (D8 — deferred with its own issue).
- Live updates of any kind — no websockets, no polling cadence changes
  (D10).
- Full entity normalization (separate user/comment/timeline tables) —
  documents stay whole payloads; the dedupe and memory wins don't need
  more.
- Background prefetch of details the user never opened.
- Notification-lane, rate-limit, or auth-gate changes.
- Agent-bridge (docs/0068) exposure of store freshness — worth an issue
  later, not here.

## 8. Superseded decisions

- **docs/0016 D2 — "no cache":** superseded by this design. The
  implementation PR adds a pointer note to 0016 §D2 so the rationale trail
  stays honest: the no-cache rule was correct for a single fresh-open page
  and is wrong for a module that behaves like a client for a live backend.
- docs/0016 D4/D5 (modal mutations, inline errors) remain in force for
  *error presentation*; their *blocking* behavior is what D4 removes.
