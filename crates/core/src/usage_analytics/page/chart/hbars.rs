//! D7's horizontal bar chart (docs/0082 D7, D14): the top groups plus
//! "other", one `chart_1` bar each with its value label, keys on the
//! left and a value axis with ticks below, its last tick naming the
//! unit ("8,000 points"). Categories are told apart by position and
//! label, never by hue.

use gpui::{App, Bounds, Pixels, SharedString, TextAlign, Window, point, px};
use gpui_component::ActiveTheme as _;
use gpui_component::plot::{
    AxisText, Grid, IntoPlot, Plot, PlotAxis, axis_gutter,
    label::{PlotLabel, TEXT_GAP, TEXT_SIZE, Text, truncate_text_to_width},
    scale::{Scale, ScaleLinear},
    shape::{Bar, BarAlignment},
};

use crate::usage_analytics::page::honesty::fmt_count;

/// The chart's fixed geometry (docs/0002 D10).
pub(in crate::usage_analytics::page) const ROW_H: f32 = 26.0;
const KEY_W: f32 = 150.0; // the key labels, left
const VALUE_PAD: f32 = 64.0; // room for the value label past the longest bar
const BAR_H: f32 = 12.0;

/// The chart's height for `rows` bars.
pub(in crate::usage_analytics::page) fn height(rows: usize) -> f32 {
    rows.max(1) as f32 * ROW_H + axis_gutter(px(TEXT_SIZE))
}

#[derive(IntoPlot)]
pub(in crate::usage_analytics::page) struct HBarChart {
    /// `(key, value)` top to bottom.
    pub rows: Vec<(String, f64)>,
    /// The value-axis ticks; the last is the axis end.
    pub ticks: Vec<f64>,
    /// The unit the last tick names.
    pub unit: &'static str,
}

impl Plot for HBarChart {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let theme = cx.theme();
        let (muted, border, foreground, series) = (
            theme.muted_foreground,
            theme.border,
            theme.foreground,
            theme.chart_1,
        );
        let w = (bounds.size.width.as_f32() - KEY_W - VALUE_PAD).max(1.0);
        let h = self.rows.len().max(1) as f32 * ROW_H;
        let plot = Bounds {
            origin: bounds.origin + point(px(KEY_W), px(0.0)),
            size: gpui::size(px(w), px(h)),
        };
        let top = self.ticks.last().copied().unwrap_or(1.0);
        let x = ScaleLinear::new([0.0, top], [0.0, w]);

        Grid::new()
            .x(self.ticks.iter().filter_map(|t| x.tick(t)).map(px))
            .stroke(border.opacity(0.5))
            .paint(&plot, window);

        let keys = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, (k, _))| {
                let key = truncate_text_to_width(
                    &SharedString::from(k.clone()),
                    px(TEXT_SIZE),
                    KEY_W - 12.0,
                    window,
                );
                Text::new(
                    key,
                    point(
                        px(KEY_W - 12.0),
                        px(i as f32 * ROW_H + (ROW_H - TEXT_SIZE) / 2.0),
                    ),
                    foreground,
                )
                .align(TextAlign::Right)
            })
            .collect();
        PlotLabel::new(keys).paint(&bounds, window, cx);

        let data: Vec<(usize, f64)> = self.rows.iter().map(|(_, v)| *v).enumerate().collect();
        let xv = x.clone();
        Bar::new()
            .data(data)
            .alignment(BarAlignment::Left)
            .band_width(BAR_H)
            .cross(|(i, _)| Some(*i as f32 * ROW_H + (ROW_H - BAR_H) / 2.0))
            .base(|_| 0.0)
            .value(move |(_, v)| xv.tick(v))
            .fill(move |_, _, _| series)
            .label(move |(_, v), origin| {
                vec![Text::new(
                    SharedString::from(fmt_count(*v as u64)),
                    origin,
                    muted,
                )]
            })
            .paint(&plot, window, cx);

        let last = self.ticks.len().saturating_sub(1);
        let labels = self.ticks.iter().enumerate().filter_map(|(i, t)| {
            let text = if i == last && i > 0 {
                format!("{} {}", fmt_count(*t as u64), self.unit)
            } else {
                fmt_count(*t as u64)
            };
            x.tick(t)
                .map(|px_x| AxisText::new(text, px(px_x), muted).align(TextAlign::Center))
        });
        PlotAxis::new()
            .x(px(h + TEXT_GAP))
            .x_axis(false)
            .x_label(labels)
            .stroke(border)
            .paint(&plot, window, cx);
    }
}
