//! The analytics calculator (docs/0054 D13): one pure module, every
//! tab, thin renderers.
//!
//! All computation over the usage records lives here as **per-tab
//! procedures** — one call per tab view, never "everything for every
//! tab at once". Inputs are the record rows (the page's single
//! refresh read, D10) plus a per-tab query; outputs are serde structs
//! of plain owned types (no gpui types leak into the API), so a
//! future CLI/MCP maps the same structs to JSON/text with zero
//! reimplementation.
//!
//! Everything derives from the frozen window `[as_of − range, as_of)`
//! (D10): the calculator windows and filters rows itself, control
//! changes are pure recomputations, and nothing here touches storage
//! or the clock. Deterministic given rows in arrival order.
//!
//! Honesty rules carried from the design: unknown costs are counted,
//! never priced as 0 (D5); REST and GraphQL points are separate
//! pools, never summed (D11); percentiles are nearest-rank (D12);
//! projections are marked `≈` by the caller (runway is the page's
//! only projection).

use serde::{Deserialize, Serialize};

use crate::usage_records::{self, Transport, UsageRecord};
use gpui::SharedString;

// ---------------------------------------------------------------------------
// Shared inputs
// ---------------------------------------------------------------------------

/// The trailing-window choices of the global range picker (D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Range {
    H1,
    D1,
    W1,
    M1,
}

impl Range {
    pub fn ms(self) -> u64 {
        match self {
            Range::H1 => 3_600_000,
            Range::D1 => 86_400_000,
            Range::W1 => 604_800_000,
            Range::M1 => 2_592_000_000,
        }
    }

    /// Picker order (D2): 1h · 1d · 1w · 1m.
    pub fn all() -> [Range; 4] {
        [Range::H1, Range::D1, Range::W1, Range::M1]
    }

    pub fn label(self) -> &'static str {
        match self {
            Range::H1 => "1h",
            Range::D1 => "1d",
            Range::W1 => "1w",
            Range::M1 => "1m",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Range::H1 => 0,
            Range::D1 => 1,
            Range::W1 => 2,
            Range::M1 => 3,
        }
    }

    pub fn from_index(ix: usize) -> Option<Range> {
        Range::all().into_iter().nth(ix)
    }
}

/// The frozen view anchor (D10): every window in every query derives
/// from `(as_of_ms, range)` — never wall-clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowSpec {
    pub as_of_ms: u64,
    pub range: Range,
}

impl WindowSpec {
    /// `[as_of − range, as_of)` — half-open on both ends.
    pub fn bounds(&self) -> (u64, u64) {
        (self.as_of_ms.saturating_sub(self.range.ms()), self.as_of_ms)
    }

    fn contains(&self, rec: &UsageRecord) -> bool {
        let (from, to) = self.bounds();
        rec.ts_epoch_ms >= from && rec.ts_epoch_ms < to
    }
}

/// The page-global filter facets (D4): predicates, not rankings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filters {
    pub transport: Option<Transport>,
    pub module: Option<String>,
    pub errors_only: bool,
}

impl Filters {
    fn matches(&self, rec: &UsageRecord) -> bool {
        if let Some(t) = self.transport {
            if rec.transport != t {
                return false;
            }
        }
        if let Some(m) = &self.module {
            if rec.module.as_ref() != m {
                return false;
            }
        }
        if self.errors_only && !is_error(rec) {
            return false;
        }
        true
    }
}

/// An errored flight for the errors-only facet and latency's error
/// rate: a network failure or a failed status (D12).
fn is_error(rec: &UsageRecord) -> bool {
    rec.network_error || rec.status >= 400
}

/// The six categoricals of the fact table (D4). `Target` is `ident`,
/// rendered `—` when `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dimension {
    Module,
    Transport,
    Surface,
    Trigger,
    Operation,
    Target,
}

impl Dimension {
    fn value(&self, rec: &UsageRecord) -> String {
        match self {
            Dimension::Module => rec.module.to_string(),
            Dimension::Transport => rec.transport.as_str().to_string(),
            Dimension::Surface => rec.surface.to_string(),
            Dimension::Trigger => rec.trigger.to_string(),
            Dimension::Operation => rec.op.to_string(),
            Dimension::Target => rec
                .ident
                .as_ref()
                .map(|i| i.to_string())
                .unwrap_or_else(|| "—".to_string()),
        }
    }

    /// Picker and table-header label (D4).
    pub fn label(self) -> &'static str {
        match self {
            Dimension::Module => "Module",
            Dimension::Transport => "Transport",
            Dimension::Surface => "Surface",
            Dimension::Trigger => "Trigger",
            Dimension::Operation => "Operation",
            Dimension::Target => "Target",
        }
    }

    /// The six categoricals in picker order (D4). Selector count is
    /// part of the contract; this list must not grow silently.
    pub fn all() -> [Dimension; 6] {
        [
            Dimension::Surface,
            Dimension::Trigger,
            Dimension::Operation,
            Dimension::Target,
            Dimension::Transport,
            Dimension::Module,
        ]
    }

    pub fn from_label(label: &str) -> Option<Dimension> {
        Dimension::all().into_iter().find(|d| d.label() == label)
    }
}

// ---------------------------------------------------------------------------
// Per-tab queries and procedures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaQuery {
    pub window: WindowSpec,
    pub group_by: Dimension,
    pub then_by: Option<Dimension>,
    pub filters: Filters,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LatencyMetric {
    P50,
    P90,
    Max,
    ErrorRate,
    Count,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencyQuery {
    pub window: WindowSpec,
    pub group_by: Dimension,
    pub then_by: Option<Dimension>,
    pub filters: Filters,
    pub metric: LatencyMetric,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrendQuery {
    pub window: WindowSpec,
    pub filters: Filters,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlowestQuery {
    pub window: WindowSpec,
    pub filters: Filters,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverviewQuery {
    pub window: WindowSpec,
}

fn scoped<'a>(
    rows: &'a [UsageRecord],
    window: &WindowSpec,
    filters: &Filters,
) -> Vec<&'a UsageRecord> {
    rows.iter()
        .filter(|r| window.contains(r) && filters.matches(r))
        .collect()
}

// ---------------------------------------------------------------------------
// Quota ranking (D4–D7)
// ---------------------------------------------------------------------------

/// One group's aggregate — the ranking-table row and the chart bar.
/// `children` carry the `Then by` nesting (one flat table, never a
/// pivot cube); a group without `Then by` has none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaGroup {
    pub key: String,
    /// Σ `cost.unwrap_or(0)` — known costs only; unattributable
    /// flights are counted in `unknown`, never priced as 0.
    pub cost: u64,
    /// Σ cost ÷ total known cost, %; `None` when nothing costed.
    pub share_pct: Option<f64>,
    pub flights: u64,
    /// Σ cost ÷ costed flights; `None` when nothing costed.
    pub avg_cost: Option<f64>,
    /// Flights with `cost: None` (network errors).
    pub unknown: u64,
    pub children: Vec<QuotaGroup>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaTotals {
    pub flights: u64,
    pub known_cost: u64,
    pub unknown_flights: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaRanking {
    /// Sorted by Σ cost desc, ties: flights desc, then key asc —
    /// fully deterministic.
    pub groups: Vec<QuotaGroup>,
    pub totals: QuotaTotals,
}

/// `rank(records_in_window, dimension) → Vec<GroupAgg>` (D7) — the
/// one aggregation implementation behind the table and both charts.
pub fn quota_ranking(rows: &[UsageRecord], q: &QuotaQuery) -> QuotaRanking {
    let scoped = scoped(rows, &q.window, &q.filters);
    let totals = {
        let known: u64 = scoped.iter().filter_map(|r| r.cost).map(|c| c as u64).sum();
        let unknown = scoped.iter().filter(|r| r.cost.is_none()).count() as u64;
        QuotaTotals {
            flights: scoped.len() as u64,
            known_cost: known,
            unknown_flights: unknown,
        }
    };
    let groups = group_tree(&scoped, q.group_by, q.then_by);
    let known = totals.known_cost;
    let mut ranking = QuotaRanking { groups, totals };
    finalize_quota(&mut ranking.groups, known);
    ranking
}

/// Group rows by `primary`, optionally nesting by `secondary`.
fn group_tree(
    rows: &[&UsageRecord],
    primary: Dimension,
    secondary: Option<Dimension>,
) -> Vec<QuotaGroup> {
    use std::collections::BTreeMap;
    let mut by_key: BTreeMap<String, Vec<&UsageRecord>> = BTreeMap::new();
    for r in rows {
        by_key.entry(primary.value(r)).or_default().push(r);
    }
    by_key
        .into_iter()
        .map(|(key, members)| {
            let cost: u64 = members
                .iter()
                .filter_map(|r| r.cost)
                .map(|c| c as u64)
                .sum();
            let costed = members.iter().filter(|r| r.cost.is_some()).count();
            let children = secondary
                .map(|sec| {
                    let mut kids = group_tree(&members, sec, None);
                    let kid_known: u64 = kids.iter().map(|g| g.cost).sum();
                    finalize_quota(&mut kids, kid_known);
                    kids
                })
                .unwrap_or_default();
            QuotaGroup {
                key,
                cost,
                share_pct: None,
                flights: members.len() as u64,
                avg_cost: if costed > 0 {
                    Some(cost as f64 / costed as f64)
                } else {
                    None
                },
                unknown: members.iter().filter(|r| r.cost.is_none()).count() as u64,
                children,
            }
        })
        .collect()
}

/// Fill shares and impose the deterministic sort (Σ cost desc,
/// flights desc, key asc).
fn finalize_quota(groups: &mut [QuotaGroup], total_known: u64) {
    for g in groups.iter_mut() {
        g.share_pct = if total_known > 0 {
            Some(g.cost as f64 / total_known as f64 * 100.0)
        } else {
            None
        };
        finalize_quota(&mut g.children, total_known);
    }
    groups.sort_by(|a, b| {
        b.cost
            .cmp(&a.cost)
            .then(b.flights.cmp(&a.flights))
            .then_with(|| a.key.cmp(&b.key))
    });
}

// ---------------------------------------------------------------------------
// Latency ranking (D12)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencyGroup {
    pub key: String,
    pub count: u64,
    /// Nearest-rank over the group's `duration_ms`s; `None` only for
    /// an empty group (never surfaces — groups exist because rows do).
    pub p50_ms: Option<u32>,
    pub p90_ms: Option<u32>,
    pub max_ms: u32,
    /// Errored flights ÷ count, % (network failures + failed
    /// statuses).
    pub error_rate_pct: Option<f64>,
    pub children: Vec<LatencyGroup>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencyTotals {
    pub flights: u64,
    pub errors: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencyRanking {
    /// Sorted by the query's metric desc, ties: count desc, key asc.
    pub groups: Vec<LatencyGroup>,
    pub totals: LatencyTotals,
}

/// The metric-selector ranking table: same dimension machinery as
/// quota, ranked by the chosen latency metric (default p90 — latency
/// is heavy-tailed; averages hide exactly the slowness being hunted).
pub fn latency_ranking(rows: &[UsageRecord], q: &LatencyQuery) -> LatencyRanking {
    let scoped = scoped(rows, &q.window, &q.filters);
    let totals = LatencyTotals {
        flights: scoped.len() as u64,
        errors: scoped.iter().filter(|r| is_error(r)).count() as u64,
    };
    let groups = latency_tree(&scoped, q.group_by, q.then_by);
    let mut ranking = LatencyRanking { groups, totals };
    finalize_latency(&mut ranking.groups, q.metric);
    ranking
}

fn latency_tree(
    rows: &[&UsageRecord],
    primary: Dimension,
    secondary: Option<Dimension>,
) -> Vec<LatencyGroup> {
    use std::collections::BTreeMap;
    let mut by_key: BTreeMap<String, Vec<&UsageRecord>> = BTreeMap::new();
    for r in rows {
        by_key.entry(primary.value(r)).or_default().push(r);
    }
    by_key
        .into_iter()
        .map(|(key, members)| {
            let children = secondary
                .map(|sec| {
                    let mut kids = latency_tree(&members, sec, None);
                    finalize_latency(&mut kids, LatencyMetric::P90);
                    kids
                })
                .unwrap_or_default();
            LatencyGroup {
                key,
                count: members.len() as u64,
                p50_ms: Some(percentile(&members, 50)),
                p90_ms: Some(percentile(&members, 90)),
                max_ms: members.iter().map(|r| r.duration_ms).max().unwrap_or(0),
                error_rate_pct: Some(
                    members.iter().filter(|r| is_error(r)).count() as f64 / members.len() as f64
                        * 100.0,
                ),
                children,
            }
        })
        .collect()
}

fn metric_value(g: &LatencyGroup, m: LatencyMetric) -> f64 {
    match m {
        LatencyMetric::P50 => g.p50_ms.unwrap_or(0) as f64,
        LatencyMetric::P90 => g.p90_ms.unwrap_or(0) as f64,
        LatencyMetric::Max => g.max_ms as f64,
        LatencyMetric::ErrorRate => g.error_rate_pct.unwrap_or(0.0),
        LatencyMetric::Count => g.count as f64,
    }
}

fn finalize_latency(groups: &mut [LatencyGroup], metric: LatencyMetric) {
    for g in groups.iter_mut() {
        finalize_latency(&mut g.children, metric);
    }
    groups.sort_by(|a, b| {
        metric_value(b, metric)
            .partial_cmp(&metric_value(a, metric))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.count.cmp(&a.count))
            .then_with(|| a.key.cmp(&b.key))
    });
}

/// Nearest-rank percentile (D12): sorted durations, rank
/// `ceil(p·n/100)` clamped to `[1, n]`, value at that 1-based rank.
fn percentile(rows: &[&UsageRecord], p: u64) -> u32 {
    let mut ds: Vec<u32> = rows.iter().map(|r| r.duration_ms).collect();
    ds.sort_unstable();
    let n = ds.len();
    if n == 0 {
        return 0;
    }
    let rank = (p * n as u64).div_ceil(100).clamp(1, n as u64) as usize;
    ds[rank - 1]
}

// ---------------------------------------------------------------------------
// Time buckets (D12 trend, D15 spend, D16 p90 line)
// ---------------------------------------------------------------------------

/// The shared ~40-bucket rule: bucket length = ceil(range ÷ 40), so
/// the count never exceeds 40 regardless of range (1h → 90 s
/// buckets, 1d → ~36 min). Empty buckets stay in the series — chart
/// continuity and honest gaps.
fn buckets(window: &WindowSpec) -> (u64, u64, usize) {
    let (start, end) = window.bounds();
    let span = end - start;
    let len = span.div_ceil(40).max(1);
    let count = ((end - start) / len) as usize;
    (start, len, count)
}

fn bucket_index(ts: u64, start: u64, len: u64) -> usize {
    ((ts - start) / len) as usize
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencyBucket {
    pub start_ms: u64,
    pub end_ms: u64,
    pub flights: u64,
    pub p50_ms: Option<u32>,
    pub p90_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencyTrend {
    pub buckets: Vec<LatencyBucket>,
}

/// The p50/p90 trend across the window (D12 view 2) — `as_of`
/// anchored like everything else.
pub fn latency_trend(rows: &[UsageRecord], q: &TrendQuery) -> LatencyTrend {
    let scoped = scoped(rows, &q.window, &q.filters);
    let (start, len, count) = buckets(&q.window);
    let mut slots: Vec<Vec<&UsageRecord>> = vec![Vec::new(); count];
    for r in scoped {
        let idx = bucket_index(r.ts_epoch_ms, start, len);
        if idx < count {
            slots[idx].push(r);
        }
    }
    LatencyTrend {
        buckets: slots
            .into_iter()
            .enumerate()
            .map(|(i, rows)| LatencyBucket {
                start_ms: start + i as u64 * len,
                end_ms: start + (i as u64 + 1) * len,
                flights: rows.len() as u64,
                p50_ms: (!rows.is_empty()).then(|| percentile(&rows, 50)),
                p90_ms: (!rows.is_empty()).then(|| percentile(&rows, 90)),
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Slowest flights (D12 view 3)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlowFlight {
    pub ts_epoch_ms: u64,
    pub op: String,
    pub ident: Option<String>,
    pub surface: String,
    pub trigger: String,
    pub status: u16,
    pub duration_ms: u32,
    /// Present so self-wait is separable from network wait.
    pub queue_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlowestFlights {
    /// Duration desc, ties: earlier flight first, then seq.
    pub flights: Vec<SlowFlight>,
}

pub fn slowest_flights(rows: &[UsageRecord], q: &SlowestQuery) -> SlowestFlights {
    let mut scoped: Vec<&UsageRecord> = scoped(rows, &q.window, &q.filters);
    scoped.sort_by(|a, b| {
        b.duration_ms
            .cmp(&a.duration_ms)
            .then(a.ts_epoch_ms.cmp(&b.ts_epoch_ms))
            .then(a.seq.cmp(&b.seq))
    });
    SlowestFlights {
        flights: scoped
            .into_iter()
            .take(q.limit)
            .map(|r| SlowFlight {
                ts_epoch_ms: r.ts_epoch_ms,
                op: r.op.to_string(),
                ident: r.ident.as_ref().map(|i| i.to_string()),
                surface: r.surface.to_string(),
                trigger: r.trigger.to_string(),
                status: r.status,
                duration_ms: r.duration_ms,
                queue_ms: r.queue_ms,
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Overview (D11, D15, D16) — one procedure, the tab's full view
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PointsSpentCard {
    /// Separate pools with independent cycles — never summed (D11).
    pub rest_points: u64,
    pub gql_points: u64,
    pub flights: u64,
    pub unattributable: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetTransportCard {
    pub transport: String,
    pub remaining: u32,
    /// % of `limit_after` from the same snapshot; `None` when the
    /// snapshot lacks a ceiling — the card degrades to absolute-only,
    /// never invents one.
    pub remaining_pct: Option<f64>,
    pub limit: Option<u32>,
    /// `as_of − snapshot ts` — staleness stays visible.
    pub snapshot_age_ms: u64,
    /// remaining ÷ (spend ÷ window hours), hours — the page's only
    /// projection (`≈` is the caller's marking). `None` when nothing
    /// was spent in the window (no pace to divide by).
    pub runway_hours: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsumerEntry {
    pub name: String,
    pub cost: u64,
    pub share_pct: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverviewCards {
    pub points_spent: PointsSpentCard,
    pub budget: Vec<BudgetTransportCard>,
    pub top_consumer: Vec<ConsumerEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BurnPoint {
    pub ts_epoch_ms: u64,
    pub remaining: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BurnSeries {
    pub transport: String,
    /// Step points, `(ts, seq)`-sorted; the last known value carries
    /// forward to `as_of` (that is what remaining *was*, as far as any
    /// response said).
    pub points: Vec<BurnPoint>,
    /// From the latest `limit_after` in-window; `None` until one
    /// exists (D14).
    pub ceiling: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BurnDown {
    pub series: Vec<BurnSeries>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpendBucket {
    pub start_ms: u64,
    pub end_ms: u64,
    pub rest: u64,
    pub gql: u64,
    /// The bucket's top surface by known cost (tooltip content:
    /// "14:00–14:36 · 9 pts · mostly pr-page").
    pub top_surface: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpendTrend {
    pub buckets: Vec<SpendBucket>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct P90Bucket {
    pub start_ms: u64,
    pub end_ms: u64,
    pub flights: u64,
    /// Pooled across transports on purpose (D16): milliseconds are
    /// one currency; the line answers "is the app feeling slow right
    /// now". The splits below serve the tooltip.
    pub pooled_p90_ms: Option<u32>,
    pub rest_p90_ms: Option<u32>,
    pub gql_p90_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct P90Trend {
    pub buckets: Vec<P90Bucket>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlowestOp {
    pub op: String,
    pub p90_ms: u32,
    pub worst_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverviewView {
    pub cards: OverviewCards,
    pub burn_down: BurnDown,
    pub spend_trend: SpendTrend,
    pub p90_trend: P90Trend,
    pub slowest_op: Option<SlowestOp>,
}

/// The Overview tab's full view (D11/D15/D16) — cards, charts, and
/// the slowest-op card, all from one call (D13).
pub fn overview(rows: &[UsageRecord], q: &OverviewQuery) -> OverviewView {
    let scoped = scoped(rows, &q.window, &Filters::default());
    let (start, len, count) = buckets(&q.window);

    // --- points spent (D11 card 1) ---
    let points_spent = PointsSpentCard {
        rest_points: scoped
            .iter()
            .filter(|r| r.transport == Transport::Rest)
            .filter_map(|r| r.cost)
            .map(|c| c as u64)
            .sum(),
        gql_points: scoped
            .iter()
            .filter(|r| r.transport == Transport::Graphql)
            .filter_map(|r| r.cost)
            .map(|c| c as u64)
            .sum(),
        flights: scoped.len() as u64,
        unattributable: scoped.iter().filter(|r| r.cost.is_none()).count() as u64,
    };

    // --- budget & runway (D11 card 2), burn-down (D15 chart 1) ---
    let mut budget = Vec::new();
    let mut burn = Vec::new();
    for transport in [Transport::Rest, Transport::Graphql] {
        let rows_t: Vec<&UsageRecord> = scoped
            .iter()
            .copied()
            .filter(|r| r.transport == transport)
            .collect();
        let mut snapshots: Vec<&&UsageRecord> = rows_t
            .iter()
            .filter(|r| r.remaining_after.is_some())
            .collect();
        snapshots.sort_by(|a, b| a.ts_epoch_ms.cmp(&b.ts_epoch_ms).then(a.seq.cmp(&b.seq)));
        if let Some(latest) = snapshots.last() {
            let spend: u64 = rows_t.iter().filter_map(|r| r.cost).map(|c| c as u64).sum();
            let window_hours = q.window.range.ms() as f64 / 3_600_000.0;
            let burn_per_hour = spend as f64 / window_hours;
            budget.push(BudgetTransportCard {
                transport: transport.as_str().to_string(),
                remaining: latest.remaining_after.unwrap_or(0),
                remaining_pct: latest
                    .limit_after
                    .map(|limit| latest.remaining_after.unwrap_or(0) as f64 / limit as f64 * 100.0),
                limit: latest.limit_after,
                snapshot_age_ms: q.window.as_of_ms.saturating_sub(latest.ts_epoch_ms),
                runway_hours: if burn_per_hour > 0.0 {
                    Some(latest.remaining_after.unwrap_or(0) as f64 / burn_per_hour)
                } else {
                    None
                },
            });
        }
        let mut points: Vec<BurnPoint> = snapshots
            .iter()
            .map(|r| BurnPoint {
                ts_epoch_ms: r.ts_epoch_ms,
                remaining: r.remaining_after.unwrap_or(0),
            })
            .collect();
        if let Some(last) = points.last() {
            // Carry the last known value to `as_of` — flight-less
            // stretches are flat, not missing (D15).
            if last.ts_epoch_ms < q.window.as_of_ms {
                let carried = last.remaining;
                points.push(BurnPoint {
                    ts_epoch_ms: q.window.as_of_ms,
                    remaining: carried,
                });
            }
        }
        let ceiling = rows_t.iter().filter_map(|r| r.limit_after).max(); // latest ≈ max ts, but max is order-free & deterministic
        burn.push(BurnSeries {
            transport: transport.as_str().to_string(),
            points,
            ceiling,
        });
    }

    // --- top consumer (D11 card 3): #1 surface + mini top-3 ---
    let known_total: u64 = scoped.iter().filter_map(|r| r.cost).map(|c| c as u64).sum();
    use std::collections::BTreeMap;
    let mut by_surface: BTreeMap<String, u64> = BTreeMap::new();
    for r in &scoped {
        if let Some(c) = r.cost {
            *by_surface.entry(r.surface.to_string()).or_insert(0) += c as u64;
        }
    }
    let mut consumers: Vec<ConsumerEntry> = by_surface
        .into_iter()
        .map(|(name, cost)| ConsumerEntry {
            name,
            cost,
            share_pct: if known_total > 0 {
                Some(cost as f64 / known_total as f64 * 100.0)
            } else {
                None
            },
        })
        .collect();
    consumers.sort_by(|a, b| b.cost.cmp(&a.cost).then(a.name.cmp(&b.name)));
    consumers.truncate(3);
    let top_consumer = consumers;

    // --- spend over time (D15 chart 2) ---
    let mut spend_slots: Vec<(u64, u64, BTreeMap<String, u64>)> =
        (0..count).map(|_| (0, 0, BTreeMap::new())).collect();
    for r in &scoped {
        let idx = bucket_index(r.ts_epoch_ms, start, len);
        if idx >= count {
            continue;
        }
        let c = r.cost.unwrap_or(0) as u64;
        match r.transport {
            Transport::Rest => spend_slots[idx].0 += c,
            Transport::Graphql => spend_slots[idx].1 += c,
            Transport::Auth => {} // never bills (docs/0050 D1)
        }
        if c > 0 {
            *spend_slots[idx].2.entry(r.surface.to_string()).or_insert(0) += c;
        }
    }
    let spend_trend = SpendTrend {
        buckets: spend_slots
            .into_iter()
            .enumerate()
            .map(|(i, (rest, gql, surfaces))| {
                let top_surface = surfaces
                    .into_iter()
                    .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0))); // cost desc, name asc on ties
                SpendBucket {
                    start_ms: start + i as u64 * len,
                    end_ms: start + (i as u64 + 1) * len,
                    rest,
                    gql,
                    top_surface: top_surface.map(|(name, _)| name),
                }
            })
            .collect(),
    };

    // --- p90 trend (D16 chart 3): pooled line, tooltip splits ---
    let mut p90_slots: Vec<(Vec<&UsageRecord>, Vec<&UsageRecord>, Vec<&UsageRecord>)> = (0..count)
        .map(|_| (Vec::new(), Vec::new(), Vec::new()))
        .collect();
    for r in &scoped {
        let idx = bucket_index(r.ts_epoch_ms, start, len);
        if idx >= count {
            continue;
        }
        match r.transport {
            Transport::Rest => p90_slots[idx].0.push(r),
            Transport::Graphql => p90_slots[idx].1.push(r),
            Transport::Auth => p90_slots[idx].2.push(r),
        }
    }
    let p90_trend = P90Trend {
        buckets: p90_slots
            .into_iter()
            .enumerate()
            .map(|(i, (rest, gql, auth))| {
                let pooled: Vec<&UsageRecord> = rest
                    .iter()
                    .chain(gql.iter())
                    .chain(auth.iter())
                    .copied()
                    .collect();
                P90Bucket {
                    start_ms: start + i as u64 * len,
                    end_ms: start + (i as u64 + 1) * len,
                    flights: pooled.len() as u64,
                    pooled_p90_ms: (!pooled.is_empty()).then(|| percentile(&pooled, 90)),
                    rest_p90_ms: (!rest.is_empty()).then(|| percentile(&rest, 90)),
                    gql_p90_ms: (!gql.is_empty()).then(|| percentile(&gql, 90)),
                }
            })
            .collect(),
    };

    // --- slowest op (D16 card 4): max-p90 group, deterministic ties ---
    let mut by_op: BTreeMap<String, Vec<&UsageRecord>> = BTreeMap::new();
    for r in &scoped {
        by_op.entry(r.op.to_string()).or_default().push(r);
    }
    let slowest_op = by_op
        .into_iter()
        .map(|(op, members)| SlowestOp {
            p90_ms: percentile(&members, 90),
            worst_ms: members.iter().map(|r| r.duration_ms).max().unwrap_or(0),
            op,
        })
        .max_by(|a, b| {
            a.p90_ms
                .cmp(&b.p90_ms)
                .then(b.worst_ms.cmp(&a.worst_ms))
                .then(b.op.cmp(&a.op))
        });

    OverviewView {
        cards: OverviewCards {
            points_spent,
            budget,
            top_consumer,
        },
        burn_down: BurnDown { series: burn },
        spend_trend,
        p90_trend,
        slowest_op,
    }
}

// ---------------------------------------------------------------------------
// Page component (docs/0054 §3 — D1/D2/D3/D10, the skeleton)
// ---------------------------------------------------------------------------

use gpui::Hsla;
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Render, Window, div, px};
use gpui_component::{
    ActiveTheme, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants},
    chart::{BarChart, LineChart, PieChart},
    h_flex,
    plot::shape::BarAlignment,
    searchable_list::SearchableVec,
    select::{Select, SelectEvent, SelectState},
    switch::Switch,
    tab::{Tab, TabBar},
    table::{Column, DataTable, TableDelegate, TableState},
    v_flex,
};

/// The tab strip (D3): one tab per designed use case. Tab selection is
/// component-local — never nav state — and switching tabs keeps the
/// range **and** the frozen `as_of` (both page-global).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageTab {
    Overview,
    Quota,
}

impl UsageTab {
    pub fn label(self) -> &'static str {
        match self {
            UsageTab::Overview => "Overview",
            UsageTab::Quota => "Quota usage",
        }
    }

    pub fn index(self) -> usize {
        match self {
            UsageTab::Overview => 0,
            UsageTab::Quota => 1,
        }
    }

    pub fn from_index(ix: usize) -> Option<Self> {
        match ix {
            0 => Some(Self::Overview),
            1 => Some(Self::Quota),
            _ => None,
        }
    }
}

/// Transport facet segments (D4): `All` plus the three wires.
const TRANSPORT_SEGMENTS: [(&str, Option<Transport>); 4] = [
    ("All", None),
    ("REST", Some(Transport::Rest)),
    ("GraphQL", Some(Transport::Graphql)),
    ("Auth", Some(Transport::Auth)),
];

/// The usage analytics page (docs/0054 §3): global range picker +
/// page-global filter facets + tab strip, all over a frozen view
/// anchor.
///
/// State discipline (D10): `as_of_ms` moves — and the store is read —
/// on first open and on refresh only. Every control change is a pure
/// recomputation over `rows`.
pub struct UsagePage {
    /// The freeze: "now, for the data on screen". Every window on
    /// every tab derives from it; only [`UsagePage::refresh`] (and
    /// first open) move it. Post-freeze arrivals have
    /// `ts >= as_of_ms` and stay invisible until refresh.
    pub(crate) as_of_ms: u64,
    /// The one bounded read per refresh (D10): rows in
    /// `[as_of − 1 month, as_of)` — the widest picker option, so any
    /// range is a re-slice of the same fetched data. The memory
    /// backend is synchronous; a blocking backend runs the read
    /// off-main (D9).
    pub(crate) rows: Vec<UsageRecord>,
    /// The global range picker (D2), default 1d.
    pub(crate) range: Range,
    /// Page-global facets (D4): predicates, not rankings — they apply
    /// to every tab.
    pub(crate) transport: Option<Transport>,
    pub(crate) module: Option<String>,
    pub(crate) errors_only: bool,
    /// Active tab (D3).
    pub(crate) tab: UsageTab,
    /// Quota tab state (D4–D7): the selectors, the cached ranking,
    /// and the table entity. `None` only in pure-derivation tests
    /// that never render; `new` always creates it — it needs a
    /// `Window` exactly once, for the Selects and the table entity.
    quota: Option<Entity<QuotaTab>>,
    /// The Overview tab's cached view (D11/D15/D16): one `overview`
    /// call per first-open/refresh/control change. Render is a pure
    /// function of this snapshot; nothing on the tab is interactive,
    /// so — unlike the quota tab — it needs no entity of its own.
    overview_view: OverviewView,
}

impl UsagePage {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut page = Self {
            as_of_ms: usage_records::now_epoch_ms(),
            rows: Vec::new(),
            range: Range::D1,
            transport: None,
            module: None,
            errors_only: false,
            tab: UsageTab::Overview,
            quota: Some(cx.new(|cx| QuotaTab::new(window, cx))),
            overview_view: empty_overview(),
        };
        // One load on first open (D10): the page never opens empty
        // demanding a click.
        page.reload(cx);
        // Both tabs derive from the fetched rows (D7) — same anchor,
        // same facets.
        page.recompute_tabs(cx);
        page
    }

    /// The bounded read, half-open on both ends — the upper bound is
    /// what makes the freeze real (D9/D10).
    fn reload(&mut self, cx: &Context<Self>) {
        let from = self.as_of_ms.saturating_sub(Range::M1.ms());
        self.rows = usage_records::source(cx).records_between(from, self.as_of_ms);
        // A module facet that outlived its rows (refresh dropped them)
        // resets to All — never a silent always-empty filter.
        if let Some(m) = &self.module {
            if !self.rows.iter().any(|r| r.module.as_ref() == m) {
                self.module = None;
            }
        }
    }

    /// The refresh button's action (D10): the ONLY path that moves
    /// `as_of` or touches the store after first open. Stamps a new
    /// freeze and re-reads the window; every view on the page then
    /// recomputes from the new anchor. This is not a fetch: memory
    /// only — no `Http`, no specs, no status spinner.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.as_of_ms = usage_records::now_epoch_ms();
        self.reload(cx);
        self.recompute_tabs(cx);
    }

    /// Push the page-global query inputs (frozen window, facets,
    /// fetched rows) into every tab (D7): a pure re-derivation — no
    /// store read. Windowless by design: the quota tab's delegate is
    /// swapped in place, so nothing here needs a `Window`.
    fn recompute_tabs(&mut self, cx: &mut Context<Self>) {
        self.overview_view = self.compute_overview();
        let Some(tab) = self.quota.clone() else {
            return;
        };
        let window = self.window();
        let filters = self.filters();
        let rows = std::mem::take(&mut self.rows);
        tab.update(cx, |tab, cx| tab.sync(window, filters, &rows, cx));
        self.rows = rows;
    }

    /// The Overview tab's view (D11/D15/D16): `overview` over the
    /// facet-filtered rows — the calculator scopes by the window
    /// itself, so facets are a pre-filter here (D4: page-global
    /// predicates apply to every tab).
    fn compute_overview(&self) -> OverviewView {
        let filtered: Vec<UsageRecord> = self
            .rows
            .iter()
            .filter(|r| self.filters().matches(r))
            .cloned()
            .collect();
        overview(
            &filtered,
            &OverviewQuery {
                window: self.window(),
            },
        )
    }

    /// The frozen window every tab derives from (D2/D10): bounds from
    /// `as_of`, never wall-clock.
    fn window(&self) -> WindowSpec {
        WindowSpec {
            as_of_ms: self.as_of_ms,
            range: self.range,
        }
    }

    fn filters(&self) -> Filters {
        Filters {
            transport: self.transport,
            module: self.module.clone(),
            errors_only: self.errors_only,
        }
    }

    /// Rows in the visible window with facets applied — the input
    /// every tab recomputes from on control changes (pure; no
    /// re-read).
    fn visible_rows(&self) -> Vec<&UsageRecord> {
        scoped(&self.rows, &self.window(), &self.filters())
    }

    /// Modules present in the fetched window — the module facet's
    /// options (the page is cross-module by design, D1).
    fn modules_in_rows(&self) -> Vec<String> {
        let mut mods: Vec<String> = self.rows.iter().map(|r| r.module.to_string()).collect();
        mods.sort();
        mods.dedup();
        mods
    }

    /// The honesty header's facts (D2): what's on screen and how far
    /// retention actually reaches.
    fn header_facts(&self) -> (usize, Option<u64>) {
        (
            self.visible_rows().len(),
            self.rows.iter().map(|r| r.ts_epoch_ms).min(),
        )
    }
}

fn format_clock(epoch_ms: u64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(epoch_ms as i64)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_else(|| "—".into())
}

impl Render for UsagePage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (flights, oldest_retained) = self.header_facts();

        v_flex()
            .id("usage-page")
            .size_full()
            .p_4()
            .gap_3()
            // Control strip (D2/D4): range picker + refresh button,
            // then the page-global facets. Every id is bridge-visible
            // (docs/0010 D10).
            .child(
                h_flex()
                    .items_center()
                    .gap_3()
                    .flex_wrap()
                    .child(
                        TabBar::new("usage-range")
                            .segmented()
                            .selected_index(self.range.index())
                            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                // Pure recomputation (D10): the new
                                // window re-derives from the same
                                // frozen `as_of` — no store read.
                                if let Some(range) = Range::from_index(*ix) {
                                    this.range = range;
                                    this.recompute_tabs(cx);
                                    cx.notify();
                                }
                            }))
                            .children(Range::all().iter().map(|r| Tab::new().label(r.label()))),
                    )
                    .child(
                        Button::new("usage-refresh")
                            .debug_selector(|| "usage-refresh".to_string())
                            .icon(IconName::RotateCw)
                            .ghost()
                            .small()
                            .tooltip("Refresh — re-stamp 'as of' and re-read the window")
                            // docs/0049 D1: icon-only buttons get an
                            // explicit accessible name.
                            .accessibility_label("Refresh")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.refresh(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        TabBar::new("usage-filter-transport")
                            .segmented()
                            .selected_index(
                                TRANSPORT_SEGMENTS
                                    .iter()
                                    .position(|(_, t)| *t == self.transport)
                                    .unwrap_or(0),
                            )
                            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                if let Some((_, t)) = TRANSPORT_SEGMENTS.get(*ix) {
                                    this.transport = *t;
                                    this.recompute_tabs(cx);
                                    cx.notify();
                                }
                            }))
                            .children(
                                TRANSPORT_SEGMENTS
                                    .iter()
                                    .map(|(label, _)| Tab::new().label(*label)),
                            ),
                    )
                    .child(
                        TabBar::new("usage-filter-module")
                            .segmented()
                            .selected_index(
                                self.module
                                    .as_ref()
                                    .and_then(|m| self.modules_in_rows().iter().position(|x| x == m))
                                    .map(|p| p + 1)
                                    .unwrap_or(0),
                            )
                            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                let mods = this.modules_in_rows();
                                this.module = match ix {
                                    0 => None,
                                    i => mods.get(i - 1).cloned(),
                                };
                                this.recompute_tabs(cx);
                                cx.notify();
                            }))
                            .children(std::iter::once(Tab::new().label("All")).chain(
                                self.modules_in_rows()
                                    .iter()
                                    .map(|m| Tab::new().label(m.clone())),
                            )),
                    )
                    .child(
                        Switch::new("usage-errors-only")
                            .checked(self.errors_only)
                            .label("errors only")
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.errors_only = *checked;
                                this.recompute_tabs(cx);
                                cx.notify();
                            })),
                    ),
            )
            // Honesty header (D2): never pretends to more history than
            // the retained records support. Exposed as StaticText
            // (docs/0049 content-a11y pattern, home.rs): a status line
            // that changes under the viewer must be announcable — and
            // it doubles as the bridge-visible freeze signal.
            .child(
                div()
                    .id("usage-honesty")
                    .role(gpui::accesskit::Role::Status)
                    .aria_label(format!(
                        "{flights} flights · as of {} · oldest retained {}",
                        format_clock(self.as_of_ms),
                        oldest_retained.map_or_else(|| "—".into(), format_clock),
                    ))
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{flights} flights · as of {} · oldest retained {}",
                        format_clock(self.as_of_ms),
                        oldest_retained.map_or_else(|| "—".into(), format_clock),
                    )),
            )
            // Tab strip (D3): one tab per designed use case; content
            // arrives with each tab's implementation (#342–#344).
            .child(
                TabBar::new("usage-tabs")
                    .underline()
                    .selected_index(self.tab.index())
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        // Tab switches keep the range and the frozen
                        // `as_of` (D3) — both page-global.
                        if let Some(tab) = UsageTab::from_index(*ix) {
                            this.tab = tab;
                            cx.notify();
                        }
                    }))
                    .child(Tab::new().label(UsageTab::Overview.label()))
                    .child(Tab::new().label(UsageTab::Quota.label())),
            )
            .child(match self.tab {
                UsageTab::Overview => {
                    render_overview_tab(&self.overview_view, cx).into_any_element()
                }
                UsageTab::Quota => match &self.quota {
                    Some(tab) => tab.clone().into_any_element(),
                    None => tab_placeholder(
                        "Quota ranking, charts and table arrive with their implementation (docs/0054 §4).",
                        cx,
                    )
                    .into_any_element(),
                },
            })
    }
}

fn tab_placeholder(text: &str, cx: &Context<UsagePage>) -> impl IntoElement {
    v_flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child(text.to_string()),
        )
}

// ---------------------------------------------------------------------------
// Overview tab (docs/0054 §5 — D11/D15/D16)
// ---------------------------------------------------------------------------

/// `OverviewView` with nothing in it — the page literal's placeholder
/// before the first `recompute_tabs` (and the skeleton tests' fill).
fn empty_overview() -> OverviewView {
    OverviewView {
        cards: OverviewCards {
            points_spent: PointsSpentCard {
                rest_points: 0,
                gql_points: 0,
                flights: 0,
                unattributable: 0,
            },
            budget: Vec::new(),
            top_consumer: Vec::new(),
        },
        burn_down: BurnDown { series: Vec::new() },
        spend_trend: SpendTrend {
            buckets: Vec::new(),
        },
        p90_trend: P90Trend {
            buckets: Vec::new(),
        },
        slowest_op: None,
    }
}

/// Second-resolution clock label for point-scale x axes. Upstream's
/// `LineChart` x scale is ordinal (a d3 point scale): equal labels
/// collapse onto the first position, so chart labels need second
/// resolution — minute labels collide on the 1h range's 90 s buckets.
fn format_hms(epoch_ms: u64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(epoch_ms as i64)
        .single()
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "—".into())
}

/// Ordinal-x labels, de-duplicated: two snapshots in the same second
/// get a `′` mark rather than folding onto one tick.
fn unique_labels(ts: &[u64]) -> Vec<SharedString> {
    let mut labels: Vec<String> = ts.iter().map(|t| format_hms(*t)).collect();
    for i in 0..labels.len() {
        let dupes = labels[..i].iter().filter(|l| **l == labels[i]).count();
        if dupes > 0 {
            labels[i].push_str(&"′".repeat(dupes));
        }
    }
    labels.into_iter().map(SharedString::from).collect()
}

/// Snapshot age copy: `3m ago`, `2h ago` — staleness stays visible
/// (D11 card 2).
fn age_of(ms: u64) -> String {
    let m = ms / 60_000;
    if m < 60 {
        format!("{m}m ago")
    } else {
        format!("{}h ago", m / 60)
    }
}

/// Runway copy: thousands-grouped hours (`≈ 59,964 h`) — big
/// numbers on a nearly-untouched window are honest, but not ugly.
fn fmt_hours(h: f64) -> String {
    let n = h.round() as u64;
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

/// Latency copy: `812 ms`, `2.3 s`.
fn fmt_ms(ms: u32) -> String {
    if ms < 1_000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", ms as f64 / 1_000.0)
    }
}

/// One D11 card: number-first, one-line details, an id + `Label` role
/// carrying the same numbers for AT (pixels are invisible).
fn stat_card(
    id: &str,
    title: &str,
    headline: SharedString,
    lines: Vec<SharedString>,
    a11y: String,
    cx: &Context<UsagePage>,
) -> impl IntoElement {
    let theme = cx.theme();
    let (border, fg, muted) = (theme.border, theme.foreground, theme.muted_foreground);
    v_flex()
        .id(id.to_string())
        .role(gpui::accesskit::Role::Label)
        .aria_label(a11y)
        .flex_1()
        .min_w(px(220.))
        .gap_1()
        .p_3()
        .border_1()
        .border_color(border)
        .rounded_md()
        .child(
            div()
                .text_sm()
                .text_color(muted)
                .child(SharedString::from(title)),
        )
        .child(div().text_lg().text_color(fg).child(headline))
        .children(
            lines
                .into_iter()
                .map(|l| div().text_sm().text_color(muted).child(l)),
        )
}

/// One chart panel: the a11y `Label` summary (the D8/D16 contract —
/// chart pixels are invisible to AT), the chart itself, and an
/// optional caption line beneath.
fn line_panel(
    id: &str,
    summary: String,
    data: Vec<(SharedString, f64)>,
    stroke: Hsla,
    step: bool,
    caption: Option<SharedString>,
    cx: &Context<UsagePage>,
) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let mut chart = LineChart::new(data)
        .id(id.to_string())
        .name(id)
        .x(|p: &(SharedString, f64)| p.0.clone())
        .y(|p: &(SharedString, f64)| p.1)
        .stroke(stroke);
    if step {
        chart = chart.step_after();
    }
    v_flex()
        .flex_1()
        .min_w(px(280.))
        .gap_1()
        .child(
            div()
                .id(format!("{id}-summary"))
                .role(gpui::accesskit::Role::Label)
                .aria_label(summary.clone())
                .text_sm()
                .text_color(muted)
                .child(summary),
        )
        .child(div().h(px(180.)).child(chart))
        .when_some(caption, |this, caption| {
            this.child(div().text_sm().text_color(muted).child(caption))
        })
}

/// The budget card's per-transport line: `REST 84% · 90 of 100 ·
/// snapshot 3m ago`, absolute-only when no ceiling was recorded, plus
/// the page's only projection (`≈`) on its own line.
fn budget_lines(b: &BudgetTransportCard) -> Vec<SharedString> {
    let mut lines = Vec::new();
    let main = match (b.remaining_pct, b.limit) {
        (Some(pct), Some(limit)) => {
            format!("{} {:.0}% · {} of {}", b.transport, pct, b.remaining, limit)
        }
        _ => format!("{} {} left", b.transport, b.remaining),
    };
    let age = format!("snapshot {}", age_of(b.snapshot_age_ms));
    lines.push(SharedString::from(format!("{main} · {age}")));
    if let Some(h) = b.runway_hours {
        lines.push(SharedString::from(format!(
            "≈ {} h at current pace",
            fmt_hours(h)
        )));
    }
    lines
}

/// The Overview tab (§5): four cards (D11/D16 card 4), then the chart
/// rows (D15 burn-down + spend, D16 p90). Everything is a pure
/// function of the cached `OverviewView` — no listeners, no state.
fn render_overview_tab(view: &OverviewView, cx: &Context<UsagePage>) -> impl IntoElement {
    let flights = view.cards.points_spent.flights;
    if flights == 0 {
        return v_flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("No flights in the window — widen the range or refresh."),
            )
            .into_any_element();
    }
    let theme = cx.theme();
    let (chart_1, chart_2) = (theme.chart_1, theme.chart_2);
    let muted_fg = theme.muted_foreground;

    // --- D11 card 1: points spent, split by pool, never summed ---
    let ps = &view.cards.points_spent;
    let points_card = stat_card(
        "usage-card-points",
        "Points spent",
        SharedString::from(format!("REST {} · GQL {}", ps.rest_points, ps.gql_points)),
        vec![SharedString::from(format!(
            "{} flights · {} unattributable",
            ps.flights, ps.unattributable
        ))],
        format!(
            "Points spent: REST {}, GraphQL {}; {} flights, {} unattributable",
            ps.rest_points, ps.gql_points, ps.flights, ps.unattributable
        ),
        cx,
    );

    // --- D11 card 2: budget & runway (absolute-only without a
    // ceiling; `≈` marks the page's only projection) ---
    let budget_headline = match view.cards.budget.first() {
        Some(b) => match b.remaining_pct {
            Some(pct) => SharedString::from(format!("{} {:.0}%", b.transport, pct)),
            None => SharedString::from(format!("{} {} left", b.transport, b.remaining)),
        },
        None => SharedString::from("no snapshot"),
    };
    let budget_lines: Vec<SharedString> = view.cards.budget.iter().flat_map(budget_lines).collect();
    let budget_lines = if budget_lines.is_empty() {
        vec![SharedString::from("no budget snapshot in the window")]
    } else {
        budget_lines
    };
    let budget_a11y = if view.cards.budget.is_empty() {
        "Budget: no snapshot in the window".to_string()
    } else {
        format!(
            "Budget and runway: {}. Runway is a projection from the window's pace.",
            budget_lines
                .iter()
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
                .join("; ")
        )
    };
    let budget_card = stat_card(
        "usage-card-budget",
        "Budget & runway",
        budget_headline,
        budget_lines,
        budget_a11y,
        cx,
    );

    // --- D11 card 3: top consumer + mini top-3 (shares of known) ---
    let (consumer_headline, consumer_lines, consumer_a11y) = match view.cards.top_consumer.first() {
        Some(top) => {
            let share = top
                .share_pct
                .map_or_else(String::new, |p| format!(" · {:.0}%", p));
            (
                SharedString::from(format!("{} · {} pts{}", top.name, top.cost, share)),
                view.cards.top_consumer[1..]
                    .iter()
                    .map(|e| {
                        SharedString::from(format!(
                            "{} {} pts{}",
                            e.name,
                            e.cost,
                            e.share_pct
                                .map_or_else(String::new, |p| format!(" · {:.0}%", p))
                        ))
                    })
                    .collect(),
                format!(
                    "Top consumer: {} at {} pts{}",
                    top.name,
                    top.cost,
                    top.share_pct
                        .map_or_else(String::new, |p| format!(" ({:.0}% of known cost)", p))
                ),
            )
        }
        None => (
            SharedString::from("—"),
            vec![SharedString::from("no known cost in the window")],
            "Top consumer: no known cost in the window".to_string(),
        ),
    };
    let consumer_card = stat_card(
        "usage-card-consumer",
        "Top consumer",
        consumer_headline,
        consumer_lines,
        consumer_a11y,
        cx,
    );

    // --- D16 card 4: slowest op — the highest-value latency fact ---
    let (slow_headline, slow_lines, slow_a11y) = match &view.slowest_op {
        Some(s) => (
            SharedString::from(s.op.clone()),
            vec![SharedString::from(format!(
                "p90 {} · worst {}",
                fmt_ms(s.p90_ms),
                fmt_ms(s.worst_ms)
            ))],
            format!(
                "Slowest op: {} p90 {}, worst single flight {}",
                s.op,
                fmt_ms(s.p90_ms),
                fmt_ms(s.worst_ms)
            ),
        ),
        None => (
            SharedString::from("—"),
            vec![SharedString::from("no flights with latency in the window")],
            "Slowest op: none in the window".to_string(),
        ),
    };
    let slowest_card = stat_card(
        "usage-card-slowest",
        "Slowest op",
        slow_headline,
        slow_lines,
        slow_a11y,
        cx,
    );

    // --- D15 chart row 1: burn-down, one panel per transport ---
    let burn_panels: Vec<AnyElement> = view
        .burn_down
        .series
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.points.is_empty())
        .map(|(i, s)| {
            let ts: Vec<u64> = s.points.iter().map(|p| p.ts_epoch_ms).collect();
            let labels = unique_labels(&ts);
            let data: Vec<(SharedString, f64)> = s
                .points
                .iter()
                .enumerate()
                .map(|(j, p)| (labels[j].clone(), p.remaining as f64))
                .collect();
            let last = s.points.last().map(|p| p.remaining).unwrap_or(0);
            let closest = s.points.iter().map(|p| p.remaining).min().unwrap_or(0);
            let (summary, caption) = match s.ceiling {
                Some(limit) => (
                    format!(
                        "{} budget burn-down: remaining {} of {} ({:.0}%), closest approach {}",
                        s.transport,
                        last,
                        limit,
                        last as f64 / limit as f64 * 100.0,
                        closest
                    ),
                    Some(SharedString::from(format!(
                        "ceiling {limit} · closest approach {closest}"
                    ))),
                ),
                None => (
                    format!(
                        "{} budget burn-down: remaining {}, no ceiling recorded",
                        s.transport, last
                    ),
                    None,
                ),
            };
            let stroke = if i % 2 == 0 { chart_1 } else { chart_2 };
            line_panel(
                &format!("usage-burn-{}", s.transport),
                summary,
                data,
                stroke,
                true,
                caption,
                cx,
            )
            .into_any_element()
        })
        .collect();
    let burn_row = if burn_panels.is_empty() {
        h_flex()
            .child(div().text_sm().text_color(muted_fg).child(
                "No budget snapshots in the window — burn-down appears with the first response.",
            ))
            .into_any_element()
    } else {
        h_flex()
            .flex_wrap()
            .gap_4()
            .items_start()
            .children(burn_panels)
            .into_any_element()
    };

    // --- D15 chart row 2 + D16: spend per transport (never pooled)
    // and the condensed pooled p90 line ---
    let spend_rest: Vec<(SharedString, f64)> = view
        .spend_trend
        .buckets
        .iter()
        .filter(|b| b.rest > 0)
        .map(|b| (SharedString::from(format_hms(b.start_ms)), b.rest as f64))
        .collect();
    let spend_gql: Vec<(SharedString, f64)> = view
        .spend_trend
        .buckets
        .iter()
        .filter(|b| b.gql > 0)
        .map(|b| (SharedString::from(format_hms(b.start_ms)), b.gql as f64))
        .collect();
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let rest_sum: u64 = view.spend_trend.buckets.iter().map(|b| b.rest).sum();
    let gql_sum: u64 = view.spend_trend.buckets.iter().map(|b| b.gql).sum();

    let p90_data: Vec<(SharedString, f64)> = view
        .p90_trend
        .buckets
        .iter()
        .filter_map(|b| {
            b.pooled_p90_ms
                .map(|ms| (SharedString::from(format_hms(b.start_ms)), ms as f64))
        })
        .collect();
    let pooled_max = view
        .p90_trend
        .buckets
        .iter()
        .filter_map(|b| b.pooled_p90_ms)
        .max();
    let rest_max = view
        .p90_trend
        .buckets
        .iter()
        .filter_map(|b| b.rest_p90_ms)
        .max();
    let gql_max = view
        .p90_trend
        .buckets
        .iter()
        .filter_map(|b| b.gql_p90_ms)
        .max();

    let mut chart_row = h_flex().flex_wrap().gap_4().items_start();
    if !spend_rest.is_empty() {
        chart_row = chart_row.child(line_panel(
            "usage-spend-rest",
            format!(
                "REST spend over time: {rest_sum} points across {} active bucket{}",
                spend_rest.len(),
                plural(spend_rest.len())
            ),
            spend_rest,
            chart_1,
            false,
            Some(SharedString::from(format!("Σ {rest_sum} pts"))),
            cx,
        ));
    }
    if !spend_gql.is_empty() {
        chart_row = chart_row.child(line_panel(
            "usage-spend-gql",
            format!(
                "GraphQL spend over time: {gql_sum} points across {} active bucket{}",
                spend_gql.len(),
                plural(spend_gql.len())
            ),
            spend_gql,
            chart_2,
            false,
            Some(SharedString::from(format!("Σ {gql_sum} pts"))),
            cx,
        ));
    }
    if !p90_data.is_empty() {
        let splits = match (rest_max, gql_max) {
            (Some(r), Some(g)) => format!(" · REST up to {} · GQL up to {}", fmt_ms(r), fmt_ms(g)),
            (Some(r), None) => format!(" · REST up to {}", fmt_ms(r)),
            (None, Some(g)) => format!(" · GQL up to {}", fmt_ms(g)),
            (None, None) => String::new(),
        };
        chart_row = chart_row.child(line_panel(
            "usage-p90",
            format!(
                "Latency p90 trend: pooled up to {}{splits}",
                pooled_max.map_or_else(|| "—".into(), fmt_ms)
            ),
            p90_data,
            theme.chart_3,
            false,
            None,
            cx,
        ));
    }

    v_flex()
        .id("usage-overview")
        .flex_1()
        .min_h_0()
        .gap_3()
        .overflow_y_scroll()
        .child(
            h_flex()
                .flex_wrap()
                .gap_3()
                .items_start()
                .child(points_card)
                .child(budget_card)
                .child(consumer_card)
                .child(slowest_card),
        )
        .child(burn_row)
        .child(chart_row)
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Quota tab (docs/0054 §4 — D4–D8)
// ---------------------------------------------------------------------------

/// The `Then by` picker's empty choice (D4): no secondary dimension.
const THEN_BY_NONE: &str = "(none)";

/// `Then by` options: the empty choice plus every dimension except
/// the active primary — a self-nest is not a question (D4's crosses
/// are pairs of *different* dimensions).
fn then_by_labels(primary: Dimension) -> Vec<SharedString> {
    std::iter::once(SharedString::from(THEN_BY_NONE))
        .chain(
            Dimension::all()
                .into_iter()
                .filter(|d| *d != primary)
                .map(|d| SharedString::from(d.label())),
        )
        .collect()
}

/// One chart slice: a ranking leaf rolled up for display (D6).
#[derive(Debug, Clone)]
struct ChartSlice {
    label: SharedString,
    cost: u64,
}

/// Flatten the ranking into chart leaves (D6): with a `Then by`
/// dimension the leaves are the children, labeled `parent ▸ child`;
/// without one, the groups are the leaves. Deterministic order:
/// Σ cost desc, then label asc (the D13 convention).
fn chart_leaves(ranking: &QuotaRanking) -> Vec<ChartSlice> {
    let mut leaves: Vec<ChartSlice> = ranking
        .groups
        .iter()
        .flat_map(|g| {
            if g.children.is_empty() {
                vec![ChartSlice {
                    label: SharedString::from(g.key.as_str()),
                    cost: g.cost,
                }]
            } else {
                g.children
                    .iter()
                    .map(|c| ChartSlice {
                        label: SharedString::from(format!("{} ▸ {}", g.key, c.key)),
                        cost: c.cost,
                    })
                    .collect()
            }
        })
        .collect();
    leaves.sort_by(|a, b| b.cost.cmp(&a.cost).then_with(|| a.label.cmp(&b.label)));
    leaves
}

/// Top-N plus `other` roll-up (D6): the residual sums the tail so the
/// chart never lies about the whole. The `other` slice lands last.
fn roll_up(mut leaves: Vec<ChartSlice>, top: usize) -> Vec<ChartSlice> {
    if leaves.len() <= top {
        return leaves;
    }
    let tail = leaves.split_off(top);
    let other_cost: u64 = tail.iter().map(|s| s.cost).sum();
    leaves.push(ChartSlice {
        label: SharedString::from("other"),
        cost: other_cost,
    });
    leaves
}

/// One flattened ranking-table row (D5): a primary group, or an
/// indented `Then by` member beneath it. One flat table, never a
/// pivot cube.
struct QuotaRow {
    key: String,
    indent: bool,
    cost: u64,
    share_pct: Option<f64>,
    flights: u64,
    avg_cost: Option<f64>,
    unknown: u64,
}

/// The ranking table's delegate (D5): renders the calculator's
/// `QuotaRanking` verbatim — sorting, shares and nesting are the
/// calculator's; the table adds no numbers of its own.
struct QuotaTable {
    rows: Vec<QuotaRow>,
    columns: Vec<Column>,
}

impl QuotaTable {
    fn new(ranking: QuotaRanking, group_by: Dimension) -> Self {
        let mut rows = Vec::new();
        for g in &ranking.groups {
            rows.push(QuotaRow {
                key: g.key.clone(),
                indent: false,
                cost: g.cost,
                share_pct: g.share_pct,
                flights: g.flights,
                avg_cost: g.avg_cost,
                unknown: g.unknown,
            });
            for child in &g.children {
                rows.push(QuotaRow {
                    key: child.key.clone(),
                    indent: true,
                    cost: child.cost,
                    share_pct: child.share_pct,
                    flights: child.flights,
                    avg_cost: child.avg_cost,
                    unknown: child.unknown,
                });
            }
        }
        Self {
            rows,
            columns: Self::columns(group_by),
        }
    }

    fn empty(group_by: Dimension) -> Self {
        Self {
            rows: Vec::new(),
            columns: Self::columns(group_by),
        }
    }

    fn columns(group_by: Dimension) -> Vec<Column> {
        vec![
            Column::new("dimension", group_by.label()),
            Column::new("cost", "Σ cost").text_right(),
            Column::new("share", "share").text_right(),
            Column::new("flights", "flights").text_right(),
            Column::new("avg", "avg").text_right(),
            Column::new("unknown", "unknown").text_right(),
        ]
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize) -> String {
        let Some(row) = self.rows.get(row_ix) else {
            return String::new();
        };
        match col_ix {
            1 => row.cost.to_string(),
            2 => row
                .share_pct
                .map_or_else(|| "—".into(), |p| format!("{p:.1}%")),
            3 => row.flights.to_string(),
            4 => row
                .avg_cost
                .map_or_else(|| "—".into(), |a| format!("{a:.1}")),
            5 => row.unknown.to_string(),
            _ => row.key.clone(),
        }
    }
}

impl TableDelegate for QuotaTable {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns
            .get(col_ix)
            .cloned()
            .unwrap_or_else(|| Column::new("dimension", ""))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(row) = self.rows.get(row_ix) else {
            return div().into_any_element();
        };
        let text = self.cell_text(row_ix, col_ix);
        let muted = cx.theme().muted_foreground;
        div()
            .flex()
            .h_full()
            .w_full()
            .items_center()
            .when(col_ix > 0, |c| c.justify_end())
            // Primary groups carry the nesting: members indent beneath
            // them (D5's one flat table).
            .when(row.indent, |c| c.pl_4())
            .when(!row.indent, |c| c.font_medium())
            // The unknown column keeps the no-guessing rule visible
            // (D5): counts of unattributable flights, dimmed at zero.
            .when(col_ix == 5 && row.unknown == 0, |c| c.text_color(muted))
            .child(text)
            .into_any_element()
    }
}

/// The page-global inputs the quota tab derives from (D7/D10): pushed
/// by the page on first open, refresh, and every page-global control
/// change. The tab's own selector changes re-apply over the same
/// inputs — a pure re-derivation, never a store read.
struct QuotaInputs {
    window: WindowSpec,
    filters: Filters,
    rows: Vec<UsageRecord>,
}

/// The quota usage tab (D4–D7): two group-by selectors, two charts,
/// and the ranking table — all a thin renderer over
/// [`quota_ranking`], which is the one aggregation implementation.
struct QuotaTab {
    group_by: Dimension,
    then_by: Option<Dimension>,
    inputs: QuotaInputs,
    ranking: QuotaRanking,
    group_by_select: Entity<SelectState<SearchableVec<SharedString>>>,
    then_by_select: Entity<SelectState<SearchableVec<SharedString>>>,
    table: Entity<TableState<QuotaTable>>,
}

impl QuotaTab {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dimension_labels: Vec<SharedString> = Dimension::all()
            .iter()
            .map(|d| SharedString::from(d.label()))
            .collect();
        let default_group_by: SharedString = Dimension::Surface.label().into();
        let group_by_select = cx.new(|cx| {
            let mut select =
                SelectState::new(SearchableVec::from(dimension_labels), None, window, cx);
            select.set_selected_value(&default_group_by, window, cx);
            select
        });
        let default_then_by: SharedString = THEN_BY_NONE.into();
        let then_by_select = cx.new(|cx| {
            let mut select = SelectState::new(
                SearchableVec::from(then_by_labels(Dimension::Surface)),
                None,
                window,
                cx,
            );
            select.set_selected_value(&default_then_by, window, cx);
            select
        });
        let table = cx.new(|cx| TableState::new(QuotaTable::empty(Dimension::Surface), window, cx));

        // The selectors re-apply the ranking over the same frozen
        // inputs (D4/D7): pick dimensions, see a ranking — no read.
        cx.subscribe_in(
            &group_by_select,
            window,
            |this, _, event: &SelectEvent<SearchableVec<SharedString>>, window, cx| {
                let SelectEvent::Confirm(Some(label)) = event else {
                    return;
                };
                let Some(dim) = Dimension::from_label(label) else {
                    return;
                };
                this.group_by = dim;
                if this.then_by == Some(dim) {
                    // A self-nest is not a question (D4): reset rather
                    // than render parent/child mirror rows.
                    this.then_by = None;
                    let none: SharedString = THEN_BY_NONE.into();
                    this.then_by_select.update(cx, |select, cx| {
                        select.set_selected_value(&none, window, cx)
                    });
                }
                this.then_by_select.update(cx, |select, cx| {
                    select.set_items(SearchableVec::from(then_by_labels(dim)), window, cx)
                });
                this.apply(cx);
                cx.notify();
            },
        )
        .detach();
        cx.subscribe_in(
            &then_by_select,
            window,
            |this, _, event: &SelectEvent<SearchableVec<SharedString>>, _, cx| {
                let SelectEvent::Confirm(Some(label)) = event else {
                    return;
                };
                this.then_by = if label == THEN_BY_NONE {
                    None
                } else {
                    Dimension::from_label(label)
                };
                this.apply(cx);
                cx.notify();
            },
        )
        .detach();

        Self {
            group_by: Dimension::Surface,
            then_by: None,
            inputs: QuotaInputs {
                window: WindowSpec {
                    as_of_ms: 0,
                    range: Range::D1,
                },
                filters: Filters::default(),
                rows: Vec::new(),
            },
            ranking: QuotaRanking {
                groups: Vec::new(),
                totals: QuotaTotals {
                    flights: 0,
                    known_cost: 0,
                    unknown_flights: 0,
                },
            },
            group_by_select,
            then_by_select,
            table,
        }
    }

    /// The page pushes its global inputs (D7/D10): frozen window,
    /// facets, fetched rows. Pure re-derivation; windowless.
    fn sync(
        &mut self,
        window: WindowSpec,
        filters: Filters,
        rows: &[UsageRecord],
        cx: &mut Context<Self>,
    ) {
        self.inputs = QuotaInputs {
            window,
            filters,
            rows: rows.to_vec(),
        };
        self.apply(cx);
    }

    /// Re-derive the ranking and the table over the current inputs
    /// (D7): `quota_ranking` is the one aggregation implementation —
    /// this adds no numbers, only layout.
    fn apply(&mut self, cx: &mut Context<Self>) {
        self.ranking = quota_ranking(
            &self.inputs.rows,
            &QuotaQuery {
                window: self.inputs.window,
                group_by: self.group_by,
                then_by: self.then_by,
                filters: self.inputs.filters.clone(),
            },
        );
        let delegate = QuotaTable::new(self.ranking.clone(), self.group_by);
        self.table.update(cx, |state, cx| {
            *state.delegate_mut() = delegate;
            state.refresh(cx);
            state.refresh_header_layout(cx);
        });
        cx.notify();
    }

    /// The D8 text summary for a chart: pixels are invisible to AT —
    /// the summary carries the top group and the total.
    fn chart_summary(&self, known: u64) -> String {
        match chart_leaves(&self.ranking).first() {
            Some(top) => format!(
                "Quota by {}: {} {} of {} points",
                self.group_by.label().to_lowercase(),
                top.label,
                top.cost,
                known
            ),
            None => format!(
                "Quota by {}: no known cost",
                self.group_by.label().to_lowercase()
            ),
        }
    }
}

impl Render for QuotaTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dim = self.group_by;
        let known = self.ranking.totals.known_cost;
        let flights = self.ranking.totals.flights;

        v_flex()
            .flex_1()
            .min_h_0()
            .gap_3()
            // Group-by controls (D4): pick dimensions, see a ranking.
            // Two selectors, never three — selector count is part of
            // the contract.
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Group by"),
                    )
                    .child(
                        Select::new(&self.group_by_select)
                            .accessibility_label("Group by dimension"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Then by"),
                    )
                    .child(
                        Select::new(&self.then_by_select).accessibility_label("Then by dimension"),
                    ),
            )
            .child(if flights == 0 {
                // Empty window: honest, not blank.
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(cx.theme().muted_foreground)
                    .child("No flights in the window — widen the range or refresh.")
                    .into_any_element()
            } else {
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .when(known > 0, |this| {
                        // Charts (D6): both straight from the table's
                        // aggregate vector — the same ranking, never a
                        // second computation.
                        let leaves = chart_leaves(&self.ranking);
                        let bars = roll_up(leaves.clone(), 12);
                        let donut = roll_up(leaves, 7);
                        let top_slice = bars.first().cloned();
                        let summary = self.chart_summary(known);

                        // Donut palette: the theme's chart tokens,
                        // cycling; `other` always muted (D6's residual
                        // must not look like a real group).
                        let palette = [
                            cx.theme().chart_1,
                            cx.theme().chart_2,
                            cx.theme().chart_3,
                            cx.theme().chart_4,
                            cx.theme().chart_5,
                        ];
                        let muted = cx.theme().muted;
                        let donut_labels: Vec<SharedString> =
                            donut.iter().map(|s| s.label.clone()).collect();
                        let donut_colors: Vec<Hsla> = donut
                            .iter()
                            .enumerate()
                            .map(|(i, s)| {
                                if s.label.as_ref() == "other" {
                                    muted
                                } else {
                                    palette[i % palette.len()]
                                }
                            })
                            .collect();

                        this.child(
                            h_flex()
                                .gap_4()
                                .items_start()
                                // The comparison view (D6): horizontal
                                // — rankings read top-down and the
                                // values are words.
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w(px(280.))
                                        .gap_2()
                                        .child(
                                            div()
                                                .id("usage-quota-bar-summary")
                                                .role(gpui::accesskit::Role::Label)
                                                .aria_label(summary.clone())
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(summary),
                                        )
                                        .child(
                                            div().h(px(220.)).child(
                                                BarChart::new(bars)
                                                    .id("usage-quota-bar")
                                                    .name("Σ cost")
                                                    .band(|s| s.label.clone())
                                                    .value(|s| s.cost as f64)
                                                    .alignment(BarAlignment::Left),
                                            ),
                                        ),
                                )
                                // The composition view (D6): donut,
                                // top 7 + other, total under it.
                                .child(
                                    v_flex()
                                        .w(px(280.))
                                        .gap_1()
                                        .items_center()
                                        .child(
                                            div()
                                                .id("usage-quota-donut-summary")
                                                .role(gpui::accesskit::Role::Label)
                                                .aria_label(format!(
                                                    "Quota composition by {}",
                                                    dim.label().to_lowercase()
                                                ))
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!(
                                                    "Composition by {}",
                                                    dim.label().to_lowercase()
                                                )),
                                        )
                                        .child(
                                            div().h(px(220.)).w_full().child(
                                                PieChart::new(donut)
                                                    .value(|s| s.cost as f32)
                                                    .label(move |s| {
                                                        let share = if known > 0 {
                                                            s.cost as f32 / known as f32 * 100.0
                                                        } else {
                                                            0.0
                                                        };
                                                        SharedString::from(format!(
                                                            "{} {:.0}%",
                                                            s.label, share
                                                        ))
                                                    })
                                                    .inner_radius(64.)
                                                    .outer_radius(100.)
                                                    .color(move |s| {
                                                        donut_labels
                                                            .iter()
                                                            .position(|l| *l == s.label)
                                                            .and_then(|i| {
                                                                donut_colors.get(i).copied()
                                                            })
                                                            .unwrap_or(muted)
                                                    }),
                                            ),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!(
                                                    "total {} points{}",
                                                    known,
                                                    top_slice.map_or_else(String::new, |t| {
                                                        format!(" · top {}", t.label)
                                                    })
                                                )),
                                        ),
                                ),
                        )
                    })
                    .when(known == 0, |this| {
                        // All-unknown window: charts would be a lie of
                        // empty pixels; the table's `—` columns carry
                        // the honesty instead.
                        this.child(
                            div().text_color(cx.theme().muted_foreground).child(
                                "No known cost in the window — unattributable flights only.",
                            ),
                        )
                    })
                    // The ranking table (D5): sorted Σ cost desc,
                    // `Then by` members nested beneath their group.
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .child(DataTable::new(&self.table).small()),
                    )
                    .into_any_element()
            })
    }
}

// ---------------------------------------------------------------------------
// Tests (docs/0054 §9)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const AS_OF: u64 = 100_000_000;
    const W: WindowSpec = WindowSpec {
        as_of_ms: AS_OF,
        range: Range::H1,
    };
    const FROM: u64 = AS_OF - 3_600_000;

    /// Fixture row: REST, cost 1, in-window by default; tests mutate
    /// for variation.
    fn rec(ts: u64, surface: &str, cost: Option<u32>) -> UsageRecord {
        UsageRecord {
            module: SharedString::from("github"),
            seq: 0,
            ts_epoch_ms: ts,
            transport: Transport::Rest,
            op: SharedString::from("pr-detail"),
            surface: SharedString::from(surface),
            trigger: SharedString::from("view-shown"),
            ident: None,
            status: 200,
            etag_304: false,
            cost,
            remaining_after: None,
            limit_after: None,
            cycle_reset_epoch: None,
            duration_ms: 100,
            queue_ms: None,
            resp_bytes: None,
            network_error: false,
        }
    }

    fn quota(dimension: Dimension, rows: &[UsageRecord]) -> QuotaRanking {
        quota_ranking(
            rows,
            &QuotaQuery {
                window: W,
                group_by: dimension,
                then_by: None,
                filters: Filters::default(),
            },
        )
    }

    #[test]
    fn window_is_half_open_on_both_ends() {
        let rows = vec![
            rec(FROM - 1, "old", Some(1)), // before the window
            rec(FROM, "edge-in", Some(1)),
            rec(AS_OF - 1, "in", Some(1)),
            rec(AS_OF, "edge-out", Some(1)), // at as_of — excluded (D10)
        ];
        let r = quota(Dimension::Surface, &rows);
        assert_eq!(r.totals.flights, 2);
        assert_eq!(r.totals.known_cost, 2);
    }

    #[test]
    fn filters_slice_transport_module_and_errors() {
        let mut err = rec(AS_OF - 10, "s", None);
        err.network_error = true;
        let mut gql = rec(AS_OF - 20, "s", Some(7));
        gql.transport = Transport::Graphql;
        let mut forgejo = rec(AS_OF - 30, "s", Some(1));
        forgejo.module = SharedString::from("forgejo");
        let rows = vec![err, gql, forgejo.clone(), rec(AS_OF - 40, "s", Some(1))];

        let q = |f: Filters| {
            quota_ranking(
                &rows,
                &QuotaQuery {
                    window: W,
                    group_by: Dimension::Module,
                    then_by: None,
                    filters: f,
                },
            )
        };
        assert_eq!(
            q(Filters {
                transport: Some(Transport::Graphql),
                ..Default::default()
            })
            .totals
            .flights,
            1
        );
        assert_eq!(
            q(Filters {
                module: Some("forgejo".into()),
                ..Default::default()
            })
            .totals
            .flights,
            1
        );
        assert_eq!(
            q(Filters {
                errors_only: true,
                ..Default::default()
            })
            .totals
            .flights,
            1
        );
        assert_eq!(q(Filters::default()).totals.flights, 4);
    }

    #[test]
    fn quota_ranks_by_cost_desc_with_flight_and_key_tiebreaks() {
        let rows = vec![
            rec(AS_OF - 10, "big", Some(9)), // 9 pts, 1 flight
            rec(AS_OF - 20, "mid-a", Some(4)),
            rec(AS_OF - 30, "mid-b", Some(4)), // tie 4: same flights → key asc
            rec(AS_OF - 40, "zero", None),     // unattributable: cost 0, unknown 1
        ];
        let r = quota(Dimension::Surface, &rows);
        let keys: Vec<&str> = r.groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(keys, vec!["big", "mid-a", "mid-b", "zero"]);
        assert_eq!(r.groups[0].cost, 9);
        assert_eq!(r.groups[0].share_pct, Some(9.0 / 17.0 * 100.0));
        assert_eq!(r.groups[3].cost, 0, "unknown is never priced");
        assert_eq!(r.groups[3].unknown, 1);
        assert_eq!(r.groups[3].avg_cost, None);
        assert_eq!(r.totals.unknown_flights, 1);
    }

    #[test]
    fn all_unknown_window_has_no_shares() {
        let rows = vec![rec(AS_OF - 10, "a", None), rec(AS_OF - 20, "b", None)];
        let r = quota(Dimension::Surface, &rows);
        assert_eq!(r.totals.known_cost, 0);
        assert!(
            r.groups
                .iter()
                .all(|g| g.share_pct.is_none() && g.avg_cost.is_none())
        );
    }

    #[test]
    fn then_by_nests_inside_the_primary() {
        let mut rows = Vec::new();
        for (surf, trig, n) in [
            ("pr-page", "view-shown", 3),
            ("pr-page", "manual", 1),
            ("home", "view-shown", 1),
        ] {
            for i in 0..n {
                let mut r = rec(AS_OF - 10 - i, surf, Some(1));
                r.trigger = SharedString::from(trig);
                rows.push(r);
            }
        }
        let r = quota_ranking(
            &rows,
            &QuotaQuery {
                window: W,
                group_by: Dimension::Surface,
                then_by: Some(Dimension::Trigger),
                filters: Filters::default(),
            },
        );
        assert_eq!(r.groups[0].key, "pr-page");
        assert_eq!(r.groups[0].flights, 4);
        assert_eq!(r.groups[0].children[0].key, "view-shown");
        assert_eq!(r.groups[0].children[0].flights, 3);
        assert_eq!(r.groups[1].key, "home");
        assert!(
            r.groups[1].children[0].children.is_empty(),
            "no third level"
        );
    }

    #[test]
    fn target_dimension_buckets_none_ident_as_dash() {
        let mut scoped = rec(AS_OF - 10, "s", Some(1));
        scoped.ident = Some(SharedString::from("o/r#1"));
        let unscoped = rec(AS_OF - 20, "s", Some(1));
        let r = quota(Dimension::Target, &[scoped, unscoped]);
        let keys: Vec<&str> = r.groups.iter().map(|g| g.key.as_str()).collect();
        assert!(keys.contains(&"o/r#1") && keys.contains(&"—"));
    }

    #[test]
    fn percentiles_are_nearest_rank() {
        // durations 10..=19 (n=10): p50 rank 5 → 14, p90 rank 9 → 18.
        let rows: Vec<UsageRecord> = (0..10)
            .map(|i| {
                let mut r = rec(AS_OF - 10, "s", Some(1));
                r.duration_ms = 10 + i;
                r
            })
            .collect();
        let ds: Vec<&UsageRecord> = rows.iter().collect();
        assert_eq!(percentile(&ds, 50), 14);
        assert_eq!(percentile(&ds, 90), 18);
        // n=2: p50 rank 1 → lower; p90 rank 2 → higher.
        let two: Vec<&UsageRecord> = ds[..2].to_vec();
        assert_eq!(percentile(&two, 50), 10);
        assert_eq!(percentile(&two, 90), 11);
    }

    #[test]
    fn metric_selector_changes_the_latency_ranking() {
        let rows: Vec<UsageRecord> = [("fast-many", 50u32, 8u32), ("slow-few", 900, 1)]
            .iter()
            .flat_map(|(op, dur, n)| {
                (0..*n).map(move |i| {
                    let mut r = rec(AS_OF - 10 - i as u64, "s", Some(1));
                    r.op = SharedString::from(*op);
                    r.duration_ms = *dur;
                    r
                })
            })
            .collect();
        let q = |metric| {
            latency_ranking(
                &rows,
                &LatencyQuery {
                    window: W,
                    group_by: Dimension::Operation,
                    then_by: None,
                    filters: Filters::default(),
                    metric,
                },
            )
        };
        assert_eq!(q(LatencyMetric::P90).groups[0].key, "slow-few");
        assert_eq!(q(LatencyMetric::Count).groups[0].key, "fast-many");
        let p90 = q(LatencyMetric::P90);
        assert_eq!(p90.groups[0].p50_ms, Some(900));
        assert_eq!(p90.totals.flights, 9);
    }

    #[test]
    fn latency_error_rate_counts_network_and_status_failures() {
        let mut net = rec(AS_OF - 10, "s", None);
        net.network_error = true;
        let mut forbidden = rec(AS_OF - 20, "s", Some(0));
        forbidden.status = 403;
        let ok = rec(AS_OF - 30, "s", Some(1));
        let r = latency_ranking(
            &[net, forbidden, ok],
            &LatencyQuery {
                window: W,
                group_by: Dimension::Surface,
                then_by: None,
                filters: Filters::default(),
                metric: LatencyMetric::ErrorRate,
            },
        );
        assert_eq!(r.totals.errors, 2);
        let rate = r.groups[0].error_rate_pct.unwrap();
        assert!((rate - 200.0 / 3.0).abs() < 1e-9, "2 of 3 errored: {rate}");
    }

    #[test]
    fn trend_buckets_cover_the_window_and_keep_empties() {
        let rows = vec![rec(FROM, "s", Some(1)), rec(AS_OF - 1, "s", Some(1))];
        let t = latency_trend(
            &rows,
            &TrendQuery {
                window: W,
                filters: Filters::default(),
            },
        );
        assert_eq!(t.buckets.len(), 40, "1h → 40 buckets of 90 s");
        assert_eq!(t.buckets[0].flights, 1);
        assert_eq!(t.buckets[1].flights, 0, "empty buckets stay (honest gaps)");
        assert_eq!(t.buckets[1].p90_ms, None);
        assert_eq!(t.buckets[39].flights, 1);
        assert_eq!(t.buckets[0].start_ms, FROM);
        assert_eq!(t.buckets[39].end_ms, AS_OF);
    }

    #[test]
    fn slowest_flights_order_limit_and_ties() {
        let mut rows = Vec::new();
        for (i, dur) in [100u32, 800, 300, 800, 50].iter().enumerate() {
            let mut r = rec(AS_OF - 10 - i as u64, "s", Some(1));
            r.duration_ms = *dur;
            r.seq = i as u64 + 1;
            r.queue_ms = Some(7);
            rows.push(r);
        }
        let s = slowest_flights(
            &rows,
            &SlowestQuery {
                window: W,
                filters: Filters::default(),
                limit: 3,
            },
        );
        assert_eq!(s.flights.len(), 3);
        assert_eq!(s.flights[0].duration_ms, 800);
        assert_eq!(s.flights[1].duration_ms, 800);
        assert!(
            s.flights[0].ts_epoch_ms < s.flights[1].ts_epoch_ms,
            "duration ties: earlier flight first"
        );
        assert_eq!(s.flights[2].duration_ms, 300);
        assert_eq!(s.flights[0].queue_ms, Some(7));
    }

    #[test]
    fn overview_points_split_budget_and_runway() {
        let mut rows = Vec::new();
        // 3 REST points across two flights, one unattributable.
        for (i, cost) in [Some(1u32), Some(2), None].iter().enumerate() {
            rows.push(rec(AS_OF - 60_000 - i as u64, "pr-page", *cost));
        }
        // 1 GQL point with budget snapshots and a ceiling.
        let mut gql = rec(AS_OF - 30_000, "home", Some(1));
        gql.transport = Transport::Graphql;
        gql.remaining_after = Some(700);
        gql.limit_after = Some(1_000);
        rows.push(gql);

        let v = overview(&rows, &OverviewQuery { window: W });
        let ps = &v.cards.points_spent;
        assert_eq!(ps.rest_points, 3, "never summed with GQL (D11)");
        assert_eq!(ps.gql_points, 1);
        assert_eq!(ps.flights, 4);
        assert_eq!(ps.unattributable, 1);

        let b = v
            .cards
            .budget
            .iter()
            .find(|b| b.transport == "graphql")
            .unwrap();
        assert_eq!(b.remaining, 700);
        assert_eq!(b.remaining_pct, Some(70.0));
        assert_eq!(b.limit, Some(1_000));
        assert_eq!(b.snapshot_age_ms, 30_000);
        // spend 1 over 1 h → runway 700 h.
        assert_eq!(b.runway_hours, Some(700.0));
        // REST has no snapshot → no budget card entry.
        assert!(v.cards.budget.iter().all(|b| b.transport != "rest"));

        let tc = &v.cards.top_consumer;
        assert_eq!(tc[0].name, "pr-page");
        assert_eq!(tc[0].cost, 3);
        assert_eq!(tc[0].share_pct, Some(75.0));
    }

    #[test]
    fn overview_budget_degrades_to_absolute_without_limit() {
        let mut r = rec(AS_OF - 10, "s", Some(1));
        r.remaining_after = Some(500);
        r.limit_after = None;
        let v = overview(&[r], &OverviewQuery { window: W });
        let b = &v.cards.budget[0];
        assert_eq!(b.remaining_pct, None, "never invents a ceiling");
        assert_eq!(b.remaining, 500);
    }

    #[test]
    fn burn_down_carries_forward_and_gets_a_ceiling() {
        let mut early = rec(AS_OF - 3_600_000 + 1_000, "s", Some(1));
        early.remaining_after = Some(900);
        early.limit_after = Some(1_000);
        let mut late = rec(AS_OF - 100, "s", Some(1));
        late.remaining_after = Some(850);
        late.limit_after = Some(1_000);
        let v = overview(&[early, late], &OverviewQuery { window: W });
        let series = &v.burn_down.series[0]; // rest
        assert_eq!(series.ceiling, Some(1_000));
        let pts = &series.points;
        assert_eq!(pts.len(), 3, "two snapshots + carry-forward");
        assert_eq!(pts[0].remaining, 900);
        assert_eq!(pts[2].ts_epoch_ms, AS_OF, "carries to as_of (D15)");
        assert_eq!(pts[2].remaining, 850);
    }

    #[test]
    fn spend_buckets_sum_to_window_totals_with_top_surface() {
        let mut rows = Vec::new();
        for i in 0..4 {
            let mut r = rec(FROM + i * 90_000 + 1, "pr-page", Some(1));
            r.seq = i + 1;
            rows.push(r);
        }
        let mut gql = rec(FROM + 91_000, "home", Some(5));
        gql.transport = Transport::Graphql;
        rows.push(gql);
        let v = overview(&rows, &OverviewQuery { window: W });
        let rest: u64 = v.spend_trend.buckets.iter().map(|b| b.rest).sum();
        let gql_sum: u64 = v.spend_trend.buckets.iter().map(|b| b.gql).sum();
        assert_eq!(rest, v.cards.points_spent.rest_points);
        assert_eq!(gql_sum, v.cards.points_spent.gql_points);
        assert_eq!(
            v.spend_trend.buckets[0].top_surface,
            Some("pr-page".to_string())
        );
        assert_eq!(
            v.spend_trend.buckets[1].top_surface,
            Some("home".to_string())
        );
    }

    #[test]
    fn p90_trend_pools_but_tooltip_splits() {
        let mut rows = Vec::new();
        for i in 0..10 {
            let mut r = rec(FROM + 1, "s", Some(1));
            r.duration_ms = 100 + i;
            rows.push(r);
        }
        let mut gql = rec(FROM + 2, "s", Some(1));
        gql.transport = Transport::Graphql;
        gql.duration_ms = 5_000;
        rows.push(gql);
        let v = overview(&rows, &OverviewQuery { window: W });
        let b = &v.p90_trend.buckets[0];
        // Pooled p90 (nearest-rank over all 11: rank ceil(9.9) = 10 → 109).
        assert_eq!(b.pooled_p90_ms, Some(109));
        assert_eq!(b.rest_p90_ms, Some(108));
        assert_eq!(b.gql_p90_ms, Some(5_000));
        assert_eq!(b.flights, 11);
    }

    #[test]
    fn slowest_op_picks_max_p90_with_deterministic_ties() {
        let mut rows = Vec::new();
        for i in 0..4 {
            let mut r = rec(AS_OF - 10, "s", Some(1));
            r.op = SharedString::from("fast-op");
            r.duration_ms = 10 + i;
            rows.push(r);
        }
        for i in 0..4 {
            let mut r = rec(AS_OF - 20, "s", Some(1));
            r.op = SharedString::from("slow-op");
            r.duration_ms = 200 + i;
            rows.push(r);
        }
        let v = overview(&rows, &OverviewQuery { window: W });
        let s = v.slowest_op.unwrap();
        assert_eq!(s.op, "slow-op");
        assert_eq!(s.p90_ms, 203);
        assert_eq!(s.worst_ms, 203);
    }

    #[test]
    fn procedures_are_deterministic_on_identical_input() {
        let mut rows: Vec<UsageRecord> = (0..20)
            .map(|i| {
                let mut r = rec(
                    AS_OF - i * 1_000,
                    if i % 2 == 0 { "a" } else { "b" },
                    Some((i % 3) as u32),
                );
                r.duration_ms = 50 + (i * 7) as u32;
                r.seq = i + 1;
                r
            })
            .collect();
        // Shuffle arrival order per the store contract? No — arrival
        // order is the contract; determinism means same input twice.
        for _ in 0..2 {
            let q = quota(Dimension::Surface, &rows);
            assert_eq!(q, quota(Dimension::Surface, &rows));
            let v = overview(&rows, &OverviewQuery { window: W });
            assert_eq!(v, overview(&rows, &OverviewQuery { window: W }));
            rows.reverse(); // and reversed input gives the same groups (BTree keys)
        }
        let q = quota(Dimension::Surface, &rows);
        assert_eq!(q.groups.len(), 2);
    }

    // -- Skeleton (D1–D3, D10) ------------------------------------------
    // Freeze units per docs/0054 §9.

    use crate::usage_records::{MemorySink, UsageStore, init_with, sink};
    use gpui::TestAppContext;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A freeze far past every range span (AS_OF itself is only 100 s
    /// past the epoch — month arithmetic would underflow).
    const BIG_AS_OF: u64 = 10_000_000_000;

    /// Directly-built page over hand-stamped rows (no store, no
    /// wall-clock): for window-derivation units.
    fn skeleton(rows: Vec<UsageRecord>) -> UsagePage {
        UsagePage {
            as_of_ms: BIG_AS_OF,
            rows,
            range: Range::D1,
            transport: None,
            module: None,
            errors_only: false,
            tab: UsageTab::Overview,
            // Pure-derivation tests never render; the tab needs a
            // window once and is not under test here.
            quota: None,
            overview_view: empty_overview(),
        }
    }

    #[test]
    fn range_windows_derive_from_the_frozen_as_of_not_wall_clock() {
        let mut p = skeleton(vec![
            rec(BIG_AS_OF - 1, "edge-in", Some(1)),
            rec(BIG_AS_OF - Range::D1.ms(), "day-boundary", Some(1)),
            rec(BIG_AS_OF - Range::M1.ms() - 1, "ancient", Some(1)),
        ]);
        // 1d (default): [as_of-1d, as_of) — edge-in + day-boundary.
        assert_eq!(p.visible_rows().len(), 2);
        p.range = Range::H1;
        assert_eq!(p.visible_rows().len(), 1, "1h drops the day-boundary row");
        p.range = Range::M1;
        // Even the month view re-slices from `as_of`: the pre-month
        // row (which the bounded fetch never returns) stays out.
        assert_eq!(p.visible_rows().len(), 2);
        // D3: tab switch keeps the range and the freeze.
        p.tab = UsageTab::Quota;
        assert_eq!((p.range, p.as_of_ms), (Range::M1, BIG_AS_OF));
    }

    #[test]
    fn honesty_header_counts_the_window_and_states_retention() {
        let p = skeleton(vec![
            rec(BIG_AS_OF - 100, "a", Some(1)),
            rec(BIG_AS_OF - 5_000, "b", None),
        ]);
        let (flights, oldest) = p.header_facts();
        assert_eq!(flights, 2);
        assert_eq!(oldest, Some(BIG_AS_OF - 5_000));
    }

    #[test]
    fn module_facet_options_come_from_the_fetched_rows() {
        let mut rows = vec![rec(BIG_AS_OF - 10, "s", Some(1))];
        rows[0].module = SharedString::from("forgejo");
        rows.push(rec(BIG_AS_OF - 20, "s", Some(1)));
        let p = skeleton(rows);
        assert_eq!(
            p.modules_in_rows(),
            vec!["forgejo".to_string(), "github".to_string()]
        );
    }

    #[gpui::test]
    fn page_loads_on_first_open_and_freezes_until_refresh(cx: &mut TestAppContext) {
        // Selects/TableState read the theme global at construction.
        cx.update(gpui_component::init);
        let store = Arc::new(MemorySink::default());
        cx.update(|cx| init_with(cx, store.clone() as Arc<dyn UsageStore>));

        // A flight before first open (a minute back — safely inside
        // any window the page will derive).
        let before = usage_records::now_epoch_ms() - 60_000;
        cx.update(|cx| sink(cx).record(rec(before, "pr-page", Some(1))));

        let (page, _window_ctx) = cx.add_window_view(UsagePage::new);
        let as_of0 = cx.update(|cx| page.read(cx).as_of_ms);
        assert!(before < as_of0);
        assert_eq!(
            cx.update(|cx| page.read(cx).rows.len()),
            1,
            "one load on first open"
        );

        // Post-freeze arrival: ts == as_of is already invisible (the
        // half-open upper bound is what makes the freeze real, D10).
        cx.update(|cx| sink(cx).record(rec(as_of0, "post-freeze", Some(1))));
        cx.update(|cx| {
            let p = page.read(cx);
            assert_eq!(p.rows.len(), 1);
            assert_eq!(p.visible_rows().len(), 1);
        });

        // Control changes are pure recomputations — the arrival stays
        // invisible through range/facet/tab flips.
        page.update(cx, |p, _| {
            p.range = Range::M1;
            p.errors_only = true;
            p.errors_only = false;
            p.tab = UsageTab::Quota;
        });
        cx.update(|cx| assert_eq!(page.read(cx).rows.len(), 1));

        // Refresh: the only path that moves `as_of` and re-reads. Sleep
        // a real tick so the new stamp strictly exceeds the row's.
        std::thread::sleep(std::time::Duration::from_millis(2));
        page.update(cx, |p, cx| p.refresh(cx));
        cx.update(|cx| {
            let p = page.read(cx);
            assert!(p.as_of_ms > as_of0, "refresh re-stamps the freeze");
            assert_eq!(p.rows.len(), 2, "refresh picks up post-freeze rows");
            assert_eq!(p.visible_rows().len(), 2);
        });
    }

    #[gpui::test]
    fn only_first_open_and_refresh_touch_the_store(cx: &mut TestAppContext) {
        // Selects/TableState read the theme global at construction.
        cx.update(gpui_component::init);
        struct CountingStore {
            reads: AtomicUsize,
        }
        impl crate::usage_records::UsageSink for CountingStore {
            fn record(&self, _rec: UsageRecord) {}
        }
        impl crate::usage_records::UsageSource for CountingStore {
            fn records_between(&self, _from: u64, _to: u64) -> Vec<UsageRecord> {
                self.reads.fetch_add(1, Ordering::Relaxed);
                Vec::new()
            }
            fn high_water(&self) -> u64 {
                0
            }
            // Overridden so the default (records_since(0)) cannot
            // masquerade as a page read.
            fn snapshot(&self) -> Vec<UsageRecord> {
                Vec::new()
            }
        }
        impl UsageStore for CountingStore {}

        let store = Arc::new(CountingStore {
            reads: AtomicUsize::new(0),
        });
        cx.update(|cx| init_with(cx, store.clone() as Arc<dyn UsageStore>));

        let (page, _window_ctx) = cx.add_window_view(UsagePage::new);
        assert_eq!(
            store.reads.load(Ordering::Relaxed),
            1,
            "one bounded read on first open"
        );

        // Control changes (D10): pure calculator calls, zero reads.
        page.update(cx, |p, _| {
            p.range = Range::W1;
            p.transport = Some(Transport::Graphql);
            p.errors_only = true;
            p.tab = UsageTab::Quota;
        });
        assert_eq!(store.reads.load(Ordering::Relaxed), 1);

        // Refresh: exactly one more bounded read — memory only, never
        // a fetch (no Http, no specs, no spinner).
        page.update(cx, |p, cx| p.refresh(cx));
        assert_eq!(store.reads.load(Ordering::Relaxed), 2);

        // A module facet that outlived its rows resets to All on the
        // next read — never a silent always-empty filter.
        page.update(cx, |p, _| {
            p.module = Some("gone".into());
        });
        page.update(cx, |p, cx| p.reload(cx));
        cx.update(|cx| assert_eq!(page.read(cx).module, None));
    }

    #[gpui::test]
    fn overview_tab_derives_from_facets_and_range_purely(cx: &mut TestAppContext) {
        // Selects/TableState read the theme global at construction.
        cx.update(gpui_component::init);
        let store = Arc::new(MemorySink::default());
        cx.update(|cx| init_with(cx, store.clone() as Arc<dyn UsageStore>));

        let now = usage_records::now_epoch_ms();
        // In the 1h heart of the default 1d window.
        let mut gql = rec(now - 60_000, "home", Some(5));
        gql.transport = Transport::Graphql;
        let mut err = rec(now - 60_000, "issue-page", None);
        err.network_error = true;
        let mut snap = rec(now - 60_000, "home", Some(0));
        snap.remaining_after = Some(90);
        snap.limit_after = Some(100);
        // Three hours back: inside 1d, outside 1h.
        let mid = rec(now - 3 * 3_600_000, "pr-page", Some(7));
        cx.update(|cx| {
            let s = sink(cx);
            s.record(rec(now - 60_000, "pr-page", Some(2)));
            s.record(gql);
            s.record(err);
            s.record(snap);
            s.record(mid);
        });

        let (page, _window_ctx) = cx.add_window_view(UsagePage::new);
        cx.update(|cx| {
            let p = page.read(cx);
            let v = &p.overview_view;
            // Pools never sum (D11): REST and GraphQL stay split.
            assert_eq!(
                (
                    v.cards.points_spent.rest_points,
                    v.cards.points_spent.gql_points
                ),
                (9, 5)
            );
            assert_eq!(
                (
                    v.cards.points_spent.flights,
                    v.cards.points_spent.unattributable
                ),
                (5, 1)
            );
            // The REST snapshot drives the budget card (D11/D14).
            assert_eq!(v.cards.budget.len(), 1);
            assert_eq!(v.cards.budget[0].remaining_pct, Some(90.0));
            assert!(v.slowest_op.is_some(), "flights carry durations");
        });
        let rows_len = cx.update(|cx| page.read(cx).rows.len());

        // Transport facet: a pure recompute over the same frozen rows
        // (D4/D10) — the REST snapshot follows the facet out.
        page.update(cx, |p, cx| {
            p.transport = Some(Transport::Graphql);
            p.recompute_tabs(cx);
        });
        cx.update(|cx| {
            let p = page.read(cx);
            let v = &p.overview_view;
            assert_eq!(
                (
                    v.cards.points_spent.rest_points,
                    v.cards.points_spent.gql_points
                ),
                (0, 5)
            );
            assert_eq!(v.cards.budget.len(), 0);
            assert_eq!(p.rows.len(), rows_len, "no store read on a facet change");
        });

        // Errors-only: the error filter is a page-global predicate.
        page.update(cx, |p, cx| {
            p.transport = None;
            p.errors_only = true;
            p.recompute_tabs(cx);
        });
        cx.update(|cx| {
            let v = &page.read(cx).overview_view;
            assert_eq!(v.cards.points_spent.flights, 1);
            assert_eq!(v.cards.points_spent.rest_points, 0);
        });

        // Range: 1h re-slices the same frozen rows — the 3h flight
        // drops out of the window without a re-read.
        page.update(cx, |p, cx| {
            p.errors_only = false;
            p.range = Range::H1;
            p.recompute_tabs(cx);
        });
        cx.update(|cx| {
            let p = page.read(cx);
            let v = &p.overview_view;
            assert_eq!(v.cards.points_spent.rest_points, 2);
            assert_eq!(p.rows.len(), rows_len, "no store read on a range change");
        });
    }

    #[gpui::test]
    fn overview_tab_picks_up_refresh_only(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let store = Arc::new(MemorySink::default());
        cx.update(|cx| init_with(cx, store.clone() as Arc<dyn UsageStore>));
        cx.update(|cx| {
            sink(cx).record(rec(
                usage_records::now_epoch_ms() - 60_000,
                "pr-page",
                Some(1),
            ))
        });

        let (page, _window_ctx) = cx.add_window_view(UsagePage::new);
        let as_of0 = cx.update(|cx| page.read(cx).as_of_ms);
        cx.update(|cx| {
            assert_eq!(
                page.read(cx).overview_view.cards.points_spent.rest_points,
                1
            )
        });

        // A post-freeze arrival: invisible to the overview (ts >=
        // as_of is out of the half-open window) until refresh.
        cx.update(|cx| sink(cx).record(rec(as_of0, "late", Some(50))));
        cx.update(|cx| {
            assert_eq!(
                page.read(cx).overview_view.cards.points_spent.rest_points,
                1,
                "post-freeze rows stay invisible to the overview"
            )
        });
        std::thread::sleep(std::time::Duration::from_millis(2));
        page.update(cx, |p, cx| p.refresh(cx));
        cx.update(|cx| {
            assert_eq!(
                page.read(cx).overview_view.cards.points_spent.rest_points,
                51,
                "refresh is the only path that moves the overview"
            )
        });
    }

    #[gpui::test]
    fn quota_tab_re_ranks_when_the_dimension_changes(cx: &mut TestAppContext) {
        // Selects/TableState read the theme global at construction.
        cx.update(gpui_component::init);
        let store = Arc::new(MemorySink::default());
        cx.update(|cx| init_with(cx, store.clone() as Arc<dyn UsageStore>));

        let t0 = usage_records::now_epoch_ms();
        cx.update(|cx| {
            sink(cx).record(rec(t0 - 10, "pr-page", Some(9)));
            let mut issue = rec(t0 - 20, "home", Some(4));
            issue.op = SharedString::from("issue-detail");
            sink(cx).record(issue);
        });

        let (page, _window_ctx) = cx.add_window_view(UsagePage::new);
        let tab = cx.update(|cx| page.read(cx).quota.clone().unwrap());
        let keys = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                tab.read(cx)
                    .ranking
                    .groups
                    .iter()
                    .map(|g| g.key.clone())
                    .collect::<Vec<String>>()
            })
        };

        // Default: group by surface (D4), sorted Σ cost desc.
        assert_eq!(keys(cx), ["pr-page", "home"]);

        // Dimension change re-applies over the same frozen inputs
        // (D7): pure re-rank — the freeze tests above own read-count.
        tab.update(cx, |t, cx| {
            t.group_by = Dimension::Operation;
            t.apply(cx);
        });
        assert_eq!(
            keys(cx),
            ["pr-detail", "issue-detail"],
            "group by operation re-ranks the same rows"
        );
    }

    #[gpui::test]
    fn quota_tab_nests_when_then_by_is_set(cx: &mut TestAppContext) {
        // Selects/TableState read the theme global at construction.
        cx.update(gpui_component::init);
        let store = Arc::new(MemorySink::default());
        cx.update(|cx| init_with(cx, store.clone() as Arc<dyn UsageStore>));

        let t0 = usage_records::now_epoch_ms();
        cx.update(|cx| {
            let mut manual = rec(t0 - 10, "pr-page", Some(9));
            manual.trigger = SharedString::from("manual");
            sink(cx).record(manual);
            let mut shown = rec(t0 - 20, "pr-page", Some(4));
            shown.trigger = SharedString::from("view-shown");
            sink(cx).record(shown);
        });

        let (page, _window_ctx) = cx.add_window_view(UsagePage::new);
        let tab = cx.update(|cx| page.read(cx).quota.clone().unwrap());
        tab.update(cx, |t, cx| {
            t.then_by = Some(Dimension::Trigger);
            t.apply(cx);
        });

        // `Then by` nests (D4/D5): children subdivide the group,
        // and the table delegates flatten to one indented list.
        let (group_count, child_count, table_rows) = cx.update(|cx| {
            let t = tab.read(cx);
            (
                t.ranking.groups.len(),
                t.ranking.groups[0].children.len(),
                t.table.read(cx).delegate().rows.len(),
            )
        });
        assert_eq!(group_count, 1);
        assert_eq!(child_count, 2, "manual and view-shown subdivide pr-page");
        assert_eq!(table_rows, 3, "group row + two indented members");
    }

    #[test]
    fn quota_nesting_flattens_into_one_table() {
        let mut manual = rec(AS_OF - 10, "pr-page", Some(9));
        manual.trigger = SharedString::from("manual");
        let mut shown = rec(AS_OF - 20, "pr-page", Some(4));
        shown.trigger = SharedString::from("view-shown");
        let ranking = quota_ranking(
            &[manual.clone(), shown.clone()],
            &QuotaQuery {
                window: W,
                group_by: Dimension::Surface,
                then_by: Some(Dimension::Trigger),
                filters: Filters::default(),
            },
        );
        assert_eq!(ranking.groups.len(), 1);
        assert_eq!(ranking.groups[0].children.len(), 2);
        // The group row's totals include its members.
        assert_eq!(ranking.groups[0].cost, 13);
        assert_eq!(ranking.groups[0].children[0].key, "manual");

        // One flat table (D5): the group row, then indented members.
        let table = QuotaTable::new(ranking, Dimension::Surface);
        assert_eq!(table.rows.len(), 3);
        assert!(!table.rows[0].indent);
        assert!(table.rows[1].indent && table.rows[2].indent);
        assert_eq!(table.cell_text(0, 0), "pr-page");
        assert_eq!(table.cell_text(1, 0), "manual");
        assert_eq!(table.cell_text(2, 1), "4");

        // The calculator's sort is the table's: cost desc within the
        // nest, and nothing re-sorts on the UI side.
        let keys: Vec<&str> = table.rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, vec!["pr-page", "manual", "view-shown"]);
    }

    #[test]
    fn chart_leaves_prefix_compound_groups_and_roll_up() {
        let rows = vec![
            rec(AS_OF - 10, "pr-page", Some(9)),
            rec(AS_OF - 20, "home", Some(4)),
            rec(AS_OF - 30, "home", Some(2)),
        ];
        // Flat grouping: leaves are the groups themselves.
        let flat = quota(Dimension::Surface, &rows);
        let leaves = chart_leaves(&flat);
        assert_eq!(
            leaves.iter().map(|s| s.label.as_ref()).collect::<Vec<_>>(),
            vec!["pr-page", "home"]
        );

        // Top-1 + other: the residual sums the tail (D6).
        let rolled = roll_up(leaves.clone(), 1);
        assert_eq!(rolled.len(), 2);
        assert_eq!(rolled[0].label, "pr-page");
        assert_eq!(rolled[1].label, "other");
        assert_eq!(rolled[1].cost, 6);
        // No roll-up when the top covers everything.
        assert_eq!(roll_up(leaves, 12).len(), 2);

        // Compound grouping (D6): leaf labels carry the primary
        // prefix — flat bars per leaf, never a nested chart.
        let mut a = rec(AS_OF - 10, "pr-page", Some(9));
        a.trigger = SharedString::from("manual");
        let mut b = rec(AS_OF - 20, "pr-page", Some(4));
        b.trigger = SharedString::from("view-shown");
        let compound = quota_ranking(
            &[a, b],
            &QuotaQuery {
                window: W,
                group_by: Dimension::Surface,
                then_by: Some(Dimension::Trigger),
                filters: Filters::default(),
            },
        );
        let leaves = chart_leaves(&compound);
        assert_eq!(
            leaves.iter().map(|s| s.label.as_ref()).collect::<Vec<_>>(),
            vec!["pr-page ▸ manual", "pr-page ▸ view-shown"]
        );
    }

    #[test]
    fn then_by_labels_exclude_the_primary() {
        // (none) + the other five dimensions, in picker order (D4).
        let labels: Vec<SharedString> = then_by_labels(Dimension::Surface);
        let labels: Vec<&str> = labels.iter().map(gpui::SharedString::as_ref).collect();
        assert_eq!(labels[0], "(none)");
        assert_eq!(labels.len(), 6);
        assert!(
            !labels.contains(&"Surface"),
            "a self-nest is not a question"
        );
    }

    #[test]
    fn every_output_struct_round_trips_through_serde_json() {
        let mut rows = vec![rec(AS_OF - 10, "s", Some(1))];
        rows[0].remaining_after = Some(9);
        rows[0].limit_after = Some(10);
        rows[0].queue_ms = Some(3);
        rows[0].duration_ms = 42;
        let w = WindowSpec {
            as_of_ms: AS_OF,
            range: Range::D1,
        };
        fn round_trips<T: Serialize + serde::de::DeserializeOwned>(v: &T) {
            let json = serde_json::to_string(v).unwrap();
            let back: T = serde_json::from_str(&json).unwrap();
            let again = serde_json::to_string(&back).unwrap();
            assert_eq!(json, again);
        }

        round_trips(&quota_ranking(
            &rows,
            &QuotaQuery {
                window: w,
                group_by: Dimension::Surface,
                then_by: Some(Dimension::Trigger),
                filters: Filters::default(),
            },
        ));
        round_trips(&latency_ranking(
            &rows,
            &LatencyQuery {
                window: w,
                group_by: Dimension::Operation,
                then_by: None,
                filters: Filters::default(),
                metric: LatencyMetric::P90,
            },
        ));
        round_trips(&latency_trend(
            &rows,
            &TrendQuery {
                window: w,
                filters: Filters::default(),
            },
        ));
        round_trips(&slowest_flights(
            &rows,
            &SlowestQuery {
                window: w,
                filters: Filters::default(),
                limit: 5,
            },
        ));
        round_trips(&overview(&rows, &OverviewQuery { window: w }));
    }
}
