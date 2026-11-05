//! The app-owned charts (docs/0082 D10a, D14) on the pinned
//! `gpui_base::plot` primitives (re-exported as `gpui_component::plot`;
//! the gpui-component story's `stacked_bar_chart.rs` is the worked
//! pattern). The stock charts' x scale is ordinal; these draw time on a
//! proportional axis (`ScaleLinear` over epoch ms), always with
//! value-axis ticks, and one series in `chart_1` (D14).
//!
//! This file holds what every chart shares: the tick maths (pure,
//! tested), the plot gutters and the a11y frame. Each chart is its own
//! module: [`columns`] (time columns), [`burn`] (the step burn-down),
//! [`steps`] (a measure's step line over time buckets) and [`hbars`]
//! (horizontal bars).

use gpui::prelude::*;
use gpui::{AnyElement, Bounds, Hsla, Pixels, SharedString, TextAlign, Window, div, point, px};
use gpui_component::plot::{
    Grid,
    label::{PlotLabel, TEXT_HEIGHT, TEXT_SIZE, Text},
};

pub(super) mod burn;
pub(super) mod columns;
pub(super) mod hbars;
pub(super) mod steps;

const MINUTE_MS: u64 = 60_000;
const HOUR_MS: u64 = 60 * MINUTE_MS;
const DAY_MS: u64 = 24 * HOUR_MS;

/// The plot's fixed geometry (docs/0002 D10: canvas sizes are exempt
/// from the rem scale and therefore named).
pub(super) const PLOT_H: f32 = 200.0; // a time chart's height
pub(super) const Y_GUTTER: f32 = 44.0; // value labels, left
pub(super) const R_GUTTER: f32 = 96.0; // guide names and the off-scale marker, right
pub(super) const TOP_PAD: f32 = 8.0; // room for the top tick label
pub(super) const DASH: [f32; 2] = [4.0, 3.0]; // the limit line and markers
pub(super) const DOT: [f32; 2] = [2.0, 3.0]; // the Low / Critical guides

/// About five ticks from 0 to a "nice" top at or above `max`: steps
/// of 1, 2, 2.5 or 5 × 10ⁿ. The domain is `0..=ticks.last()`.
pub(super) fn value_ticks(max: f64) -> Vec<f64> {
    let max = if max.is_finite() && max > 0.0 {
        max
    } else {
        1.0
    };
    let raw = max / 5.0;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|m| m * mag)
        .find(|s| *s >= raw)
        .unwrap_or(10.0 * mag);
    // Whole-unit data never needs fractional ticks.
    let step = step.max(1.0);
    let n = (max / step).ceil() as usize;
    (0..=n.max(1)).map(|i| i as f64 * step).collect()
}

/// How a chart's values read on its axis and labels: counts
/// (`1,200`), durations (`500 ms`, `1.5 s`) or percentages (`2.5%`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ValueFormat {
    Count,
    Duration,
    Percent,
}

impl ValueFormat {
    pub(super) fn label(self, v: f64) -> String {
        use crate::usage_analytics::page::format;
        match self {
            ValueFormat::Count => format::count(v.max(0.0).round() as u64),
            ValueFormat::Duration => format::duration(v.max(0.0).round() as u64),
            ValueFormat::Percent if v.fract().abs() < f64::EPSILON => format!("{v:.0}%"),
            ValueFormat::Percent => format!("{v:.1}%"),
        }
    }
}

/// A tick on the time axis: where, and what it reads.
#[derive(Debug, PartialEq)]
pub(super) struct TimeTick {
    pub ms: u64,
    pub label: String,
}

/// Clock-aligned ticks over `[from, to]` in the local zone at
/// `tz_offset_ms` (D10a): minutes and hours read `HH:MM` and gain the
/// date at local midnight; day ticks read `Nov 5`.
pub(super) fn time_ticks(from_ms: u64, to_ms: u64, tz_offset_ms: i64) -> Vec<TimeTick> {
    if to_ms <= from_ms {
        return Vec::new();
    }
    let span = to_ms - from_ms;
    let step = match span {
        s if s <= 90 * MINUTE_MS => 10 * MINUTE_MS,
        s if s <= 6 * HOUR_MS => HOUR_MS,
        s if s <= 36 * HOUR_MS => 3 * HOUR_MS,
        s if s <= 8 * DAY_MS => DAY_MS,
        s if s <= 35 * DAY_MS => 5 * DAY_MS,
        _ => 14 * DAY_MS,
    };
    let off = tz_offset_ms;
    // Sub-day steps align to the local clock; day steps start at the
    // first local midnight and count whole days from there.
    let unit = step.min(DAY_MS) as i64;
    let local_from = from_ms as i64 + off;
    let first = (local_from + unit - 1).div_euclid(unit) * unit - off;
    let zone = chrono::FixedOffset::east_opt((off / 1_000) as i32)
        .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).expect("UTC"));
    let mut out = Vec::new();
    let mut t = first.max(0) as u64;
    while t <= to_ms {
        let local =
            chrono::DateTime::from_timestamp_millis(t as i64).map(|d| d.with_timezone(&zone));
        let midnight = (t as i64 + off).rem_euclid(DAY_MS as i64) == 0;
        let label = local.map_or_else(
            || "—".to_string(),
            |d| {
                if step >= DAY_MS || midnight {
                    d.format("%b %-d").to_string()
                } else {
                    d.format("%H:%M").to_string()
                }
            },
        );
        out.push(TimeTick { ms: t, label });
        t += step;
    }
    out
}

/// The inner plot area of `outer`: gutters left and right, the top pad
/// and the x-axis gutter below.
pub(super) fn inner(outer: &Bounds<Pixels>, axis_gutter: f32) -> Bounds<Pixels> {
    let w = (outer.size.width.as_f32() - Y_GUTTER - R_GUTTER).max(1.0);
    let h = (outer.size.height.as_f32() - TOP_PAD - axis_gutter).max(1.0);
    Bounds {
        origin: point(outer.origin.x + px(Y_GUTTER), outer.origin.y + px(TOP_PAD)),
        size: gpui::size(px(w), px(h)),
    }
}

/// Value labels right-aligned in the left gutter, one per tick at the
/// inner area's `y(tick)`; `grid` draws faint lines across.
pub(super) fn paint_value_axis(
    outer: &Bounds<Pixels>,
    ticks: &[(f64, f32)],
    format: ValueFormat,
    color: Hsla,
    grid: Hsla,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let texts = ticks
        .iter()
        .map(|(v, y)| {
            Text::new(
                format.label(*v),
                point(px(Y_GUTTER - 6.0), px(TOP_PAD + y - TEXT_SIZE / 2.0)),
                color,
            )
            .align(TextAlign::Right)
        })
        .collect();
    PlotLabel::new(texts).paint(outer, window, cx);
    let inner = inner(outer, 0.0);
    Grid::new()
        .y(ticks.iter().map(|(_, y)| px(*y)))
        .stroke(grid)
        .paint(&inner, window);
}

/// How a guide line is drawn: its dash pattern and colours.
#[derive(Clone, Copy)]
pub(super) struct GuideStyle {
    pub dash: [f32; 2],
    pub stroke: Hsla,
    pub text: Hsla,
}

impl GuideStyle {
    /// The limit: dashed, in `foreground` (D8).
    pub(super) fn limit(foreground: Hsla) -> Self {
        GuideStyle {
            dash: DASH,
            stroke: foreground.opacity(0.6),
            text: foreground,
        }
    }

    /// A Low / Critical guide: dotted, muted (D8).
    pub(super) fn guide(muted: Hsla) -> Self {
        GuideStyle {
            dash: DOT,
            stroke: muted.opacity(0.7),
            text: muted,
        }
    }
}

/// A dashed horizontal line across the inner area at `y`, named in the
/// right gutter (the limit, the Low and Critical guides, D8).
pub(super) fn paint_guide(
    outer: &Bounds<Pixels>,
    inner: &Bounds<Pixels>,
    (y, name): (f32, &str),
    style: GuideStyle,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let GuideStyle { dash, stroke, text } = style;
    Grid::new()
        .y([px(y)])
        .stroke(stroke)
        .dash_array(&[px(dash[0]), px(dash[1])])
        .paint(inner, window);
    let x = outer.size.width.as_f32() - R_GUTTER + 6.0;
    PlotLabel::new(vec![Text::new(
        SharedString::from(name.to_string()),
        point(px(x), px(TOP_PAD + y - TEXT_HEIGHT / 2.0)),
        text,
    )])
    .paint(outer, window, cx);
}

/// A chart's frame (D19): one node with the chart's single a11y
/// summary, the plot inside at its fixed height.
pub(super) fn frame(id: &'static str, summary: String, plot: AnyElement) -> impl IntoElement {
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .role(gpui::accesskit::Role::Label)
        .aria_label(summary)
        .w_full()
        .h(px(PLOT_H))
        .child(plot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_ticks_reach_a_nice_top_at_or_above_the_data() {
        assert_eq!(
            value_ticks(5_000.0),
            [0.0, 1_000.0, 2_000.0, 3_000.0, 4_000.0, 5_000.0]
        );
        assert_eq!(value_ticks(4_292.0).last(), Some(&5_000.0));
        assert_eq!(value_ticks(19.0), [0.0, 5.0, 10.0, 15.0, 20.0]);
        assert_eq!(
            value_ticks(0.0),
            [0.0, 1.0],
            "an empty range still has an axis"
        );
        assert_eq!(
            value_ticks(3.0),
            [0.0, 1.0, 2.0, 3.0],
            "no fractional ticks"
        );
    }

    #[test]
    fn time_ticks_align_to_the_local_clock_and_date_at_midnight() {
        // 2026-11-05 00:00 UTC.
        let midnight = 1_793_836_800_000u64;
        let utc = time_ticks(
            midnight - 2 * HOUR_MS - 5 * MINUTE_MS,
            midnight + 22 * HOUR_MS,
            0,
        );
        let labels: Vec<_> = utc.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Nov 5", "03:00", "06:00", "09:00", "12:00", "15:00", "18:00", "21:00"
            ]
        );
        // A +4 h zone moves the local midnight four hours earlier.
        let east = time_ticks(midnight - 5 * HOUR_MS, midnight, 4 * HOUR_MS as i64);
        let at = |ms: u64| east.iter().find(|t| t.ms == ms).map(|t| t.label.as_str());
        assert_eq!(at(midnight - 4 * HOUR_MS), Some("Nov 5"));
        assert_eq!(at(midnight - 5 * HOUR_MS), Some("23:00"));
    }

    #[test]
    fn values_read_in_their_own_unit() {
        assert_eq!(ValueFormat::Count.label(1_200.0), "1,200");
        assert_eq!(ValueFormat::Duration.label(500.0), "500 ms");
        assert_eq!(ValueFormat::Duration.label(1_500.0), "1.5 s");
        assert_eq!(ValueFormat::Percent.label(5.0), "5%");
        assert_eq!(ValueFormat::Percent.label(2.5), "2.5%");
        // Duration ticks land on readable steps.
        assert_eq!(
            value_ticks(1_900.0),
            [0.0, 500.0, 1_000.0, 1_500.0, 2_000.0]
        );
    }

    #[test]
    fn longer_ranges_tick_by_days() {
        let midnight = 1_793_836_800_000u64;
        let week = time_ticks(midnight - 7 * DAY_MS, midnight, 0);
        assert_eq!(week.len(), 8);
        assert_eq!(week.last().map(|t| t.label.as_str()), Some("Nov 5"));
        let quarter = time_ticks(midnight - 90 * DAY_MS, midnight, 0);
        assert!(quarter.len() <= 8, "{}", quarter.len());
        let hour = time_ticks(midnight, midnight + HOUR_MS, 0);
        assert_eq!(hour.len(), 7, "every 10 min");
        assert!(time_ticks(midnight, midnight, 0).is_empty());
    }
}
