//! The redesigned usage surfaces' procedures (docs/0082 D15): one
//! procedure per band, both audiences.
//!
//! A submodule rather than more of `usage_analytics.rs`: that file
//! already carries the retired tabs' procedures *and* their page
//! component (~6k lines). Those stay untouched until the view issues
//! retire them. The new vocabulary (budgets, presets with
//! `Current window`, status classes) lives here, so the two sets never
//! blur. The 0054 D13 contract is unchanged: rows in arrival order
//! plus a serde query in, plain owned serde structs out, no storage,
//! no clock, deterministic.
//!
//! Rules this module enforces for every procedure:
//! - **One budget per figure** (D2): REST requests and GraphQL points
//!   are scoped separately and never pooled. A budget belongs to one
//!   module (`"github"` today, docs/0082 §12).
//! - **`auth` and `local` never enter budget figures** (D16): only the
//!   Requests procedures ([`request_trend`], [`request_log`]) see them.
//! - **Unknown costs are counted, never priced** (0054 D5).
//! - **No snapshot is its own result**, never 0 (D4).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{Dimension, is_error, percentile_of};
use crate::usage_records::{Transport, UsageRecord};

const MINUTE_MS: u64 = 60_000;
const HOUR_MS: u64 = 60 * MINUTE_MS;
const DAY_MS: u64 = 24 * HOUR_MS;

/// GitHub's primary rate-limit window: an hour, for REST requests and
/// GraphQL points alike (re-verified 2026-11-05 against GitHub's
/// "Rate limits for the REST API" and "Rate limits and query limits for
/// the GraphQL API" pages). The reset is epoch **seconds**.
pub const WINDOW_MS: u64 = HOUR_MS;

/// D6 guard: responses carrying a snapshot in the current window.
pub const PROJECTION_MIN_RESPONSES: u64 = 5;
/// D6 guard: time elapsed in the current window.
pub const PROJECTION_MIN_ELAPSED_MS: u64 = 5 * MINUTE_MS;

/// D5 ladder: remaining ≤ 25 % is Low, ≤ 10 % is Critical.
pub const LOW_REMAINING_PCT: f64 = 25.0;
pub const CRITICAL_REMAINING_PCT: f64 = 10.0;

/// D7: the chart's top groups; the rest fold into "other".
pub const WHERE_TOP_N: usize = 8;
/// D9: the avoidable list's length.
pub const AVOIDABLE_TOP_N: usize = 5;

/// D9's timer-and-refocus triggers, as the modules stamp them
/// (GitHub's `Trigger::{Poll, Refocus, Probe}`).
const TIMER_TRIGGERS: [&str; 3] = ["poll", "refocus", "probe"];

// ---------------------------------------------------------------------------
// Shared inputs
// ---------------------------------------------------------------------------

/// The two independent budgets (D2, 0050 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Budget {
    Rest,
    Graphql,
}

impl Budget {
    pub fn transport(self) -> Transport {
        match self {
            Budget::Rest => Transport::Rest,
            Budget::Graphql => Transport::Graphql,
        }
    }

    /// The card order: REST, then GraphQL (D4).
    pub fn all() -> [Budget; 2] {
        [Budget::Rest, Budget::Graphql]
    }

    /// The budget a transport spends, if any: `auth` and `local` have
    /// none (D2, D16).
    pub fn of(transport: Transport) -> Option<Budget> {
        match transport {
            Transport::Rest => Some(Budget::Rest),
            Transport::Graphql => Some(Budget::Graphql),
            Transport::Auth | Transport::Local => None,
        }
    }

    /// The unit the summary lines name (D7, D21).
    pub fn unit(self) -> &'static str {
        match self {
            Budget::Rest => "requests",
            Budget::Graphql => "points",
        }
    }
}

/// The trailing ranges (D11), shared by both views.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Span {
    H1,
    H24,
    D7,
    D30,
    D90,
}

impl Span {
    pub fn ms(self) -> u64 {
        match self {
            Span::H1 => HOUR_MS,
            Span::H24 => DAY_MS,
            Span::D7 => 7 * DAY_MS,
            Span::D30 => 30 * DAY_MS,
            Span::D90 => 90 * DAY_MS,
        }
    }

    pub fn all() -> [Span; 5] {
        [Span::H1, Span::H24, Span::D7, Span::D30, Span::D90]
    }
}

/// The Budget view's range presets (D11): `Current window` plus the
/// trailing spans. The Requests view takes a [`Span`] only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Preset {
    /// The selected budget's current window (Q5): from its start
    /// (`reset − 1 h`) to `as_of`.
    CurrentWindow,
    Trailing(Span),
}

/// A resolved half-open interval `[from_ms, to_ms)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bounds {
    pub from_ms: u64,
    pub to_ms: u64,
}

impl Bounds {
    fn trailing(as_of_ms: u64, span: Span) -> Self {
        Bounds {
            from_ms: as_of_ms.saturating_sub(span.ms()),
            to_ms: as_of_ms,
        }
    }

    fn contains(&self, ts: u64) -> bool {
        ts >= self.from_ms && ts < self.to_ms
    }
}

/// One budget of one module over one preset — the scope of every
/// Budget-view band below the cards (D7–D9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetScope {
    pub module: String,
    pub budget: Budget,
    pub as_of_ms: u64,
    pub preset: Preset,
}

impl BudgetScope {
    /// The interval this scope reads. `None` only for `Current window`
    /// when the budget has no open window (no snapshot in the last hour
    /// whose reset is still ahead of `as_of`): its start is unknown, so
    /// the bands are empty rather than guessed.
    pub fn bounds(&self, rows: &[UsageRecord]) -> Option<Bounds> {
        match self.preset {
            Preset::Trailing(span) => Some(Bounds::trailing(self.as_of_ms, span)),
            Preset::CurrentWindow => current_window(rows, &self.module, self.budget, self.as_of_ms)
                .map(|w| Bounds {
                    from_ms: w.start_ms,
                    to_ms: self.as_of_ms,
                }),
        }
    }
}

/// A record that spends `budget` in `module` (D2, D16).
fn spends(rec: &UsageRecord, module: &str, budget: Budget) -> bool {
    rec.transport == budget.transport() && rec.module.as_ref() == module
}

/// A response that carried a budget snapshot. Such a record's
/// `cycle_reset_epoch` comes from the same response, so it names the
/// record's own window; the recorder's carry-forward (0053 D3) only
/// fills records without one.
fn is_snapshot(rec: &UsageRecord) -> bool {
    rec.remaining_after.is_some()
}

fn reset_ms(rec: &UsageRecord) -> Option<u64> {
    rec.cycle_reset_epoch.map(|s| s.saturating_mul(1_000))
}

/// The newest snapshot of `budget` inside `bounds`, by `(ts, seq)`.
fn latest_snapshot<'a>(
    rows: &'a [UsageRecord],
    module: &str,
    budget: Budget,
    bounds: Bounds,
) -> Option<&'a UsageRecord> {
    rows.iter()
        .filter(|r| spends(r, module, budget) && is_snapshot(r) && bounds.contains(r.ts_epoch_ms))
        .max_by(|a, b| a.ts_epoch_ms.cmp(&b.ts_epoch_ms).then(a.seq.cmp(&b.seq)))
}

/// A budget's open window at `as_of`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentWindow {
    /// `reset − 1 h` (D6).
    pub start_ms: u64,
    pub reset_ms: u64,
}

/// The window the latest snapshot of the last hour belongs to, if its
/// reset is still ahead of `as_of`. Any snapshot of an open window is
/// younger than an hour, so nothing older needs reading.
pub fn current_window(
    rows: &[UsageRecord],
    module: &str,
    budget: Budget,
    as_of_ms: u64,
) -> Option<CurrentWindow> {
    let last_hour = Bounds {
        from_ms: as_of_ms.saturating_sub(WINDOW_MS),
        to_ms: as_of_ms,
    };
    let latest = latest_snapshot(rows, module, budget, last_hour)?;
    let reset = reset_ms(latest)?;
    (reset > as_of_ms).then(|| CurrentWindow {
        start_ms: reset.saturating_sub(WINDOW_MS),
        reset_ms: reset,
    })
}

/// The D5 ladder. Ordered by severity, so "the worst state a window
/// reached" is a `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ThresholdState {
    Normal,
    Low,
    Critical,
    /// Live: the budget's pause is in force (`rate_limited_until`,
    /// per budget since Q10) or the open window reports 0 left. In
    /// history: the window reached 0.
    Exhausted,
}

impl ThresholdState {
    /// The ladder over one snapshot. `None` without a usable ceiling:
    /// the fraction is unknown, so no state is invented.
    pub fn of(remaining: u32, limit: Option<u32>) -> Option<ThresholdState> {
        let limit = limit.filter(|l| *l > 0)?;
        if remaining == 0 {
            return Some(ThresholdState::Exhausted);
        }
        let pct = remaining as f64 / limit as f64 * 100.0;
        Some(if pct <= CRITICAL_REMAINING_PCT {
            ThresholdState::Critical
        } else if pct <= LOW_REMAINING_PCT {
            ThresholdState::Low
        } else {
            ThresholdState::Normal
        })
    }
}

/// Requests and Σ known cost of a record set — the summary lines.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpendTotals {
    pub requests: u64,
    /// Σ known cost, in the budget's unit.
    pub known_cost: u64,
    /// Requests with no attributable cost — counted, never priced.
    pub unknown: u64,
}

impl SpendTotals {
    fn of<'a>(rows: impl IntoIterator<Item = &'a UsageRecord>) -> Self {
        let mut t = SpendTotals::default();
        for r in rows {
            t.add(r);
        }
        t
    }

    fn add(&mut self, r: &UsageRecord) {
        self.requests += 1;
        match r.cost {
            Some(c) => self.known_cost += c as u64,
            None => self.unknown += 1,
        }
    }
}

fn share(part: u64, total: u64) -> Option<f64> {
    (total > 0).then(|| part as f64 / total as f64 * 100.0)
}

/// The scope's records: one budget, one module, inside the bounds.
fn budget_rows<'a>(
    rows: &'a [UsageRecord],
    scope: &BudgetScope,
) -> (Option<Bounds>, Vec<&'a UsageRecord>) {
    let Some(bounds) = scope.bounds(rows) else {
        return (None, Vec::new());
    };
    let scoped = rows
        .iter()
        .filter(|r| spends(r, &scope.module, scope.budget) && bounds.contains(r.ts_epoch_ms))
        .collect();
    (Some(bounds), scoped)
}

// ---------------------------------------------------------------------------
// D4/D6/D7 — budget cards, projection, default budget
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetCardsQuery {
    pub module: String,
    pub as_of_ms: u64,
    pub preset: Preset,
    /// The live per-budget pause (`rate_limited_until`, 0050 D4 as
    /// amended by Q10) captured with `as_of`, in epoch ms. `None` when
    /// no pause is known; the records alone still mark an open window
    /// that reports 0 left as exhausted.
    pub rest_paused_until_ms: Option<u64>,
    pub graphql_paused_until_ms: Option<u64>,
}

/// The latest snapshot's verbatim numbers (0050 D1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetSnapshot {
    pub remaining: u32,
    pub limit: Option<u32>,
    /// remaining ÷ limit, %; `None` without a ceiling.
    pub remaining_pct: Option<f64>,
    /// `cycle_reset_epoch` in ms; `None` when the response carried no
    /// reset.
    pub reset_ms: Option<u64>,
    pub snapshot_ts_ms: u64,
    /// `as_of − snapshot ts`; the view words "under a minute" (D4).
    pub snapshot_age_ms: u64,
    /// The reset is still ahead of `as_of`. A closed window's numbers
    /// are verbatim but stale: GitHub has refilled the budget since.
    pub window_open: bool,
}

/// The D6 projection, or why there is none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Projection {
    /// No snapshot in the range (D4: never shown as 0).
    NoSnapshot,
    /// The latest snapshot's window has reset; there is no current
    /// window to project.
    WindowClosed,
    /// The snapshot lacks a reset or a ceiling, so `spent` is unknown.
    Unavailable,
    /// The budget is paused or reports 0 left; requests resume at
    /// `resumes_at_ms` (D4's exhausted copy).
    Exhausted { resumes_at_ms: u64 },
    /// Below the D6 guard: "Not enough responses in this window to
    /// project".
    NotEnoughData { responses: u64, elapsed_ms: u64 },
    /// `as_of + remaining ÷ pace ≥ reset`. `pace_per_min` is in the
    /// budget's unit.
    OnTrack { pace_per_min: f64 },
    /// "≈ runs out ~HH:MM at this pace".
    RunsOut { at_ms: u64, pace_per_min: f64 },
}

/// The current window's drop as GitHub reports it, beside this app's
/// recorded cost (D6, Q4). Not rendered; `external` is the input of
/// the external-consumer estimate (#705).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowDrop {
    pub window: CurrentWindow,
    /// `limit − remaining` of the latest snapshot in the window.
    pub spent: u32,
    /// This app's Σ known cost in `[window start, as_of)`.
    pub recorded_cost: u64,
    /// `spent − recorded_cost`: spend by other clients sharing the
    /// token. Can be slightly negative from record timing at the
    /// window edge; consumers clamp.
    pub external: i64,
    /// Responses carrying a snapshot in the window (the D6 guard's
    /// count).
    pub responses: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetCard {
    pub budget: Budget,
    /// `None`: "No response from GitHub in this range yet" (D4).
    pub snapshot: Option<BudgetSnapshot>,
    /// The D5 state badge; `None` when no state is known (no
    /// snapshot, no ceiling, or a closed window) and not paused.
    pub state: Option<ThresholdState>,
    pub projection: Projection,
    pub window_drop: Option<WindowDrop>,
    /// The range's recorded spend for this budget (the meta line).
    /// Under `Current window` the range is this budget's own window.
    pub recorded: SpendTotals,
    /// The interval `recorded` covers; `None` under `Current window`
    /// without an open window.
    pub bounds: Option<Bounds>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetCards {
    /// Always both budgets, REST first (D4, Q5).
    pub cards: Vec<BudgetCard>,
    /// D7's default selection (Q6).
    pub default_budget: Budget,
}

/// Band 1 (D4): both budgets' cards, the D6 projection and the D7
/// default budget.
pub fn budget_cards(rows: &[UsageRecord], q: &BudgetCardsQuery) -> BudgetCards {
    let cards: Vec<BudgetCard> = Budget::all()
        .into_iter()
        .map(|budget| {
            let paused = match budget {
                Budget::Rest => q.rest_paused_until_ms,
                Budget::Graphql => q.graphql_paused_until_ms,
            }
            .filter(|until| *until > q.as_of_ms);
            budget_card(rows, q, budget, paused)
        })
        .collect();
    let default_budget = default_budget(&cards);
    BudgetCards {
        cards,
        default_budget,
    }
}

fn budget_card(
    rows: &[UsageRecord],
    q: &BudgetCardsQuery,
    budget: Budget,
    paused_until: Option<u64>,
) -> BudgetCard {
    let scope = BudgetScope {
        module: q.module.clone(),
        budget,
        as_of_ms: q.as_of_ms,
        preset: q.preset,
    };
    let (bounds, scoped) = budget_rows(rows, &scope);
    let recorded = SpendTotals::of(scoped.iter().copied());
    let latest = bounds.and_then(|b| latest_snapshot(rows, &q.module, budget, b));

    let snapshot = latest.map(|r| {
        let remaining = r.remaining_after.unwrap_or(0);
        let reset = reset_ms(r);
        BudgetSnapshot {
            remaining,
            limit: r.limit_after,
            remaining_pct: r
                .limit_after
                .filter(|l| *l > 0)
                .map(|l| remaining as f64 / l as f64 * 100.0),
            reset_ms: reset,
            snapshot_ts_ms: r.ts_epoch_ms,
            snapshot_age_ms: q.as_of_ms.saturating_sub(r.ts_epoch_ms),
            window_open: reset.is_some_and(|reset| reset > q.as_of_ms),
        }
    });

    // The current window is the latest snapshot's, when still open.
    let window = snapshot.as_ref().and_then(|s| {
        let reset = s.reset_ms?;
        s.window_open.then(|| CurrentWindow {
            start_ms: reset.saturating_sub(WINDOW_MS),
            reset_ms: reset,
        })
    });
    let window_drop = match (window, snapshot.as_ref().and_then(|s| s.limit)) {
        (Some(w), Some(limit)) => {
            let s = snapshot.as_ref().expect("a window implies a snapshot");
            let in_window = |r: &&UsageRecord| {
                spends(r, &q.module, budget)
                    && r.ts_epoch_ms >= w.start_ms
                    && r.ts_epoch_ms < q.as_of_ms
            };
            let recorded_cost: u64 = rows
                .iter()
                .filter(in_window)
                .filter_map(|r| r.cost)
                .map(|c| c as u64)
                .sum();
            let responses = rows
                .iter()
                .filter(in_window)
                .filter(|r| is_snapshot(r) && reset_ms(r) == Some(w.reset_ms))
                .count() as u64;
            let spent = limit.saturating_sub(s.remaining);
            Some(WindowDrop {
                window: w,
                spent,
                recorded_cost,
                external: spent as i64 - recorded_cost as i64,
                responses,
                elapsed_ms: q.as_of_ms.saturating_sub(w.start_ms),
            })
        }
        _ => None,
    };

    // The pause outranks everything (D5); then the snapshot decides.
    let projection = match (paused_until, &snapshot, &window_drop) {
        (Some(until), _, _) => Projection::Exhausted {
            resumes_at_ms: until,
        },
        (None, None, _) => Projection::NoSnapshot,
        (None, Some(s), _) if s.reset_ms.is_some() && !s.window_open => Projection::WindowClosed,
        (None, Some(s), _) if s.remaining == 0 && s.window_open => Projection::Exhausted {
            resumes_at_ms: s.reset_ms.unwrap_or_default(),
        },
        (None, Some(_), None) => Projection::Unavailable,
        (None, Some(s), Some(d)) => project(s.remaining, d),
    };

    let state = if paused_until.is_some() {
        Some(ThresholdState::Exhausted)
    } else {
        snapshot
            .as_ref()
            .filter(|s| s.window_open)
            .and_then(|s| ThresholdState::of(s.remaining, s.limit))
    };

    BudgetCard {
        budget,
        snapshot,
        state,
        projection,
        window_drop,
        recorded,
        bounds,
    }
}

/// D6: pace = window drop ÷ elapsed, behind the guard.
fn project(remaining: u32, d: &WindowDrop) -> Projection {
    if d.responses < PROJECTION_MIN_RESPONSES || d.elapsed_ms < PROJECTION_MIN_ELAPSED_MS {
        return Projection::NotEnoughData {
            responses: d.responses,
            elapsed_ms: d.elapsed_ms,
        };
    }
    let pace_per_min = d.spent as f64 * MINUTE_MS as f64 / d.elapsed_ms as f64;
    if d.spent == 0 {
        return Projection::OnTrack { pace_per_min };
    }
    // remaining ÷ pace in integers: remaining × elapsed ÷ spent, ceiled.
    let as_of = d.window.start_ms + d.elapsed_ms;
    let to_empty = (remaining as u128 * d.elapsed_ms as u128).div_ceil(d.spent as u128);
    let at_ms = as_of.saturating_add(to_empty.min(u64::MAX as u128) as u64);
    if at_ms < d.window.reset_ms {
        Projection::RunsOut {
            at_ms,
            pace_per_min,
        }
    } else {
        Projection::OnTrack { pace_per_min }
    }
}

/// D7 (Q6): the budget with the smaller remaining fraction of an open
/// window; REST on a tie, or when either fraction is unknown. An
/// exhausted (or paused) budget counts as 0 %; no snapshot, no
/// ceiling or a closed window (its numbers predate a refill) count as
/// unknown.
pub fn default_budget(cards: &[BudgetCard]) -> Budget {
    let pct = |b: Budget| {
        let c = cards.iter().find(|c| c.budget == b)?;
        if c.state == Some(ThresholdState::Exhausted) {
            return Some(0.0);
        }
        c.snapshot
            .as_ref()
            .filter(|s| s.window_open)
            .and_then(|s| s.remaining_pct)
    };
    match (pct(Budget::Rest), pct(Budget::Graphql)) {
        (Some(rest), Some(gql)) if gql < rest => Budget::Graphql,
        _ => Budget::Rest,
    }
}

// ---------------------------------------------------------------------------
// D7 — where it went
// ---------------------------------------------------------------------------

/// D7's group-by dimensions: Transport leaves the list, because the
/// budget decides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpendDimension {
    Surface,
    Trigger,
    Operation,
    Target,
    Module,
}

impl SpendDimension {
    fn dimension(self) -> Dimension {
        match self {
            SpendDimension::Surface => Dimension::Surface,
            SpendDimension::Trigger => Dimension::Trigger,
            SpendDimension::Operation => Dimension::Operation,
            SpendDimension::Target => Dimension::Target,
            SpendDimension::Module => Dimension::Module,
        }
    }

    pub fn label(self) -> &'static str {
        self.dimension().label()
    }

    /// Picker order (D7).
    pub fn all() -> [SpendDimension; 5] {
        [
            SpendDimension::Surface,
            SpendDimension::Trigger,
            SpendDimension::Operation,
            SpendDimension::Target,
            SpendDimension::Module,
        ]
    }
}

/// `Group by` plus an optional `Then by`: the two-selector cap (0054
/// D4) is the shape of the query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhereQuery {
    pub scope: BudgetScope,
    pub group_by: SpendDimension,
    pub then_by: Option<SpendDimension>,
}

/// One row of the D7 table: key · Cost · share · requests · avg cost ·
/// free (304) · errors · unknown cost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpendRow {
    pub key: String,
    /// Σ known cost, in the budget's unit (the `Cost` column, Q11).
    pub cost: u64,
    /// Share of the scope's Σ known cost, %.
    pub share_pct: Option<f64>,
    pub requests: u64,
    /// Σ cost ÷ costed requests.
    pub avg_cost: Option<f64>,
    /// `etag_304` revalidations.
    pub free_304: u64,
    pub errors: u64,
    pub unknown: u64,
    /// The `Then by` members, all of them (D7).
    pub children: Vec<SpendRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhereItWent {
    pub budget: Budget,
    pub unit: String,
    pub bounds: Option<Bounds>,
    pub totals: SpendTotals,
    /// Every top-level group, sorted cost desc, requests desc, key asc.
    /// The chart draws the first [`WHERE_TOP_N`].
    pub groups: Vec<SpendRow>,
    /// The groups beyond [`WHERE_TOP_N`] folded into one "other" row
    /// (no children); `None` when there are none.
    pub other: Option<SpendRow>,
}

/// Band 2 (D7): one budget's spend, grouped — never pooled across
/// budgets (D2).
pub fn where_it_went(rows: &[UsageRecord], q: &WhereQuery) -> WhereItWent {
    let (bounds, scoped) = budget_rows(rows, &q.scope);
    let totals = SpendTotals::of(scoped.iter().copied());
    let mut groups = spend_rows(&scoped, q.group_by, q.then_by, totals.known_cost);
    sort_spend(&mut groups);
    let other = (groups.len() > WHERE_TOP_N).then(|| {
        let rest = &groups[WHERE_TOP_N..];
        let cost: u64 = rest.iter().map(|g| g.cost).sum();
        let unknown: u64 = rest.iter().map(|g| g.unknown).sum();
        let requests: u64 = rest.iter().map(|g| g.requests).sum();
        SpendRow {
            key: "other".to_string(),
            cost,
            share_pct: share(cost, totals.known_cost),
            requests,
            avg_cost: avg(cost, requests - unknown),
            free_304: rest.iter().map(|g| g.free_304).sum(),
            errors: rest.iter().map(|g| g.errors).sum(),
            unknown,
            children: Vec::new(),
        }
    });
    WhereItWent {
        budget: q.scope.budget,
        unit: q.scope.budget.unit().to_string(),
        bounds,
        totals,
        groups,
        other,
    }
}

fn avg(cost: u64, costed: u64) -> Option<f64> {
    (costed > 0).then(|| cost as f64 / costed as f64)
}

fn spend_rows(
    rows: &[&UsageRecord],
    primary: SpendDimension,
    secondary: Option<SpendDimension>,
    total_known: u64,
) -> Vec<SpendRow> {
    let mut by_key: BTreeMap<String, Vec<&UsageRecord>> = BTreeMap::new();
    for r in rows {
        by_key
            .entry(primary.dimension().value(r))
            .or_default()
            .push(r);
    }
    by_key
        .into_iter()
        .map(|(key, members)| {
            let t = SpendTotals::of(members.iter().copied());
            let mut children = secondary
                .map(|sec| spend_rows(&members, sec, None, total_known))
                .unwrap_or_default();
            sort_spend(&mut children);
            SpendRow {
                key,
                cost: t.known_cost,
                share_pct: share(t.known_cost, total_known),
                requests: t.requests,
                avg_cost: avg(t.known_cost, t.requests - t.unknown),
                free_304: members.iter().filter(|r| r.etag_304).count() as u64,
                errors: members.iter().filter(|r| is_error(r)).count() as u64,
                unknown: t.unknown,
                children,
            }
        })
        .collect()
}

fn sort_spend(rows: &mut [SpendRow]) {
    rows.sort_by(|a, b| {
        b.cost
            .cmp(&a.cost)
            .then(b.requests.cmp(&a.requests))
            .then_with(|| a.key.cmp(&b.key))
    });
}

// ---------------------------------------------------------------------------
// D8 — history across windows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryQuery {
    pub scope: BudgetScope,
    /// Local offset from UTC, ms: per-day columns start at local
    /// midnight. The caller supplies it; the calculator reads no clock.
    pub tz_offset_ms: i64,
}

/// What the view needs to choose D8's axis (Q3); the choice itself
/// (about 90 % of the limit) stays in the view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScaleInputs {
    /// The tallest column's value (or, for the burn-down, the limit).
    pub data_max: u64,
    /// The latest ceiling in the scope; `None` without one.
    pub limit: Option<u32>,
    /// Where the Low guide sits on this chart's value axis: 75 % of the
    /// limit spent for columns, 25 % remaining for the burn-down.
    pub low_guide: Option<f64>,
    /// The Critical guide: 90 % spent for columns, 10 % remaining for
    /// the burn-down.
    pub critical_guide: Option<f64>,
}

impl ScaleInputs {
    fn spent(data_max: u64, limit: Option<u32>) -> Self {
        let l = limit.map(f64::from);
        ScaleInputs {
            data_max,
            limit,
            low_guide: l.map(|l| l * (1.0 - LOW_REMAINING_PCT / 100.0)),
            critical_guide: l.map(|l| l * (1.0 - CRITICAL_REMAINING_PCT / 100.0)),
        }
    }

    fn remaining(limit: Option<u32>) -> Self {
        let l = limit.map(f64::from);
        ScaleInputs {
            data_max: limit.map_or(0, u64::from),
            limit,
            low_guide: l.map(|l| l * LOW_REMAINING_PCT / 100.0),
            critical_guide: l.map(|l| l * CRITICAL_REMAINING_PCT / 100.0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BurnStep {
    pub ts_ms: u64,
    pub remaining: u32,
}

/// `Current window`: the step burn-down of `remaining_after` (Q5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BurnDown {
    pub window: CurrentWindow,
    pub as_of_ms: u64,
    /// The window's snapshots by `(ts, seq)`; the last value carries to
    /// `as_of`.
    pub steps: Vec<BurnStep>,
    /// The value axis is the whole budget, 0 to the limit (D8).
    pub scale: ScaleInputs,
}

/// One column per window of the selected budget (ranges ≤ 7 days).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowColumn {
    pub start_ms: u64,
    pub reset_ms: u64,
    /// The column height (D8): Σ known cost of the window's records
    /// inside the range (edge windows are clipped to it).
    pub cost: u64,
    pub requests: u64,
    pub unknown: u64,
    /// GitHub's deepest reported drop in the window, `limit − min
    /// remaining`; `None` without a ceiling.
    pub peak_drop: Option<u32>,
    pub limit: Option<u32>,
    /// The worst D5 state the window reached (its colour).
    pub state: Option<ThresholdState>,
}

/// One column per day (30 and 90 days).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DayColumn {
    /// Local midnight (per `tz_offset_ms`).
    pub day_start_ms: u64,
    pub day_end_ms: u64,
    /// The column height (D8): the day's peak window spend — the
    /// largest per-window Σ known cost among windows starting that day.
    pub peak_window_cost: u64,
    pub peak_window_reset_ms: Option<u64>,
    /// GitHub's deepest drop among the day's windows (its closest
    /// approach to the limit).
    pub peak_drop: Option<u32>,
    /// The day's total (tooltip and a11y summary, D8).
    pub total_cost: u64,
    pub requests: u64,
    pub unknown: u64,
    pub windows: u64,
    /// The worst state any of the day's windows reached.
    pub state: Option<ThresholdState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HistoryView {
    /// `Current window` without an open window.
    NoWindow,
    BurnDown(BurnDown),
    /// "Spend per window" (ranges ≤ 7 days).
    Windows {
        columns: Vec<WindowColumn>,
        scale: ScaleInputs,
    },
    /// "Peak window per day" (30 and 90 days). Every day of the range
    /// is present, empty days included.
    Days {
        columns: Vec<DayColumn>,
        scale: ScaleInputs,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowHistory {
    pub budget: Budget,
    pub unit: String,
    pub bounds: Option<Bounds>,
    pub view: HistoryView,
    /// In-range records of the budget that fall in no known window (no
    /// snapshot of their window in range): counted, never placed.
    pub unwindowed: SpendTotals,
}

/// Band 3 (D8): spend per quota window, never a cycle selector.
pub fn window_history(rows: &[UsageRecord], q: &HistoryQuery) -> WindowHistory {
    let scope = &q.scope;
    let (bounds, scoped) = budget_rows(rows, scope);
    let mut out = WindowHistory {
        budget: scope.budget,
        unit: scope.budget.unit().to_string(),
        bounds,
        view: HistoryView::NoWindow,
        unwindowed: SpendTotals::default(),
    };
    let span = match scope.preset {
        Preset::CurrentWindow => {
            if let Some(w) = current_window(rows, &scope.module, scope.budget, scope.as_of_ms) {
                out.view = HistoryView::BurnDown(burn_down(&scoped, w, scope.as_of_ms));
            }
            return out;
        }
        Preset::Trailing(span) => span,
    };

    let (columns, unwindowed) = window_columns(&scoped);
    out.unwindowed = unwindowed;
    let limit = scoped
        .iter()
        .filter(|r| is_snapshot(r) && r.limit_after.is_some())
        .max_by(|a, b| a.ts_epoch_ms.cmp(&b.ts_epoch_ms).then(a.seq.cmp(&b.seq)))
        .and_then(|r| r.limit_after);

    out.view = match span {
        Span::H1 | Span::H24 | Span::D7 => {
            let data_max = columns.iter().map(|c| c.cost).max().unwrap_or(0);
            HistoryView::Windows {
                columns,
                scale: ScaleInputs::spent(data_max, limit),
            }
        }
        Span::D30 | Span::D90 => {
            let b = bounds.expect("trailing presets always resolve");
            let days = day_columns(&columns, b, q.tz_offset_ms);
            let data_max = days.iter().map(|d| d.peak_window_cost).max().unwrap_or(0);
            HistoryView::Days {
                columns: days,
                scale: ScaleInputs::spent(data_max, limit),
            }
        }
    };
    out
}

fn burn_down(scoped: &[&UsageRecord], w: CurrentWindow, as_of_ms: u64) -> BurnDown {
    let mut snaps: Vec<&&UsageRecord> = scoped
        .iter()
        .filter(|r| is_snapshot(r) && reset_ms(r) == Some(w.reset_ms))
        .collect();
    snaps.sort_by(|a, b| a.ts_epoch_ms.cmp(&b.ts_epoch_ms).then(a.seq.cmp(&b.seq)));
    let limit = snaps.last().and_then(|r| r.limit_after);
    let mut steps: Vec<BurnStep> = snaps
        .iter()
        .map(|r| BurnStep {
            ts_ms: r.ts_epoch_ms,
            remaining: r.remaining_after.unwrap_or(0),
        })
        .collect();
    if let Some(last) = steps.last()
        && last.ts_ms < as_of_ms
    {
        let remaining = last.remaining;
        steps.push(BurnStep {
            ts_ms: as_of_ms,
            remaining,
        });
    }
    BurnDown {
        window: w,
        as_of_ms,
        steps,
        scale: ScaleInputs::remaining(limit),
    }
}

/// Windows are discovered from snapshots (their own reset), then every
/// record of the budget lands in the window whose `[reset − 1 h, reset)`
/// holds its timestamp — robust to the recorder's cross-transport
/// carry-forward of `cycle_reset_epoch`.
fn window_columns(scoped: &[&UsageRecord]) -> (Vec<WindowColumn>, SpendTotals) {
    let mut windows: BTreeMap<u64, (WindowColumn, Option<u32>)> = BTreeMap::new();
    for r in scoped.iter().filter(|r| is_snapshot(r)) {
        let Some(reset) = reset_ms(r) else { continue };
        let remaining = r.remaining_after.unwrap_or(0);
        let (col, min_remaining) = windows.entry(reset).or_insert_with(|| {
            (
                WindowColumn {
                    start_ms: reset.saturating_sub(WINDOW_MS),
                    reset_ms: reset,
                    cost: 0,
                    requests: 0,
                    unknown: 0,
                    peak_drop: None,
                    limit: None,
                    state: None,
                },
                None,
            )
        });
        if r.limit_after.is_some() {
            col.limit = r.limit_after.max(col.limit);
        }
        *min_remaining = Some(min_remaining.map_or(remaining, |m| m.min(remaining)));
    }
    let mut unwindowed = SpendTotals::default();
    for r in scoped {
        // The first window resetting after the record, if it began
        // before it.
        let slot = windows
            .range(r.ts_epoch_ms + 1..)
            .next()
            .map(|(k, _)| *k)
            .filter(|reset| reset.saturating_sub(WINDOW_MS) <= r.ts_epoch_ms);
        match slot.and_then(|k| windows.get_mut(&k)) {
            Some((col, _)) => {
                col.requests += 1;
                match r.cost {
                    Some(c) => col.cost += c as u64,
                    None => col.unknown += 1,
                }
            }
            None => unwindowed.add(r),
        }
    }
    let columns = windows
        .into_values()
        .map(|(mut col, min_remaining)| {
            if let (Some(limit), Some(min)) = (col.limit, min_remaining) {
                col.peak_drop = Some(limit.saturating_sub(min));
                col.state = ThresholdState::of(min, Some(limit));
            }
            col
        })
        .collect();
    (columns, unwindowed)
}

fn local_day_start(ts: u64, tz_offset_ms: i64) -> u64 {
    let local = ts as i64 + tz_offset_ms;
    (local.div_euclid(DAY_MS as i64) * DAY_MS as i64 - tz_offset_ms).max(0) as u64
}

fn day_columns(windows: &[WindowColumn], b: Bounds, tz_offset_ms: i64) -> Vec<DayColumn> {
    let first = local_day_start(b.from_ms, tz_offset_ms);
    let mut days: Vec<DayColumn> = Vec::new();
    let mut day = first;
    while day < b.to_ms {
        days.push(DayColumn {
            day_start_ms: day,
            day_end_ms: day + DAY_MS,
            peak_window_cost: 0,
            peak_window_reset_ms: None,
            peak_drop: None,
            total_cost: 0,
            requests: 0,
            unknown: 0,
            windows: 0,
            state: None,
        });
        day += DAY_MS;
    }
    for w in windows {
        // A window belongs to the day it starts on; one that began
        // before the first day (clipped by the range) joins the first.
        let ix = (w.start_ms.max(first) - first) / DAY_MS;
        let Some(d) = days.get_mut(ix as usize) else {
            continue;
        };
        d.windows += 1;
        d.total_cost += w.cost;
        d.requests += w.requests;
        d.unknown += w.unknown;
        if d.peak_window_reset_ms.is_none() || w.cost > d.peak_window_cost {
            d.peak_window_cost = w.cost;
            d.peak_window_reset_ms = Some(w.reset_ms);
        }
        d.peak_drop = d.peak_drop.max(w.peak_drop);
        d.state = d.state.max(w.state);
    }
    days
}

// ---------------------------------------------------------------------------
// D9 — avoidable spend
// ---------------------------------------------------------------------------

/// Exclusive, checked in this order (D9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AvoidableCategory {
    /// Cost paid on failed requests (non-2xx/3xx, network error).
    Errors,
    /// Cost paid on 200s by an op that usually comes back 304.
    RevalidationGap,
    /// Cost from `poll`, `refocus` and `probe` triggers.
    TimerAndRefocus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvoidableQuery {
    pub scope: BudgetScope,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvoidableEntry {
    pub op: String,
    pub category: AvoidableCategory,
    pub cost: u64,
    /// Share of the budget's range spend, %.
    pub share_pct: Option<f64>,
    /// The requests counted in this entry.
    pub requests: u64,
    /// The op's 304s and 200s in range: the reason line's numbers for a
    /// revalidation gap ("200 on 12 of 40 checks").
    pub op_304s: u64,
    pub op_200s: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Avoidable {
    pub budget: Budget,
    pub unit: String,
    pub bounds: Option<Bounds>,
    /// The budget's range spend (the share denominator).
    pub total_cost: u64,
    /// The top [`AVOIDABLE_TOP_N`], cost desc, requests desc, op asc,
    /// category order. Entries costing 0 are not avoidable spend and
    /// never appear.
    pub entries: Vec<AvoidableEntry>,
    /// Ranked entries before the cut.
    pub total_entries: usize,
}

/// A full 200 response (not a revalidation).
fn is_200(r: &UsageRecord) -> bool {
    r.status == 200 && !r.etag_304
}

/// Band 4 (D9): one budget's operations ranked by avoidable cost.
/// Heuristics, not proof.
pub fn avoidable(rows: &[UsageRecord], q: &AvoidableQuery) -> Avoidable {
    let (bounds, scoped) = budget_rows(rows, &q.scope);
    let total_cost = SpendTotals::of(scoped.iter().copied()).known_cost;

    let mut by_op: BTreeMap<String, Vec<&UsageRecord>> = BTreeMap::new();
    for r in &scoped {
        by_op.entry(r.op.to_string()).or_default().push(r);
    }
    let mut entries = Vec::new();
    for (op, members) in by_op {
        let n304 = members.iter().filter(|r| r.etag_304).count() as u64;
        let n200 = members.iter().filter(|r| is_200(r)).count() as u64;
        let usually_304 = n200 > 0 && n304 > n200;
        let mut cats: BTreeMap<AvoidableCategory, (u64, u64)> = BTreeMap::new();
        for r in members {
            let category = if is_error(r) {
                AvoidableCategory::Errors
            } else if usually_304 && is_200(r) {
                AvoidableCategory::RevalidationGap
            } else if TIMER_TRIGGERS.contains(&r.trigger.as_ref()) {
                AvoidableCategory::TimerAndRefocus
            } else {
                continue;
            };
            let slot = cats.entry(category).or_default();
            slot.0 += r.cost.unwrap_or(0) as u64;
            slot.1 += 1;
        }
        for (category, (cost, requests)) in cats {
            if cost == 0 {
                continue;
            }
            entries.push(AvoidableEntry {
                op: op.clone(),
                category,
                cost,
                share_pct: share(cost, total_cost),
                requests,
                op_304s: n304,
                op_200s: n200,
            });
        }
    }
    entries.sort_by(|a, b| {
        b.cost
            .cmp(&a.cost)
            .then(b.requests.cmp(&a.requests))
            .then_with(|| a.op.cmp(&b.op))
            .then(a.category.cmp(&b.category))
    });
    let total_entries = entries.len();
    entries.truncate(AVOIDABLE_TOP_N);
    Avoidable {
        budget: q.scope.budget,
        unit: q.scope.budget.unit().to_string(),
        bounds,
        total_cost,
        entries,
        total_entries,
    }
}

// ---------------------------------------------------------------------------
// D10 — the Requests view: trend (with its ranking) and the log
// ---------------------------------------------------------------------------

/// The `Status class` facet (D10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StatusClass {
    /// 2xx (and any other non-error status that isn't a 304).
    Success,
    /// 304 revalidation.
    NotModified,
    ClientError,
    ServerError,
    /// No response: the flight failed on the network.
    Network,
}

impl StatusClass {
    pub fn of(r: &UsageRecord) -> StatusClass {
        if r.network_error {
            StatusClass::Network
        } else if r.etag_304 || r.status == 304 {
            StatusClass::NotModified
        } else if r.status >= 500 {
            StatusClass::ServerError
        } else if r.status >= 400 {
            StatusClass::ClientError
        } else {
            StatusClass::Success
        }
    }
}

/// The Requests view's facets (D10): all four transports reachable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestFilters {
    pub transport: Option<Transport>,
    pub module: Option<String>,
    pub status_class: Option<StatusClass>,
}

impl RequestFilters {
    fn matches(&self, r: &UsageRecord) -> bool {
        self.transport.is_none_or(|t| r.transport == t)
            && self.module.as_ref().is_none_or(|m| r.module.as_ref() == m)
            && self.status_class.is_none_or(|c| StatusClass::of(r) == c)
    }

    /// Q7: `cost` is offered only for a single quota transport, so a
    /// trend never pools requests and points (D2).
    pub fn cost_allowed(&self) -> bool {
        matches!(self.transport, Some(Transport::Rest | Transport::Graphql))
    }
}

fn request_rows<'a>(
    rows: &'a [UsageRecord],
    b: Bounds,
    f: &RequestFilters,
) -> Vec<&'a UsageRecord> {
    rows.iter()
        .filter(|r| b.contains(r.ts_epoch_ms) && f.matches(r))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Measure {
    Count,
    Cost,
    P50,
    P90,
    Max,
    ErrorRate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestTrendQuery {
    pub as_of_ms: u64,
    pub span: Span,
    /// Local offset from UTC, ms: buckets align to the local clock.
    pub tz_offset_ms: i64,
    pub filters: RequestFilters,
    pub measure: Measure,
    pub group_by: Dimension,
    pub then_by: Option<Dimension>,
}

/// The clock-aligned bucket of each span (D10: whole hours, 6-hour
/// blocks or days; the 1-hour range uses 5-minute buckets, since one
/// hour bucket is no trend).
pub fn trend_bucket_ms(span: Span) -> u64 {
    match span {
        Span::H1 => 5 * MINUTE_MS,
        Span::H24 => HOUR_MS,
        Span::D7 => 6 * HOUR_MS,
        Span::D30 | Span::D90 => DAY_MS,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrendBucket {
    pub start_ms: u64,
    pub end_ms: u64,
    pub requests: u64,
    /// The measure's value: `None` for an empty bucket's percentile or
    /// error rate, and for `Cost` when cost isn't allowed (Q7).
    pub value: Option<f64>,
}

/// One ranking row (D10 band 3), every measure filled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestGroup {
    pub key: String,
    pub requests: u64,
    /// Σ known cost; `None` unless cost is allowed (Q7).
    pub cost: Option<u64>,
    pub unknown: u64,
    pub p50_ms: u32,
    pub p90_ms: u32,
    pub max_ms: u32,
    pub error_rate_pct: f64,
    pub children: Vec<RequestGroup>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestTrend {
    pub bounds: Bounds,
    pub measure: Measure,
    /// False when `Cost` is not offered for this facet (Q7): the view
    /// disables the option with its one-line reason.
    pub cost_allowed: bool,
    pub bucket_ms: u64,
    /// Every bucket from the aligned start to `as_of`, empty ones
    /// included. The first and last are clipped by the range.
    pub buckets: Vec<TrendBucket>,
    /// Sorted by the measure desc (by count when cost is refused),
    /// requests desc, key asc.
    pub groups: Vec<RequestGroup>,
    pub requests: u64,
    pub errors: u64,
}

fn measure_of(rows: &[&UsageRecord], m: Measure, cost_allowed: bool) -> Option<f64> {
    let durations = || rows.iter().map(|r| r.duration_ms).collect::<Vec<_>>();
    match m {
        Measure::Count => Some(rows.len() as f64),
        Measure::Cost => {
            cost_allowed.then(|| rows.iter().filter_map(|r| r.cost).map(|c| c as f64).sum())
        }
        _ if rows.is_empty() => None,
        Measure::P50 => Some(percentile_of(durations(), 50) as f64),
        Measure::P90 => Some(percentile_of(durations(), 90) as f64),
        Measure::Max => rows.iter().map(|r| r.duration_ms as f64).reduce(f64::max),
        Measure::ErrorRate => {
            Some(rows.iter().filter(|r| is_error(r)).count() as f64 / rows.len() as f64 * 100.0)
        }
    }
}

/// Bands 2 and 3 (D10): the selected measure over clock-aligned time,
/// and the ranking table mirroring it by group.
pub fn request_trend(rows: &[UsageRecord], q: &RequestTrendQuery) -> RequestTrend {
    let bounds = Bounds::trailing(q.as_of_ms, q.span);
    let scoped = request_rows(rows, bounds, &q.filters);
    let cost_allowed = q.filters.cost_allowed();
    let len = trend_bucket_ms(q.span);
    let align = |ts: u64| {
        let local = ts as i64 + q.tz_offset_ms;
        (local.div_euclid(len as i64) * len as i64 - q.tz_offset_ms).max(0) as u64
    };
    let first = align(bounds.from_ms);
    let count = (bounds.to_ms.saturating_sub(first)).div_ceil(len) as usize;
    let mut slots: Vec<Vec<&UsageRecord>> = vec![Vec::new(); count];
    for r in &scoped {
        let ix = ((r.ts_epoch_ms - first) / len) as usize;
        if let Some(slot) = slots.get_mut(ix) {
            slot.push(r);
        }
    }
    let buckets = slots
        .iter()
        .enumerate()
        .map(|(i, members)| TrendBucket {
            start_ms: first + i as u64 * len,
            end_ms: first + (i as u64 + 1) * len,
            requests: members.len() as u64,
            value: measure_of(members, q.measure, cost_allowed),
        })
        .collect();
    let rank_by = if q.measure == Measure::Cost && !cost_allowed {
        Measure::Count
    } else {
        q.measure
    };
    let mut groups = request_groups(&scoped, q.group_by, q.then_by, cost_allowed, rank_by);
    sort_requests(&mut groups, rank_by);
    RequestTrend {
        bounds,
        measure: q.measure,
        cost_allowed,
        bucket_ms: len,
        buckets,
        groups,
        requests: scoped.len() as u64,
        errors: scoped.iter().filter(|r| is_error(r)).count() as u64,
    }
}

fn request_groups(
    rows: &[&UsageRecord],
    primary: Dimension,
    secondary: Option<Dimension>,
    cost_allowed: bool,
    rank_by: Measure,
) -> Vec<RequestGroup> {
    let mut by_key: BTreeMap<String, Vec<&UsageRecord>> = BTreeMap::new();
    for r in rows {
        by_key.entry(primary.value(r)).or_default().push(r);
    }
    by_key
        .into_iter()
        .map(|(key, members)| {
            let durations: Vec<u32> = members.iter().map(|r| r.duration_ms).collect();
            let mut children = secondary
                .map(|sec| request_groups(&members, sec, None, cost_allowed, rank_by))
                .unwrap_or_default();
            sort_requests(&mut children, rank_by);
            RequestGroup {
                requests: members.len() as u64,
                cost: cost_allowed.then(|| {
                    members
                        .iter()
                        .filter_map(|r| r.cost)
                        .map(|c| c as u64)
                        .sum()
                }),
                unknown: members.iter().filter(|r| r.cost.is_none()).count() as u64,
                p50_ms: percentile_of(durations.clone(), 50),
                p90_ms: percentile_of(durations, 90),
                max_ms: members.iter().map(|r| r.duration_ms).max().unwrap_or(0),
                error_rate_pct: members.iter().filter(|r| is_error(r)).count() as f64
                    / members.len() as f64
                    * 100.0,
                key,
                children,
            }
        })
        .collect()
}

fn group_measure(g: &RequestGroup, m: Measure) -> f64 {
    match m {
        Measure::Count => g.requests as f64,
        Measure::Cost => g.cost.unwrap_or(0) as f64,
        Measure::P50 => g.p50_ms as f64,
        Measure::P90 => g.p90_ms as f64,
        Measure::Max => g.max_ms as f64,
        Measure::ErrorRate => g.error_rate_pct,
    }
}

fn sort_requests(groups: &mut [RequestGroup], m: Measure) {
    groups.sort_by(|a, b| {
        group_measure(b, m)
            .total_cmp(&group_measure(a, m))
            .then(b.requests.cmp(&a.requests))
            .then_with(|| a.key.cmp(&b.key))
    });
}

/// The log's sortable columns (D10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogSortKey {
    Time,
    Operation,
    Target,
    Surface,
    Trigger,
    Status,
    Cost,
    Duration,
    Queue,
    Size,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogSort {
    pub key: LogSortKey,
    pub descending: bool,
}

impl Default for LogSort {
    /// Newest first (D10).
    fn default() -> Self {
        LogSort {
            key: LogSortKey::Time,
            descending: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestLogQuery {
    pub as_of_ms: u64,
    pub span: Span,
    pub filters: RequestFilters,
    pub sort: LogSort,
}

/// The log's cost cell spells its unit, because the log mixes
/// transports (D10): "12 points", "1 request", "free (304)",
/// "unknown", "no budget".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CostCell {
    Requests(u32),
    Points(u32),
    Free304,
    Unknown,
    NoBudget,
}

impl CostCell {
    fn of(r: &UsageRecord) -> CostCell {
        let Some(budget) = Budget::of(r.transport) else {
            return CostCell::NoBudget;
        };
        match r.cost {
            None => CostCell::Unknown,
            Some(_) if r.etag_304 => CostCell::Free304,
            Some(c) => match budget {
                Budget::Rest => CostCell::Requests(c),
                Budget::Graphql => CostCell::Points(c),
            },
        }
    }

    /// The numeric sort value: no budget and unknown sort below 0.
    fn sort_value(self) -> Option<u32> {
        match self {
            CostCell::Requests(c) | CostCell::Points(c) => Some(c),
            CostCell::Free304 => Some(0),
            CostCell::Unknown | CostCell::NoBudget => None,
        }
    }
}

/// One log row: time · op · target · surface · trigger · status · cost
/// · duration · queue · size (D10). The status reason and date
/// formatting are the view's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogRow {
    pub seq: u64,
    pub ts_ms: u64,
    pub module: String,
    pub transport: Transport,
    pub op: String,
    pub target: Option<String>,
    pub surface: String,
    pub trigger: String,
    /// 0 on network error.
    pub status: u16,
    pub status_class: StatusClass,
    pub cost: CostCell,
    pub duration_ms: u32,
    pub queue_ms: Option<u32>,
    pub resp_bytes: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestLog {
    pub bounds: Bounds,
    pub sort: LogSort,
    pub rows: Vec<LogRow>,
}

/// Band 5 (D10): every request in the selection, sorted. Ties fall
/// back to newest first, so any sort is deterministic.
pub fn request_log(rows: &[UsageRecord], q: &RequestLogQuery) -> RequestLog {
    let bounds = Bounds::trailing(q.as_of_ms, q.span);
    let mut out: Vec<LogRow> = request_rows(rows, bounds, &q.filters)
        .into_iter()
        .map(|r| LogRow {
            seq: r.seq,
            ts_ms: r.ts_epoch_ms,
            module: r.module.to_string(),
            transport: r.transport,
            op: r.op.to_string(),
            target: r.ident.as_ref().map(|i| i.to_string()),
            surface: r.surface.to_string(),
            trigger: r.trigger.to_string(),
            status: r.status,
            status_class: StatusClass::of(r),
            cost: CostCell::of(r),
            duration_ms: r.duration_ms,
            queue_ms: r.queue_ms,
            resp_bytes: r.resp_bytes,
        })
        .collect();
    out.sort_by(|a, b| {
        use std::cmp::Ordering;
        let primary = match q.sort.key {
            LogSortKey::Time => Ordering::Equal,
            LogSortKey::Operation => a.op.cmp(&b.op),
            LogSortKey::Target => a.target.cmp(&b.target),
            LogSortKey::Surface => a.surface.cmp(&b.surface),
            LogSortKey::Trigger => a.trigger.cmp(&b.trigger),
            LogSortKey::Status => a.status.cmp(&b.status),
            LogSortKey::Cost => a.cost.sort_value().cmp(&b.cost.sort_value()),
            LogSortKey::Duration => a.duration_ms.cmp(&b.duration_ms),
            LogSortKey::Queue => a.queue_ms.cmp(&b.queue_ms),
            LogSortKey::Size => a.resp_bytes.cmp(&b.resp_bytes),
        };
        let time = a.ts_ms.cmp(&b.ts_ms).then(a.seq.cmp(&b.seq));
        let primary = if q.sort.key == LogSortKey::Time {
            time
        } else {
            primary
        };
        let primary = if q.sort.descending {
            primary.reverse()
        } else {
            primary
        };
        primary.then(time.reverse())
    });
    RequestLog {
        bounds,
        sort: q.sort,
        rows: out,
    }
}

#[cfg(test)]
mod tests;
