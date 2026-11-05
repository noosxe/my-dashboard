//! A measure over time buckets as a step line (docs/0082 D10, D10a,
//! Q14): each bucket's value held flat across its real span on a
//! proportional time axis, a `chart_1` area beneath. A bucket with no
//! value (no requests to take a percentile of) breaks the line: a gap,
//! never a drop to 0. A single valued bucket is a marked point with its
//! value label and a drop line (D18), never an empty frame.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ElementId, Pixels, Point, SharedString, TextAlign, Window, fill,
    point, px, size,
};
use gpui_component::ActiveTheme as _;
use gpui_component::plot::{
    AxisText, Curve, Grid, IntoPlot, Plot, PlotAxis, TooltipState, axis_gutter,
    label::{PlotLabel, TEXT_HEIGHT, TEXT_SIZE, Text},
    scale::{Scale, ScaleLinear},
    shape::{Area, Line},
    tooltip::{CrossLine, Tooltip},
};

use super::columns::Tip;
use super::{ValueFormat, Y_GUTTER, inner, paint_value_axis, time_ticks};

/// The point mark's diameter (docs/0002 D10: a named canvas size).
const DOT_D: f32 = 7.0;

/// One bucket: its span and its value (`None` is a gap).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::usage_analytics::page) struct Step {
    pub start_ms: u64,
    pub end_ms: u64,
    pub value: Option<f64>,
}

/// The runs of consecutive valued buckets, each as step points: every
/// bucket's start at its value, then the run's last end, clipped to
/// `[from, to]`.
pub(in crate::usage_analytics::page) fn runs(
    steps: &[Step],
    from: u64,
    to: u64,
) -> Vec<Vec<(u64, f64)>> {
    let mut out = Vec::new();
    // Buckets are contiguous, so a step at each start carries its value
    // to the next start; the run closes at its last bucket's end.
    for run in steps.split(|s| s.value.is_none()).filter(|r| !r.is_empty()) {
        let mut pts: Vec<(u64, f64)> = run
            .iter()
            .filter_map(|s| s.value.map(|v| (s.start_ms.clamp(from, to), v)))
            .collect();
        if let (Some(last), Some(&(_, v))) = (run.last(), pts.last()) {
            pts.push((last.end_ms.clamp(from, to), v));
        }
        out.push(pts);
    }
    out
}

#[derive(IntoPlot)]
pub(in crate::usage_analytics::page) struct StepsChart {
    pub id: &'static str,
    pub from_ms: u64,
    pub to_ms: u64,
    pub tz_offset_ms: i64,
    pub steps: Vec<Step>,
    /// The value-axis ticks; the last is the axis top.
    pub ticks: Vec<f64>,
    pub format: ValueFormat,
    /// The hovered bucket's tooltip, formatted on hover only.
    pub tip: Rc<dyn Fn(usize) -> Option<Tip>>,
}

impl StepsChart {
    fn gutter() -> f32 {
        axis_gutter(px(TEXT_SIZE))
    }

    fn scales(
        &self,
        outer: &Bounds<Pixels>,
    ) -> (Bounds<Pixels>, ScaleLinear<f64>, ScaleLinear<f64>) {
        let inner = inner(outer, Self::gutter());
        let (w, h) = (inner.size.width.as_f32(), inner.size.height.as_f32());
        let x = ScaleLinear::new([self.from_ms as f64, self.to_ms as f64], [0.0, w]);
        let top = self.ticks.last().copied().unwrap_or(1.0);
        (inner, x, ScaleLinear::new([0.0, top], [h, 0.0]))
    }

    fn step_at(&self, outer: &Bounds<Pixels>, pos_x: f32) -> Option<usize> {
        let w = inner(outer, Self::gutter()).size.width.as_f32();
        let rel = (pos_x - Y_GUTTER) / w;
        if !(0.0..=1.0).contains(&rel) {
            return None;
        }
        let t = self.from_ms as f64 + rel as f64 * (self.to_ms - self.from_ms) as f64;
        self.steps
            .iter()
            .position(|s| (s.start_ms as f64) <= t && t < s.end_ms as f64)
    }
}

impl Plot for StepsChart {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let theme = cx.theme();
        let (muted, border, foreground, series) = (
            theme.muted_foreground,
            theme.border,
            theme.foreground,
            theme.chart_1,
        );
        let (inner, x, y) = self.scales(&bounds);
        let h = inner.size.height.as_f32();

        let ticks: Vec<(f64, f32)> = self
            .ticks
            .iter()
            .filter_map(|v| y.tick(v).map(|py| (*v, py)))
            .collect();
        paint_value_axis(
            &bounds,
            &ticks,
            self.format,
            muted,
            border.opacity(0.5),
            window,
            cx,
        );

        let valued: Vec<&Step> = self.steps.iter().filter(|s| s.value.is_some()).collect();
        if let [only] = valued.as_slice() {
            // D18: one sample is a point, its value and a drop line.
            let v = only.value.unwrap_or(0.0);
            let mid = (only.start_ms.max(self.from_ms) + only.end_ms.min(self.to_ms)) / 2;
            let (px_x, px_y) = (
                x.tick(&(mid as f64)).unwrap_or(0.0),
                y.tick(&v).unwrap_or(h),
            );
            Grid::new().x([px(px_x)]).stroke(series).paint(
                &Bounds {
                    origin: point(inner.origin.x, inner.origin.y + px(px_y)),
                    size: size(inner.size.width, px(h - px_y)),
                },
                window,
            );
            window.paint_quad(
                fill(
                    Bounds {
                        origin: inner.origin
                            + point(px(px_x - DOT_D / 2.0), px(px_y - DOT_D / 2.0)),
                        size: size(px(DOT_D), px(DOT_D)),
                    },
                    series,
                )
                .corner_radii(px(DOT_D / 2.0)),
            );
            PlotLabel::new(vec![Text::new(
                SharedString::from(self.format.label(v)),
                point(px(px_x + DOT_D), px(px_y - TEXT_HEIGHT)),
                foreground,
            )])
            .paint(&inner, window, cx);
        } else {
            for run in runs(&self.steps, self.from_ms, self.to_ms) {
                let (xa, ya, xl, yl) = (x.clone(), y.clone(), x.clone(), y.clone());
                Area::new()
                    .data(run.clone())
                    .x(move |(t, _)| xa.tick(&(*t as f64)))
                    .y0(h)
                    .y1(move |(_, v)| ya.tick(v))
                    .fill(series.opacity(0.3))
                    .curve(Curve::StepAfter)
                    .paint(&inner, window);
                Line::new()
                    .data(run)
                    .x(move |(t, _)| xl.tick(&(*t as f64)))
                    .y(move |(_, v)| yl.tick(v))
                    .stroke(series)
                    .stroke_width(px(1.5))
                    .curve(Curve::StepAfter)
                    .paint(&inner, window);
            }
        }

        let labels = time_ticks(self.from_ms, self.to_ms, self.tz_offset_ms)
            .into_iter()
            .filter_map(|t| {
                x.tick(&(t.ms as f64))
                    .map(|px_x| AxisText::new(t.label, px(px_x), muted).align(TextAlign::Center))
            });
        PlotAxis::new()
            .x(px(h))
            .x_label(labels)
            .stroke(border)
            .paint(&inner, window, cx);
    }

    fn id(&self) -> Option<ElementId> {
        Some(self.id.into())
    }

    fn tooltip_state(
        &self,
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _cx: &App,
    ) -> Option<TooltipState> {
        if position.y.as_f32() > bounds.size.height.as_f32() - Self::gutter() {
            return None;
        }
        let ix = self.step_at(&bounds, position.x.as_f32())?;
        let (_, x, _) = self.scales(&bounds);
        let s = &self.steps[ix];
        let mid = (s.start_ms.max(self.from_ms) + s.end_ms.min(self.to_ms)) / 2;
        let cx_px = x.tick(&(mid as f64)).unwrap_or(0.0) + Y_GUTTER;
        Some(TooltipState::new(ix, point(px(cx_px), position.y), vec![]))
    }

    fn tooltip(
        &self,
        state: &TooltipState,
        cursor: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<AnyElement> {
        let (title, rows) = (self.tip)(state.index)?;
        let mut tip = Tooltip::new(cursor, bounds.size)
            .gap(px(8.))
            .cross_line(
                CrossLine::new(state.cross_line)
                    .height(bounds.size.height.as_f32() - Self::gutter()),
            )
            .title(title);
        for (k, v) in rows {
            tip = tip.plain_row(k, v);
        }
        Some(tip.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(start: u64, value: Option<f64>) -> Step {
        Step {
            start_ms: start,
            end_ms: start + 10,
            value,
        }
    }

    #[test]
    fn runs_break_at_empty_buckets_and_close_at_the_last_end() {
        let steps = [
            step(0, Some(5.0)),
            step(10, Some(7.0)),
            step(20, None),
            step(30, Some(2.0)),
        ];
        assert_eq!(
            runs(&steps, 0, 40),
            [
                vec![(0, 5.0), (10, 7.0), (20, 7.0)],
                vec![(30, 2.0), (40, 2.0)]
            ]
        );
        assert!(runs(&[step(0, None)], 0, 10).is_empty());
    }

    #[test]
    fn runs_clip_to_the_range() {
        // The aligned first bucket starts before the range.
        let steps = [step(0, Some(1.0)), step(10, Some(3.0))];
        assert_eq!(runs(&steps, 4, 15), [vec![(4, 1.0), (10, 3.0), (15, 3.0)]]);
    }
}
