//! D7's table (docs/0082 D7, D19, D20): it mirrors the chart with every
//! group and every nested `Then by` row. Columns: key · `Cost` · share
//! (with an inline share bar) · requests · avg cost · free (304) ·
//! errors · unknown cost. Sortable through upstream `perform_sort`;
//! every cell exposes its text (`cell_label`). Rows come from the
//! derived [`WhereItWent`]; the delegate only flattens and orders them.

use std::cmp::Ordering;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{App, Context, SharedString, Window, div, px};
use gpui_component::ActiveTheme as _;
use gpui_component::h_flex;
use gpui_component::table::{Column, ColumnSort, TableDelegate, TableState};

use super::format;
use crate::usage_analytics::surfaces::{SpendDimension, SpendRow, WhereItWent};

/// The share bar's track width (docs/0002 D10: a named canvas size).
const SHARE_BAR_W: f32 = 64.0;
/// The table shows this many rows before it scrolls (D20's cap).
pub(super) const MAX_VISIBLE_ROWS: usize = 16;

/// The columns in order (D7, Q11: the cost column is `Cost`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Key,
    Cost,
    Share,
    Requests,
    Avg,
    Free,
    Errors,
    Unknown,
}

const COLS: [Col; 8] = [
    Col::Key,
    Col::Cost,
    Col::Share,
    Col::Requests,
    Col::Avg,
    Col::Free,
    Col::Errors,
    Col::Unknown,
];

impl Col {
    fn name(self, group_by: SpendDimension) -> &'static str {
        match self {
            Col::Key => group_by.label(),
            Col::Cost => "Cost",
            Col::Share => "Share",
            Col::Requests => "Requests",
            Col::Avg => "Avg cost",
            Col::Free => "Free (304)",
            Col::Errors => "Errors",
            Col::Unknown => "Unknown cost",
        }
    }

    /// Order two rows by this column, ascending.
    fn cmp(self, a: &SpendRow, b: &SpendRow) -> Ordering {
        let f = |x: Option<f64>, y: Option<f64>| {
            x.unwrap_or(-1.0)
                .partial_cmp(&y.unwrap_or(-1.0))
                .unwrap_or(Ordering::Equal)
        };
        match self {
            Col::Key => a.key.cmp(&b.key),
            // Share is the cost's share: one order.
            Col::Cost | Col::Share => a.cost.cmp(&b.cost),
            Col::Requests => a.requests.cmp(&b.requests),
            Col::Avg => f(a.avg_cost, b.avg_cost),
            Col::Free => a.free_304.cmp(&b.free_304),
            Col::Errors => a.errors.cmp(&b.errors),
            Col::Unknown => a.unknown.cmp(&b.unknown),
        }
    }
}

/// One drawn row: a group, or a nested member labelled with its own
/// dimension ("operation: notifications", D7).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct FlatRow {
    pub label: String,
    pub nested: bool,
    pub row: SpendRow,
}

/// Every group, each followed by all its members; both levels ordered
/// by `sort` (the procedure's order when unsorted: cost desc, requests
/// desc, key asc).
fn flatten(
    groups: &[SpendRow],
    then_by: Option<SpendDimension>,
    sort: Option<(Col, ColumnSort)>,
) -> Vec<FlatRow> {
    let order = |rows: &mut Vec<&SpendRow>| {
        if let Some((col, dir)) = sort {
            rows.sort_by(|a, b| {
                let o = col.cmp(a, b);
                let o = if dir == ColumnSort::Descending {
                    o.reverse()
                } else {
                    o
                };
                o.then_with(|| a.key.cmp(&b.key))
            });
        }
    };
    let mut top: Vec<&SpendRow> = groups.iter().collect();
    order(&mut top);
    let mut out = Vec::new();
    for g in top {
        out.push(FlatRow {
            label: g.key.clone(),
            nested: false,
            row: SpendRow {
                children: Vec::new(),
                ..g.clone()
            },
        });
        let mut kids: Vec<&SpendRow> = g.children.iter().collect();
        order(&mut kids);
        let dim = then_by.map_or("", |d| d.label());
        out.extend(kids.into_iter().map(|c| FlatRow {
            label: format!("{}: {}", dim.to_lowercase(), c.key),
            nested: true,
            row: c.clone(),
        }));
    }
    out
}

fn cell_text(r: &FlatRow, col: Col) -> String {
    let row = &r.row;
    match col {
        Col::Key => r.label.clone(),
        Col::Cost => format::count(row.cost),
        Col::Share => format::share(row.share_pct),
        Col::Requests => format::count(row.requests),
        Col::Avg => format::avg(row.avg_cost),
        Col::Free => format::count(row.free_304),
        Col::Errors => format::count(row.errors),
        Col::Unknown => format::count(row.unknown),
    }
}

/// The table's delegate over one derived [`WhereItWent`].
pub(super) struct SpendTable {
    group_by: SpendDimension,
    then_by: Option<SpendDimension>,
    /// The derivation the rows came from; a new one replaces them.
    source: Option<Arc<WhereItWent>>,
    sort: Option<(Col, ColumnSort)>,
    rows: Vec<FlatRow>,
}

impl SpendTable {
    pub(super) fn empty() -> Self {
        SpendTable {
            group_by: SpendDimension::Surface,
            then_by: None,
            source: None,
            sort: Some((Col::Cost, ColumnSort::Descending)),
            rows: Vec::new(),
        }
    }

    /// Whether `data` is already the table's source.
    pub(super) fn shows(&self, data: &Arc<WhereItWent>) -> bool {
        self.source.as_ref().is_some_and(|s| Arc::ptr_eq(s, data))
    }

    pub(super) fn set(
        &mut self,
        data: Arc<WhereItWent>,
        group_by: SpendDimension,
        then_by: Option<SpendDimension>,
    ) {
        self.group_by = group_by;
        self.then_by = then_by;
        self.rows = flatten(&data.groups, then_by, self.sort);
        self.source = Some(data);
    }

    pub(super) fn len(&self) -> usize {
        self.rows.len()
    }
}

impl TableDelegate for SpendTable {
    fn columns_count(&self, _: &App) -> usize {
        COLS.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let col = COLS.get(col_ix).copied().unwrap_or(Col::Key);
        let key: SharedString = format!("{col:?}").to_lowercase().into();
        let mut c = Column::new(key, col.name(self.group_by));
        if col == Col::Key {
            return c.width(px(200.)).sort(self.sort_of(col));
        }
        // Wide enough for each header plus upstream's sort icon.
        c = c.text_right().width(px(match col {
            Col::Free => 104.,
            Col::Unknown => 120.,
            _ => 92.,
        }));
        if col == Col::Share {
            // Share follows cost; sorting it would be a second cost sort.
            return c.width(px(120.));
        }
        c.sort(self.sort_of(col))
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
        let Some(col) = COLS.get(col_ix).copied() else {
            return;
        };
        self.sort = (sort != ColumnSort::Default).then_some((col, sort));
        if let Some(data) = &self.source {
            self.rows = flatten(&data.groups, self.then_by, self.sort);
        }
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let (Some(r), Some(col)) = (self.rows.get(row_ix), COLS.get(col_ix).copied()) else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let text = cell_text(r, col);
        let zero = match col {
            Col::Free => r.row.free_304 == 0,
            Col::Errors => r.row.errors == 0,
            Col::Unknown => r.row.unknown == 0,
            _ => false,
        };
        let cell = h_flex()
            .size_full()
            .items_center()
            .when(col != Col::Key, |c| c.justify_end())
            .when(col == Col::Key && r.nested, |c| {
                c.pl_4().text_color(theme.muted_foreground)
            })
            // Zero counts recede, so the non-zero ones stand out.
            .when(zero, |c| c.text_color(theme.muted_foreground));
        if col == Col::Share {
            let pct = r.row.share_pct.unwrap_or(0.0).clamp(0.0, 100.0) as f32;
            return cell
                .gap_3()
                .child(
                    div()
                        .w(px(SHARE_BAR_W))
                        .h(px(4.))
                        .rounded_full()
                        .bg(theme.chart_1.opacity(0.25))
                        .child(
                            div()
                                .h_full()
                                .rounded_full()
                                .bg(theme.chart_1)
                                .w(px(SHARE_BAR_W * pct / 100.0)),
                        ),
                )
                .child(text)
                .into_any_element();
        }
        cell.child(text).into_any_element()
    }

    fn cell_label(&self, row_ix: usize, col_ix: usize, _: &App) -> Option<SharedString> {
        let r = self.rows.get(row_ix)?;
        Some(cell_text(r, *COLS.get(col_ix)?).into())
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, cx: &App) -> String {
        self.cell_label(row_ix, col_ix, cx)
            .map(|s| s.to_string())
            .unwrap_or_default()
    }
}

impl SpendTable {
    fn sort_of(&self, col: Col) -> ColumnSort {
        match self.sort {
            Some((c, dir)) if c == col => dir,
            _ => ColumnSort::Default,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, cost: u64, requests: u64, children: Vec<SpendRow>) -> SpendRow {
        SpendRow {
            key: key.into(),
            cost,
            share_pct: Some(cost as f64),
            requests,
            avg_cost: Some(cost as f64 / requests.max(1) as f64),
            free_304: 0,
            errors: 0,
            unknown: 0,
            children,
        }
    }

    fn groups() -> Vec<SpendRow> {
        vec![
            row(
                "pr-page",
                60,
                5,
                vec![
                    row("pr-files", 40, 2, vec![]),
                    row("pr-meta", 20, 3, vec![]),
                ],
            ),
            row("repo-view", 30, 9, vec![row("repo-labels", 30, 9, vec![])]),
        ]
    }

    #[test]
    fn every_nested_row_is_shown_under_its_group_and_labels_its_dimension() {
        let flat = flatten(&groups(), Some(SpendDimension::Operation), None);
        let labels: Vec<_> = flat.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "pr-page",
                "operation: pr-files",
                "operation: pr-meta",
                "repo-view",
                "operation: repo-labels"
            ]
        );
        assert!(flat[1].nested && !flat[0].nested);
        assert!(flat[0].row.children.is_empty(), "rows are flat");
    }

    #[test]
    fn a_sort_orders_both_levels_and_keeps_members_with_their_group() {
        let by_requests = flatten(
            &groups(),
            Some(SpendDimension::Operation),
            Some((Col::Requests, ColumnSort::Descending)),
        );
        let labels: Vec<_> = by_requests.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "repo-view",
                "operation: repo-labels",
                "pr-page",
                "operation: pr-meta",
                "operation: pr-files"
            ]
        );
        let by_key = flatten(&groups(), None, Some((Col::Key, ColumnSort::Ascending)));
        assert_eq!(by_key[0].label, "pr-page");
    }

    #[test]
    fn cells_expose_the_text_they_draw() {
        let flat = flatten(&groups(), None, None);
        let r = &flat[0];
        let texts: Vec<_> = COLS.iter().map(|c| cell_text(r, *c)).collect();
        assert_eq!(texts, ["pr-page", "60", "60%", "5", "12.0", "0", "0", "0"]);
        assert_eq!(Col::Cost.name(SpendDimension::Surface), "Cost", "Q11");
        assert_eq!(Col::Key.name(SpendDimension::Trigger), "Trigger");
    }
}
