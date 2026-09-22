//! API-usage calculator (docs/0050): count every GitHub-bound request,
//! account for it the way GitHub does, and show the budget in a footer
//! chip while the GitHub module is on screen.
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
//! separate windows — each response reports its own API's bucket, and
//! the chip shows both remaining values (`GH 3,712 · GQL 4,950`), with
//! the `x/y` + per-window detail in a hover tooltip and the spoken
//! label. A GraphQL value shows `?` until the first GraphQL response
//! lands: `GET /rate_limit`'s `resources.graphql` is a phantom
//! (always 5,000/0-used, §2) and can never supply it.
//!
//! Sections: the pure book (D3, unit-tested without gpui) → the
//! counting seam (D2, a decorator around `dyn Http`) → the footer chip
//! (D5, a self-refreshing view contributed through
//! `ModuleEntry::status_item`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gpui::*;
use gpui_component::{
    ActiveTheme as _, IconName, Sizable as _,
    button::{Button, ButtonVariants},
    h_flex, v_flex,
};

use dashboard_core::nav::{Navigator, Page};
use dashboard_core::usage_records::{RecordDraft, Recorder, Transport};

use crate::auth::now_epoch;
use crate::http::{Http, HttpError, HttpResult, RateLimit, Verb};
use crate::service::GithubState;

/// Shared ledger handle — the `SharedToken` precedent (docs/0050 D2): a
/// small cross-thread slot between the background request path (the
/// wrapper writes) and the UI (the chip reads).
pub(crate) type UsageLedger = Arc<Mutex<UsageBook>>;

/// A fresh, empty ledger handle (a free fn — `Arc` admits no inherent
/// impls).
pub(crate) fn new_ledger() -> UsageLedger {
    Arc::new(Mutex::new(UsageBook::default()))
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
    /// has not reported yet (rendered as an ellipsis).
    Budget {
        rest: Option<BucketDisplay>,
        graphql: Option<BucketDisplay>,
    },
    /// The operational backoff (`rate_limited_until`) is in future —
    /// shown over any snapshot (0050 D4).
    Exhausted { resumes_in_secs: u64 },
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

    /// The single display derivation (D3 table). `rate_limited_until`
    /// is read, never written, here — it stays the operational truth
    /// (D4).
    pub(crate) fn display(&self, now: u64, rate_limited_until: Option<u64>) -> UsageDisplay {
        if let Some(until) = rate_limited_until {
            if until > now {
                return UsageDisplay::Exhausted {
                    resumes_in_secs: until - now,
                };
            }
        }
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
            Err(_) => RecordDraft {
                transport,
                op: SharedString::from(self.op),
                surface: SharedString::from(self.origin.surface.as_str()),
                trigger: SharedString::from(self.origin.trigger.as_str()),
                ident: self.ident.clone(),
                status: 0,
                etag_304: false,
                // Network errors are not attributable — never a guessed
                // number (D1); a timed-out flight's server-side cost is
                // unknowable client-side.
                cost: None,
                remaining_after: None,
                // No response → no ceiling, no body (docs/0054 D14).
                // The queue wait still happened and stays measurable.
                limit_after: None,
                cycle_reset_epoch: None,
                duration_ms,
                queue_ms,
                resp_bytes: None,
                network_error: true,
            },
            Ok(result) => {
                let etag_304 = result.status == 304;
                // Booking rules verbatim from docs/0050 (D1): REST = 1
                // point per completed flight, 304 revalidations and the
                // free probe = 0. GraphQL below prefers the body's own
                // `rateLimit.cost`.
                let (cost, remaining, limit, cycle) = match transport {
                    Transport::Graphql => match graphql_flight_cost(result.body.as_deref()) {
                        Some((cost, remaining, reset, limit)) => {
                            (Some(cost), Some(remaining), Some(limit), Some(reset))
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
                RecordDraft {
                    transport,
                    op: SharedString::from(self.op),
                    surface: SharedString::from(self.origin.surface.as_str()),
                    trigger: SharedString::from(self.origin.trigger.as_str()),
                    ident: self.ident.clone(),
                    status: result.status,
                    etag_304,
                    cost,
                    remaining_after: remaining,
                    limit_after: limit,
                    cycle_reset_epoch: cycle,
                    duration_ms,
                    queue_ms,
                    resp_bytes,
                    network_error: false,
                }
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
/// plus the response's own ceiling. Mirrors
/// [`graphql_budget`]'s parsing contract.
fn graphql_flight_cost(body: Option<&str>) -> Option<(u32, u32, u64, u32)> {
    #[derive(serde::Deserialize)]
    struct RateLimitField {
        cost: i64,
        remaining: i64,
        #[serde(rename = "resetAt")]
        reset_at: String,
        limit: i64,
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
        rl.limit.max(0) as u32,
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
                book.on_response(bucket, graphql, snapshot, now_epoch());
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
// The footer chip (D5) — contributed through `ModuleEntry::status_item`.
// ---------------------------------------------------------------------------

/// Self-refreshing status chip (docs/0050 D5): renders the display
/// derivation of the shared book, and re-renders only when its content
/// actually changes. Created once in `lib.rs::init`; the status-item
/// closure shows it only while the GitHub module page is active.
pub(crate) struct UsageChip {
    state: Entity<GithubState>,
    usage: UsageLedger,
    /// The chip → page hook (docs/0054 §1): the details affordance
    /// navigates to the analytics page. The chip's own popover is
    /// unchanged — this only adds the way in.
    nav: Entity<Navigator>,
}

impl UsageChip {
    pub fn new(
        state: Entity<GithubState>,
        usage: UsageLedger,
        nav: Entity<Navigator>,
        cx: &mut Context<Self>,
    ) -> Self {
        // 1 Hz self-refresh (D5): one short mutex read per second; the
        // notify fires only on an actual content change, so a stable
        // budget draws nothing. The chip is captured for the app's
        // lifetime by the status-item closure, so this loop never
        // outlives anything it reads.
        let timer_state = state.clone();
        let timer_usage = usage.clone();
        cx.spawn(async move |this, cx| {
            let mut last_key: Option<String> = None;
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let key = cx.update(|cx| {
                    let rate_limited_until = timer_state.read(cx).rate_limited_until;
                    let book = timer_usage.lock().unwrap();
                    format!("{:?}", book.display(now_epoch(), rate_limited_until))
                });
                if last_key.as_deref() != Some(key.as_str()) {
                    last_key = Some(key);
                    let _ = this.update(cx, |_, cx| cx.notify());
                }
            }
        })
        .detach();
        Self { state, usage, nav }
    }

    fn current_display(&self, cx: &App) -> (UsageDisplay, (u64, u64, u64, u64)) {
        let rate_limited_until = self.state.read(cx).rate_limited_until;
        let book = self.usage.lock().unwrap();
        (
            book.display(now_epoch(), rate_limited_until),
            book.session_counters(),
        )
    }
}

/// Thousands-group a number for the chip body (`4,821`) — the D5 copy.
fn group_thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Compact countdown for the chip body (`41m`, `1h 05m`) — the D5 copy.
fn compact_duration(secs: u64) -> String {
    let m = secs / 60;
    if m < 60 {
        format!("{m}m")
    } else {
        format!("{}h {:02}m", m / 60, m % 60)
    }
}

/// Spoken countdown for the a11y label (`41 minutes`, `1 hour 5 minutes`).
fn spoken_duration(secs: u64) -> String {
    let m = secs / 60;
    if m < 60 {
        format!("{m} minutes")
    } else {
        format!("{} hours {} minutes", m / 60, m % 60)
    }
}

/// `14:03` from an epoch — the stale display's "as of" wall clock.
fn local_hhmm(epoch: u64) -> String {
    use chrono::TimeZone as _;
    chrono::Local
        .timestamp_opt(epoch as i64, 0)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_else(|| "??:??".into())
}

/// One bucket's chip-body fragment: remaining only (`3,712`) — the
/// limits and windows live in the hover tooltip and the spoken label
/// (D5).
fn bucket_short(d: Option<BucketDisplay>) -> String {
    match d {
        None => "…".to_string(),
        Some(d) => group_thousands(d.remaining),
    }
}

/// The chip body from a display derivation: `GH 3,712 · GQL 4,950` —
/// remaining values only (D5). A REST bucket with no snapshot yet
/// shows `…`; the GraphQL bucket shows `?` until the first real
/// GraphQL response lands (the probe cannot supply it — §2).
fn chip_body(display: &UsageDisplay) -> (String, bool) {
    match display {
        UsageDisplay::Pending => ("GH … · GQL ?".to_string(), false),
        UsageDisplay::Budget { rest, graphql } => (
            format!(
                "GH {} · GQL {}",
                bucket_short(*rest),
                graphql.map_or_else(|| "?".to_string(), |d| group_thousands(d.remaining))
            ),
            false,
        ),
        UsageDisplay::Exhausted { resumes_in_secs } => (
            format!(
                "GH rate-limited · resumes {}",
                compact_duration(*resumes_in_secs)
            ),
            true,
        ),
    }
}

/// One bucket's hover-tooltip line: `GH 3,712/5,000 resets in 13m` —
/// the line a stale window trades for its "as of" clock, since its
/// countdown already ran out (D5). `missing` is the no-snapshot
/// placeholder — `…/…` for REST, `?` for the probe-unservable GQL.
fn tooltip_line(name: &str, missing: &str, d: Option<BucketDisplay>) -> String {
    match d {
        None => format!("{name} {missing}"),
        Some(d) => {
            let head = format!(
                "{name} {}/{}",
                group_thousands(d.remaining),
                group_thousands(d.limit)
            );
            if d.stale {
                match d.snapshot_at {
                    Some(at) => format!("{head} · as of {}", local_hhmm(at)),
                    None => head,
                }
            } else {
                format!("{head} resets in {}", compact_duration(d.resets_in_secs))
            }
        }
    }
}

/// One bucket's spoken fragment for the a11y label.
fn bucket_label(name: &str, unit: &str, d: Option<BucketDisplay>) -> String {
    match d {
        None => format!("{name}: no data yet"),
        Some(d) => {
            let mut out = format!(
                "{name}: {} of {} {} remaining, resets in {}",
                group_thousands(d.remaining),
                group_thousands(d.limit),
                unit,
                spoken_duration(d.resets_in_secs)
            );
            if d.stale {
                if let Some(at) = d.snapshot_at {
                    out = format!("{out}, last updated {}", local_hhmm(at));
                }
            }
            out
        }
    }
}

impl Render for UsageChip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (display, counters) = self.current_display(cx);
        let (total, graphql, rest, auth) = counters;
        let session =
            format!("session: {total} requests ({graphql} GraphQL, {rest} REST, {auth} auth)");

        // Spoken form per D5 — the sole a11y carrier (a hover tooltip
        // is mouse-only): both sub-budgets spelled out with their own
        // windows and units, plus session counters and staleness.
        let label = match &display {
            UsageDisplay::Pending => format!("GitHub API usage pending; {session}"),
            UsageDisplay::Budget { rest, graphql } => format!(
                "GitHub API: {}; {}; {session}",
                bucket_label("REST", "requests", *rest),
                bucket_label("GraphQL", "points", *graphql)
            ),
            UsageDisplay::Exhausted { resumes_in_secs } => format!(
                "GitHub API rate limited, fetches resume in {}; {session}",
                spoken_duration(*resumes_in_secs)
            ),
        };
        let (body, warning) = chip_body(&display);

        let color = if warning {
            cx.theme().warning
        } else {
            cx.theme().muted_foreground
        };
        // The hover tooltip (D5): built fresh at hover time from the
        // same ledger — `GH x/y resets in Zm` / `GQL x/y resets in Zm`.
        // It is deliberately additive detail for sighted mouse users;
        // the aria label above stays the sole a11y carrier (0049).
        let tooltip_usage = self.usage.clone();
        // The details affordance (docs/0054 §1): a small icon button
        // beside the chip body — the chip's numbers stay one glance;
        // "what does this mean over time" is a navigation away.
        let nav = self.nav.clone();
        let chip = h_flex()
            .gap_1()
            .id("github-usage")
            .role(accesskit::Role::Label)
            .aria_label(label)
            .text_color(color)
            .child(SharedString::from(body))
            .tooltip(move |_window, cx| {
                // Read the book as it is when the tooltip opens; the
                // rate-limit pause is passed `None` so the tooltip shows
                // the budget detail even while the chip reads Exhausted.
                let book = tooltip_usage.lock().unwrap();
                let display = book.display(now_epoch(), None);
                let lines = match display {
                    UsageDisplay::Budget { rest, graphql } => {
                        vec![
                            tooltip_line("GH", "…/…", rest),
                            tooltip_line("GQL", "?", graphql),
                        ]
                    }
                    _ => vec!["GH …/…".to_string(), "GQL …/…".to_string()],
                };
                cx.new(|_| UsageTooltip { lines }).into()
            });
        h_flex().gap_1().items_center().child(chip).child(
            Button::new("github-usage-details")
                .ghost()
                .small()
                .icon(IconName::ChartPie)
                .accessibility_label("Usage analytics")
                .tooltip("Usage analytics — cards, burn-down and latency trends")
                .on_click(move |_, _, cx| {
                    nav.update(cx, |nav, cx| nav.go(Page::Usage, cx));
                }),
        )
    }
}

/// The chip's hover tooltip (D5): two lines, one per primary budget.
struct UsageTooltip {
    lines: Vec<String>,
}

impl Render for UsageTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (popover, foreground, border) = (theme.popover, theme.popover_foreground, theme.border);
        v_flex()
            .id("github-usage-tooltip")
            .role(accesskit::Role::Label)
            .aria_label(self.lines.join("; "))
            .bg(popover)
            .text_color(foreground)
            .border_1()
            .border_color(border)
            .rounded_sm()
            .px_2()
            .py_1()
            .gap_1()
            .children(self.lines.iter().cloned().map(SharedString::from))
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
        Bucket, BudgetSnapshot, CountingHttp, UsageBook, UsageDisplay, UsageLedger, bucket_label,
        chip_body, compact_duration, group_thousands, parse_rate_limit_body, spoken_duration,
        tooltip_line,
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
        assert_eq!(book.display(1_000, None), UsageDisplay::Pending);
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
            stripped(book.display(2_000, None)),
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
            stripped(book.display(2_000, None)),
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
            stripped(book.display(1_200, None)),
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
            stripped(book.display(1_100, None)),
            budget(Some((5, 5000, 900, false)), Some((2, 5000, 900, false)))
        );

        // An answer without rate info changes nothing shown — the
        // last snapshot stands until a carrying response replaces it.
        book.on_response(Bucket::Primary, true, None, 1_150);
        assert_eq!(
            stripped(book.display(1_200, None)),
            budget(Some((5, 5000, 800, false)), Some((2, 5000, 800, false)))
        );
        // A REST transport failure clears one bookkept slot; the
        // shown values still mirror the last snapshot verbatim.
        book.on_failure(Bucket::Primary, false);
        assert_eq!(
            stripped(book.display(1_300, None)),
            budget(Some((5, 5000, 700, false)), Some((2, 5000, 700, false)))
        );
    }

    #[test]
    fn passed_reset_marks_only_that_bucket_stale() {
        let mut book = UsageBook::default();
        book.on_response(Bucket::Primary, false, Some(snap(5000, 120, 2_000)), 1_000);
        book.on_response(Bucket::Primary, true, Some(snap(5000, 4_940, 3_000)), 1_000);
        // Past the REST window: REST goes stale, GraphQL does not.
        let UsageDisplay::Budget { rest, graphql } = book.display(2_100, None) else {
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
    fn rate_limited_backoff_wins_over_any_snapshot() {
        // D4: the operational pause is global today — an exhausted
        // bucket halts both lanes; the display defers to it.
        let mut book = UsageBook::default();
        book.on_response(Bucket::Primary, false, Some(snap(5000, 0, 9_000)), 1_000);
        book.on_response(Bucket::Primary, true, Some(snap(5000, 4_000, 9_000)), 1_000);
        assert_eq!(
            stripped(book.display(2_000, Some(5_000))),
            UsageDisplay::Exhausted {
                resumes_in_secs: 3_000
            }
        );
        // An expired backoff defers to the snapshots again.
        assert_eq!(
            stripped(book.display(5_001, Some(5_000))),
            budget(
                Some((0, 5000, 3_999, false)),
                Some((4_000, 5000, 3_999, false))
            )
        );
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
            stripped(book.display(1_100, None)),
            budget(
                Some((5_000, 5000, 900, false)),
                Some((5_000, 5000, 900, false)),
            )
        );
        // Auth/other requests never touch either primary book.
        book.on_failure(Bucket::Auth, false);
        assert_eq!(
            stripped(book.display(1_100, None)),
            budget(
                Some((5_000, 5000, 900, false)),
                Some((5_000, 5000, 900, false))
            )
        );
        // GraphQL answers without rate info change nothing shown.
        book.on_response(Bucket::Primary, true, None, 1_200);
        book.on_response(Bucket::Primary, true, None, 1_200);
        assert_eq!(
            stripped(book.display(1_300, None)),
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
        assert_eq!(book.display(1_100, None), UsageDisplay::Pending);
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
            stripped(book.display(2_000, None)),
            budget(
                Some((3_712, 5000, 780, false)),
                Some((29, 5000, 570, false)), // untouched — traffic's number
            )
        );

        // A later probe re-folds core; graphql still never moves.
        book.fold_probe(snap(5000, 3_700, 2_780), 2_500);
        assert_eq!(
            stripped(book.display(2_500, None)),
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
            stripped(book.display(2_000, None)),
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
        let UsageDisplay::Budget { rest, graphql } = book.display(1_000, None) else {
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
        assert_eq!(book.display(1_000, None), UsageDisplay::Pending);
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
        assert_eq!(book.display(1_000, None), UsageDisplay::Pending);
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

    #[test]
    fn chip_copy_formatters() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(5000), "5,000");
        assert_eq!(group_thousands(4821), "4,821");
        assert_eq!(compact_duration(41 * 60), "41m");
        assert_eq!(compact_duration(65 * 60), "1h 05m");
        assert_eq!(spoken_duration(41 * 60), "41 minutes");
        assert_eq!(spoken_duration(65 * 60), "1 hours 5 minutes");
        let none = super::BucketDisplay {
            remaining: 1,
            limit: 1,
            resets_in_secs: 0,
            stale: false,
            snapshot_at: None,
        };
        assert_eq!(bucket_label("REST", "requests", None), "REST: no data yet");
        assert_eq!(
            bucket_label("GraphQL", "points", Some(none)),
            "GraphQL: 1 of 1 points remaining, resets in 0 minutes"
        );
    }

    #[test]
    fn chip_body_shows_remaining_only() {
        let rest = super::BucketDisplay {
            remaining: 3_712,
            limit: 5_000,
            resets_in_secs: 13 * 60,
            stale: false,
            snapshot_at: Some(1_000),
        };
        let gql = super::BucketDisplay {
            remaining: 4_950,
            limit: 5_000,
            resets_in_secs: 9 * 60,
            stale: false,
            snapshot_at: Some(1_000),
        };
        // No limits, no windows in the visible body (D5) — those moved
        // to the tooltip and the spoken label.
        assert_eq!(
            chip_body(&UsageDisplay::Budget {
                rest: Some(rest),
                graphql: Some(gql),
            }),
            ("GH 3,712 · GQL 4,950".to_string(), false)
        );
        // A missing bucket renders an ellipsis, the other still shows.
        assert_eq!(
            chip_body(&UsageDisplay::Budget {
                rest: None,
                graphql: Some(gql)
            }),
            ("GH … · GQL 4,950".to_string(), false)
        );
        assert_eq!(
            chip_body(&UsageDisplay::Pending),
            ("GH … · GQL ?".to_string(), false)
        );
        assert_eq!(
            chip_body(&UsageDisplay::Exhausted {
                resumes_in_secs: 12 * 60
            }),
            ("GH rate-limited · resumes 12m".to_string(), true)
        );
    }

    #[test]
    fn tooltip_lines_carry_limit_and_window() {
        let rest = super::BucketDisplay {
            remaining: 3_712,
            limit: 5_000,
            resets_in_secs: 13 * 60,
            stale: false,
            snapshot_at: Some(1_000),
        };
        let gql = super::BucketDisplay {
            remaining: 4_950,
            limit: 5_000,
            resets_in_secs: 9 * 60,
            stale: false,
            snapshot_at: Some(1_000),
        };
        assert_eq!(
            tooltip_line("GH", "…/…", Some(rest)),
            "GH 3,712/5,000 resets in 13m"
        );
        assert_eq!(
            tooltip_line("GQL", "?", Some(gql)),
            "GQL 4,950/5,000 resets in 9m"
        );
        assert_eq!(tooltip_line("GQL", "?", None), "GQL ?");
        assert_eq!(tooltip_line("GH", "…/…", None), "GH …/…");
        // A stale window swaps its countdown for the as-of clock —
        // the exact clock is local-time, so assert the shape.
        let stale = super::BucketDisplay { stale: true, ..gql };
        let line = tooltip_line("GQL", "?", Some(stale));
        assert!(line.starts_with("GQL 4,950/5,000 · as of "), "got: {line}");
    }
}
