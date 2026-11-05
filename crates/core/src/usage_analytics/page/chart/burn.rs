//! The `Current window` burn-down (docs/0082 D8, D10a, Q14): a step
//! line of `remaining_after` across the window with a `chart_1` area
//! beneath, the whole budget on the value axis, the limit and the Low /
//! Critical guides, a dashed `as of` marker and the reset at the right
//! edge. A single sample is a marked point with its value label and a
//! drop line (D18), never an empty frame.

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

use super::{DASH, GuideStyle, Y_GUTTER, inner, paint_guide, paint_value_axis, time_ticks};
use crate::usage_analytics::page::honesty::{fmt_count, format_clock};

/// The point mark's diameter (docs/0002 D10: a named canvas size).
const DOT_D: f32 = 7.0;

#[derive(IntoPlot)]
pub(in crate::usage_analytics::page) struct BurnChart {
    pub id: &'static str,
    pub start_ms: u64,
    pub reset_ms: u64,
    pub as_of_ms: u64,
    pub tz_offset_ms: i64,
    /// `(ts, remaining)` by time; the last value carries to `as_of`.
    pub steps: Vec<(u64, f64)>,
    /// The value-axis ticks, 0 to the limit (the whole budget, D8).
    pub ticks: Vec<f64>,
    /// `(value, name)`: the Low and Critical guides (25 % and 10 %
    /// remaining).
    pub guides: Vec<(f64, &'static str)>,
    pub limit: Option<f64>,
    /// "points" / "requests", for the tooltip.
    pub unit: &'static str,
}

impl BurnChart {
    fn gutter() -> f32 {
        axis_gutter(px(TEXT_SIZE))
    }

    fn scales(
        &self,
        outer: &Bounds<Pixels>,
    ) -> (Bounds<Pixels>, ScaleLinear<f64>, ScaleLinear<f64>) {
        let inner = inner(outer, Self::gutter());
        let (w, h) = (inner.size.width.as_f32(), inner.size.height.as_f32());
        let x = ScaleLinear::new([self.start_ms as f64, self.reset_ms as f64], [0.0, w]);
        let top = self.ticks.last().copied().unwrap_or(1.0);
        (inner, x, ScaleLinear::new([0.0, top], [h, 0.0]))
    }

    /// The step in force at `t`: the last sample at or before it.
    fn step_at(&self, t: f64) -> Option<usize> {
        self.steps.iter().rposition(|(ts, _)| (*ts as f64) <= t)
    }
}

impl Plot for BurnChart {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let theme = cx.theme();
        let (muted, border, foreground, series) = (
            theme.muted_foreground,
            theme.border,
            theme.foreground,
            theme.chart_1,
        );
        let (inner, x, y) = self.scales(&bounds);
        let (w, h) = (inner.size.width.as_f32(), inner.size.height.as_f32());

        let ticks: Vec<(f64, f32)> = self
            .ticks
            .iter()
            .filter_map(|v| y.tick(v).map(|py| (*v, py)))
            .collect();
        paint_value_axis(&bounds, &ticks, muted, border.opacity(0.5), window, cx);

        if let [(ts, v)] = self.steps.as_slice() {
            // D18: one sample is a point, its value and a drop line.
            let (px_x, px_y) = (x.tick(&(*ts as f64)).unwrap_or(0.0), y.tick(v).unwrap_or(h));
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
                SharedString::from(fmt_count(*v as u64)),
                point(px(px_x + DOT_D), px(px_y - TEXT_HEIGHT)),
                foreground,
            )])
            .paint(&inner, window, cx);
        } else if !self.steps.is_empty() {
            // The last value carries to `as_of` (the step's end).
            let mut pts = self.steps.clone();
            if let Some(&(_, last)) = pts.last() {
                pts.push((self.as_of_ms, last));
            }
            let (xa, ya, xl, yl) = (x.clone(), y.clone(), x.clone(), y.clone());
            Area::new()
                .data(pts.clone())
                .x(move |(t, _)| xa.tick(&(*t as f64)))
                .y0(h)
                .y1(move |(_, v)| ya.tick(v))
                .fill(series.opacity(0.3))
                .curve(Curve::StepAfter)
                .paint(&inner, window);
            Line::new()
                .data(pts)
                .x(move |(t, _)| xl.tick(&(*t as f64)))
                .y(move |(_, v)| yl.tick(v))
                .stroke(series)
                .stroke_width(px(1.5))
                .curve(Curve::StepAfter)
                .paint(&inner, window);
        }

        let labels = time_ticks(self.start_ms, self.reset_ms, self.tz_offset_ms)
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

        if let Some(limit) = self.limit.and_then(|l| y.tick(&l)) {
            paint_guide(
                &bounds,
                &inner,
                (limit, "limit"),
                GuideStyle::limit(foreground),
                window,
                cx,
            );
        }
        for (v, name) in &self.guides {
            if let Some(gy) = y.tick(v) {
                paint_guide(
                    &bounds,
                    &inner,
                    (gy, name),
                    GuideStyle::guide(muted),
                    window,
                    cx,
                );
            }
        }

        // The `as of` marker (dashed) and the reset at the right edge.
        if let Some(ax) = x.tick(&(self.as_of_ms as f64)) {
            Grid::new()
                .x([px(ax)])
                .stroke(muted)
                .dash_array(&[px(DASH[0]), px(DASH[1])])
                .paint(&inner, window);
            PlotLabel::new(vec![
                Text::new(
                    SharedString::from(format!("as of {}", format_clock(self.as_of_ms))),
                    point(px(ax - 4.0), px(h - TEXT_HEIGHT - 2.0)),
                    muted,
                )
                .align(TextAlign::Right),
            ])
            .paint(&inner, window, cx);
        }
        Grid::new()
            .x([px(w)])
            .stroke(foreground)
            .paint(&inner, window);
        PlotLabel::new(vec![
            Text::new(
                SharedString::from(format!("resets {}", format_clock(self.reset_ms))),
                point(px(w - 4.0), px(h - TEXT_HEIGHT - 2.0)),
                foreground,
            )
            .align(TextAlign::Right),
        ])
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
        let (inner, x, _) = self.scales(&bounds);
        let rel = (position.x.as_f32() - Y_GUTTER) / inner.size.width.as_f32();
        let t = self.start_ms as f64 + rel as f64 * (self.reset_ms - self.start_ms) as f64;
        if t > self.as_of_ms as f64 {
            return None;
        }
        let ix = self.step_at(t)?;
        let cx_px = x.tick(&(self.steps[ix].0 as f64)).unwrap_or(0.0) + Y_GUTTER;
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
        let (ts, v) = self.steps.get(state.index)?;
        let left = match self.limit {
            Some(l) => format!("{} of {}", fmt_count(*v as u64), fmt_count(l as u64)),
            None => fmt_count(*v as u64),
        };
        Some(
            Tooltip::new(cursor, bounds.size)
                .gap(px(8.))
                .cross_line(
                    CrossLine::new(state.cross_line)
                        .height(bounds.size.height.as_f32() - Self::gutter()),
                )
                .title(format_clock(*ts))
                .plain_row(format!("{} left", self.unit), left)
                .into_any_element(),
        )
    }
}
