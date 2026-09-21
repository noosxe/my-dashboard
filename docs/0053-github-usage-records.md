# 0053 — GitHub usage records: a per-flight cost log (#324)

| | |
|---|---|
| Issue | #324 (phase 1: collection pipeline; the analytics UI stays deferred) |
| Status | design |
| Depends on | 0050 (merged: buckets, booking rules, `CountingHttp` seam, chip), 0031 (merged: spec discipline), 0051 (merged: dispatch-path inventory) |

## 1. Context & problem

The usage calculator (docs/0050) answers *what is left*. The owner's
question set for analytics is retrospective: **which pages eat the
quota, how much is spent per quota cycle, and which requests are
rarely worth their cost.** None of that is answerable from the current
state: `UsageBook` keeps aggregates only (per-bucket counters, the
latest header snapshots, probe state) — there is no per-request
history, and `CountingHttp` — the one seam that sees every flight —
has no idea *which surface* caused a request.

This doc designs the collection pipeline only: one immutable record
per HTTP flight, cost-first, origin-attributed, held in memory. The
records are shaped as the future SQLite rows (the analytics page is a
separate, later design). Page/view *visit* analytics — and correlating
visits with costs — is explicitly future work (§7).

One requirement is settled up front: **the pipeline is module-global.**
GitHub is its first reporter, but Forgejo/Gitea-class modules later must
report request usage into the *same* log — so the record carries a
`module` field and the machinery lives in core, not in the GitHub
module (D6).

## 2. Facts (receipts)

- **The flight layers.** `CountingHttp` (usage.rs, wraps the single
  `UreqClient` at the `lib.rs::init` seam) sees verb/url/etag and the
  `HttpResult` for **every** flight — REST pages, GraphQL, one-shot
  GETs, mutations, the token refresh POST, the free probe — but is
  shared and stateless: it cannot know the dispatching surface.
- **The spec runners see the semantics.** `run_spec` fans out by
  `Kind` into `run_rest` (page-stitched REST walk, ETag page-1
  revalidation), `run_graphql_fetch` (single flight), `run_plain_get`,
  and the per-kind GQL/REST runners. Each holds `spec.kind`,
  `spec.url`, `spec.ident`, and the per-flight `status`/`rate`/`etag`
  plus a wall-clock duration — everything a record needs except an
  origin label.
- **One spec ≠ one flight.** `run_rest` walks pages (`max_pages`), and
  `run_spec_tracked` re-enters `run_spec` on 401-refresh retries — a
  record must be per *flight*, not per spec (docs/0050 counts REST
  points per flight; the retry fan-out is real traffic).
- **Rate data on both transports.** REST flights carry
  `RateLimit { limit, remaining, reset_epoch }` from
  `x-ratelimit-*` headers; GraphQL responses carry `RateLimitField`
  (`cost`/`used`, `serde(default)`) where the query asks for it.
- **Cost semantics are already pinned** by docs/0050 D1/D3: primary
  bucket = 1 point per REST flight (304 revalidations free, the
  `GET /rate_limit` probe free), auth POSTs count-only (no quota),
  GraphQL = points from the response `rateLimit` field when present.
- **`FetchSpec` literals: 45 across 6 files** (service 23, pr_page 7,
  notification_page 6, new_pr_page 4, issue_page 3, new_issue_page 2) —
  an added field is a mechanical sweep, not a redesign.
- **Dispatches outside `FetchSpec`:** the token refresh (`run_refresh`)
  and the connect flow call the client directly; the probe and
  mutations go through specs (`Kind::RateLimit`, `Kind::*Mutation`).
- **The core-global precedent exists.** `dashboard_core::status`'s
  `InFlight` tracker is a `gpui::Global` created by `status::init(cx)`
  in `main`, keyed by `SharedString` module id (`status::begin(cx,
  MODULE_ID.into())` at the github call site) — the exact shape a
  module-keyed usage-records global needs.

## 3. Decisions

**D1 — One record per HTTP flight, costs not counts.** A `UsageRecord`
is a flat, sqlite-ready row emitted for every real flight:

```rust
struct UsageRecord {
    module: SharedString,         // D6 — "github" today; "forgejo"/"gitea" later
    seq: u64,                      // monotonic, the table key
    ts_epoch_ms: u64,
    transport: Transport,          // Rest | Graphql | Auth
    op: &'static str,              // kind_name() — "pr-detail", "notifications", …
    surface: SharedString,        // D2 — stamped from the module's closed enums
    trigger: SharedString,        // D2 — ditto; TEXT columns in the sqlite row
    ident: Option<SharedString>,   // owner/repo, repo#n, …
    status: u16,                   // 0 on network error
    etag_304: bool,
    cost: Option<u32>,             // None = not attributable (GQL without the field)
    remaining_after: Option<u32>,  // header/response snapshot, when present
    cycle_reset_epoch: Option<u64>,// D3 — denormalized for per-cycle GROUP BY
    duration_ms: u32,
    network_error: bool,
}
```

`cost` follows the 0050 booking rules verbatim: REST primary flight →
`Some(1)`, 304 → `Some(0)`, probe → `Some(0)`, auth → `Some(0)`,
GraphQL → `rateLimit.cost` when the query carries the field, else
`None` (never a guessed number). `remaining_after` is recorded so
later analytics can reconcile nominal costs against real snapshots.
No entity handles, no `Context` types — everything is owned/Copy: this
struct *is* the future SQLite row.

**D2 — Origin rides on the spec, stamped at dispatch.** `FetchSpec`
gains `origin: Origin { surface, trigger }` (two small enums). The
taxonomy comes from the goal questions:

- `surface`: `HomeCard | ModuleHome | RepoView | PrPage | IssuePage |
  NotificationPage | Composer | ServiceTick | Connect | Auth | Background`
  — "which page eats the quota".
- `trigger`: `Bootstrap | ViewShown | Refocus | Manual | Expand |
  MutationRefetch | Probe | Auth` — the docs/0052 trigger set, so
  records explain *why*, not just *where*.

Origins are stamped where the dispatch is made (page/card fires,
composer sends, the tick's probe arm, connect steps) — the same places
docs/0051 §3 inventoried. Enforcement is a ladder, so no surface can
silently slip through:

- **Compile time — the field is required, there is no default.** Every
  `FetchSpec` literal (all 45 today) and every future one must name an
  explicit `origin` or it does not compile. `Origin::BACKGROUND`
  survives only as an escape hatch you *write out* for genuinely
  ownerless dispatches — never a value code falls into by omission.
  The enum being closed has the same effect one dimension up: a new
  page is a new `Surface` variant, and adding it is a visible act that
  forces the origin question.
- **Test time — the dispatch table is exhaustive.** One table test per
  docs/0051 §3 inventory path: drive it over `MockHttp` + a real
  `RecordLog`, then assert (a) every record's `(surface, trigger)` is
  the expected pair, (b) the record count equals the flights the mock
  served (emission completeness — a runner that forgets to emit
  fails here, not in production data), and (c) zero `Background`
  records.
- **Runtime — Background is loud.** Emitting a `Background` record
  logs `warn!` and bumps a counter, so a path that ships unstamped
  surfaces in the first session's logs and bridge dumps instead of
  quietly misattributing analytics. Emission stays in the flight
  runners (D4), **not** in `CountingHttp`: a shared mutable origin slot
  there would race concurrent background runs, and task-locals do not
  survive the background executor — both rejected.

**D3 — A capped in-memory log as a core global.** `RecordLog` is an
`Arc<Mutex<VecDeque<UsageRecord>>>` behind a `gpui::Global` in
`dashboard_core::usage_records` — created by `usage_records::init(cx)`
in `main` and reached through `cx`, the `status::InFlight` pattern
(§2), one FIFO across all modules. Cap
`RECORD_CAP = 10_000` (~1–2 MB; a heavy day is thousands of flights —
the cap is the working set for analytics, totals live in the
counters). `seq` is log-local and never resets. `cycle_reset_epoch`
is stamped from the flight's own `rate.reset_epoch` when present,
else carried from the log's last-known reset — REST windows are
hourly and GraphQL reports the same primary reset, so every record
lands in a groupable cycle bucket without join logic later.

**D4 — Plumbing: a `FetchCtx` through the runners.** The flight
runners take `FetchCtx<'a> { http: &'a dyn Http, records: &'a RecordSink }`
instead of bare `&dyn Http` — one mechanical signature change.
`run_spec_tracked` builds the ctx from the state (it already owns the
entity) — the sink comes from the core global (§2/D3) — so
is correct accounting. `run_refresh` and the connect flow — direct
client callers — emit their `Auth` records at the call site with the
same sink. `MockHttp` tests need no trait change: the sink is a plain
`Arc` they can construct and assert against.

**D5 — Additive only.** `CountingHttp`, `UsageBook`, and the chip are
untouched (D5 of 0050 stays the display; this pipeline is the audit
trail beside it). Nothing renders records in this phase.

**D6 — Module-global by construction.** The record carries `module:
SharedString` ("github" today), and nothing in the pipeline assumes
GitHub: `Transport` (REST/GQL/auth) is a universal cut, `op`/
`surface`/`trigger` are module vocabulary stamped from **each
module's own closed enums** — GitHub keeps its `Origin { Surface,
Trigger }` exactly as D2 designs it, converting via `as_str()` at
emission, so the per-module compiler enforcement survives intact
while the shared row stays schema-stable. A future Forgejo/Gitea
module adopts the sink, the cap, and the sqlite shape without
sharing GitHub's taxonomy. One cap across modules is deliberate:
analytics slices by `module` like any other column; per-module
quotas are the modules' own booking rules, not the log's concern.
The chip and `UsageBook` stay GitHub-local (0050's display layer).
## 4. State & plumbing changes

- **core** (new `usage_records.rs`, the D6 global): `UsageRecord`
  (`module` first), `Transport`, `RecordLog` + `RecordSink`,
  `RECORD_CAP`, `init(cx)` + `gpui::Global` — wired in `main` beside
  `status::init`.
- github `usage.rs`: `Origin { Surface, Trigger }` closed enums +
  `as_str()`; booking-rule helpers stay here (module-local).
- `service.rs`: `FetchSpec.origin`; `FetchCtx`; runner signatures;
  emission in `run_rest` (per page), `run_graphql_fetch`,
  `run_plain_get`, the paged-view and detail runners — each stamps
  `module: "github"`; `run_refresh`/connect emission.
- `page.rs`/`card.rs`/`composers/tick`: stamp origins at dispatch
  (one-line each, over the docs/0051 inventory).

## 5. Testing

- Emission units over `MockHttp` + a real `RecordLog`: a 3-page REST
  walk → 3 records of cost 1 sharing origin/ident; GQL with
  `rateLimit { cost }` → that cost; 304 → cost 0; probe → cost 0;
  refresh POST → `Transport::Auth`, cost 0.
- **The dispatch table (D2, test layer):** one case per docs/0051 §3
  inventory path — every record's `(surface, trigger)` matches the
  expected pair, record count == flights the mock served, and zero
  `Background` records. This subsumes the trigger assertions above.
- Cycle stamping: flights with snapshots stamp their own reset; the
  carry-forward path stamps the last-known one.
- Cap eviction at small `RECORD_CAP` in tests; `seq` monotonic across
  eviction.
- Module-global isolation: records carry their `module`; a synthetic
  second reporter writing through the same global log coexists with
  GitHub's records (the D6 contract for Forgejo/Gitea later).

## 6. Acceptance

- Every dispatch path in the docs/0051 §3 inventory emits exactly one
  record per real flight, with its stamped `(surface, trigger)` — the
  dispatch-table test is the gate; in a live session any `Background`
  record logs a `warn!` and is a stamping bug to fix, not noise.
- The chip's numbers are unchanged (regression: existing chip tests
  green).
- Gates: fmt, clippy zero warnings, `cargo test --workspace`.

## 7. Non-goals

- **The analytics UI** — per-surface rollups, per-cycle history,
  rare-request ranking, export. Deferred issue (this pipeline is its
  data contract).
- **SQLite persistence** — deferred issue; the record shape and
  `cycle_reset_epoch` denormalization exist so the migration is a
  table, not a redesign.
- **Page/view visit analytics and visit↔cost correlation** — deferred
  issue; would be a separate record type, not an extension of this
  one.
- **Adopting the pipeline in other modules** — Forgejo/Gitea reporting
  is future work; this doc only guarantees the shared sink, schema and
  cap don't assume GitHub (D6).

## 8. Docs amendments (ride with the implementation PR)

- **0050** D3: pointer note — the ledger keeps the live budget; the
  records pipeline (0053) is the per-flight audit trail beside it.
- README row for this doc.
