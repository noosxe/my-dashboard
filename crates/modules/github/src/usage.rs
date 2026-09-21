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

/// A fresh, empty ledger handle (a free fn — `Arc` admits no
/// inherent impls).
pub(crate) fn new_ledger() -> UsageLedger {
    Arc::new(Mutex::new(UsageBook::default()))
}

/// Minimum spacing between `/rate_limit` probes (docs/0050 D3).
const PROBE_COOLDOWN_SECS: u64 = 30;

// ---------------------------------------------------------------------------
// Pure bookkeeping (D3) — no gpui types below this line.
// ---------------------------------------------------------------------------

/// Which budget a request draws on (D1): buckets are classified by URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bucket {
    /// `api.github.com` — REST and GraphQL alike draw on the one
    /// 5,000/hour per-token budget.
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

/// The display derivation (D3) — everything the chip renders comes from
/// here; render-side code never re-derives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsageDisplay {
    /// Connected, no snapshot yet — skeleton until the first response.
    Pending,
    /// Server snapshot present. `remaining` already subtracts in-flight
    /// primary requests (the prediction); `stale` marks a window whose
    /// `reset_at` passed without a newer snapshot (0050 D3: keep
    /// last-known, arm the probe).
    Budget {
        remaining: u32,
        limit: u32,
        resets_in_secs: u64,
        stale: bool,
    },
    /// The operational backoff (`rate_limited_until`) is in future —
    /// shown over any snapshot (0050 D4).
    Exhausted { resumes_in_secs: u64 },
}

/// The pure ledger (docs/0050 D3) — the `InFlightBook` pattern.
#[derive(Debug, Default)]
pub(crate) struct UsageBook {
    /// Last server snapshot of the primary bucket.
    limit: Option<u32>,
    remaining: Option<u32>,
    reset_at: Option<u64>,
    /// Epoch when the snapshot landed — the stale display's "as of".
    snapshot_at: Option<u64>,
    /// Primary-bucket requests started but not yet answered.
    inflight: u32,
    /// Session counters (all buckets; D5 tooltip / a11y label).
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
                } else {
                    self.session_rest += 1;
                }
                self.inflight = self.inflight.saturating_add(1);
            }
            Bucket::Auth => self.session_auth += 1,
            Bucket::Other => {}
        }
    }

    /// A response landed: clears one in-flight slot and — for the
    /// primary bucket — replaces the snapshot wholesale. This is the
    /// entire reconciliation mechanism (0050 D3): every response is a
    /// full sync, so the display is exact within one response.
    pub(crate) fn on_response(
        &mut self,
        bucket: Bucket,
        snapshot: Option<BudgetSnapshot>,
        now: u64,
    ) {
        if bucket != Bucket::Primary {
            return;
        }
        self.inflight = self.inflight.saturating_sub(1);
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
    pub(crate) fn on_failure(&mut self, bucket: Bucket) {
        if bucket == Bucket::Primary {
            self.inflight = self.inflight.saturating_sub(1);
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
        match (self.limit, self.remaining, self.reset_at) {
            (Some(limit), Some(remaining), Some(reset_at)) => UsageDisplay::Budget {
                remaining: remaining.saturating_sub(self.inflight),
                limit,
                resets_in_secs: reset_at.saturating_sub(now),
                stale: now >= reset_at,
            },
            _ => UsageDisplay::Pending,
        }
    }

    /// Whether the free `/rate_limit` probe should fire (D3): cooled
    /// down AND the snapshot is missing or past its window. The probe
    /// is armed only on ungated ticks, so an active pause never probes;
    /// an expiring pause clears at the top of the same tick and probes.
    pub(crate) fn probe_due(&self, now: u64) -> bool {
        let cooled = self
            .last_probe_at
            .is_none_or(|at| now.saturating_sub(at) >= PROBE_COOLDOWN_SECS);
        if !cooled {
            return false;
        }
        match (self.remaining, self.reset_at) {
            (None, _) => true,
            (_, Some(reset_at)) => now >= reset_at,
            (_, None) => false,
        }
    }

    /// Stamp the probe cooldown at arming time (D3): a failed probe
    /// retries no sooner than the cooldown, never in a tight loop.
    pub(crate) fn mark_probe(&mut self, now: u64) {
        self.last_probe_at = Some(now);
    }

    /// Epoch when the snapshot landed — the stale display's "as of".
    pub(crate) fn snapshot_at(&self) -> Option<u64> {
        self.snapshot_at
    }

    /// Test seam: install a fresh-looking snapshot without traffic.
    #[cfg(test)]
    pub(crate) fn seed_snapshot(&mut self, limit: u32, remaining: u32, reset_at: u64, now: u64) {
        self.on_response(
            Bucket::Primary,
            Some(BudgetSnapshot {
                limit,
                remaining,
                reset_at,
            }),
            now,
        );
        // `on_response` decrements in-flight; a seed starts from none.
    }
}

/// Parse the `/rate_limit` probe body (D3): `resources.core` is the
/// documented-authoritative primary bucket breakdown. Returns `None`
/// on any shape mismatch — the caller just skips the fold (the probe
/// response's own headers, if any, were already folded by the wrapper).
pub(crate) fn parse_rate_limit_body(body: &str) -> Option<BudgetSnapshot> {
    #[derive(serde::Deserialize)]
    struct Core {
        limit: u32,
        remaining: u32,
        reset: u64,
    }
    #[derive(serde::Deserialize)]
    struct Resources {
        core: Core,
    }
    #[derive(serde::Deserialize)]
    struct Body {
        resources: Resources,
    }
    let parsed: Body = serde_json::from_str(body).ok()?;
    Some(BudgetSnapshot {
        limit: parsed.resources.core.limit,
        remaining: parsed.resources.core.remaining,
        reset_at: parsed.resources.core.reset,
    })
}

/// The GraphQL body's `rateLimit` object (D1): preferred over headers
/// when present. The body object carries no `limit`, so the harvested
/// headers fill it in (`x-ratelimit-limit` rides every api.github.com
/// response). A huge document is parsed here a second time on the
/// background thread — the price of body-authoritative accounting.
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
                // D1: body `rateLimit` when present (GraphQL), else the
                // harvested headers.
                let snapshot = if graphql {
                    graphql_budget(result.body.as_deref(), result.rate)
                        .or_else(|| result.rate.map(Into::into))
                } else {
                    result.rate.map(Into::into)
                };
                book.on_response(bucket, snapshot, now_epoch());
            }
            Err(_) => book.on_failure(bucket),
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

    fn current_display(&self, cx: &App) -> (UsageDisplay, (u64, u64, u64, u64), Option<u64>) {
        let rate_limited_until = self.state.read(cx).rate_limited_until;
        let book = self.usage.lock().unwrap();
        (
            book.display(now_epoch(), rate_limited_until),
            book.session_counters(),
            book.snapshot_at(),
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

impl Render for UsageChip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (display, counters, snapshot_at) = self.current_display(cx);
        let (total, graphql, rest, auth) = counters;
        let session =
            format!("session: {total} requests ({graphql} GraphQL, {rest} REST, {auth} auth)");

        // Chip body + spoken form per D5; the session counters ride the
        // a11y label (a text status is not interactive, so there is no
        // hover tooltip to host them — 0049 D1 shape).
        let (body, label, warning) = match display {
            UsageDisplay::Pending => (
                "GH …/…".to_string(),
                format!("GitHub API usage pending; {session}"),
                false,
            ),
            UsageDisplay::Budget {
                remaining,
                limit,
                resets_in_secs,
                stale,
            } => {
                let as_of = stale.then(|| {
                    snapshot_at
                        .map(|at| format!(" · as of {}", local_hhmm(at)))
                        .unwrap_or_default()
                });
                let body = if stale {
                    format!(
                        "GH {}/{}{}",
                        group_thousands(remaining),
                        group_thousands(limit),
                        as_of.unwrap_or_default()
                    )
                } else {
                    format!(
                        "GH {}/{} · resets {}",
                        group_thousands(remaining),
                        group_thousands(limit),
                        compact_duration(resets_in_secs)
                    )
                };
                let mut label = format!(
                    "GitHub API: {} of {} requests remaining, resets in {}",
                    group_thousands(remaining),
                    group_thousands(limit),
                    spoken_duration(resets_in_secs)
                );
                if let Some(at) = snapshot_at.filter(|_| stale) {
                    label = format!("{label}, last updated {}", local_hhmm(at));
                }
                label = format!("{label}; {session}");
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
        Bucket, BudgetSnapshot, CountingHttp, UsageBook, UsageDisplay, UsageLedger,
        compact_duration, group_thousands, parse_rate_limit_body, spoken_duration,
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

    // --- book: counting + reconciliation ---------------------------------

    #[test]
    fn no_snapshot_displays_pending() {
        let book = UsageBook::default();
        assert_eq!(book.display(1_000, None), UsageDisplay::Pending);
    }

    #[test]
    fn response_replaces_the_snapshot_wholesale() {
        let mut book = UsageBook::default();
        book.on_request_start(Bucket::Primary, false);
        book.on_response(Bucket::Primary, Some(snap(5000, 4980, 2_000)), 1_000);
        assert_eq!(
            book.display(1_200, None),
            UsageDisplay::Budget {
                remaining: 4980,
                limit: 5000,
                resets_in_secs: 800,
                stale: false,
            }
        );
        // A later response replaces, not merges: even a lower `limit`
        // or higher `remaining` wins (server is the ledger, D3).
        book.on_request_start(Bucket::Primary, false);
        book.on_response(Bucket::Primary, Some(snap(5000, 10, 9_000)), 1_500);
        assert_eq!(
            book.display(1_600, None),
            UsageDisplay::Budget {
                remaining: 10,
                limit: 5000,
                resets_in_secs: 7_400,
                stale: false,
            }
        );
    }

    #[test]
    fn inflight_prediction_floors_at_zero_and_clears() {
        let mut book = UsageBook::default();
        book.on_response(Bucket::Primary, Some(snap(5000, 2, 2_000)), 1_000);
        // Three requests start while the snapshot says 2 remaining.
        book.on_request_start(Bucket::Primary, true);
        book.on_request_start(Bucket::Primary, false);
        book.on_request_start(Bucket::Primary, false);
        assert_eq!(
            book.display(1_100, None),
            UsageDisplay::Budget {
                remaining: 0, // floored, never negative
                limit: 5000,
                resets_in_secs: 900,
                stale: false,
            }
        );
        // One answer with no rate headers clears its slot only.
        book.on_response(Bucket::Primary, None, 1_150);
        assert_eq!(
            book.display(1_200, None),
            UsageDisplay::Budget {
                remaining: 0, // 2 − 2 still in flight
                limit: 5000,
                resets_in_secs: 800,
                stale: false,
            }
        );
        // Transport failures clear theirs too.
        book.on_failure(Bucket::Primary);
        book.on_failure(Bucket::Primary);
        assert_eq!(
            book.display(1_300, None),
            UsageDisplay::Budget {
                remaining: 2, // prediction gone — back to server fact
                limit: 5000,
                resets_in_secs: 700,
                stale: false,
            }
        );
    }

    #[test]
    fn passed_reset_marks_stale_and_counts_down_zero() {
        let mut book = UsageBook::default();
        book.on_response(Bucket::Primary, Some(snap(5000, 120, 2_000)), 1_000);
        assert_eq!(
            book.display(2_000, None),
            UsageDisplay::Budget {
                remaining: 120,
                limit: 5000,
                resets_in_secs: 0,
                stale: true, // keep last-known until the next response
            }
        );
        assert_eq!(book.snapshot_at(), Some(1_000));
    }

    #[test]
    fn rate_limited_backoff_wins_over_the_snapshot() {
        let mut book = UsageBook::default();
        book.on_response(Bucket::Primary, Some(snap(5000, 0, 9_000)), 1_000);
        assert_eq!(
            book.display(2_000, Some(5_000)),
            UsageDisplay::Exhausted {
                resumes_in_secs: 3_000
            }
        );
        // An expired backoff defers to the snapshot again.
        assert_eq!(
            book.display(5_001, Some(5_000)),
            UsageDisplay::Budget {
                remaining: 0,
                limit: 5000,
                resets_in_secs: 3_999,
                stale: false,
            }
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
        // One of the three primary requests answered; the two
        // unanswered GraphQL reads stay predicted (D1: booked at
        // start), and the auth/other requests never touch the
        // prediction.
        book.on_response(Bucket::Primary, Some(snap(5000, 5, 2_000)), 1_000);
        assert_eq!(
            book.display(1_100, None),
            UsageDisplay::Budget {
                remaining: 5 - 2, // the two in-flight GraphQL reads
                limit: 5000,
                resets_in_secs: 900,
                stale: false,
            }
        );
        // Finishing an auth request clears no in-flight slot.
        book.on_failure(Bucket::Auth);
        assert_eq!(
            book.display(1_100, None),
            UsageDisplay::Budget {
                remaining: 3,
                limit: 5000,
                resets_in_secs: 900,
                stale: false,
            }
        );
        // The two GraphQL answers land without headers: prediction
        // clears, back to server fact.
        book.on_response(Bucket::Primary, None, 1_200);
        book.on_response(Bucket::Primary, None, 1_200);
        assert_eq!(
            book.display(1_300, None),
            UsageDisplay::Budget {
                remaining: 5,
                limit: 5000,
                resets_in_secs: 700,
                stale: false,
            }
        );
    }

    #[test]
    fn reset_wipes_everything_for_the_next_account() {
        let mut book = UsageBook::default();
        book.on_request_start(Bucket::Primary, true);
        book.on_response(Bucket::Primary, Some(snap(5000, 1, 2_000)), 1_000);
        book.mark_probe(1_000);
        book.reset();
        assert_eq!(book.display(1_100, None), UsageDisplay::Pending);
        assert_eq!(book.session_counters(), (0, 0, 0, 0));
        assert!(book.probe_due(1_100)); // cooldown wiped too
    }

    // --- book: probe arming (D3) ------------------------------------------

    #[test]
    fn probe_due_when_no_snapshot_or_window_passed() {
        let mut book = UsageBook::default();
        assert!(book.probe_due(1_000)); // nothing seen yet

        book.on_response(Bucket::Primary, Some(snap(5000, 5, 2_000)), 1_000);
        assert!(!book.probe_due(1_500)); // fresh window
        assert!(book.probe_due(2_000)); // rolled past reset_at

        // Cooldown: one probe per 30 s, even while stale.
        book.mark_probe(2_000);
        assert!(!book.probe_due(2_029));
        assert!(book.probe_due(2_030));
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
    fn wrapper_counts_every_request_and_folds_headers() {
        let mock = Arc::new(crate::http::mock::MockHttp::new(vec![
            http_result(200, rate(5000, 4998, 2_000), Some("[]")),
            http_result(200, rate(5000, 4997, 2_000), Some("[]")),
        ]));
        let ledger: UsageLedger = Arc::new(Mutex::new(UsageBook::default()));
        let http = CountingHttp::wrap(mock.clone(), ledger.clone());

        http.request(
            Verb::Get,
            "https://api.github.com/notifications?per_page=100&page=1",
            None,
            None,
        )
        .unwrap();
        http.request(
            Verb::Post,
            "https://github.com/login/device/code",
            Some("c=1"),
            None,
        )
        .unwrap();

        let book = ledger.lock().unwrap();
        // Two requests booked; the auth one never touched the budget.
        assert_eq!(book.session_counters(), (2, 0, 1, 1));
        assert_eq!(
            book.display(1_000, None),
            UsageDisplay::Budget {
                remaining: 4998, // the primary response's fact — the auth
                // bucket's headers never fold into it
                limit: 5000,
                resets_in_secs: 1_000,
                stale: false,
            }
        );
        drop(book);
        assert_eq!(mock.recorded().len(), 2); // transparent passthrough
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
        let UsageDisplay::Budget {
            remaining,
            limit,
            resets_in_secs: _,
            stale: _,
        } = book.display(1_000, None)
        else {
            panic!("expected a budget display");
        };
        // Body wins for remaining/resetAt; headers fill the limit.
        assert_eq!(remaining, 4310);
        assert_eq!(limit, 5000);
        assert_eq!(book.session_counters(), (1, 1, 0, 0));
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

    #[test]
    fn unknown_hosts_book_count_only() {
        let mock = Arc::new(crate::http::mock::MockHttp::new(vec![http_result(
            200,
            None,
            Some("ok"),
        )]));
        let ledger: UsageLedger = Arc::new(Mutex::new(UsageBook::default()));
        CountingHttp::wrap(mock, ledger.clone())
            .request(Verb::Get, "https://example.com/x", None, None)
            .unwrap();
        let book = ledger.lock().unwrap();
        assert_eq!(book.session_counters(), (1, 0, 0, 0)); // no lane for Other
        assert_eq!(book.display(1_000, None), UsageDisplay::Pending);
    }

    // --- probe body + formatters -------------------------------------------

    #[test]
    fn rate_limit_body_parses_the_core_bucket() {
        let body = serde_json::json!({
            "resources": {
                "core": { "limit": 5000, "remaining": 4995, "reset": 1_893_456_000 },
                "search": { "limit": 30, "remaining": 30, "reset": 1 }
            }
        })
        .to_string();
        assert_eq!(
            parse_rate_limit_body(&body),
            Some(snap(5000, 4995, 1_893_456_000))
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
    }
}
