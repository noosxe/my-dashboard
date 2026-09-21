# 0050 — GitHub API Usage Calculator

Addresses [#294](https://github.com/noosxe/my-dashboard/issues/294).

> Design-first flow: this doc ships as its own PR; implementation follows a
> separate issue once this merges.

## 1. Summary

Count every GitHub-bound HTTP request this app makes, account for it the way
GitHub does, and show the resulting budget consumption in a footer chip
whenever the GitHub module is on screen. The core decision up front:

**GitHub's server-reported numbers are the ledger; local counting is only
prediction between responses.** There is no client-side cost formula
anywhere in the design — the issue's measurements (2026-09-20, live API)
show the documented GraphQL formula is gone (real cost: 1 point/request,
flat, verified by server `rateLimit.used` deltas up to 10,100-node
queries), so any estimator would be wrong by up to two orders of magnitude.
Every response we already receive carries the authoritative budget state;
the calculator's job is to *collect* it, *predict* across in-flight
requests, and *display* it.

## 2. Verified facts (anchors)

- **All GitHub-bound traffic flows through one seam**: `state.client:
  Arc<dyn Http>`, built once as `UreqClient` in `lib.rs init`. The three
  transports (`run_spec` — one request per REST page, `run_plain_get`,
  `post_graphql`) *and* the auth flows (device-flow start, token poll,
  token refresh) all borrow that same client. Wrapping it sees everything.
- **REST responses already parse the budget**: `finish()` in http.rs
  harvests `x-ratelimit-limit/remaining/reset` into `HttpResult.rate`
  (docs/0005 §3.3). `used` is derivable as `limit − remaining`; nothing
  new needs parsing.
- **GraphQL responses already select `rateLimit { cost remaining resetAt }`**
  on most documents; `gql::RateLimitField` keeps only
  `remaining`/`reset_at`. The body field is the authoritative snapshot;
  `x-ratelimit-*` headers are also present on GraphQL responses (the
  shipped backoff already relies on that) and serve as fallback.
- **Exhaustion has an operational home**: `GithubState.rate_limited_until`
  (epoch; backoff per docs/0036). It is the source of truth for *pausing*
  fetches and stays exactly as is.
- **The footer bar** is `core::status::bar` (docs/0013): one bar under
  both columns, right region carries per-module in-flight spinners driven
  by the `InFlight` tracker (a pure book struct, unit-tested without
  gpui — the precedent this design follows for its ledger).
- **REST + GraphQL share one budget**: authenticated REST requests and
  GraphQL points draw on the same 5,000/hour per-token budget. One
  combined number is correct, not a simplification.
- **`GET /rate_limit` is free**: per GitHub docs it does not count against
  the primary limit, and returns the full bucket breakdown — the ideal
  reconcile probe.
- **Auth/device-flow endpoints** (`github.com/login/...`) have their own
  small per-IP buckets, not the primary budget.

## D1 — Accounting model

| Flow | Cost booked | Server truth consumed |
|---|---|---|
| GraphQL request (any document) | 1 point | body `rateLimit` (remaining/resetAt, `cost` when present) else `x-ratelimit-*` |
| REST request (any endpoint, incl. one-shot GETs, mutations, 304s) | 1 point | `x-ratelimit-*` headers |
| Paged REST fetch | 1 per page (each page is its own request through the seam) | last page's headers reconcile the whole run |
| Auth/refresh (`github.com/login/...`) | auth bucket, count-only | headers when present; no primary-budget effect |

Rules:

- **No client-side formula.** The local "1 point per request" is a
  *prediction* the next response overwrites with fact. If GitHub ever
  changes real costs, the display self-corrects on the next response —
  nothing to update in this codebase.
- **Buckets are classified by URL**: `api.github.com` → primary
  (REST and GraphQL alike), `github.com/login/*` → auth. Nothing else is
  called today; an unknown host books count-only into the session totals.
- **Every request counts the moment it starts** (in-flight prediction),
  and every response immediately replaces the snapshot with fact. A 304
  counts (it consumes budget upstream); the response's headers still
  reconcile it.

## D2 — The counting seam: one decorator, zero call-site changes

`CountingHttp` wraps the existing `dyn Http`:

```rust
struct CountingHttp {
    inner: Arc<dyn Http>,          // UreqClient in production
    ledger: UsageLedger,           // Arc<Mutex<UsageBook>> (D3)
}
impl Http for CountingHttp { /* record start → inner.request /
                                post_graphql → record response */ }
```

- Constructed once, in `lib.rs init`, wrapping the `UreqClient` before it
  enters `GithubState` — requirement 3 (middleware placement) is satisfied
  by construction: no transport, page, or auth caller knows it exists.
- The ledger `Arc` clones into the wrapper and into the state entity (the
  `SharedToken` precedent: a small cross-thread slot shared between the
  background request path and the UI).
- `MockHttp` tests are untouched — they construct `GithubState` with bare
  mocks; the wrapper is production-only. Wrapper behavior itself is tested
  by wrapping `MockHttp` (D6).
- Thread-safety: the wrapper lives on background tasks only; `UsageBook`
  sits behind a `std::sync::Mutex`, held across no awaits (record start /
  record response are two short critical sections per request).

## D3 — The ledger: `UsageBook` (pure) + display derivation

A plain struct, unit-tested without gpui (the `InFlightBook` pattern):

```rust
struct UsageBook {
    // Last server snapshot (primary bucket).
    limit: Option<u32>, remaining: Option<u32>, reset_at: Option<u64>,
    snapshot_at: Option<Instant>,          // local clock, for staleness
    inflight: u32,                          // started, not yet answered
    // Session counters (all buckets).
    session_total: u64, session_graphql: u64, session_rest: u64,
    session_auth: u64,
    last_error: Option<String>,             // network failures, for tooltip
}
```

Methods (pure, all logic lives here):

- `on_request_start(bucket)` — bumps `inflight` + session counters.
- `on_response(snapshot)` — clears one `inflight`, replaces the snapshot
  wholesale. **This is the entire reconciliation mechanism**: every
  response is a full sync, so displayed usage is exact within one
  response — the issue's "periodic sync" is satisfied by traffic itself.
- `on_failure()` — clears one `inflight`, notes the error.
- `reset()` — account switch wipes the book (per-token budget). Token
  *rotation* for the same account keeps it (same user+app budget).
- `display(now, rate_limited_until) -> UsageDisplay` — the single
  derivation everything renders from:

| state | shown |
|---|---|
| snapshot present, no rollover | `remaining = max(0, snapshot.remaining − inflight)`, plus `limit`, `reset_at` countdown |
| `now` past `reset_at` without a newer snapshot | keep last-known, flag stale (chip tooltip says "as of HH:MM"), arm the probe |
| `rate_limited_until` in future (the operational backoff) | exhausted presentation, countdown from `rate_limited_until` |
| no snapshot yet (fresh connect, no traffic) | skeleton "…/…" placeholder until the first response lands |

**Stale-window probe**: `GET /rate_limit` (free, D2 seam, books 0) fired
(a) when the chip is visible with a stale snapshot past `reset_at`, or
(b) when `rate_limited_until` expires — never more than once per 30 s.
It flows as a normal spec through the wrapper, so its own response
reconciles the ledger like any other.

## D4 — Exhaustion: one operational truth, one display mapping

`rate_limited_until` remains the *only* writer of pause behavior (docs/
0036 backoff unchanged — the existing `remaining == 0` arms in `apply`
keep setting it). The display *reads* it alongside the ledger snapshot;
the two agree by construction because both originate from the same
responses. The chip never writes fetch-affecting state.

## D5 — The footer chip: module-contributed status items

**Core hook (generic, reusable)** — `ModuleEntry` grows:

```rust
pub type StatusItemFn = Arc<dyn Fn(&Page, &App) -> Option<AnyElement>>;
// ModuleEntry { ..., status_item: Option<StatusItemFn> }
```

`status::bar(tracker, registry, active_page, cx)` appends each module's
contribution after the in-flight spinners. The shell passes the active
`Page`; **each provider decides its own visibility** — for GitHub: render
iff `Page::Module("github")`, exactly the issue's scoping (Home and
Settings show nothing). This is the "don't reimplement per page" shape:
future modules plug in with one closure, and the bar owns ordering and
lifecycle.

**The chip itself** is a small self-refreshing view (`UsageChip`):

- Reads the shared `UsageBook` + `rate_limited_until` on a 1 s timer,
  `cx.notify()` only when the rendered string actually changes — zero
  churn when idle, one short mutex read per second while a GitHub page
  is visible. Live updates during fetches fall out (in-flight prediction
  moves the number per request, including background auto-refresh).
- Copy (terse, one bar line):
  - normal: `GH 4,821/5,000 · resets 41m` (combined REST+GraphQL budget)
  - exhausted: `GH rate-limited · resumes 12m` (warning color)
  - placeholder: `GH …/…` before the first snapshot
  - tooltip: session totals (`session: 127 req · 124 GraphQL · 3 REST`)
    and snapshot age; the auth-flow count rides here too.
- A11y per docs/0049 D1: `.id("github-usage")` + `Role::StaticText` +
  explicit `aria_label` carrying the full spoken form (numbers spelled
  out, not the compressed chip text); state change (exhausted) is part of
  the label.

## D6 — Testing and split

Single implementation issue (cohesive, one seam):

- **Pure units (the bulk)**: `UsageBook` — snapshot replacement, in-flight
  prediction floor, rollover/stale flagging, display strings for all four
  states, reset semantics, per-bucket session counters, probe arming
  window.
- **Wrapper**: `CountingHttp` over `MockHttp` — N scripted pages → N
  booked requests, snapshot reconciles from the last response, failures
  clear in-flight, auth URLs book the auth bucket.
- **gql fixtures**: `RateLimitField` gains `cost`/`used` with
  `serde(default)` — existing fixture JSONs keep passing.
- **Core hook**: a registered `status_item` appears only for its page.
- **Bridge/human**: chip scoped to GitHub pages (Home ↔ module), number
  moves during a fetch, exhausted copy verified visually against the
  existing backoff (real exhaustion is unit-covered, not forced live).

## Non-goals

- Usage history/graphs and budget warnings (possible follow-ups; the
  ledger already carries the raw session counters they would need).
- Other providers' rate limits.
- Secondary-bucket chips (auth/device-flow budgets stay count-only +
  tooltip; they surface operationally through existing logs/backoff).
- Any client-side reimplementation of a cost formula, for any endpoint.
- REST Search's separate bucket — the app uses GraphQL search only.

## Acceptance criteria

- Every GitHub-bound HTTP call the app makes — REST pages, GraphQL,
  one-shot GETs, mutations, auth flows — passes through `CountingHttp`
  and books its D1 cost; no call site changed.
- Displayed usage equals GitHub's server-reported numbers within one
  response of any traffic; a stale window self-heals via the free probe.
- The chip appears only on GitHub module pages, updates live (including
  background auto-refresh), and is labeled per docs/0049 D1.
- Exhaustion display derives from `rate_limited_until` — one operational
  source of truth, unchanged backoff behavior.
- Account switch resets the ledger; same-account token rotation doesn't.

## Human checks (owner)

- [ ] Walk Home → GitHub module: chip appears; leave: chip gone.
- [ ] Trigger a refresh and watch the number move per request; the
      tooltip's session counters grow.
- [ ] Spend down to zero (or catch the existing backoff): chip shows the
      exhausted form and a resume countdown that matches when fetches
      resume.
- [ ] Reconnect as another account: usage resets with the first snapshot.
