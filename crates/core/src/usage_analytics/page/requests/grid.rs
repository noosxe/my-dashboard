//! A small static table for Diagnostics' cross-tabs (docs/0082 D10,
//! D19): a header row, then text rows, sized to their content (D20) and
//! never a Tab stop. Each cell is its own `Cell` node with its text, so
//! assistive technology reads the grid cell by cell.

use gpui::prelude::*;
use gpui::{AnyElement, App, ElementId, FontWeight, SharedString, div, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};

/// A column: its header, its width (docs/0002 D10: a named canvas
/// size) and whether it reads right-aligned (numbers).
pub(super) struct GridCol {
    pub name: &'static str,
    pub width: f32,
    pub right: bool,
}

/// One body cell: its text, and whether it recedes (zeros, dashes).
pub(super) struct GridCell {
    pub text: String,
    pub muted: bool,
}

impl GridCell {
    pub(super) fn new(text: String) -> Self {
        let muted = text == "0" || text == "—";
        GridCell { text, muted }
    }
}

pub(super) fn grid(
    id: &'static str,
    label: String,
    cols: &[GridCol],
    rows: Vec<Vec<GridCell>>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let cell = |c: &GridCol, text: SharedString, key: ElementId| {
        div()
            .id(key)
            .role(gpui::accesskit::Role::Cell)
            .aria_label(text.clone())
            .w(px(c.width))
            .flex_none()
            .px_2()
            .py_1()
            .truncate()
            .when(c.right, |d| d.text_right())
            .child(text)
    };
    let header = h_flex()
        .id((id, 0usize))
        .role(gpui::accesskit::Role::Row)
        .border_b_1()
        .border_color(theme.border)
        .text_color(theme.muted_foreground)
        .font_weight(FontWeight::MEDIUM)
        .children(cols.iter().enumerate().map(|(j, c)| {
            cell(c, c.name.into(), ElementId::from((id, 1_000 + j)))
                .role(gpui::accesskit::Role::ColumnHeader)
        }));
    let body = rows.into_iter().enumerate().map(|(i, row)| {
        h_flex()
            .id((id, (i + 1) * 100_000))
            .role(gpui::accesskit::Role::Row)
            .when(i > 0, |r| {
                r.border_t_1().border_color(theme.border.opacity(0.5))
            })
            .children(row.into_iter().zip(cols).enumerate().map(|(j, (g, c))| {
                cell(
                    c,
                    g.text.into(),
                    ElementId::from((id, (i + 1) * 100_000 + j + 1)),
                )
                .when(g.muted, |d| d.text_color(theme.muted_foreground))
            }))
    });
    v_flex()
        .id(id)
        .role(gpui::accesskit::Role::Table)
        .aria_label(label)
        .text_sm()
        .child(header)
        .children(body)
        .into_any_element()
}
