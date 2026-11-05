//! API-usage calculator (docs/0050): count every GitHub-bound request
//! and account for it the way GitHub does. The footer chip that shows
//! the budget lives next door in [`crate::usage_chip`].
//!
//! The core decision (0050 §1): **GitHub's server-reported numbers are
//! the ledger.** There is no client-side cost formula anywhere — every
//! response carries the authoritative budget state, and the book below
//! collects it and derives the display **verbatim** — no local
//! prediction is folded into shown values; in-flight counting is kept
//! as bookkeeping for the deferred usage-analytics task.
//!
//! Two primary budgets, not one (0050 D1): GitHub enforces REST (`core`)
//! and GraphQL as **separate** primary limits with separate counters and
//! separate windows — each response reports its own API's bucket. A
//! GraphQL value stays missing until the first GraphQL response lands:
//! `GET /rate_limit`'s `resources.graphql` is a phantom (always
//! 5,000/0-used, §2) and can never supply it.
//!
//! Sections: the pure book (D3, unit-tested without gpui) → the
//! counting seam (D2, a decorator around `dyn Http`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gpui::SharedString;

use dashboard_core::usage_records::{RecordDraft, Recorder, Transport};

use crate::http::{Http, HttpError, HttpResult, RateLimit, Verb};

/// Shared ledger handle — the `SharedToken` precedent (docs/0050 D2): a
/// small cross-thread slot between the background request path (the
/// wrapper writes) and the UI (the chip reads).
pub(crate) type UsageLedger = Arc<Mutex<UsageBook>>;

/// A fresh, empty ledger handle (a free fn — `Arc` admits no inherent
/// impls).
pub(crate) fn new_ledger() -> UsageLedger {
    Arc::new(Mutex::new(UsageBook::default()))
}

/// The budget clock, epoch seconds (docs/0081 §12.1): every judgement
/// against a server `reset_at` — the book's snapshots, the chip's
/// countdown, the rate-limit pause gate, the probe cooldown — reads
/// this. Under `DASHBOARD_FIXED_NOW` (debug builds) it is pinned, so
/// fixture reset times relative to the pinned instant render the same
/// countdown on any calendar day; the wall clock otherwise. Token
/// expiry stays on [`crate::auth::now_epoch`].
pub(crate) fn budget_now() -> u64 {
    dashboard_core::freshness::clock_epoch_ms() / 1000
}

/// Minimum spacing between `/rate_limit` probes (docs/0050 D3).
const PROBE_COOLDOWN_SECS: u64 = 30;

// ---------------------------------------------------------------------------
// Pure bookkeeping (D3) — no gpui types below this line.
// ---------------------------------------------------------------------------

/// Which budget a request draws on (D1): buckets are classified by URL
/// and — within the api.github.com host — by transport, because GitHub
/// tracks REST (`core`) and GraphQL as separate primary limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bucket {
    /// `api.github.com` — one of the two primary sub-budgets: REST
    /// (`core`, requests) or GraphQL (points), each 5,000/hour with its
    /// own window.
    Primary,
    /// `github.com/login/...` — the device-flow/refresh endpoints with
    /// their own small per-IP buckets: counted, never budget-displayed.
    Auth,
    /// Anything else (nothing today): count-only.
    Other,
}

impl Bucket {
    /// D1's URL classification. `api.github.com` covers REST and the
    /// GraphQL endpoint alike; `github.com/login/*` is the OAuth pair.
    /// Which *primary sub-budget* a request draws on is decided by the
    /// transport (REST vs GraphQL), not the URL.
    pub(crate) fn classify(url: &str) -> Self {
        if url.starts_with(crate::api::API_BASE) {
            Bucket::Primary
        } else if url.starts_with("https://github.com/login/") {
            Bucket::Auth
        } else {
            Bucket::Other
        }
    }
}

/// One server-reported budget snapshot (D1) — limit/remaining/reset
/// from `x-ratelimit-*` headers or a GraphQL `rateLimit` body object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BudgetSnapshot {
    pub(crate) limit: u32,
    pub(crate) remaining: u32,
    /// UTC epoch seconds when the window resets.
    pub(crate) reset_at: u64,
}

impl From<RateLimit> for BudgetSnapshot {
    fn from(rate: RateLimit) -> Self {
        Self {
            limit: rate.limit,
            remaining: rate.remaining,
            reset_at: rate.reset_epoch,
        }
    }
}

/// One bucket's display derivation — the numbers the chip renders for a
/// single sub-budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BucketDisplay {
    /// Server `remaining` minus this bucket's in-flight requests (the
    /// prediction, D1).
    pub(crate) remaining: u32,
    pub(crate) limit: u32,
    /// `reset_at − now`, clamped (0 once past).
    pub(crate) resets_in_secs: u64,
    /// The window rolled past `reset_at` without a newer snapshot
    /// (0050 D3: keep last-known, arm the probe).
    pub(crate) stale: bool,
    /// Epoch when this bucket's snapshot landed — the stale "as of".
    pub(crate) snapshot_at: Option<u64>,
}

/// The display derivation (D3) — everything the chip renders comes from
/// here; render-side code never re-derives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsageDisplay {
    /// Connected, no snapshot from either bucket yet — skeleton until
    /// the first response.
    Pending,
    /// At least one bucket's snapshot is present. `rest` / `graphql`
    /// are the two separate primary budgets (D1); `None` = that bucket
    /// has not reported yet (rendered as a placeholder).
    Budget {
        rest: Option<BucketDisplay>,
        graphql: Option<BucketDisplay>,
    },
}

/// One sub-budget's books: the last server snapshot plus this bucket's
/// in-flight bookkeeping. The display shows the server's numbers
/// verbatim — the in-flight counts are retained for the future
/// usage-analytics task (deferred; see docs/0050 D3), not shown.
#[derive(Debug, Default)]
struct SubBook {
    limit: Option<u32>,
    remaining: Option<u32>,
    reset_at: Option<u64>,
    /// Epoch when the snapshot landed — the stale display's "as of".
    snapshot_at: Option<u64>,
    /// This bucket's requests started but not yet answered. Kept as
    /// data for the deferred usage-analytics task; nothing reads it
    /// outside tests until then.
    #[cfg_attr(not(test), allow(dead_code))]
    inflight: u32,
}

impl SubBook {
    fn on_start(&mut self) {
        self.inflight = self.inflight.saturating_add(1);
    }

    /// A response landed: clears one in-flight slot and replaces the
    /// snapshot wholesale. This is the entire reconciliation mechanism
    /// (0050 D3): every response is a full sync of its own bucket, so
    /// the display is exact within one response.
    fn on_response(&mut self, snapshot: Option<BudgetSnapshot>, now: u64) {
        self.inflight = self.inflight.saturating_sub(1);
        self.fold(snapshot, now);
    }

    /// Overwrite the snapshot without touching in-flight — the probe's
    /// body fold path (the wrapper already reconciled the request slot
    /// from the same response's headers).
    fn fold(&mut self, snapshot: Option<BudgetSnapshot>, now: u64) {
        if let Some(s) = snapshot {
            self.limit = Some(s.limit);
            self.remaining = Some(s.remaining);
            self.reset_at = Some(s.reset_at);
            self.snapshot_at = Some(now);
        }
    }

    /// A transport-level failure: clears the in-flight slot; the book
    /// keeps the last snapshot (a network error changed nothing
    /// upstream).
    fn on_failure(&mut self) {
        self.inflight = self.inflight.saturating_sub(1);
    }

    /// The server's numbers, verbatim (0050 D3) — no local prediction
    /// is folded into the shown values.
    fn display(&self, now: u64) -> Option<BucketDisplay> {
        Some(BucketDisplay {
            remaining: self.remaining?,
            limit: self.limit?,
            resets_in_secs: self.reset_at?.saturating_sub(now),
            stale: self.reset_at.is_some_and(|reset| now >= reset),
            snapshot_at: self.snapshot_at,
        })
    }

    /// The probe wants this bucket: never sampled, or past its window.
    fn needs_refresh(&self, now: u64) -> bool {
        match (self.remaining, self.reset_at) {
            (None, _) => true,
            (_, Some(reset_at)) => now >= reset_at,
            (_, None) => false,
        }
    }
}

/// The pure ledger (docs/0050 D3) — the `InFlightBook` pattern. One
/// `SubBook` per primary sub-budget (REST / GraphQL): separate server
/// counters, separate windows, separate in-flight predictions.
#[derive(Debug, Default)]
pub(crate) struct UsageBook {
    rest: SubBook,
    graphql: SubBook,
    /// Session counters (all buckets; D5 a11y label).
    session_total: u64,
    session_graphql: u64,
    session_rest: u64,
    session_auth: u64,
    /// Epoch of the last armed `/rate_limit` probe (D3 cooldown).
    last_probe_at: Option<u64>,
}

impl UsageBook {
    /// A request started (D1): booked the moment it leaves, so the
    /// display predicts it before the answer arrives.
    pub(crate) fn on_request_start(&mut self, bucket: Bucket, graphql: bool) {
        self.session_total += 1;
        match bucket {
            Bucket::Primary => {
                if graphql {
                    self.session_graphql += 1;
                    self.graphql.on_start();
                } else {
                    self.session_rest += 1;
                    self.rest.on_start();
                }
            }
            Bucket::Auth => self.session_auth += 1,
            Bucket::Other => {}
        }
    }

    /// A response landed: clears one in-flight slot in the right
    /// sub-budget and replaces that bucket's snapshot wholesale (D1:
    /// REST responses carry the `core` bucket, GraphQL responses the
    /// `graphql` bucket — one number per bucket, never mixed).
    pub(crate) fn on_response(
        &mut self,
        bucket: Bucket,
        graphql: bool,
        snapshot: Option<BudgetSnapshot>,
        now: u64,
    ) {
        if bucket != Bucket::Primary {
            return;
        }
        if graphql {
            self.graphql.on_response(snapshot, now);
        } else {
            self.rest.on_response(snapshot, now);
        }
    }

    /// Fold the free probe's body (D3): `resources.core` only — the
    /// REST sub-budget. `resources.graphql` is a phantom on this
    /// endpoint (always 5,000/used-0; measured, 0050 §2), so the
    /// GraphQL book reconciles from GraphQL traffic exclusively.
    pub(crate) fn fold_probe(&mut self, core: BudgetSnapshot, now: u64) {
        self.rest.fold(Some(core), now);
    }

    /// A transport-level failure: clears the in-flight slot in the
    /// right sub-budget.
    pub(crate) fn on_failure(&mut self, bucket: Bucket, graphql: bool) {
        if bucket == Bucket::Primary {
            if graphql {
                self.graphql.on_failure();
            } else {
                self.rest.on_failure();
            }
        }
    }

    /// Account switch (0050 D3): per-token budget — the whole book
    /// goes. Token *rotation* for the same account keeps it.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Session counters for the chip's accessible label (D5).
    pub(crate) fn session_counters(&self) -> (u64, u64, u64, u64) {
        (
            self.session_total,
            self.session_graphql,
            self.session_rest,
            self.session_auth,
        )
    }

    /// The single display derivation (D3 table). The per-budget pause
    /// (`rate_limited_until`, D4) is not the book's: the chip lays it
    /// over each bucket ([`crate::usage_chip`]).
    pub(crate) fn display(&self, now: u64) -> UsageDisplay {
        let rest = self.rest.display(now);
        let graphql = self.graphql.display(now);
        if rest.is_none() && graphql.is_none() {
            UsageDisplay::Pending
        } else {
            UsageDisplay::Budget { rest, graphql }
        }
    }

    /// Whether the free `/rate_limit` probe should fire (D3): cooled
    /// down AND the REST sub-budget is missing or past its window.
    /// The GraphQL bucket never arms the probe — its `/rate_limit`
    /// field is a phantom (§2), so probing cannot refresh it; real
    /// GraphQL traffic does (and 99.9% of the time there is some).
    /// The probe is armed only on ungated ticks, so an active pause
    /// never probes; an expiring pause clears at the top of the same
    /// tick and probes.
    pub(crate) fn probe_due(&self, now: u64) -> bool {
        let cooled = self
            .last_probe_at
            .is_none_or(|at| now.saturating_sub(at) >= PROBE_COOLDOWN_SECS);
        cooled && self.rest.needs_refresh(now)
    }

    /// Stamp the probe cooldown at arming time (D3): a failed probe
    /// retries no sooner than the cooldown, never in a tight loop.
    pub(crate) fn mark_probe(&mut self, now: u64) {
        self.last_probe_at = Some(now);
    }

    /// Test seam: install fresh-looking snapshots for both sub-budgets
    /// without traffic.
    #[cfg(test)]
    pub(crate) fn seed_snapshots(
        &mut self,
        rest: BudgetSnapshot,
        graphql: BudgetSnapshot,
        now: u64,
    ) {
        self.rest.fold(Some(rest), now);
        self.graphql.fold(Some(graphql), now);
    }
}

/// Parse the `/rate_limit` probe body (D3): `resources.core` only —
/// the one field on this endpoint that tracks its real bucket.
/// `resources.graphql` is deliberately ignored: GitHub reports a
/// phantom 5,000/0-used there regardless of the bucket's true state
/// (measured live during #323 review, 0050 §2), so folding it would
/// poison the GraphQL book with a fake full budget. Returns the
/// `core` snapshot, or `None` on a malformed/body-without-core —
/// the caller just skips the fold (the probe response's own
/// headers, if any, were already folded by the wrapper).
pub(crate) fn parse_rate_limit_body(body: &str) -> Option<BudgetSnapshot> {
    #[derive(serde::Deserialize)]
    struct Core {
        limit: u32,
        remaining: u32,
        reset: u64,
    }
    #[derive(serde::Deserialize)]
    struct Resources {
        #[serde(default)]
        core: Option<Core>,
    }
    #[derive(serde::Deserialize)]
    struct Body {
        resources: Resources,
    }
    let parsed: Body = serde_json::from_str(body).ok()?;
    parsed.resources.core.map(|c| BudgetSnapshot {
        limit: c.limit,
        remaining: c.remaining,
        reset_at: c.reset,
    })
}

/// The GraphQL body's `rateLimit` object (D1): preferred over headers
/// when present. The body object carries no `limit`, so the harvested
/// headers fill it in (`x-ratelimit-limit` on a GraphQL response
/// reports the graphql bucket's limit). A huge document is parsed here
/// a second time on the background thread — the price of
/// body-authoritative accounting.
fn graphql_budget(body: Option<&str>, headers: Option<RateLimit>) -> Option<BudgetSnapshot> {
    #[derive(serde::Deserialize)]
    struct RateLimitField {
        remaining: i64,
        #[serde(rename = "resetAt")]
        reset_at: String,
    }
    #[derive(serde::Deserialize)]
    struct Data {
        #[serde(rename = "rateLimit")]
        rate_limit: Option<RateLimitField>,
    }
    #[derive(serde::Deserialize)]
    struct Envelope {
        data: Option<Data>,
    }
    let body = body?;
    let env: Envelope = serde_json::from_str(body).ok()?;
    let rl = env.data?.rate_limit?;
    let reset_at = chrono::DateTime::parse_from_rfc3339(&rl.reset_at)
        .ok()?
        .timestamp()
        .max(0) as u64;
    Some(BudgetSnapshot {
        limit: headers.map_or(0, |r| r.limit),
        remaining: rl.remaining.max(0) as u32,
        reset_at,
    })
}

// ---------------------------------------------------------------------------
// Origin (docs/0053 D2) — the module's closed vocabulary. Required on
// every `FetchSpec` (no default: compiler-enforced), converted to the
// core record's stringly fields at emission. `BACKGROUND` is the
// loud escape hatch: emitting one warns and counts (§2 runtime layer).
// ---------------------------------------------------------------------------

/// *Where* a dispatch came from — "which page eats the quota".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    HomeCard,
    ModuleHome,
    RepoView,
    PrPage,
    IssuePage,
    NotificationPage,
    Composer,
    ServiceTick,
    Connect,
    Auth,
    Background,
}

impl Surface {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Surface::HomeCard => "home-card",
            Surface::ModuleHome => "module-home",
            Surface::RepoView => "repo-view",
            Surface::PrPage => "pr-page",
            Surface::IssuePage => "issue-page",
            Surface::NotificationPage => "notification-page",
            Surface::Composer => "composer",
            Surface::ServiceTick => "service-tick",
            Surface::Connect => "connect",
            Surface::Auth => "auth",
            Surface::Background => "background",
        }
    }
}

/// *Why* a dispatch happened — docs/0052's trigger set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Trigger {
    Bootstrap,
    ViewShown,
    Refocus,
    Manual,
    Expand,
    MutationRefetch,
    /// docs/0072 D3: document drift — a summary-layer landing moved the
    /// checks rollup under a cached detail; the page revalidates so both
    /// surfaces report the same status.
    Drift,
    /// docs/0080 D3: the unfocused-poll arm — the notifications lane
    /// polled on a timer while the window is inactive (the single
    /// docs/0052 D1 carve-out; Usage receipts name it).
    Poll,
    /// docs/0081 D10: scroll-driven pagination — the commits list asked
    /// for its next page near the bottom. Distinct from `Manual` (the
    /// home "Load more" click) so passive scrolling isn't billed as a
    /// deliberate action.
    Paginate,
    Probe,
    Auth,
}

impl Trigger {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Trigger::Bootstrap => "bootstrap",
            Trigger::ViewShown => "view-shown",
            Trigger::Refocus => "refocus",
            Trigger::Manual => "manual",
            Trigger::Expand => "expand",
            Trigger::MutationRefetch => "mutation-refetch",
            Trigger::Drift => "drift",
            Trigger::Poll => "poll",
            Trigger::Paginate => "paginate",
            Trigger::Probe => "probe",
            Trigger::Auth => "auth",
        }
    }
}

/// Surface × trigger, stamped on the `FetchSpec` at dispatch (D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Origin {
    pub(crate) surface: Surface,
    pub(crate) trigger: Trigger,
}

impl Origin {
    /// The escape hatch you write out for genuinely ownerless
    /// dispatches — never a default (D2 compile-time layer). Used by
    /// transport-level tests that don't assert origins.
    #[allow(dead_code)] // lib target: only tests reach for it today
    pub(crate) const BACKGROUND: Origin = Origin {
        surface: Surface::Background,
        trigger: Trigger::Bootstrap,
    };

    pub(crate) const fn new(surface: Surface, trigger: Trigger) -> Self {
        Origin { surface, trigger }
    }
}

/// Session count of `Background`-originated records (D2 runtime
/// layer): a stamping bug that slipped compile and tests still shows
/// up here and in the warn log.
pub(crate) static BACKGROUND_RECORDS: AtomicU64 = AtomicU64::new(0);

/// The counter behind [`BACKGROUND_RECORDS`] — for tests and probes.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn background_records() -> u64 {
    BACKGROUND_RECORDS.load(Ordering::Relaxed)
}

/// The auth-flight recording view (`token-refresh` op) — shared by
/// `run_refresh` and the records tests. `dispatched_at` is the spawn
/// instant (docs/0054 D14: auth flights stamp queue wait from spawn).
pub(crate) fn recording_for_auth<'a>(
    http: &'a dyn Http,
    recorder: &'a Recorder,
    dispatched_at: Instant,
) -> RecordingHttp<'a> {
    RecordingHttp::with_transport(
        http,
        recorder,
        Origin::new(Surface::Auth, Trigger::Auth),
        "token-refresh",
        Transport::Auth,
        true,
        Some(dispatched_at),
    )
}

// ---------------------------------------------------------------------------
// The recording decorator (docs/0053 D4) — `FetchCtx` materialized as
// a per-run wrapper built inside `run_spec`, which knows `spec.origin`.
// Every runner's `http.request`/`http.post_graphql` call emits one
// record per real flight; future runners inherit emission for free.
// Per-run by construction — no shared mutable origin slot to race.
// ---------------------------------------------------------------------------

/// One run's recording view over the real client. `free` marks the
/// `GET /rate_limit` probe (docs/0050: unbilled). `dispatched_at`
/// (docs/0054 D14) anchors the queue-wait stamp: wait from spec
/// dispatch to flight start — `None` when the caller couldn't
/// capture it.
pub(crate) struct RecordingHttp<'a> {
    inner: &'a dyn Http,
    recorder: &'a Recorder,
    origin: Origin,
    op: &'static str,
    ident: Option<SharedString>,
    free: bool,
    /// docs/0053 D4: the auth/connect wrappers force `Transport::Auth` —
    /// the login endpoints are REST-shaped POSTs, but they are not API
    /// traffic and never bill (docs/0050 D1).
    transport_override: Option<Transport>,
    dispatched_at: Option<Instant>,
}

impl<'a> RecordingHttp<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        inner: &'a dyn Http,
        recorder: &'a Recorder,
        origin: Origin,
        op: &'static str,
        ident: Option<&str>,
        free: bool,
        dispatched_at: Option<Instant>,
    ) -> Self {
        if origin.surface == Surface::Background {
            BACKGROUND_RECORDS.fetch_add(1, Ordering::Relaxed);
            log::warn!(
                "usage-records: {op} dispatched with a Background origin — stamp it (docs/0053 D2)"
            );
        }
        Self {
            inner,
            recorder,
            origin,
            op,
            ident: ident.map(SharedString::from),
            free,
            transport_override: None,
            dispatched_at,
        }
    }

    /// `new` with a forced transport — the auth/connect views.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_transport(
        inner: &'a dyn Http,
        recorder: &'a Recorder,
        origin: Origin,
        op: &'static str,
        transport: Transport,
        free: bool,
        dispatched_at: Option<Instant>,
    ) -> Self {
        Self {
            transport_override: Some(transport),
            ..Self::new(inner, recorder, origin, op, None, free, dispatched_at)
        }
    }

    /// The queue wait at flight start (docs/0054 D14) — captured
    /// *before* the flight so it excludes transport time.
    fn queue_ms_at(&self, sent_at: Instant) -> Option<u32> {
        self.dispatched_at
            .and_then(|d| sent_at.checked_duration_since(d))
            .map(|wait| wait.as_millis().min(u32::MAX as u128) as u32)
    }

    fn draft(
        &self,
        transport: Transport,
        res: &Result<HttpResult, HttpError>,
        sent_at: Instant,
        queue_ms: Option<u32>,
    ) -> RecordDraft {
        let transport = self.transport_override.unwrap_or(transport);
        let duration_ms = sent_at.elapsed().as_millis().min(u32::MAX as u128) as u32;
        match res {
            Err(_) => {
                let mut draft = RecordDraft::new(
                    transport,
                    SharedString::from(self.op),
                    SharedString::from(self.origin.surface.as_str()),
                    SharedString::from(self.origin.trigger.as_str()),
                    self.ident.clone(),
                );
                // Network errors are not attributable — never a guessed
                // number (D1); a timed-out flight's server-side cost is
                // unknowable client-side. No response → no ceiling, no
                // body (docs/0054 D14). The queue wait still happened
                // and stays measurable.
                draft.duration_ms = duration_ms;
                draft.queue_ms = queue_ms;
                draft.network_error = true;
                draft
            }
            Ok(result) => {
                let etag_304 = result.status == 304;
                // Booking rules verbatim from docs/0050 (D1): REST = 1
                // point per completed flight, 304 revalidations and the
                // free probe = 0. GraphQL below prefers the body's own
                // `rateLimit.cost`.
                let (cost, remaining, limit, cycle) = match transport {
                    Transport::Graphql => match graphql_flight_cost(result.body.as_deref()) {
                        Some((cost, remaining, reset, limit)) => {
                            (Some(cost), Some(remaining), limit, Some(reset))
                        }
                        None => (
                            None,
                            result.rate.map(|r| r.remaining),
                            result.rate.map(|r| r.limit),
                            result.rate.map(|r| r.reset_epoch),
                        ),
                    },
                    _ => {
                        let cost = if self.free || etag_304 {
                            Some(0)
                        } else {
                            Some(1)
                        };
                        (
                            cost,
                            result.rate.map(|r| r.remaining),
                            // docs/0054 D14: the ceiling rides the same
                            // header snapshot as `remaining` — auth
                            // endpoints send no rate headers, so auth
                            // flights are `None` for free.
                            result.rate.map(|r| r.limit),
                            result.rate.map(|r| r.reset_epoch),
                        )
                    }
                };
                // docs/0054 D14: payload size is the other half of "why
                // was this slow" — `None` exactly when there is no
                // body (304 revalidations).
                let resp_bytes = result
                    .body
                    .as_ref()
                    .map(|b| b.len().min(u32::MAX as usize) as u32);
                let mut draft = RecordDraft::new(
                    transport,
                    SharedString::from(self.op),
                    SharedString::from(self.origin.surface.as_str()),
                    SharedString::from(self.origin.trigger.as_str()),
                    self.ident.clone(),
                );
                draft.status = result.status;
                draft.etag_304 = etag_304;
                draft.cost = cost;
                draft.remaining_after = remaining;
                draft.limit_after = limit;
                draft.cycle_reset_epoch = cycle;
                draft.duration_ms = duration_ms;
                draft.queue_ms = queue_ms;
                draft.resp_bytes = resp_bytes;
                draft
            }
        }
    }
}

impl Http for RecordingHttp<'_> {
    fn request(
        &self,
        verb: Verb,
        url: &str,
        body: Option<&str>,
        etag: Option<&str>,
    ) -> Result<HttpResult, HttpError> {
        let sent_at = Instant::now();
        let queue_ms = self.queue_ms_at(sent_at);
        let res = self.inner.request(verb, url, body, etag);
        self.recorder
            .emit(self.draft(Transport::Rest, &res, sent_at, queue_ms));
        res
    }

    fn post_graphql(
        &self,
        query: &str,
        variables: &serde_json::Value,
    ) -> Result<HttpResult, HttpError> {
        let sent_at = Instant::now();
        let queue_ms = self.queue_ms_at(sent_at);
        let res = self.inner.post_graphql(query, variables);
        self.recorder
            .emit(self.draft(Transport::Graphql, &res, sent_at, queue_ms));
        res
    }
}

/// The GraphQL body's `rateLimit` object for records (docs/0053 D1,
/// docs/0054 D14): `(cost, remaining, resetAt→epoch, limit)` when the
/// query asked for the field — the authoritative per-request cost,
/// plus the response's own ceiling. Tolerates selection sets that omit
/// `limit` (older queries, minimal probes): a missing field must not
/// void the cost — that drift once made every real flight
/// unattributable (docs/0053 §7, drift fix). Mirrors
/// [`graphql_budget`]'s parsing contract: only the fields every query
/// carries are required.
fn graphql_flight_cost(body: Option<&str>) -> Option<(u32, u32, u64, Option<u32>)> {
    #[derive(serde::Deserialize)]
    struct RateLimitField {
        cost: i64,
        remaining: i64,
        #[serde(rename = "resetAt")]
        reset_at: String,
        limit: Option<i64>,
    }
    #[derive(serde::Deserialize)]
    struct Data {
        #[serde(rename = "rateLimit")]
        rate_limit: Option<RateLimitField>,
    }
    #[derive(serde::Deserialize)]
    struct Envelope {
        data: Option<Data>,
    }
    let env: Envelope = serde_json::from_str(body?).ok()?;
    let rl = env.data?.rate_limit?;
    let reset = chrono::DateTime::parse_from_rfc3339(&rl.reset_at)
        .ok()?
        .timestamp()
        .max(0) as u64;
    Some((
        rl.cost.max(0) as u32,
        rl.remaining.max(0) as u32,
        reset,
        rl.limit.map(|l| l.max(0) as u32),
    ))
}

// ---------------------------------------------------------------------------
// The counting seam (D2) — one decorator around the single client.
// ---------------------------------------------------------------------------

/// Wraps the real `dyn Http` (docs/0050 D2): every transport — REST
/// pages, GraphQL, one-shot GETs, mutations, the auth flows — borrows
/// `state.client`, so recording here sees 100% of GitHub-bound traffic
/// with zero call-site changes. Sync, background-thread only; the two
/// short mutex critical sections never span an await (there are none).
pub(crate) struct CountingHttp {
    inner: Arc<dyn Http>,
    ledger: UsageLedger,
}

impl CountingHttp {
    /// Wrap and erase in one step — the constructor site in `lib.rs`
    /// hands the result straight into `GithubState`.
    pub(crate) fn wrap(inner: Arc<dyn Http>, ledger: UsageLedger) -> Arc<dyn Http> {
        Arc::new(Self { inner, ledger })
    }

    fn start(&self, bucket: Bucket, graphql: bool) {
        self.ledger
            .lock()
            .unwrap()
            .on_request_start(bucket, graphql);
    }

    fn finish(&self, bucket: Bucket, graphql: bool, res: &Result<HttpResult, HttpError>) {
        let mut book = self.ledger.lock().unwrap();
        match res {
            Ok(result) => {
                // D1: GraphQL body `rateLimit` when present, else the
                // harvested headers — either way this response reports
                // the graphql bucket, never `core`.
                let snapshot = if graphql {
                    graphql_budget(result.body.as_deref(), result.rate)
                        .or_else(|| result.rate.map(Into::into))
                } else {
                    result.rate.map(Into::into)
                };
                book.on_response(bucket, graphql, snapshot, budget_now());
            }
            Err(_) => book.on_failure(bucket, graphql),
        }
    }
}

impl Http for CountingHttp {
    fn request(
        &self,
        verb: Verb,
        url: &str,
        body: Option<&str>,
        etag: Option<&str>,
    ) -> Result<HttpResult, HttpError> {
        let bucket = Bucket::classify(url);
        self.start(bucket, false);
        let res = self.inner.request(verb, url, body, etag);
        self.finish(bucket, false, &res);
        res
    }

    fn post_graphql(
        &self,
        query: &str,
        variables: &serde_json::Value,
    ) -> Result<HttpResult, HttpError> {
        self.start(Bucket::Primary, true);
        let res = self.inner.post_graphql(query, variables);
        self.finish(Bucket::Primary, true, &res);
        res
    }
}

// ---------------------------------------------------------------------------
// Tests (docs/0050 D6) — the pure book is the bulk; the seam is exercised
// over the scripted mock transport.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // NOTE: no `use super::*` here — usage.rs glob-imports `gpui::*`,
    // whose `test` re-export would shadow the builtin `#[test]`
    // attribute and blow the proc-macro stack (rustc SIGSEGV; the
    // page.rs tests note). Import selectively.
    use super::{
        Bucket, BudgetSnapshot, CountingHttp, UsageBook, UsageDisplay, UsageLedger,
        parse_rate_limit_body,
    };
    use crate::http::{HttpError, HttpResult, RateLimit, Verb};
    use std::sync::{Arc, Mutex};

    fn snap(limit: u32, remaining: u32, reset_at: u64) -> BudgetSnapshot {
        BudgetSnapshot {
            limit,
            remaining,
            reset_at,
        }
    }

    fn budget(
        rest: Option<(u32, u32, u64, bool)>,
        graphql: Option<(u32, u32, u64, bool)>,
    ) -> UsageDisplay {
        let conv = |(remaining, limit, resets_in_secs, stale): (u32, u32, u64, bool)| {
            super::BucketDisplay {
                remaining,
                limit,
                resets_in_secs,
                stale,
                snapshot_at: None,
            }
        };
        UsageDisplay::Budget {
            rest: rest.map(conv),
            graphql: graphql.map(conv),
        }
    }

    /// Drop `snapshot_at` (per-test wall-clock) before comparing — the
    /// stale path tests it explicitly where it matters.
    fn stripped(display: UsageDisplay) -> UsageDisplay {
        match display {
            UsageDisplay::Budget { rest, graphql } => UsageDisplay::Budget {
                rest: rest.map(|mut d| {
                    d.snapshot_at = None;
                    d
                }),
                graphql: graphql.map(|mut d| {
                    d.snapshot_at = None;
                    d
                }),
            },
            other => other,
        }
    }

    // --- book: counting + reconciliation ---------------------------------

    #[test]
    fn no_snapshot_displays_pending() {
        let book = UsageBook::default();
        assert_eq!(book.display(1_000), UsageDisplay::Pending);
    }

    #[test]
    fn rest_and_graphql_snapshots_stay_separate() {
        // Two independent server counters (D1): a REST response carries
        // the `core` bucket, a GraphQL response the `graphql` bucket —
        // never mixed, and their windows tick independently.
        let mut book = UsageBook::default();
        book.on_request_start(Bucket::Primary, false);
        book.on_response(
            Bucket::Primary,
            false,
            Some(snap(5000, 3_700, 2_780)),
            1_000,
        );
        book.on_request_start(Bucket::Primary, true);
        book.on_response(Bucket::Primary, true, Some(snap(5000, 4_950, 2_570)), 1_000);
        assert_eq!(
            stripped(book.display(2_000)),
            budget(
                Some((3_700, 5000, 780, false)),
                Some((4_950, 5000, 570, false)),
            )
        );
        // A later REST response replaces the REST snapshot only.
        book.on_request_start(Bucket::Primary, false);
        book.on_response(
            Bucket::Primary,
            false,
            Some(snap(5000, 3_690, 2_780)),
            1_500,
        );
        assert_eq!(
            stripped(book.display(2_000)),
            budget(
                Some((3_690, 5000, 780, false)),
                Some((4_950, 5000, 570, false)),
            )
        );
    }

    #[test]
    fn one_bucket_reporting_alone_still_displays() {
        let mut book = UsageBook::default();
        book.on_request_start(Bucket::Primary, true);
        book.on_response(Bucket::Primary, true, Some(snap(5000, 4_999, 2_000)), 1_000);
        assert_eq!(
            stripped(book.display(1_200)),
            budget(None, Some((4_999, 5000, 800, false)))
        );
    }

    #[test]
    fn display_shows_server_numbers_verbatim() {
        let mut book = UsageBook::default();
        book.on_response(Bucket::Primary, false, Some(snap(5000, 5, 2_000)), 1_000);
        book.on_response(Bucket::Primary, true, Some(snap(5000, 2, 2_000)), 1_000);
        // In-flight starts are bookkept (session counters, sub-book
        // slots) but the shown values stay verbatim — no prediction
        // is folded into the display (owner decision, #323 review).
        book.on_request_start(Bucket::Primary, false);
        book.on_request_start(Bucket::Primary, false);
        book.on_request_start(Bucket::Primary, true);
        assert_eq!(
            stripped(book.display(1_100)),
            budget(Some((5, 5000, 900, false)), Some((2, 5000, 900, false)))
        );

        // An answer without rate info changes nothing shown — the
        // last snapshot stands until a carrying response replaces it.
        book.on_response(Bucket::Primary, true, None, 1_150);
        assert_eq!(
            stripped(book.display(1_200)),
            budget(Some((5, 5000, 800, false)), Some((2, 5000, 800, false)))
        );
        // A REST transport failure clears one bookkept slot; the
        // shown values still mirror the last snapshot verbatim.
        book.on_failure(Bucket::Primary, false);
        assert_eq!(
            stripped(book.display(1_300)),
            budget(Some((5, 5000, 700, false)), Some((2, 5000, 700, false)))
        );
    }

    #[test]
    fn passed_reset_marks_only_that_bucket_stale() {
        let mut book = UsageBook::default();
        book.on_response(Bucket::Primary, false, Some(snap(5000, 120, 2_000)), 1_000);
        book.on_response(Bucket::Primary, true, Some(snap(5000, 4_940, 3_000)), 1_000);
        // Past the REST window: REST goes stale, GraphQL does not.
        let UsageDisplay::Budget { rest, graphql } = book.display(2_100) else {
            panic!("expected a budget display");
        };
        let rest = rest.expect("rest snapshot");
        let graphql = graphql.expect("graphql snapshot");
        assert!(rest.stale);
        assert_eq!(rest.resets_in_secs, 0);
        assert!(!graphql.stale);
        assert_eq!(graphql.resets_in_secs, 900);
    }

    #[test]
    fn session_counters_split_by_bucket_and_lane() {
        let mut book = UsageBook::default();
        book.on_request_start(Bucket::Primary, true);
        book.on_request_start(Bucket::Primary, true);
        book.on_request_start(Bucket::Primary, false);
        book.on_request_start(Bucket::Auth, false);
        book.on_request_start(Bucket::Other, false);
        assert_eq!(book.session_counters(), (5, 2, 1, 1));
        // Seed both sub-books with fresh snapshots; the in-flight
        // starts are bookkept but shown values stay verbatim.
        book.seed_snapshots(snap(5000, 5_000, 2_000), snap(5000, 5_000, 2_000), 1_000);
        assert_eq!(
            stripped(book.display(1_100)),
            budget(
                Some((5_000, 5000, 900, false)),
                Some((5_000, 5000, 900, false)),
            )
        );
        // Auth/other requests never touch either primary book.
        book.on_failure(Bucket::Auth, false);
        assert_eq!(
            stripped(book.display(1_100)),
            budget(
                Some((5_000, 5000, 900, false)),
                Some((5_000, 5000, 900, false))
            )
        );
        // GraphQL answers without rate info change nothing shown.
        book.on_response(Bucket::Primary, true, None, 1_200);
        book.on_response(Bucket::Primary, true, None, 1_200);
        assert_eq!(
            stripped(book.display(1_300)),
            budget(
                Some((5_000, 5000, 700, false)),
                Some((5_000, 5000, 700, false))
            )
        );
    }

    #[test]
    fn reset_wipes_everything_for_the_next_account() {
        let mut book = UsageBook::default();
        book.on_request_start(Bucket::Primary, true);
        book.on_response(Bucket::Primary, true, Some(snap(5000, 1, 2_000)), 1_000);
        book.on_response(Bucket::Primary, false, Some(snap(5000, 2, 2_000)), 1_000);
        book.mark_probe(1_000);
        book.reset();
        assert_eq!(book.display(1_100), UsageDisplay::Pending);
        assert_eq!(book.session_counters(), (0, 0, 0, 0));
        assert!(book.probe_due(1_100)); // cooldown wiped too
    }

    // --- book: probe arming (D3) ------------------------------------------

    #[test]
    fn probe_due_tracks_the_rest_bucket_only() {
        let mut book = UsageBook::default();
        assert!(book.probe_due(1_000)); // nothing seen yet

        // A REST response alone satisfies the probe: a missing GraphQL
        // book never arms it (the probe cannot refresh GQL — §2).
        book.on_response(Bucket::Primary, false, Some(snap(5000, 5, 12_000)), 1_000);
        assert!(!book.probe_due(1_500));

        // GraphQL went stale; REST still fresh — quiet.
        book.on_response(Bucket::Primary, true, Some(snap(5000, 5, 2_500)), 1_600);
        assert!(!book.probe_due(5_000));

        // The REST window rolling re-arms it.
        assert!(book.probe_due(12_000));

        // Cooldown: one probe per 30 s, even while stale.
        book.mark_probe(12_000);
        assert!(!book.probe_due(12_029));
        assert!(book.probe_due(12_030));
    }

    #[test]
    fn probe_folds_core_and_never_touches_graphql() {
        // GraphQL state arrives first from traffic.
        let mut book = UsageBook::default();
        book.on_response(Bucket::Primary, true, Some(snap(5000, 29, 2_570)), 1_000);

        // The probe folds `resources.core` only.
        book.fold_probe(snap(5000, 3_712, 2_780), 2_000);
        assert_eq!(
            stripped(book.display(2_000)),
            budget(
                Some((3_712, 5000, 780, false)),
                Some((29, 5000, 570, false)), // untouched — traffic's number
            )
        );

        // A later probe re-folds core; graphql still never moves.
        book.fold_probe(snap(5000, 3_700, 2_780), 2_500);
        assert_eq!(
            stripped(book.display(2_500)),
            budget(Some((3_700, 5000, 280, false)), Some((29, 5000, 70, false)),)
        );
    }

    // --- seam: CountingHttp over the mock (D2/D6) --------------------------

    fn http_result(
        status: u16,
        rate: Option<RateLimit>,
        body: Option<&str>,
    ) -> Result<HttpResult, HttpError> {
        Ok(HttpResult {
            status,
            etag: Some("\"a\"".into()),
            rate,
            body: body.map(str::to_string),
        })
    }

    fn rate(limit: u32, remaining: u32, reset_epoch: u64) -> Option<RateLimit> {
        Some(RateLimit {
            limit,
            remaining,
            reset_epoch,
        })
    }

    #[test]
    fn wrapper_routes_headers_to_their_own_bucket() {
        // A REST response reporting 3,700 and a GraphQL response
        // reporting 4,950 land in separate buckets (the flip-flop bug
        // this structure exists to prevent).
        let mock = Arc::new(crate::http::mock::MockHttp::new(vec![
            http_result(200, rate(5000, 3_700, 2_780), Some("[]")),
            http_result(200, None, Some("{}")),
        ]));
        let ledger: UsageLedger = Arc::new(Mutex::new(UsageBook::default()));
        let http = CountingHttp::wrap(mock, ledger.clone());

        http.request(
            Verb::Get,
            "https://api.github.com/notifications?per_page=100&page=1",
            None,
            None,
        )
        .unwrap();
        http.post_graphql("{ q }", &serde_json::json!({})).unwrap();

        let book = ledger.lock().unwrap();
        assert_eq!(book.session_counters(), (2, 1, 1, 0));
        assert_eq!(
            stripped(book.display(2_000)),
            budget(
                Some((3_700, 5000, 780, false)),
                None, // GraphQL answer carried no rate info
            )
        );
    }

    #[test]
    fn wrapper_prefers_graphql_body_rate_limit_over_headers() {
        let body = serde_json::json!({
            "data": { "rateLimit": { "cost": 1, "remaining": 4310, "resetAt": "2030-01-01T00:00:00Z" } }
        })
        .to_string();
        let mock = Arc::new(crate::http::mock::MockHttp::new(vec![http_result(
            200,
            rate(5000, 9999, 9_999), // headers disagree on purpose
            Some(&body),
        )]));
        let ledger: UsageLedger = Arc::new(Mutex::new(UsageBook::default()));
        CountingHttp::wrap(mock, ledger.clone())
            .post_graphql("{ q }", &serde_json::json!({}))
            .unwrap();

        let book = ledger.lock().unwrap();
        let UsageDisplay::Budget { rest, graphql } = book.display(1_000) else {
            panic!("expected a budget display");
        };
        assert_eq!(rest, None); // no REST traffic — core bucket untouched
        let graphql = graphql.expect("graphql snapshot");
        // Body wins for remaining/resetAt; headers fill the limit.
        assert_eq!(graphql.remaining, 4310);
        assert_eq!(graphql.limit, 5000);
        assert_eq!(book.session_counters(), (1, 1, 0, 0));
    }

    #[test]
    fn graphql_flight_cost_parses_real_selection_sets() {
        // What every query actually asks for (docs/0053 §5): cost,
        // remaining, resetAt — no limit. The cost must still book.
        let body = serde_json::json!({
            "data": {
                "rateLimit": { "cost": 1, "remaining": 4310, "resetAt": "2030-01-01T00:00:00Z" }
            }
        })
        .to_string();
        assert_eq!(
            super::graphql_flight_cost(Some(&body)),
            Some((1, 4310, 1_893_456_000, None))
        );
        // With the full selection set (docs/0054 D14): the ceiling rides.
        let body = serde_json::json!({
            "data": {
                "rateLimit": { "cost": 27, "remaining": 4310, "resetAt": "2030-01-01T00:00:00Z", "limit": 5000 }
            }
        })
        .to_string();
        assert_eq!(
            super::graphql_flight_cost(Some(&body)),
            Some((27, 4310, 1_893_456_000, Some(5000)))
        );
        // No rateLimit in the body → unattributable, never a guess.
        assert_eq!(super::graphql_flight_cost(Some(r#"{"data": {}}"#)), None);
        assert_eq!(super::graphql_flight_cost(None), None);
    }

    #[test]
    fn auth_and_unknown_hosts_never_touch_primary_books() {
        let mock = Arc::new(crate::http::mock::MockHttp::new(vec![
            http_result(200, rate(5000, 4_997, 2_000), Some("ok")), // auth bucket headers
            http_result(200, None, Some("ok")),                     // unknown host
        ]));
        let ledger: UsageLedger = Arc::new(Mutex::new(UsageBook::default()));
        let http = CountingHttp::wrap(mock, ledger.clone());
        http.request(
            Verb::Post,
            "https://github.com/login/device/code",
            Some("c=1"),
            None,
        )
        .unwrap();
        http.request(Verb::Get, "https://example.com/x", None, None)
            .unwrap();
        let book = ledger.lock().unwrap();
        // Auth POST → auth lane; example.com → Other (no lane);
        // no primary REST traffic at all.
        assert_eq!(book.session_counters(), (2, 0, 0, 1));
        assert_eq!(book.display(1_000), UsageDisplay::Pending);
    }

    #[test]
    fn wrapper_failure_clears_the_inflight_slot() {
        let mock = Arc::new(crate::http::mock::MockHttp::new(vec![Err(HttpError(
            "dns".into(),
        ))]));
        let ledger: UsageLedger = Arc::new(Mutex::new(UsageBook::default()));
        let http = CountingHttp::wrap(mock, ledger.clone());
        assert!(
            http.request(Verb::Get, "https://api.github.com/rate_limit", None, None)
                .is_err()
        );
        let book = ledger.lock().unwrap();
        assert_eq!(book.session_counters(), (1, 0, 1, 0)); // primary REST lane
        assert_eq!(book.display(1_000), UsageDisplay::Pending);
    }

    // --- probe body + formatters -------------------------------------------

    #[test]
    fn rate_limit_body_decodes_core_and_ignores_phantom_graphql() {
        let body = serde_json::json!({
            "resources": {
                "core": { "limit": 5000, "remaining": 3712, "reset": 1_893_456_000 },
                "graphql": { "limit": 5000, "remaining": 5000, "reset": 1_893_454_800 },
                "search": { "limit": 30, "remaining": 30, "reset": 1 }
            }
        })
        .to_string();
        // The phantom graphql entry (always 5,000/0-used live) is
        // deliberately ignored — only core comes out.
        assert_eq!(
            parse_rate_limit_body(&body),
            Some(snap(5000, 3_712, 1_893_456_000))
        );
        let core_only = serde_json::json!({
            "resources": { "core": { "limit": 5000, "remaining": 3712, "reset": 1_893_456_000 } }
        })
        .to_string();
        assert_eq!(
            parse_rate_limit_body(&core_only),
            Some(snap(5000, 3_712, 1_893_456_000))
        );
        // No core sub-object → nothing to fold.
        let gql_only = serde_json::json!({
            "resources": { "graphql": { "limit": 5000, "remaining": 5000, "reset": 1 } }
        })
        .to_string();
        assert_eq!(parse_rate_limit_body(&gql_only), None);
        assert_eq!(parse_rate_limit_body("[]"), None);
    }
}
