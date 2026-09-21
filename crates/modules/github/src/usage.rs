//! API-usage calculator (docs/0050): count every GitHub-bound request,
//! account for it the way GitHub does, and show the budget in a footer
//! chip while the GitHub module is on screen.
//!
//! The core decision (0050 §1): **GitHub's server-reported numbers are
//! the ledger; local counting is only prediction between responses.**
//! There is no client-side cost formula anywhere — every response
//! carries the authoritative budget state, and the book below collects
//! it, predicts across in-flight requests, and derives the display.
//!
//! Two primary budgets, not one (0050 D1): GitHub enforces REST (`core`)
//! and GraphQL as **separate** primary limits with separate counters and
//! separate windows — each response reports its own API's bucket, and
//! the chip shows both (`GH 3,712/5,000 · GQL 4,950`).
//!
//! Sections: the pure book (D3, unit-tested without gpui) → the
//! counting seam (D2, a decorator around `dyn Http`) → the footer chip
//! (D5, a self-refreshing view contributed through
//! `ModuleEntry::status_item`).

use std::sync::{Arc, Mutex};

use gpui::*;
use gpui_component::{ActiveTheme as _, h_flex};

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
/// in-flight prediction.
#[derive(Debug, Default)]
struct SubBook {
    limit: Option<u32>,
    remaining: Option<u32>,
    reset_at: Option<u64>,
    /// Epoch when the snapshot landed — the stale display's "as of".
    snapshot_at: Option<u64>,
    /// This bucket's requests started but not yet answered.
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

    fn display(&self, now: u64) -> Option<BucketDisplay> {
        Some(BucketDisplay {
            remaining: self.remaining?.saturating_sub(self.inflight),
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

    /// Fold the free probe's body (D3): one call reconciles both
    /// sub-budgets at once — the body carries `resources.core` *and*
    /// `resources.graphql`.
    pub(crate) fn fold_probe(&mut self, probe: &ProbeBodies, now: u64) {
        self.rest.fold(probe.core, now);
        self.graphql.fold(probe.graphql, now);
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
    /// down AND either sub-budget is missing or past its window. The
    /// probe is armed only on ungated ticks, so an active pause never
    /// probes; an expiring pause clears at the top of the same tick and
    /// probes.
    pub(crate) fn probe_due(&self, now: u64) -> bool {
        let cooled = self
            .last_probe_at
            .is_none_or(|at| now.saturating_sub(at) >= PROBE_COOLDOWN_SECS);
        if !cooled {
            return false;
        }
        self.rest.needs_refresh(now) || self.graphql.needs_refresh(now)
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

/// The `/rate_limit` probe body's bucket breakdown (D3) — one call
/// reports every primary sub-budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProbeBodies {
    pub(crate) core: Option<BudgetSnapshot>,
    pub(crate) graphql: Option<BudgetSnapshot>,
}

/// Parse the `/rate_limit` probe body (D3): `resources.core` and
/// `resources.graphql` are the documented-authoritative primary bucket
/// breakdowns. Returns `None` on a malformed body — the caller just
/// skips the fold (the probe response's own headers, if any, were
/// already folded by the wrapper); a missing sub-object stays `None`
/// inside a well-formed body.
pub(crate) fn parse_rate_limit_body(body: &str) -> Option<ProbeBodies> {
    #[derive(serde::Deserialize)]
    struct Core {
        limit: u32,
        remaining: u32,
        reset: u64,
    }
    #[derive(serde::Deserialize)]
    struct Graphql {
        limit: u32,
        remaining: u32,
        reset: u64,
    }
    #[derive(serde::Deserialize)]
    struct Resources {
        #[serde(default)]
        core: Option<Core>,
        #[serde(default)]
        graphql: Option<Graphql>,
    }
    #[derive(serde::Deserialize)]
    struct Body {
        resources: Resources,
    }
    let parsed: Body = serde_json::from_str(body).ok()?;
    Some(ProbeBodies {
        core: parsed.resources.core.map(|c| BudgetSnapshot {
            limit: c.limit,
            remaining: c.remaining,
            reset_at: c.reset,
        }),
        graphql: parsed.resources.graphql.map(|g| BudgetSnapshot {
            limit: g.limit,
            remaining: g.remaining,
            reset_at: g.reset,
        }),
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
}

impl UsageChip {
    pub fn new(state: Entity<GithubState>, usage: UsageLedger, cx: &mut Context<Self>) -> Self {
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
        Self { state, usage }
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

/// One bucket's chip-body fragment: `3,712/5,000` (full form) or
/// `4,950` when its limit equals `shared_limit` and the other bucket
/// already shows it (the D5 terse copy).
fn bucket_body(d: Option<BucketDisplay>, shared_limit: Option<u32>) -> String {
    match d {
        None => "…/…".to_string(),
        Some(d) if Some(d.limit) == shared_limit => group_thousands(d.remaining),
        Some(d) => format!(
            "{}/{}",
            group_thousands(d.remaining),
            group_thousands(d.limit)
        ),
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

        // Chip body + spoken form per D5; the session counters ride the
        // a11y label (a text status is not interactive, so there is no
        // hover tooltip to host them — 0049 D1 shape). Both sub-budgets
        // are shown (D1): `GH 3,712/5,000 · GQL 4,950`.
        let (body, label, warning) = match display {
            UsageDisplay::Pending => (
                "GH …/… · GQL …".to_string(),
                format!("GitHub API usage pending; {session}"),
                false,
            ),
            UsageDisplay::Budget { rest, graphql } => {
                // The terse body omits GQL's limit while it matches the
                // REST limit; the spoken form always spells both.
                let shared_limit = match (rest, graphql) {
                    (Some(r), Some(g)) if r.limit == g.limit => Some(r.limit),
                    (Some(r), None) => Some(r.limit),
                    _ => None,
                };
                // REST always shows its full `x/y` form per the D5 copy.
                let rest_body = match rest {
                    Some(d) => format!(
                        "{}/{}",
                        group_thousands(d.remaining),
                        group_thousands(d.limit)
                    ),
                    None => "…/…".to_string(),
                };
                let gql_body = bucket_body(graphql, shared_limit);

                // Visible suffix: the nearest live window's countdown —
                // or the stale marker when any window rolled without a
                // newer snapshot (the spoken form has per-bucket detail).
                let suffix = {
                    let stale_at = [rest, graphql]
                        .into_iter()
                        .flatten()
                        .filter(|d| d.stale)
                        .filter_map(|d| d.snapshot_at)
                        .min();
                    if let Some(at) = stale_at {
                        format!(" · as of {}", local_hhmm(at))
                    } else {
                        let nearest = [rest, graphql]
                            .into_iter()
                            .flatten()
                            .map(|d| d.resets_in_secs)
                            .min()
                            .unwrap_or_default();
                        format!(" · resets {}", compact_duration(nearest))
                    }
                };
                let body = format!("GH {rest_body} · GQL {gql_body}{suffix}");

                let rest_label = bucket_label("REST", "requests", rest);
                let gql_label = bucket_label("GraphQL", "points", graphql);
                let label = format!("GitHub API: {rest_label}; {gql_label}; {session}");
                (body, label, false)
            }
            UsageDisplay::Exhausted { resumes_in_secs } => (
                format!(
                    "GH rate-limited · resumes {}",
                    compact_duration(resumes_in_secs)
                ),
                format!(
                    "GitHub API rate limited, fetches resume in {}; {session}",
                    spoken_duration(resumes_in_secs)
                ),
                true,
            ),
        };

        let color = if warning {
            cx.theme().warning
        } else {
            cx.theme().muted_foreground
        };
        h_flex()
            .gap_1()
            .id("github-usage")
            .role(accesskit::Role::Label)
            .aria_label(label)
            .text_color(color)
            .child(SharedString::from(body))
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
        Bucket, BudgetSnapshot, CountingHttp, ProbeBodies, UsageBook, UsageDisplay, UsageLedger,
        bucket_label, compact_duration, group_thousands, parse_rate_limit_body, spoken_duration,
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
    fn inflight_prediction_is_per_bucket() {
        let mut book = UsageBook::default();
        book.on_response(Bucket::Primary, false, Some(snap(5000, 5, 2_000)), 1_000);
        book.on_response(Bucket::Primary, true, Some(snap(5000, 2, 2_000)), 1_000);
        // Two REST and one GraphQL start; each prediction hits its own
        // bucket, floored at zero.
        book.on_request_start(Bucket::Primary, false);
        book.on_request_start(Bucket::Primary, false);
        book.on_request_start(Bucket::Primary, true);
        assert_eq!(
            stripped(book.display(1_100, None)),
            budget(Some((3, 5000, 900, false)), Some((1, 5000, 900, false)))
        );
        // A GraphQL answer without rate info clears only its slot.
        book.on_response(Bucket::Primary, true, None, 1_150);
        assert_eq!(
            stripped(book.display(1_200, None)),
            budget(Some((3, 5000, 800, false)), Some((2, 5000, 800, false)))
        );
        // A REST transport failure clears one REST slot.
        book.on_failure(Bucket::Primary, false);
        assert_eq!(
            stripped(book.display(1_300, None)),
            budget(Some((4, 5000, 700, false)), Some((2, 5000, 700, false)))
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
        // Seed both sub-books with fresh snapshots (fold-style — the
        // in-flight predictions stay as the starts left them).
        book.seed_snapshots(snap(5000, 5_000, 2_000), snap(5000, 5_000, 2_000), 1_000);
        assert_eq!(
            stripped(book.display(1_100, None)),
            budget(
                Some((4_999, 5000, 900, false)), // 1 REST in flight
                Some((4_998, 5000, 900, false)), // 2 GraphQL in flight
            )
        );
        // Auth/other requests never touch either primary prediction.
        book.on_failure(Bucket::Auth, false);
        assert_eq!(
            stripped(book.display(1_100, None)),
            budget(
                Some((4_999, 5000, 900, false)),
                Some((4_998, 5000, 900, false))
            )
        );
        // The two GraphQL answers land without rate info: that
        // bucket's prediction clears, the REST one keeps its own.
        book.on_response(Bucket::Primary, true, None, 1_200);
        book.on_response(Bucket::Primary, true, None, 1_200);
        assert_eq!(
            stripped(book.display(1_300, None)),
            budget(
                Some((4_999, 5000, 700, false)),
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
    fn probe_due_when_either_bucket_needs_refresh() {
        let mut book = UsageBook::default();
        assert!(book.probe_due(1_000)); // nothing seen yet

        // One bucket fresh, the other missing: still due.
        book.on_response(Bucket::Primary, false, Some(snap(5000, 5, 2_000)), 1_000);
        assert!(book.probe_due(1_500));

        // Both fresh: quiet.
        book.on_response(Bucket::Primary, true, Some(snap(5000, 5, 2_000)), 1_500);
        assert!(!book.probe_due(1_600));

        // Either bucket rolling past its window re-arms it.
        assert!(book.probe_due(2_000));

        // Cooldown: one probe per 30 s, even while stale.
        book.mark_probe(2_000);
        assert!(!book.probe_due(2_029));
        assert!(book.probe_due(2_030));
    }

    #[test]
    fn fold_probe_reconciles_both_buckets_at_once() {
        let mut book = UsageBook::default();
        let probe = ProbeBodies {
            core: Some(snap(5000, 3_712, 2_780)),
            graphql: Some(snap(5000, 4_950, 2_570)),
        };
        book.fold_probe(&probe, 2_000);
        assert_eq!(
            stripped(book.display(2_000, None)),
            budget(
                Some((3_712, 5000, 780, false)),
                Some((4_950, 5000, 570, false)),
            )
        );
        // A partial body folds what it has, keeps the rest.
        let partial = ProbeBodies {
            core: Some(snap(5000, 3_700, 2_780)),
            graphql: None,
        };
        book.fold_probe(&partial, 2_500);
        assert_eq!(
            stripped(book.display(2_500, None)),
            budget(
                Some((3_700, 5000, 280, false)),
                Some((4_950, 5000, 70, false)),
            )
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
    fn rate_limit_body_carries_both_buckets() {
        let body = serde_json::json!({
            "resources": {
                "core": { "limit": 5000, "remaining": 3712, "reset": 1_893_456_000 },
                "graphql": { "limit": 5000, "remaining": 4950, "reset": 1_893_454_800 },
                "search": { "limit": 30, "remaining": 30, "reset": 1 }
            }
        })
        .to_string();
        assert_eq!(
            parse_rate_limit_body(&body),
            Some(ProbeBodies {
                core: Some(snap(5000, 3_712, 1_893_456_000)),
                graphql: Some(snap(5000, 4_950, 1_893_454_800)),
            })
        );
        // A body without the graphql sub-object parses; that slot stays
        // empty rather than failing the whole fold.
        let core_only = serde_json::json!({
            "resources": { "core": { "limit": 5000, "remaining": 3712, "reset": 1_893_456_000 } }
        })
        .to_string();
        assert_eq!(
            parse_rate_limit_body(&core_only),
            Some(ProbeBodies {
                core: Some(snap(5000, 3_712, 1_893_456_000)),
                graphql: None,
            })
        );
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
}
