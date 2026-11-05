//! Columns on a proportional time axis (docs/0082 D8, D10a): one column
//! per window or per day, placed by its real start and end, so gaps in
//! time read as gaps. A column without a value is not drawn: a gap,
//! never a 0-height bar.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, ElementId, Hsla, IntoElement as _, PathBuilder, Pixels, Point,
    SharedString, TextAlign, Window, point, px,
};
use gpui_component::ActiveTheme as _;
use gpui_component::plot::{
    AxisText, IntoPlot, Plot, PlotAxis, TooltipState, axis_gutter,
    label::{PlotLabel, TEXT_HEIGHT, TEXT_SIZE, Text},
    scale::{Scale, ScaleLinear},
    shape::Bar,
    tooltip::{CrossLine, Tooltip},
};

use super::{
    GuideStyle, R_GUTTER, TOP_PAD, Y_GUTTER, inner, paint_guide, paint_value_axis, time_ticks,
};

/// One column: its span on the time axis, its height (`None` is a gap)
/// and its mark colour (D5 on marks only).
#[derive(Debug, Clone)]
pub(in crate::usage_analytics::page) struct ColumnMark {
    pub start_ms: u64,
    pub end_ms: u64,
    pub value: Option<f64>,
    pub color: Hsla,
}

/// A horizontal guide (D8): the limit (dashed) or a Low / Critical
/// guide (dotted), named in the right gutter.
#[derive(Debug, Clone)]
pub(in crate::usage_analytics::page) struct Guide {
    pub value: f64,
    pub name: &'static str,
    pub limit: bool,
}

/// A hovered column's tooltip: title and labelled rows.
pub(in crate::usage_analytics::page) type Tip = (String, Vec<(String, String)>);

/// The off-scale marker's arrow (docs/0002 D10: a named canvas size).
const ARROW_W: f32 = 7.0;

/// A small up arrow at `(x, y)` (the top-left of a label line).
fn paint_up_arrow(outer: &Bounds<Pixels>, x: f32, y: f32, color: Hsla, window: &mut Window) {
    let o = outer.origin;
    let at = |dx: f32, dy: f32| point(o.x + px(x + dx), o.y + px(y + dy));
    let mut path = PathBuilder::stroke(px(1.2));
    path.move_to(at(ARROW_W / 2.0, TEXT_HEIGHT));
    path.line_to(at(ARROW_W / 2.0, 1.0));
    path.move_to(at(0.5, ARROW_W / 2.0 + 0.5));
    path.line_to(at(ARROW_W / 2.0, 1.0));
    path.line_to(at(ARROW_W - 0.5, ARROW_W / 2.0 + 0.5));
    if let Ok(p) = path.build() {
        window.paint_path(p, color);
    }
}

#[derive(IntoPlot)]
pub(in crate::usage_analytics::page) struct ColumnsChart {
    pub id: &'static str,
    pub from_ms: u64,
    pub to_ms: u64,
    pub tz_offset_ms: i64,
    pub marks: Vec<ColumnMark>,
    /// The value-axis ticks; the last is the axis top.
    pub ticks: Vec<f64>,
    pub guides: Vec<Guide>,
    /// The off-scale limit marker (Q3), drawn with an up arrow:
    /// `("limit 5,000", "above scale")`.
    pub off_scale: Option<(String, String)>,
    /// The hovered column's tooltip, formatted on hover only.
    pub tip: Rc<dyn Fn(usize) -> Option<Tip>>,
}

impl ColumnsChart {
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
        let y = ScaleLinear::new([0.0, top], [h, 0.0]);
        (inner, x, y)
    }

    /// A column's drawn width: 80 % of its span, at least one pixel.
    fn band(&self, x: &ScaleLinear<f64>, m: &ColumnMark) -> (f32, f32) {
        let x0 = x
            .tick(&(m.start_ms.max(self.from_ms) as f64))
            .unwrap_or(0.0);
        let x1 = x.tick(&(m.end_ms.min(self.to_ms) as f64)).unwrap_or(x0);
        let span = (x1 - x0).max(0.0);
        let bw = (span * 0.8).max(1.0).min(span.max(1.0));
        (x0 + (span - bw) / 2.0, bw)
    }

    fn mark_at(&self, outer: &Bounds<Pixels>, pos_x: f32) -> Option<usize> {
        let inner_w = inner(outer, Self::gutter()).size.width.as_f32();
        let rel = (pos_x - Y_GUTTER) / inner_w;
        if !(0.0..=1.0).contains(&rel) {
            return None;
        }
        let t = self.from_ms as f64 + rel as f64 * (self.to_ms - self.from_ms) as f64;
        self.marks
            .iter()
            .position(|m| (m.start_ms as f64) <= t && t < m.end_ms as f64)
    }
}

impl Plot for ColumnsChart {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let theme = cx.theme();
        let (muted, border, foreground) = (theme.muted_foreground, theme.border, theme.foreground);
        let (inner, x, y) = self.scales(&bounds);
        let h = inner.size.height.as_f32();

        let ticks: Vec<(f64, f32)> = self
            .ticks
            .iter()
            .filter_map(|v| y.tick(v).map(|py| (*v, py)))
            .collect();
        paint_value_axis(&bounds, &ticks, muted, border.opacity(0.5), window, cx);

        let bars: Vec<(f32, f32, f32, Hsla)> = self
            .marks
            .iter()
            .filter_map(|m| {
                let v = m.value?;
                let (cross, bw) = self.band(&x, m);
                Some((cross, bw, y.tick(&v).unwrap_or(h), m.color))
            })
            .collect();
        // One `Bar` per distinct width keeps every column its own span.
        for (cross, bw, top, color) in bars {
            Bar::new()
                .data([()])
                .band_width(bw)
                .cross(move |_| Some(cross))
                .base(move |_| h)
                .value(move |_| Some(top))
                .fill(move |_, _, _| color)
                .paint(&inner, window, cx);
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

        for g in &self.guides {
            let Some(gy) = y.tick(&g.value) else { continue };
            let style = if g.limit {
                GuideStyle::limit(foreground)
            } else {
                GuideStyle::guide(muted)
            };
            paint_guide(&bounds, &inner, (gy, g.name), style, window, cx);
        }

        if let Some((line, sub)) = &self.off_scale {
            let gx = bounds.size.width.as_f32() - R_GUTTER + 6.0;
            // The marker's up arrow (Q3), drawn: the label font has no
            // arrow glyph, and a fallback font would tint it.
            paint_up_arrow(&bounds, gx, TOP_PAD, foreground, window);
            PlotLabel::new(vec![
                Text::new(
                    SharedString::from(line.clone()),
                    point(px(gx + ARROW_W + 3.0), px(TOP_PAD)),
                    foreground,
                ),
                Text::new(
                    SharedString::from(sub.clone()),
                    point(px(gx), px(TOP_PAD + TEXT_HEIGHT + 2.0)),
                    muted,
                ),
            ])
            .paint(&bounds, window, cx);
        }
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
        let ix = self.mark_at(&bounds, position.x.as_f32())?;
        let (_, x, _) = self.scales(&bounds);
        let (cross, bw) = self.band(&x, &self.marks[ix]);
        Some(TooltipState::new(
            ix,
            point(px(Y_GUTTER + cross + bw / 2.0), position.y),
            vec![],
        ))
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
        let (_, x, _) = self.scales(&bounds);
        let (_, bw) = self.band(&x, self.marks.get(state.index)?);
        let mut tip = Tooltip::new(cursor, bounds.size)
            .gap(px(8.))
            .cross_line(
                CrossLine::new(state.cross_line)
                    .height(bounds.size.height.as_f32() - Self::gutter())
                    .band(px(bw)),
            )
            .title(title);
        for (k, v) in rows {
            tip = tip.plain_row(k, v);
        }
        Some(tip.into_any_element())
    }
}
