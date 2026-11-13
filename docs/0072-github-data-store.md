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
spec runners are unchanged. `land_detail` is also where a read that
predates a confirmed write is refused (D4, #739).

### D3 — Reads are stale-while-revalidate

`open_pr` paints whatever the entry has — summary always (home landed it),
detail when cached — and kicks the detail spec in the background only when
the document is cold or stale (D9). `run_read_deduped` already collapses a
double-open. Skeletons appear only on cold documents. The lazy lanes keep
their own behavior (docs/0016 stage 4/5: Files REST walk, checks pane) —
they render from the landed detail as today, just out of the store instead
of out of the page.

**The detail read's result lands on the store, not the page (#743).** The
flight record (`books.in_flight`) lives on the document, so whoever armed
the read, the result clears it: the page that dispatched folds the landed
detail into its own caches too, but a page dropped mid-flight (Esc) no
longer takes the result with it. The spawn lands or fails the read on the
state entity regardless (`pr_page/detail_read.rs`), the same
session-scoped rule as D4's resolver. A reopen during the flight is
skipped by the shared dispatch guard and picks the landed detail up from
the store. The Files and Threads reads are page-owned records (they die
with the page; the shared guard clears either way), so they are not
affected. The issue page follows the same rule (#806,
`issue_page/detail_read.rs`): its detail result lands or fails the entry's
flight record whether or not the page that armed it is alive. The one
result the store refuses is a read older than a confirmed write (D4,
#739): its flight record still clears, page or no page.

**The lane holds an index, not rows (docs/0073):** the home lane's row
copies (`HomePullRequests::nodes`) were the third surface holding PR
facts — the one that let a confirmed merge go unnoticed until the next
landing (#561). Superseded: lanes record server key order + pagination
books (`HomeLane`), and the open list *derives* from the documents
(docs/0073 D3). D4's "confirmed state is a shared fact" is structural
now: the confirmation writes the entry, and every surface reading the
entry — including the derived list — is correct by construction.

**The rollup is a shared fact (shipped hardening, after a field report):**
both layers carry `statusCheckRollup.state`, and the two lanes refresh at
different cadences (summary: the 60 s home lane; detail: this TTL) — which
let a row show checks passing while its page still showed them running.
The layers now propagate in both directions: `land_detail` writes the
fresh rollup into the summary layer on the spot (the row's glyph never
waits for the next home tick; a degraded `None` never blanks a good
glyph), and `land_summary` that *moves* the rollup marks the entry
`rollup_dirty` — which voids the SWR skip (reopen revalidates) and makes
the open page revalidate in the background via its store subscription
(`Trigger::Drift`). Unchanged landings never mark, so the common tick
case keeps the full TTL; only the catch-up landing clears the mark.

**The catch-up fires at most once per arming (#731).** The trigger is the
page's own store subscription, and a failed read notifies the store too,
so the catch-up carries its own gates. Any detail read dispatched while the
mark is armed spends that arming, including the reopen read and a manual
refresh. A failed read keeps the mark, so a reopen still revalidates and
the row keeps the fresher glyph, but the page does not retry on its own.
The read recovers through a manual refresh, a reopen, the refocus
trigger, or a landing that moves the rollup again, which re-arms the
catch-up. An unspent arming also waits for the read lane's failure backoff
(docs/0036 D2) and for the detail lane's gate: signed out, or a paused
GraphQL budget (docs/0050 D4, per budget since docs/0082 Q10). A held
arming is not spent, and there is no timer: the first store notification
past the gate fires it.

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

**Confirmed state is a shared fact (shipped hardening, after a field
report):** the confirmation lands its deterministic post-state into
*both* store surfaces — the document (summary + detail) and the home
lane row (`home_prs`) — so the open-PRs list reflects the merge/close
the moment it confirms, not when the next lane landing arrives. The open
list filters non-`OPEN` rows locally, mirroring its own `is:open`
search; store-side lane rows stay put so "Load more" keeps appending
against the unfiltered chain. The next lane landing reconciles the rest
with server truth (merged metadata, ordering, total counts).

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

**Stage 3 (#554) — the conversation writes ride the same machinery.**
Comment/review submit, description edit, the docs/0019-D5 timeline-row
writes, and the docs/0030-D7 Files-tab writes arm through
`begin_pending` like the header ops; `PendingKind` carries their pending
labels (the home-row pill reads "commenting…" the same way it reads
"merging…"), their toast copy, and the inline scopes they had as
page-modal ops (Timeline / Files — the docs/0016 D5 placement is kept,
so store failures render exactly where page-modal failures did). Only
merge/close patch a post-state into the document; every other
confirmation patches nothing (a new comment row or an edited body is
only learnable by refetch), so it **voids the cached detail's
freshness** (`detail_at = None`): an open page refetches through its
arming hook (threads refetch for the Files-tab writes), and a page that
died mid-flight revalidates on next open instead of serving a document
that predates the confirmed write. Write surfaces disable while the
entry has any op in flight (D5), the submitting surface shows its own
spinner, and confirmation runs the surface-closing the page-modal path
did (draft cleared, composer closed — a failure keeps the draft). The
page-modal ops outside this stage (#212 thread-comment edit/delete,
resolve toggles, rail metadata) keep their old path and old guards,
except that the ones that change the detail now confirm on the store
(#807, below). The issue page's stage-4 pass
(#555) moved ITS whole set — including the reaction/lock toggles — onto
the same machinery.

**A read older than a confirmed write never lands (#739).** A write's
refetch shares the dispatch guard with every other read, so it is skipped
while one flies. That earlier read may not show the write, and landing it
would hide the comment (or flip a patched merge back to `OPEN`) and stamp
the document fresh, cancelling the void above. So a read is marked
outdated (`books.read_outdated`, `pr_page/write_refetch.rs`) when a write
confirms during its flight (`resolve_pending`) or when a write's refetch
hits the guard (the page-modal writes, which never reach `resolve_pending`).
A failed store write marks nothing.

- **It does not land.** `land_detail` closes the flight, drops the body,
  and voids `detail_at`, so nothing claims freshness. The document keeps
  what it had. This holds with the page gone.
- **Exactly one read replaces it,** once the flight has landed or failed.
  Any dispatched read clears the mark, whichever trigger sent it, so a
  failing replacement cannot re-trigger itself. The replacement goes out
  as `Trigger::MutationRefetch` and is not held by the failure backoff, as
  no write's refetch is, but it does wait for the lane's gate (docs/0050 D4):
  a held mark fires on the first store notification past the gate, the
  same as a held catch-up (D3). It is also the read a drift catch-up would
  have asked for, so the two never add up to a third.
- **No page, no replacement.** The voided stamp makes the next open
  revalidate. A page reopened during the flight is skipped by the guard
  and picks the replacement up through its store subscription.
- **Threads follow the same rule on the page.** The threads record is
  page-owned, so its mark is too, and it is set when a write's threads
  refetch hits the guard. An outdated threads body does not land, and one
  read replaces it. A flight left by a dropped page notifies the store when
  it ends, so a reopened page that owes the replacement wakes up.
- **The issue page follows the same rule (#806).** `land_issue_detail` /
  `resolve_issue_pending` mark and refuse exactly like their PR twins, a
  confirmed close or reopen patch is not overwritten by an older body, and
  `issue_page/write_refetch.rs` queues the one replacement read. The issue
  page has a single read lane (comments and timeline ride the detail
  query), so there is no threads-style second record to guard.
- **Manual refresh is unchanged.** It still rides out a flying read
  (docs/0031 D1/D2): no write made that read outdated. The Files lane has
  no write refetch (only merge/close change patches, and those end the
  page's useful life), so a write never marks it. A head change does
  (#752, below).

**Page-modal detail writes confirm on the store (#807).** Labels,
assignees, reviewers, milestone and reactions arm `busy` on the page,
not `begin_pending`, and used to resolve only through the page. A page
dropped mid-write took the confirmation with it: nothing voided
`detail_at`, and a reopen inside the TTL served the pre-write document
as fresh. The spawn (`pr_page/page_write_confirm.rs`) now hands the
result to the state entity first. A clean 200 voids `detail_at` and
marks a read in flight as outdated, exactly as `resolve_pending` does
for the store ops; a failure changes nothing. The page still owns the
inline error, the 401/403 forwarding and its own refetch. These ops were
not migrated to `begin_pending`: that would add a pending pill, a
success toast per click (a reaction toast is noise) and the D5 write
lock for what are quick edits, all visible product changes, while the
bug is only about where the confirmation lands. Thread writes (resolve,
comment edit/delete) change the page-owned threads record only and stay
as they were.

**A new head outdates the lanes that ride the old one (#752).** The page
keeps the head its last landing carried. A landing with a different
`headRefOid` (Update branch, or any refresh that sees a push) refetches
the Files list and the threads when those lanes are open, once per
change (`pr_page/head_change.rs`); Idle lanes load on first open. The
diff follows the Files list on its next paint, and check logs reset on
the new head (#747). The refetches ride the shared guard as
`Trigger::MutationRefetch`: one flying already predates the push, so
it is marked outdated, never lands, and one replacement goes out (the
#739 rule, now on the page's Files and threads records). A line comment
composed on the old patch cannot land silently on the wrong line: the
thread mutation takes no commit id, so the page records the head a
composer or the first queued draft was started on, and refuses to add or
submit while that is not the current head, with an inline Files error
and the text kept. Discarding, or arming a fresh composer with nothing
queued, clears it.

**UNKNOWN mergeable re-polls, bounded (#752).** GitHub computes
mergeability lazily, so the read after a push usually says `UNKNOWN`. A
landing of an OPEN PR with that state schedules a detail re-poll
(`Trigger::Recheck`) after 4 s, up to 3 in a row (`pr_page/mergeable_poll.rs`).
It stops at the first resolved answer. Only a landing that did not come
from a poll (open, refresh, refocus, write refetch, a new head) refills
the budget. The timer is a task the page owns, so a dropped page cancels
it. A failed poll has no landing and schedules nothing; a paused GraphQL
lane skips its poll. After the budget the merge rail keeps saying "GitHub
is still calculating merge status" until the next refresh or refocus.

**Stage 4 (#555) — issue documents join the store.** Issues live in
their own `(repo, number)` map (`IssueEntry`: the snapshot's open-issue
rows as the summary layer, the issue page's query as the detail layer)
with the same freshness books, SWR gates, eviction cap (one store-wide
cap across both domains), and pending machinery as PRs. The repo
snapshot's open-issue rows land as summaries — snapshot rows carry no
`state` (the query asks for open-only), so a row landing never moves a
patched or detail-landed state, and the state itself propagates from
detail landings both ways. The repo view derives its issue rows from
the snapshot ⊗ the documents (docs/0073 D3) and grows the pending pill
("closing…" the way PR rows read "merging…"); only a confirmed close —
or a closed detail landing — drops the row, and a confirmed reopen
brings it back. Close/reopen patch their post-state into both layers
(the one field the payload speaks verbatim) and revalidate in the
background; every other issue write (comment/description/timeline-row
writes, title, the three rail pickers, reaction/lock toggles) patches
nothing and voids the cached detail's freshness, refetching through
its arming hook — exactly the stage-3 rule. Failures keep the prior
document, record inline in the op's documented scope (Timeline /
Rail — docs/0020 D5 placement kept), and stage the warning toast.

**Amended (stale issue-document labels):** two gaps in the stage-4
wiring let a document's summary outlive its lane — `apply_repo_lane`'s
Issues arm never called `land_issue_summary` (only the issue page's
open seeded the document), and `land_issue_detail` patched only `state`
into the summary. Since the repo view renders the document when one
exists (docs/0073 D3), a label added upstream after the document was
seeded never reached the row — the row stayed frozen while the detail
page, a fresh fetch per open, showed the truth. Amended: the Issues
lane now lands its rows as summaries on every landing (the same shape
as the PR lane's stage-2 landing; `land_issue_summary` still never
moves a patched or detail-landed state), and a detail landing
propagates the fresh label set (`labels` + `labels_total`) into the
summary beside the state.

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

### D8 — Persistence (phase 2, #556)

The store writes through to `app_db` (docs/0063) as schema v3, table
`github_documents` — one row per document, natural key
`(kind, repo, number)` under UNIQUE (the `id` stays the engine-assigned
rowid, §9's rule), each layer as opaque JSON with wall-clock epoch-ms
stamps. Core is type-blind: `StoredDocument` carries JSON strings; the
module owns serde. The plumbing reuses the app-DB shapes exactly — the
buffered writer thread, best-effort commit (log-only failure; the
in-memory store stays the session's source of truth), the lazy reader,
degraded isolation (reads empty, writes drop — the store then runs
session-scoped, exactly pre-#556), and the hourly prune.

- **Write-through**: every store mutation that changes a document's
  content persists the row — summary landings, detail landings, state
  patches, freshness voids. Books, pending ops, and mutation errors
  stay memory-only; **pending ops are memory-only by decision** (an
  interrupted write must never replay — there is no way to know
  server-side whether it landed). A persisted detail layer exists iff
  it carries a live stamp: a voided body persists summary-only, so a
  restore cannot claim freshness the confirmed write invalidated.
- **Restore**: at state construction (the one blocking read on the
  path — the docs/0054 §8 precedent), newest-touched first, bounded to
  the table cap. Rows parse with the live parser; a schema-skew row is
  skipped, never fatal. Summaries restore unconditionally (a stale
  summary still beats a skeleton; the snapshot revalidates on its own
  lanes). **Details restore only within a 1-hour TTL of their landing
  (owner-tuned)** — beyond it the row restores summary-only. In-session
  gates keep their own TTLs: a restored 30-minute detail paints stale
  and refetches on open, which is the point.
- **Bounds**: the in-memory cap and the table cap are the same number
  (1024, owner-set). Memory evicts on write (D9's two-pass, pending
  pinned); the table prunes drop-oldest-touched in the hourly pass,
  arrival order breaking equal-touch ties.
- **Sign-out** wipes the persisted rows with the memory maps (the
  `GithubCache::clear` symmetry; it also caught a #555 leak — the issue
  map had never been cleared on sign-out).

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
   Shipped: #554, D4's stage-3 paragraph.
4. **Issues** — `Kind::Issue` documents for issue_page/new_issue_page.
   Shipped: #555, D1's stage-4 paragraph.
5. **Persistence** — the `app_db` write-through and boot restore (D8).
   Shipped: #556, D8.

Stages 1–2 are the design's core; 3–4 are mechanical follow-ons on the
same rules and can split into their own issues.

## 7. Non-goals

- ~~**Persistence** to `app_db` (D8 — deferred with its own issue).~~
  Shipped: #556 (D8).
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
