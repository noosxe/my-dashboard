# 0031 — Fetch record + per-key in-flight dedup (the shared freshness substrate)

| | |
| --- | --- |
| Issue | #218 (consumers: #188 auto-refresh detail pages, #194 refresh on focus, #193 cache TTL / staleness marking, #192 last-synced indicator) |
| Date | 2026-09-16 |
| Status | design — pending owner review |
| Depends on | docs/0029 §2/§4 (freshness roadmap), docs/0028 (`AuthGate` — the existing single-flight precedent), docs/0016 D2/D8 + docs/0030 D1 (page-owned fetch state machines) |

## 1. Context

The GitHub module's fetch lane is deliberately split (0005 Q6/Q7): pure
state machinery — `plan_tick`, `run_spec`, `apply`, all unit-tested on a
scripted transport — with thin gpui orchestration on top. Two structural
gaps surfaced when this lane was compared against a TanStack-Query-style
client store (#218):

1. **No per-key in-flight dedup for reads.** Only the 401 refresh is
   single-flighted (`AuthGate`, 0028). Read dispatch sites are
   independent lanes: the tick loop, `GithubPage::run_specs` (repo-open,
   mark-refetch), the PR page's three lazy fetches, the issue page, the
   notification page. Any lane can fire a spec while another lane's
   identical fetch is still running — today that window is narrow (e.g.
   the notification page's post-write `Notifications` refetch racing the
   tick walk), but #188 (timers) and #194 (focus triggers) add refetch
   sources and widen it structurally. Two same-key walks also mean an
   *older* multi-page run can apply after a fresher one.

2. **No shared fetch record.** Every surface hand-rolls its lifecycle:
   `FilesFetch`/`ThreadsFetch` (`Idle/Fetching/Ready/Failed`) on the PR
   page, `detail: Option<PrDetail>` + `loading` + `load_error` on the
   detail pages, and nothing at all on most `GithubState` surfaces. The
   freshness tickets all need *status + when-was-this-fetched*: #192
   reads an age, #193 compares one against a TTL, #188/#194 decide
   whether a refetch is due. Without a shared record, four tickets
   re-invent the same state machine.

This doc designs the two primitives. It deliberately does **not** design
the consumers — trigger policies, TTLs, and indicators stay with their
tickets.

## 2. Goals and non-goals

Goals:

- **Per-key in-flight dedup for read kinds**, effective across all
  dispatch lanes, with skip semantics (a skipped dispatch is a no-op for
  the caller).
- **One shared fetch record** — status, data, `fetched_at`, and an
  in-flight flag kept orthogonal so stale-while-revalidate rendering
  (show data while a refetch runs) falls out for free.
- **Adopt the record on the #188 proving consumers**: the PR page
  (detail, files, threads) and the issue page detail.
- Keep the scripted-transport testability of the lane: the guard must be
  unit-testable without gpui, like everything else in service.rs.

Non-goals (each stays with its ticket):

- **No central query cache, no serialized-key framework.** State stays
  in per-entity fields; GPUI ownership unchanged. This is plumbing, not
  a port.
- **No refetch triggers, TTL policy, or staleness UI** (#188/#194,
  #193, #192 respectively).
- **Mutations unchanged**: `BusyOp` gating + explicit post-write
  refetch-spec lists (0019 D6, 0030 D4). No automatic invalidation
  graph.
- **No disk-format changes**: `last-known` blobs carry no `fetched_at`
  in v1; cold-start staleness marking is #193's to extend.
- **No ETag work**: notifications keep the only conditional request
  (0005 §3.3).

## 3. Current mechanics (verified in-tree 2026-09-16)

- Every fetch and mutation crosses one await point:
  `run_spec_tracked` (service.rs) — refresh-gated (0028), background
  `run_spec`, `SpecResult` returned to the caller. Callers pass
  `Entity<GithubState>`; `run_spec_tracked` itself never applies.
- Read dispatch sites: the tick loop (service.rs — self-serializing:
  each beat awaits its own specs before sleeping),
  `GithubPage::run_specs` (page.rs: repo-open specs, mark-refetch,
  connect flow), PR page detail/files/threads (pr_page.rs — page-owned
  results per 0016 D2: 200 bodies parsed page-side, only non-200
  forwarded to `GithubState::apply`), issue page detail (issue_page.rs),
  notification-page thread fetch (notification_page.rs).
- Mutation dispatch sites: `run_mutation` (service.rs), PR/issue page
  mutation calls, thread-op rows (page.rs) — all user-initiated
  one-shots.
- Key material: every spec carries `kind` + `ident: Option<String>`.
  Verified unique per target: `PrDetail` specs carry
  `"owner/repo#number"` (pr_page.rs `detail_spec`), per-repo kinds carry
  `"owner/repo"`, global kinds carry `None`. `Kind` discriminates
  `Notifications` from `RepoNotifications` and reads from mutations.
- Existing lifecycle state machines: `FilesFetch`/`ThreadsFetch`
  (pr_page.rs) and the detail page's `detail`/`loading`/`load_error`
  triple — the shapes the record replaces.

## 4. Design decisions

### D1 — The guard is a read wrapper around `run_spec_tracked`, not a change to it

New `run_read_deduped(state, spec, cx) -> Option<SpecResult>` in
service.rs: arm the key, run `run_spec_tracked`, clear the key, return
`Some(result)`. `run_spec_tracked` stays untouched — mutations and the
connect flow keep calling it directly (they are user-initiated one-shots
already gated by `BusyOp`/`ConnectPhase`; the guard must never block a
retry after a failed write's refetch).

Keying on `(Kind, Option<String> ident)` — §3 verified this uniquely
identifies a read target. The key set lives on `GithubState` (every
caller already holds the entity).

Rationale — one wrapper at the transport boundary covers all lanes
(tick, page, PR page, issue page) without each learning about the guard,
and a `None` return keeps the skip decision at the transport instead of
inventing a sentinel result every `apply` path must learn to ignore (D7).

### D2 — Skip, never queue

A dispatch that finds its key armed is dropped: the wrapper returns
`None`, the caller applies nothing. Rationale: every current and planned
trigger is periodic or re-arming (tick, tab open, focus, timer, manual
refresh) — skipping converges on the next trigger, while queueing
bursts requests and can apply an *older* queued response after a fresher
in-flight one. The in-flight fetch IS the refresh the caller wanted.
Known cost: a manual refresh pressed while a same-key fetch is in flight
is a no-op; #188's refresh affordance can disable itself once the
record's `in_flight` is visible to it.

### D3 — One fetch record: status × in-flight, orthogonally

```rust
pub(crate) struct Fetch<T> {
    status: FetchStatus<T>,
    pub(crate) in_flight: bool,
    pub(crate) last_error: Option<String>,
}

enum FetchStatus<T> {
    Idle,
    Ready { data: T, fetched_at: Instant },
    Failed { error: String },
}
```

Transitions (all on the main thread, in the existing `apply_*` paths):

- `begin()` — `in_flight = true`. No other change.
- `land(data)` — `in_flight = false`, `status = Ready { data,
  fetched_at: Instant::now() }`, `last_error = None`.
- `fail(error)` — `in_flight = false`; with no data yet → `status =
  Failed`; with data → status untouched, `last_error = Some(error)`
  (stale-on-error, matching 0005's stale-data-over-error principle and
  the page surfaces' current behavior).

Derived queries: `data()`, `is_loading()` (`in_flight` with no data —
first-load spinner), `age()` (`Option<Duration>` off `fetched_at`).
Stale-while-revalidate falls out: `in_flight && Ready` means "show data,
refetch running" with zero extra state.

This replaces `FilesFetch`/`ThreadsFetch` (mechanical — same states plus
the record's additions) and the detail pages'
`detail`/`loading`/`load_error` triple (`Ready` = `Some(detail)`,
`Loading` folded into `in_flight` + `Idle`, `load_error` split into
`Failed` and `last_error` by data presence — which fixes an ambiguity
the triple couldn't express).

### D4 — `fetched_at` is apply-time, session-local, unpersisted

Set to `Instant::now()` when a result lands on the main thread — not at
dispatch (a queued-behind-network run would otherwise look fresher than
it is). `Instant` is monotonic and session-scoped; the `last-known` disk
blobs do not carry it (D-non-goal: #193 owns any persisted-staleness
format). Consumers compute `age()` at render; no TTL constant lives
here.

### D5 — Adoption: PR page first, issue page detail with it

`PrDetailPage` folds `detail`/`loading`/`load_error` into
`Fetch<PrDetail>`, `files: FilesFetch` into `Fetch<PrFiles-payload>`,
`threads: ThreadsFetch` into `Fetch<PrThreads>`; the issue page's detail
triple likewise. These are exactly the surfaces #188 refreshes, so the
record is proven by its first consumer in the same breath — the guard
(D1/D2) is exercised by the PR page's refresh + the tick lane today.

`GithubState`'s list surfaces (repos, notifications) do **not** adopt
the record in v1: their cadence belongs to the tick loop, and retrofit
them is #192/#193's business when they need an age to display.

### D6 — Skip signaling: `None`, not a sentinel `SpecResult`

`run_read_deduped` returning `Option<SpecResult>` keeps "skipped" out of
the result vocabulary entirely — no `status: 0` convention, no changes
to `GithubState::apply` or the page-side parsers. Mechanical migration
at the ~7 read call sites; mutation sites stay on `run_spec_tracked`.

### D7 — Bridge/a11y contract

The substrate ships no new interactive elements (0010 D10 n/a). Consumer
tickets own their affordances — #188's refresh button and #192's
indicator will carry the ids/roles per 0010 D10, and can read
`in_flight`/`age()` off the record to drive their states.

## 5. Testing

- **`Fetch<T>` transition table** (pure): begin → land; begin → fail
  with no data (`Failed`); begin → fail with data (status preserved +
  `last_error`); `land` clears `last_error`; `age()`/`is_loading()`
  derivations.
- **Guard round-trip on the scripted transport**: same key fired from
  two lanes → exactly one HTTP call recorded, second caller gets `None`
  and state is untouched; different keys → both run; a mutation kind
  fired while its read sibling is in flight → unblocked; single-lane
  sequences unchanged from today.
- **Existing suites stay green**: the pure service machinery gains only
  the wrapper; `FilesFetch`/`ThreadsFetch` replacement is covered by the
  existing PR-page tests after the mechanical rename.

## 6. Implementation plan

1. This design doc (docs-only PR).
2. After merge: one implementation issue off #218 (the scope is a single
   focused PR): the `Fetch<T>` record, `run_read_deduped`, PR-page +
   issue-page adoption, the tests above. Owner go-ahead per the
   big-feature workflow.
3. Consumers follow as their own tickets land (#188 → #194 folded in →
   #192 → #193), each reading the record instead of growing a new
   lifecycle.

## 7. Open questions (owner)

1. **Issue-page detail in the same implementation PR?** D5 includes it;
   dropping it would make the first PR smaller but leave the two detail
   surfaces temporarily divergent.
2. **Skip semantics for the manual-refresh race** (D2) — acceptable for
   v1, or should #188's refresh control be required to disable itself
   before shipping?
3. **`GithubState` list surfaces now or with #192?** D5 defers them;
   including them now would grow the PR meaningfully for no consumer
   benefit until an age is rendered.
