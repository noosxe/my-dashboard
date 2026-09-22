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

use crate::usage_records::{Transport, UsageRecord};
#[cfg(test)]
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
