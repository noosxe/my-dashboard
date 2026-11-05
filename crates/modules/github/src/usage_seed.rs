//! Seeded usage ledger for verification captures (docs/0081 §12.1,
//! #536). Debug builds only — this module is not compiled into release
//! builds at all.
//!
//! With the GitHub fixture mode on (`DASHBOARD_GITHUB_FIXTURE`),
//! `DASHBOARD_GITHUB_USAGE_SEED=<days>` (1–90, the retention window,
//! docs/0057 D7) appends a synthetic,
//! deterministic flight history to the usage store at module init —
//! before any window exists — so the usage page opens populated. The
//! history ends at the capture clock's "now"
//! ([`dashboard_core::freshness::clock_epoch_ms`]): under
//! `DASHBOARD_FIXED_NOW` the same days, rows and numbers come out every
//! run. Only fixture identities appear (`fixture-user/...`).
//!
//! The model: a fixed-seed PRNG walks the window with exponential gaps
//! (dense in UTC working hours, sparse at night, rare bursts), picks a
//! weighted `(op, transport, surface, trigger)` profile from the
//! module's own vocabulary, and books it against two independent hourly
//! budget windows (REST `core` and GraphQL, offset from each other), so
//! `remaining_after` burns down inside each window and refills at its
//! `cycle_reset_epoch`. Durations, queue waits and payload sizes are
//! drawn per profile; 304 revalidations, HTTP errors and network
//! failures are mixed in at low rates.
//!
//! Idempotent per store: if any row already sits in the seeded window
//! the seed is skipped (a restarted session keeps its one copy).

use std::sync::Arc;

use dashboard_core::usage_records::{Transport, UsageRecord, UsageSink, UsageSource};

use crate::service::{Kind, Mutation};
use crate::usage::{Surface, Trigger};

/// The seed variable: whole days of history (1–90: the store's
/// retention, so the usage page's 90-day reach can be captured,
/// docs/0082 D11).
pub(crate) const SEED_ENV: &str = "DASHBOARD_GITHUB_USAGE_SEED";

/// The PRNG seed — fixed, so a given anchor and day count always
/// produce the same ledger.
const SEED: u64 = 0x0081_0536;

/// The longest seed: the store's 90-day retention (docs/0057 D7).
const MAX_DAYS: u32 = 90;

const HOUR_MS: u64 = 3_600_000;
const DAY_MS: u64 = 24 * HOUR_MS;
const LIMIT: u32 = 5_000;
const REPOS: [&str; 3] = [
    "fixture-user/atlas",
    "fixture-user/beacon",
    "fixture-user/compass",
];

/// SplitMix64 — tiny, dependency-free, and stable across platforms.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in `lo..=hi`.
    fn range(&mut self, lo: u32, hi: u32) -> u32 {
        lo + (self.next() % u64::from(hi - lo + 1)) as u32
    }

    fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    /// Exponential with the given mean.
    fn exp(&mut self, mean: f64) -> f64 {
        -mean * (1.0 - self.unit()).ln()
    }

    /// Log-normal around `median` with spread `sigma` (Box–Muller).
    fn lognormal(&mut self, median: f64, sigma: f64) -> f64 {
        let (u1, u2) = (1.0 - self.unit(), self.unit());
        let z = (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos();
        median * (sigma * z).exp()
    }
}

/// What a profile's `ident` is scoped to.
#[derive(Clone, Copy)]
enum Scope {
    None,
    Repo,
    Number,
}

/// One kind of flight the seed draws: the op (from `Kind::kind_name`,
/// so the vocabulary is the shipped one), its wire and origin, a
/// GraphQL cost range, a draw weight, the ident scope, the median
/// latency and the typical payload size.
struct Profile {
    kind: Kind,
    transport: Transport,
    surface: Surface,
    trigger: Trigger,
    cost: (u32, u32),
    weight: u32,
    scope: Scope,
    median_ms: f64,
    bytes: u32,
}

// Positional on purpose: the profile table below reads as one row per op.
#[allow(clippy::too_many_arguments)]
const fn p(
    kind: Kind,
    transport: Transport,
    surface: Surface,
    trigger: Trigger,
    cost: (u32, u32),
    weight: u32,
    scope: Scope,
    median_ms: f64,
    bytes: u32,
) -> Profile {
    Profile {
        kind,
        transport,
        surface,
        trigger,
        cost,
        weight,
        scope,
        median_ms,
        bytes,
    }
}

use Scope::{None as NoScope, Number, Repo};
use Surface as S;
use Transport::{Graphql as Gql, Rest};
use Trigger as T;

#[rustfmt::skip]
const PROFILES: &[Profile] = &[
    p(Kind::SessionInventory, Gql, S::ModuleHome, T::Bootstrap, (1, 3), 4, NoScope, 520.0, 14_000),
    p(Kind::CardTotals, Gql, S::HomeCard, T::ViewShown, (1, 2), 6, NoScope, 380.0, 2_400),
    p(Kind::AccountPullRequests, Gql, S::HomeCard, T::Refocus, (2, 6), 5, NoScope, 610.0, 18_000),
    p(Kind::Notifications, Rest, S::ServiceTick, T::Poll, (1, 1), 14, NoScope, 240.0, 6_500),
    p(Kind::RepoPulls, Gql, S::RepoView, T::ViewShown, (3, 12), 8, Repo, 700.0, 22_000),
    p(Kind::RepoIssues, Gql, S::RepoView, T::ViewShown, (2, 9), 6, Repo, 560.0, 16_000),
    p(Kind::RepoCommits, Gql, S::RepoView, T::ViewShown, (1, 4), 4, Repo, 450.0, 21_000),
    p(Kind::RepoCommitsMore, Gql, S::RepoView, T::Paginate, (1, 4), 2, Repo, 430.0, 20_000),
    p(Kind::PrDetail, Gql, S::PrPage, T::ViewShown, (8, 31), 7, Number, 1_150.0, 48_000),
    p(Kind::PrFiles, Rest, S::PrPage, T::Expand, (1, 1), 3, Number, 520.0, 160_000),
    p(Kind::PrThreads, Gql, S::PrPage, T::ViewShown, (2, 8), 3, Number, 640.0, 12_000),
    p(Kind::JobLog, Rest, S::PrPage, T::Expand, (1, 1), 1, Number, 1_900.0, 900_000),
    p(Kind::IssueDetail, Gql, S::IssuePage, T::ViewShown, (3, 10), 3, Number, 690.0, 26_000),
    p(Kind::ThreadDetail, Rest, S::NotificationPage, T::ViewShown, (1, 1), 3, Repo, 260.0, 4_200),
    p(Kind::RepoLabels, Rest, S::Composer, T::ViewShown, (1, 1), 1, Repo, 230.0, 3_100),
    p(Kind::RepoAssignableUsers, Rest, S::Composer, T::ViewShown, (1, 1), 1, Repo, 250.0, 5_400),
    p(Kind::PrMutation(Mutation::Comment), Gql, S::PrPage, T::Manual, (1, 1), 1, Number, 820.0, 1_800),
    p(Kind::RateLimit, Rest, S::ServiceTick, T::Probe, (0, 0), 1, NoScope, 150.0, 1_100),
];

/// Ops whose REST flights revalidate by ETag (the 304 mix).
fn revalidates(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Notifications | Kind::ThreadDetail | Kind::RepoLabels
    )
}

/// One budget bucket's walk: hourly windows ending at `cycle_end`, the
/// running `remaining` inside the current window.
struct Window {
    cycle_end: u64,
    remaining: u32,
}

impl Window {
    fn roll(&mut self, t: u64) {
        while t >= self.cycle_end {
            self.cycle_end += HOUR_MS;
            self.remaining = LIMIT;
        }
    }
}

/// The deterministic history: `days` of flights ending just before
/// `anchor_ms`, oldest first.
pub(crate) fn generate(anchor_ms: u64, days: u32) -> Vec<UsageRecord> {
    let mut rng = Rng(SEED);
    let start = anchor_ms.saturating_sub(u64::from(days) * DAY_MS);
    let total_weight: u32 = PROFILES.iter().map(|p| p.weight).sum();
    // Two hourly windows on independent clocks (REST on the hour + 23m,
    // GraphQL + 41m), the way real budgets drift apart.
    let base = start - start % HOUR_MS;
    let mut rest = Window {
        cycle_end: base + 23 * 60_000,
        remaining: LIMIT,
    };
    let mut gql = Window {
        cycle_end: base + 41 * 60_000,
        remaining: LIMIT,
    };
    rest.roll(start);
    gql.roll(start);

    let mut out = Vec::new();
    let mut t = start;
    loop {
        let hour = (t / HOUR_MS) % 24;
        let mut rate = if (8..=19).contains(&hour) { 1.0 } else { 0.12 };
        if rng.chance(0.01) {
            rate *= 8.0; // a burst — a big review session
        }
        t += rng.exp(45_000.0 / rate) as u64 + 1;
        if t >= anchor_ms {
            break;
        }
        let mut pick = rng.range(1, total_weight);
        let profile = PROFILES
            .iter()
            .find(|p| {
                if pick <= p.weight {
                    true
                } else {
                    pick -= p.weight;
                    false
                }
            })
            .unwrap_or(&PROFILES[0]);
        rest.roll(t);
        gql.roll(t);
        out.push(flight(&mut rng, profile, t, &mut rest, &mut gql));
    }
    out
}

fn flight(rng: &mut Rng, p: &Profile, t: u64, rest: &mut Window, gql: &mut Window) -> UsageRecord {
    let graphql = p.transport == Transport::Graphql;
    let network_error = rng.chance(0.006);
    let etag_304 = !network_error && !graphql && revalidates(p.kind) && rng.chance(0.7);
    let status = if network_error {
        0
    } else if etag_304 {
        304
    } else if rng.chance(0.006) {
        [502, 403, 404][rng.range(0, 2) as usize]
    } else {
        200
    };
    let cost = if network_error {
        None
    } else if etag_304 || p.kind == Kind::RateLimit {
        Some(0)
    } else if graphql && rng.chance(0.03) {
        None // a query without the rateLimit field
    } else {
        Some(rng.range(p.cost.0, p.cost.1))
    };
    let window = if graphql { gql } else { rest };
    if let Some(c) = cost {
        window.remaining = window.remaining.saturating_sub(c);
    }

    let ident = match p.scope {
        Scope::None => None,
        Scope::Repo => Some(REPOS[rng.range(0, 2) as usize].to_string()),
        Scope::Number => Some(format!(
            "{}#{}",
            REPOS[rng.range(0, 2) as usize],
            rng.range(1, 60)
        )),
    };
    let duration_ms = if network_error {
        [10_000, rng.range(200, 3_000)][rng.range(0, 1) as usize]
    } else {
        rng.lognormal(p.median_ms, 0.55).clamp(40.0, 12_000.0) as u32
    };
    let queue_ms = if rng.chance(0.1) {
        None
    } else if rng.chance(0.02) {
        Some(rng.range(150, 900))
    } else {
        Some(rng.exp(18.0) as u32 + 1)
    };
    let resp_bytes = (!etag_304 && !network_error)
        .then(|| (f64::from(p.bytes) * (0.3 + 1.5 * rng.unit())) as u32 + 64);

    let mut rec = UsageRecord::new(
        "github",
        p.kind.kind_name(),
        p.surface.as_str(),
        p.trigger.as_str(),
    );
    rec.ts_epoch_ms = t;
    rec.transport = p.transport;
    rec.ident = ident.map(Into::into);
    rec.status = status;
    rec.etag_304 = etag_304;
    rec.cost = cost;
    rec.remaining_after = (!network_error).then_some(window.remaining);
    rec.limit_after = (!network_error).then_some(LIMIT);
    rec.cycle_reset_epoch = Some(window.cycle_end / 1000);
    rec.duration_ms = duration_ms;
    rec.queue_ms = queue_ms;
    rec.resp_bytes = resp_bytes;
    rec.network_error = network_error;
    rec
}

/// The day count `SEED_ENV` asks for, when the fixture mode is on.
pub(crate) fn seed_days(fixture: bool) -> Option<u32> {
    let raw = std::env::var(SEED_ENV).ok().filter(|v| !v.is_empty())?;
    if !fixture {
        log::warn!("usage seed: {SEED_ENV} needs the GitHub fixture mode — ignored");
        return None;
    }
    match raw.trim().parse::<u32>() {
        Ok(d @ 1..=MAX_DAYS) => Some(d),
        _ => {
            log::warn!(
                "usage seed: {SEED_ENV}={raw} is not a day count in 1..={MAX_DAYS} — ignored"
            );
            None
        }
    }
}

/// Append the seeded history to the store unless the window already
/// holds rows. Returns the number of rows written.
pub(crate) fn seed(
    sink: &Arc<dyn UsageSink>,
    source: &Arc<dyn UsageSource>,
    anchor_ms: u64,
    days: u32,
) -> usize {
    let from = anchor_ms.saturating_sub(u64::from(days) * DAY_MS);
    if !source.records_between(from, anchor_ms).is_empty() {
        log::info!("usage seed: window already populated — skipped");
        return 0;
    }
    let rows = generate(anchor_ms, days);
    let n = rows.len();
    for rec in rows {
        sink.record(rec);
    }
    log::info!("usage seed: {n} fixture flights over {days} days");
    n
}

#[cfg(test)]
mod tests {
    use super::{DAY_MS, HOUR_MS, LIMIT, generate, seed};
    use dashboard_core::usage_records::{
        MemorySink, Transport, UsageSink, UsageSource, UsageStore,
    };
    use std::collections::HashSet;
    use std::sync::Arc;

    /// 2026-10-23T12:00:00Z — the captures' pinned instant.
    const ANCHOR: u64 = 1_792_756_800_000;

    #[test]
    fn deterministic_and_inside_the_window() {
        let a = generate(ANCHOR, 7);
        let b = generate(ANCHOR, 7);
        assert_eq!(a.len(), b.len());
        assert!(a.len() > 3_000, "a week should be thousands of flights");
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(
                (x.ts_epoch_ms, &x.op, x.cost, x.duration_ms, x.resp_bytes),
                (y.ts_epoch_ms, &y.op, y.cost, y.duration_ms, y.resp_bytes)
            );
        }
        assert!(a.windows(2).all(|w| w[0].ts_epoch_ms < w[1].ts_epoch_ms));
        assert!(a[0].ts_epoch_ms >= ANCHOR - 7 * DAY_MS);
        assert!(a.last().unwrap().ts_epoch_ms < ANCHOR);
        // Every day of the window carries traffic.
        let days: HashSet<u64> = a.iter().map(|r| r.ts_epoch_ms / DAY_MS).collect();
        assert!(days.len() >= 7);
    }

    #[test]
    fn realistic_mix() {
        let rows = generate(ANCHOR, 7);
        let has = |f: &dyn Fn(&dashboard_core::usage_records::UsageRecord) -> bool| {
            rows.iter().filter(|r| f(r)).count()
        };
        assert!(has(&|r| r.transport == Transport::Rest) > 500);
        assert!(has(&|r| r.transport == Transport::Graphql) > 500);
        assert!(has(&|r| r.etag_304 && r.status == 304) > 50);
        assert!(has(&|r| r.network_error && r.status == 0 && r.cost.is_none()) > 0);
        assert!(has(&|r| matches!(r.status, 403 | 404 | 502)) > 0);
        assert!(
            has(&|r| r.transport == Transport::Graphql && r.cost.is_none() && !r.network_error) > 0
        );
        // Non-zero latency, queue and payload where a flight has them.
        assert!(rows.iter().all(|r| r.duration_ms > 0));
        assert!(has(&|r| r.queue_ms.is_some_and(|q| q > 0)) > rows.len() / 2);
        assert!(
            rows.iter()
                .filter(|r| !r.etag_304 && !r.network_error)
                .all(|r| r.resp_bytes.is_some_and(|b| b > 0))
        );
        let ops: HashSet<&str> = rows.iter().map(|r| r.op.as_ref()).collect();
        let surfaces: HashSet<&str> = rows.iter().map(|r| r.surface.as_ref()).collect();
        let triggers: HashSet<&str> = rows.iter().map(|r| r.trigger.as_ref()).collect();
        assert!(ops.len() >= 15 && surfaces.len() >= 7 && triggers.len() >= 7);
        assert!(ops.contains("pr-detail") && triggers.contains("probe"));
    }

    #[test]
    fn fixture_identities_only() {
        for r in generate(ANCHOR, 3) {
            assert_eq!(r.module.as_ref(), "github");
            if let Some(id) = &r.ident {
                assert!(id.starts_with("fixture-user/"), "{id}");
            }
        }
    }

    #[test]
    fn budgets_burn_down_per_hourly_window() {
        let rows = generate(ANCHOR, 3);
        let cycles: HashSet<(&str, u64)> = rows
            .iter()
            .filter_map(|r| Some((r.transport.as_str(), r.cycle_reset_epoch?)))
            .collect();
        assert!(cycles.len() > 100, "many hourly windows per bucket");
        for tr in [Transport::Rest, Transport::Graphql] {
            let mut last: Option<(u64, u32)> = None;
            for r in rows
                .iter()
                .filter(|r| r.transport == tr && !r.network_error)
            {
                let (cycle, rem) = (r.cycle_reset_epoch.unwrap(), r.remaining_after.unwrap());
                assert!(cycle * 1000 > r.ts_epoch_ms);
                assert!(cycle * 1000 - r.ts_epoch_ms <= HOUR_MS);
                assert_eq!(r.limit_after, Some(LIMIT));
                if let Some((c, prev)) = last
                    && c == cycle
                {
                    assert!(rem <= prev, "remaining never climbs inside a window");
                }
                last = Some((cycle, rem));
            }
        }
    }

    /// docs/0082 D11's measurement gate: a 90-day read over the seeded
    /// ledger at heavy-day volume, against one frame (16 ms). Ignored by
    /// default (it writes ~10⁵ rows to a scratch SQLite file); run with
    /// `cargo test -p dashboard-github --release usage_read_gate --
    /// --ignored --nocapture`. The result is recorded in docs/0082 D11.
    #[test]
    #[ignore]
    fn usage_read_gate() {
        use dashboard_core::app_db::SQLiteStore;
        use dashboard_core::usage_analytics as ua;
        use std::time::Instant;

        let anchor = dashboard_core::freshness::clock_epoch_ms();
        let seeded = generate(anchor, 90);
        let start = anchor - 90 * DAY_MS;
        let day_of = |ts: u64| (ts - start) / DAY_MS;
        let mut per_day = vec![0usize; 90];
        for r in &seeded {
            per_day[day_of(r.ts_epoch_ms) as usize] += 1;
        }
        let (heavy_day, heavy_n) = per_day
            .iter()
            .enumerate()
            .max_by_key(|(_, n)| **n)
            .map(|(d, n)| (d as u64, *n))
            .unwrap();
        // Every day a heavy day: the heaviest seeded day, replayed 90×.
        let heavy: Vec<_> = (0..90u64)
            .flat_map(|k| {
                seeded
                    .iter()
                    .filter(move |r| day_of(r.ts_epoch_ms) == heavy_day)
                    .map(move |r| {
                        let mut r = r.clone();
                        r.ts_epoch_ms = r.ts_epoch_ms - heavy_day * DAY_MS + k * DAY_MS;
                        r
                    })
            })
            .collect();

        let median = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        for (name, rows) in [("seeded 90d", seeded.clone()), ("heavy 90d", heavy)] {
            let path = std::env::temp_dir().join(format!(
                "usage-read-gate-{}-{}.sqlite",
                std::process::id(),
                name.replace(' ', "-")
            ));
            let _ = std::fs::remove_file(&path);
            let store = SQLiteStore::open_at(path.clone());
            for r in &rows {
                store.record(r.clone());
            }
            store.flush_blocking();
            let from = anchor - 90 * DAY_MS;
            let mut read_ms = Vec::new();
            let mut got = Vec::new();
            for _ in 0..5 {
                let t = Instant::now();
                got = store.records_between(from, anchor);
                read_ms.push(t.elapsed().as_secs_f64() * 1e3);
            }
            // The page's recompute at 90d: the interim bodies' procedures
            // (docs/0082 §11 item 2) plus the header's budget cards.
            let window = ua::WindowSpec::trailing(anchor, 90 * DAY_MS);
            let filters = ua::Filters::default();
            let mut calc_ms = Vec::new();
            for _ in 0..5 {
                let t = Instant::now();
                let _ = ua::overview(&got, &ua::OverviewQuery { window });
                let _ = ua::quota_ranking(
                    &got,
                    &ua::QuotaQuery {
                        window,
                        group_by: ua::Dimension::Surface,
                        then_by: None,
                        filters: filters.clone(),
                    },
                );
                let _ = ua::latency_ranking(
                    &got,
                    &ua::LatencyQuery {
                        window,
                        group_by: ua::Dimension::Operation,
                        then_by: None,
                        filters: filters.clone(),
                        metric: ua::LatencyMetric::P90,
                    },
                );
                let _ = ua::latency_trend(
                    &got,
                    &ua::TrendQuery {
                        window,
                        filters: filters.clone(),
                    },
                );
                let _ = ua::slowest_flights(
                    &got,
                    &ua::SlowestQuery {
                        window,
                        filters: filters.clone(),
                        limit: 20,
                    },
                );
                let _ = ua::latency_glance(
                    &got,
                    &ua::GlanceQuery {
                        window,
                        filters: filters.clone(),
                    },
                );
                let _ = ua::duration_histogram(
                    &got,
                    &ua::HistogramQuery {
                        window,
                        filters: filters.clone(),
                    },
                );
                let _ = ua::revalidation_comparison(
                    &got,
                    &ua::RevalidationQuery {
                        window,
                        filters: filters.clone(),
                    },
                );
                let _ = ua::payload_split(
                    &got,
                    &ua::PayloadSplitQuery {
                        window,
                        filters: filters.clone(),
                    },
                );
                let _ = ua::contention(
                    &got,
                    &ua::ContentionQuery {
                        window,
                        filters: filters.clone(),
                    },
                );
                let _ = ua::surfaces::budget_cards(
                    &got,
                    &ua::surfaces::BudgetCardsQuery {
                        module: "github".into(),
                        as_of_ms: anchor,
                        preset: ua::surfaces::Preset::CurrentWindow,
                        rest_paused_until_ms: None,
                        graphql_paused_until_ms: None,
                    },
                );
                calc_ms.push(t.elapsed().as_secs_f64() * 1e3);
            }
            println!(
                "{name}: {} rows (heaviest seeded day {heavy_n}) · read median {:.1} ms · recompute median {:.1} ms",
                got.len(),
                median(read_ms),
                median(calc_ms),
            );
            drop(store);
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn seed_is_idempotent_per_store() {
        let store = Arc::new(MemorySink::new(100_000));
        let sink: Arc<dyn UsageSink> = store.clone();
        let source: Arc<dyn UsageSource> = store.clone();
        let n = seed(&sink, &source, ANCHOR, 2);
        assert!(n > 0);
        assert_eq!(seed(&sink, &source, ANCHOR, 2), 0);
        assert_eq!(store.snapshot().len(), n);
        let _: &dyn UsageStore = store.as_ref();
    }
}
