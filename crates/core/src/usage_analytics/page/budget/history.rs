//! Band 3 (docs/0082 D8): spend per quota window, never a cycle
//! selector. Heights are GitHub's reported drop (§2.1, #709; §4
//! principle): `peak_drop`, other clients included. A window or day
//! without a reported ceiling has no height, so the chart leaves a gap.
//! This app's recorded figures appear only in each column's tooltip and
//! in the chart's a11y summary.

use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, App};

use super::{band_frame, format, ladder};
use crate::usage_analytics::page::chart::{
    self,
    burn::BurnChart,
    columns::{ColumnMark, ColumnsChart, Guide, Tip},
};
use crate::usage_analytics::surfaces::{
    Budget, BurnDown, DayColumn, HistoryView, Preset, ScaleInputs, Span, ThresholdState,
    WindowColumn, WindowHistory,
};

/// D8 (Q3): the axis shows the whole budget once the data reaches
/// about 90 % of the limit.
const FULL_SCALE_AT: f64 = 0.9;
const DAY_MS: u64 = 24 * 60 * 60_000;

/// The band title for `preset` (D21), known before the data lands so
/// the skeleton keeps it.
pub(super) fn title(preset: Preset) -> &'static str {
    match preset {
        Preset::CurrentWindow => "This window",
        Preset::Trailing(Span::D30 | Span::D90) => "Peak window per day",
        Preset::Trailing(_) => "Spend per window",
    }
}

/// D8's column axis (Q3): the ticks, the guides drawn, and the
/// off-scale limit marker when the limit sits above the axis.
#[derive(Debug, PartialEq)]
pub(super) struct Axis {
    pub ticks: Vec<f64>,
    pub guides: Vec<(f64, &'static str, bool)>,
    pub off_scale: Option<(String, String)>,
}

pub(super) fn columns_axis(s: &ScaleInputs) -> Axis {
    let limit = s.limit.filter(|l| *l > 0).map(f64::from);
    let full = limit.is_some_and(|l| s.data_max as f64 >= FULL_SCALE_AT * l);
    let ticks = chart::value_ticks(match (full, limit) {
        (true, Some(l)) => l,
        _ => s.data_max as f64,
    });
    let top = ticks.last().copied().unwrap_or(1.0);
    let mut guides = Vec::new();
    let mut off_scale = None;
    if let Some(l) = limit {
        if l <= top {
            guides.push((l, "limit", true));
        } else {
            off_scale = Some((
                format!("limit {}", format::count(l as u64)),
                "above scale".to_string(),
            ));
        }
    }
    for (v, name) in [(s.critical_guide, "Critical"), (s.low_guide, "Low")] {
        if let Some(v) = v.filter(|v| *v <= top) {
            guides.push((v, name, false));
        }
    }
    Axis {
        ticks,
        guides,
        off_scale,
    }
}

fn state_word(state: Option<ThresholdState>) -> &'static str {
    match state {
        Some(s) => ladder::word(s).unwrap_or("Normal"),
        None => "no state",
    }
}

/// "4,292 of 5,000 points (Low)", GitHub's reported drop.
fn drop_words(
    drop: Option<u32>,
    limit: Option<u32>,
    state: Option<ThresholdState>,
    b: Budget,
) -> String {
    let Some(d) = drop else {
        return "no limit reported".into();
    };
    let d = u64::from(d);
    match limit {
        Some(l) => format!(
            "{} of {} {} ({})",
            format::count(d),
            format::count(u64::from(l)),
            format::unit(u64::from(l), b),
            state_word(state)
        ),
        None => format::amount(d, b),
    }
}

/// "3,900 points over 412 requests (2 with no reported cost)"; for
/// REST "12 requests counted against the limit, of 14 sent".
fn recorded_words(cost: u64, requests: u64, unknown: u64, b: Budget) -> String {
    let mut s = format::cost_of_sent(cost, requests, b, false);
    if unknown > 0 {
        s.push_str(&format!(" ({unknown} with no reported cost)"));
    }
    s
}

/// The band's summary line, from GitHub's reporting only.
pub(super) fn summary(h: &WindowHistory, preset: Preset) -> Option<String> {
    let name = super::budget_name(h.budget);
    let dated = !matches!(preset, Preset::CurrentWindow);
    match &h.view {
        HistoryView::NoWindow => None,
        HistoryView::BurnDown(b) => Some(burn_summary(b, h.budget)),
        HistoryView::Windows { columns, .. } => {
            if columns.is_empty() {
                return None;
            }
            let n = columns.len();
            let windows = format!("{n} {name} window{}", if n == 1 { "" } else { "s" });
            let peak = columns
                .iter()
                .filter(|c| c.peak_drop.is_some())
                .max_by(|a, b| {
                    a.peak_drop
                        .cmp(&b.peak_drop)
                        .then(b.start_ms.cmp(&a.start_ms))
                });
            Some(match peak {
                Some(c) => format!(
                    "GitHub's reported spend per window: {windows}; peak {}, {}.",
                    format::window_span(c.start_ms, c.reset_ms, dated),
                    drop_words(c.peak_drop, c.limit, c.state, h.budget)
                ),
                None => {
                    format!("{windows}; GitHub reported no limit for them, so they have no height.")
                }
            })
        }
        HistoryView::Days { columns, scale } => {
            if columns.iter().all(|d| d.windows == 0) {
                return None;
            }
            let peak = columns
                .iter()
                .filter(|d| d.peak_drop.is_some())
                .max_by(|a, b| {
                    a.peak_drop
                        .cmp(&b.peak_drop)
                        .then(b.day_start_ms.cmp(&a.day_start_ms))
                });
            Some(match peak {
                Some(d) => format!(
                    "GitHub's closest approach to the {name} limit, per day; highest {}, {}.",
                    format::day(d.day_start_ms),
                    drop_words(d.peak_drop, scale.limit, d.state, h.budget)
                ),
                None => format!(
                    "GitHub reported no {name} limit in this range, so the days have no height."
                ),
            })
        }
    }
}

fn burn_summary(b: &BurnDown, budget: Budget) -> String {
    let name = super::budget_name(budget);
    let span = format::window_span(b.window.start_ms, b.window.reset_ms, false);
    let left = b.steps.last().map(|s| u64::from(s.remaining));
    let n = b.steps.len();
    let responses = format!("{n} response{}", if n == 1 { "" } else { "s" });
    match (left, b.scale.limit) {
        (Some(l), Some(limit)) => format!(
            "{name} window {span}: {} of {} {} left after {responses}; resets {}.",
            format::count(l),
            format::count(u64::from(limit)),
            format::unit(u64::from(limit), budget),
            format::clock(b.window.reset_ms)
        ),
        (Some(l), None) => format!(
            "{name} window {span}: {} left after {responses}; resets {}.",
            format::amount(l, budget),
            format::clock(b.window.reset_ms)
        ),
        _ => format!(
            "{name} window {span}: no response yet; resets {}.",
            format::clock(b.window.reset_ms)
        ),
    }
}

/// The band's empty copy (D18).
pub(super) fn empty_copy(h: &WindowHistory, preset: Preset) -> String {
    let name = super::budget_name(h.budget);
    match preset {
        Preset::CurrentWindow => format!(
            "No open {name} window: the last one has reset, or GitHub hasn't answered yet. Open a GitHub page or pick a longer range."
        ),
        _ => format!(
            "No {name} responses from GitHub {}. Widen the range or open a GitHub page.",
            format::range_phrase(preset)
        ),
    }
}

/// The chart's one a11y summary (D19): GitHub's figures, then this
/// app's recorded figures over the same columns.
pub(super) fn chart_label(h: &WindowHistory, line: &str) -> String {
    let b = h.budget;
    let (cost, requests, unknown) = match &h.view {
        HistoryView::Windows { columns, .. } => columns.iter().fold((0, 0, 0), |(c, r, u), w| {
            (c + w.cost, r + w.requests, u + w.unknown)
        }),
        HistoryView::Days { columns, .. } => columns.iter().fold((0, 0, 0), |(c, r, u), d| {
            (c + d.total_cost, r + d.requests, u + d.unknown)
        }),
        HistoryView::BurnDown(_) | HistoryView::NoWindow => return line.to_string(),
    };
    let mut s = line.to_string();
    let scale = match &h.view {
        HistoryView::Windows { scale, .. } | HistoryView::Days { scale, .. } => Some(scale),
        _ => None,
    };
    if let Some((limit, _)) = scale.and_then(|sc| columns_axis(sc).off_scale) {
        s.push_str(&format!(" The {limit} is above the chart's scale."));
    }
    s.push_str(&format!(
        " Recorded by this app across these windows: {}.",
        recorded_words(cost, requests, unknown, b)
    ));
    if h.unwindowed.requests > 0 {
        s.push_str(&format!(
            " {} fell outside every reported window.",
            format::requests(h.unwindowed.requests)
        ));
    }
    s
}

fn window_tip(c: &WindowColumn, b: Budget, dated: bool) -> Tip {
    (
        format::window_span(c.start_ms, c.reset_ms, dated),
        vec![
            (
                "Reported by GitHub".into(),
                drop_words(c.peak_drop, c.limit, c.state, b),
            ),
            (
                "Recorded by this app".into(),
                recorded_words(c.cost, c.requests, c.unknown, b),
            ),
        ],
    )
}

fn day_tip(d: &DayColumn, limit: Option<u32>, b: Budget) -> Tip {
    let peak = match d.peak_window_reset_ms {
        Some(reset) => format!(
            "{}, {}",
            format::window_span(reset.saturating_sub(60 * 60_000), reset, false),
            drop_words(d.peak_drop, limit, d.state, b)
        ),
        None if d.windows == 0 => "no windows".into(),
        None => "no limit reported".into(),
    };
    (
        format::day(d.day_start_ms),
        vec![
            ("Peak window (GitHub)".into(), peak),
            (
                "Recorded total".into(),
                recorded_words(d.total_cost, d.requests, d.unknown, b),
            ),
            (
                "Recorded peak window".into(),
                format::amount(d.peak_window_cost, b),
            ),
        ],
    )
}

/// Band 3's chart for `h`, `None` when the view has nothing to draw.
pub(super) fn chart(h: &Arc<WindowHistory>, tz_offset_ms: i64, cx: &App) -> Option<AnyElement> {
    let b = h.budget;
    let bounds = h.bounds?;
    let dated = bounds.to_ms - bounds.from_ms > DAY_MS;
    let (marks, axis): (Vec<ColumnMark>, Axis) = match &h.view {
        HistoryView::NoWindow => return None,
        HistoryView::BurnDown(burn) => {
            return Some(burn_chart(burn, b, tz_offset_ms).into_any_element());
        }
        HistoryView::Windows { columns, scale } => (
            columns
                .iter()
                .map(|c| ColumnMark {
                    start_ms: c.start_ms,
                    end_ms: c.reset_ms,
                    value: c.peak_drop.map(f64::from),
                    color: ladder::mark_color(c.state, cx),
                })
                .collect(),
            columns_axis(scale),
        ),
        HistoryView::Days { columns, scale } => (
            columns
                .iter()
                .map(|d| ColumnMark {
                    start_ms: d.day_start_ms,
                    end_ms: d.day_end_ms,
                    value: d.peak_drop.map(f64::from),
                    color: ladder::mark_color(d.state, cx),
                })
                .collect(),
            columns_axis(scale),
        ),
    };
    let data = h.clone();
    let tip = Rc::new(move |ix: usize| -> Option<Tip> {
        match &data.view {
            HistoryView::Windows { columns, .. } => {
                columns.get(ix).map(|c| window_tip(c, b, dated))
            }
            HistoryView::Days { columns, scale } => {
                columns.get(ix).map(|d| day_tip(d, scale.limit, b))
            }
            _ => None,
        }
    });
    Some(
        ColumnsChart {
            id: "usage-history-plot",
            from_ms: bounds.from_ms,
            to_ms: bounds.to_ms,
            tz_offset_ms,
            marks,
            ticks: axis.ticks,
            format: chart::ValueFormat::Count,
            guides: axis
                .guides
                .into_iter()
                .map(|(value, name, limit)| Guide { value, name, limit })
                .collect(),
            off_scale: axis.off_scale,
            tip,
        }
        .into_any_element(),
    )
}

fn burn_chart(burn: &BurnDown, b: Budget, tz_offset_ms: i64) -> BurnChart {
    let s = &burn.scale;
    let top = s.limit.map_or_else(
        || {
            burn.steps
                .iter()
                .map(|st| f64::from(st.remaining))
                .fold(0.0, f64::max)
        },
        f64::from,
    );
    BurnChart {
        id: "usage-history-plot",
        start_ms: burn.window.start_ms,
        reset_ms: burn.window.reset_ms,
        as_of_ms: burn.as_of_ms,
        tz_offset_ms,
        steps: burn
            .steps
            .iter()
            .map(|st| (st.ts_ms, f64::from(st.remaining)))
            .collect(),
        ticks: chart::value_ticks(top),
        guides: [(s.low_guide, "Low"), (s.critical_guide, "Critical")]
            .into_iter()
            .filter_map(|(v, n)| v.map(|v| (v, n)))
            .collect(),
        limit: s.limit.map(f64::from),
        unit: b.unit(),
    }
}

/// Band 3: title, summary and chart, or the band's Empty.
pub(super) fn band(
    h: &Arc<WindowHistory>,
    preset: Preset,
    tz_offset_ms: i64,
    cx: &App,
) -> AnyElement {
    let body = match summary(h, preset) {
        None => {
            band_frame::empty("usage-history-empty", empty_copy(h, preset), cx).into_any_element()
        }
        Some(line) => {
            let label = chart_label(h, &line);
            gpui_component::v_flex()
                .gap_3()
                .child(band_frame::summary(line, cx))
                .when_some(chart(h, tz_offset_ms, cx), |this, plot| {
                    this.child(chart::frame("usage-history-chart", label, plot))
                })
                .into_any_element()
        }
    };
    band_frame::section("usage-history", title(preset), None::<gpui::Div>, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_analytics::surfaces::{Bounds, SpendTotals};

    fn scale(data_max: u64, limit: Option<u32>) -> ScaleInputs {
        let l = limit.map(f64::from);
        ScaleInputs {
            data_max,
            limit,
            low_guide: l.map(|l| l * 0.75),
            critical_guide: l.map(|l| l * 0.9),
        }
    }

    #[test]
    fn low_data_fits_the_axis_and_marks_the_limit_off_scale() {
        let a = columns_axis(&scale(19, Some(5_000)));
        assert_eq!(a.ticks.last(), Some(&20.0));
        assert!(a.guides.is_empty(), "no guide falls inside 0–20");
        assert_eq!(
            a.off_scale,
            Some(("limit 5,000".into(), "above scale".into()))
        );
    }

    #[test]
    fn data_near_the_limit_shows_the_whole_budget_with_guides() {
        let a = columns_axis(&scale(4_600, Some(5_000)));
        assert_eq!(a.ticks.last(), Some(&5_000.0));
        assert_eq!(
            a.guides,
            [
                (5_000.0, "limit", true),
                (4_500.0, "Critical", false),
                (3_750.0, "Low", false)
            ]
        );
        assert_eq!(a.off_scale, None);
        // Below 90 % but rounding up past the limit: the limit is on
        // the scale, so it is drawn, not marked off-scale.
        let b = columns_axis(&scale(4_292, Some(5_000)));
        assert_eq!(b.off_scale, None);
        assert_eq!(b.guides[0], (5_000.0, "limit", true));
        // No ceiling: neither.
        assert_eq!(columns_axis(&scale(30, None)).off_scale, None);
    }

    fn column(start: u64, drop: Option<u32>, state: Option<ThresholdState>) -> WindowColumn {
        WindowColumn {
            start_ms: start,
            reset_ms: start + 3_600_000,
            cost: 12,
            requests: 10,
            unknown: 1,
            peak_drop: drop,
            limit: Some(5_000),
            state,
        }
    }

    fn history(view: HistoryView) -> WindowHistory {
        WindowHistory {
            budget: Budget::Graphql,
            unit: "points".into(),
            bounds: Some(Bounds {
                from_ms: 0,
                to_ms: DAY_MS,
            }),
            view,
            unwindowed: SpendTotals {
                requests: 2,
                known_cost: 0,
                unknown: 0,
            },
        }
    }

    #[test]
    fn the_summary_reports_github_and_the_recorded_figures_go_to_the_label() {
        let h = history(HistoryView::Windows {
            columns: vec![
                column(10 * 3_600_000, Some(4_292), Some(ThresholdState::Low)),
                column(11 * 3_600_000, None, None),
            ],
            scale: scale(4_292, Some(5_000)),
        });
        let line = summary(&h, Preset::Trailing(Span::H24)).unwrap();
        assert!(
            line.starts_with("GitHub's reported spend per window: 2 GraphQL windows; peak "),
            "{line}"
        );
        assert!(line.ends_with(", 4,292 of 5,000 points (Low)."), "{line}");
        assert!(
            !line.contains("recorded"),
            "no recorded figure in the summary: {line}"
        );
        let label = chart_label(&h, &line);
        assert!(
            label.ends_with(
                "Recorded by this app across these windows: 24 points over 20 requests (2 with no reported cost). 2 requests fell outside every reported window."
            ),
            "{label}"
        );
        assert_eq!(title(Preset::Trailing(Span::H24)), "Spend per window");
        assert_eq!(title(Preset::Trailing(Span::D90)), "Peak window per day");
        let (_, rows) = window_tip(&column(0, None, None), Budget::Graphql, false);
        assert_eq!(rows[0].1, "no limit reported", "a gap says why");
    }

    #[test]
    fn empty_ranges_and_closed_windows_get_band_empties() {
        let none = history(HistoryView::NoWindow);
        assert_eq!(summary(&none, Preset::CurrentWindow), None);
        assert!(empty_copy(&none, Preset::CurrentWindow).starts_with("No open GraphQL window"));
        let empty = history(HistoryView::Windows {
            columns: vec![],
            scale: scale(0, None),
        });
        assert_eq!(summary(&empty, Preset::Trailing(Span::H24)), None);
        assert_eq!(
            empty_copy(&empty, Preset::Trailing(Span::D7)),
            "No GraphQL responses from GitHub in the last 7 days. Widen the range or open a GitHub page."
        );
        assert_eq!(title(Preset::CurrentWindow), "This window");
    }
}
