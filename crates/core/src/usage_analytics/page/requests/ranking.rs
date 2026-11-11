//! Band 3 (docs/0082 D10): the ranking table mirroring the trend by
//! group. Columns: key · requests · p50 · p90 · max · error rate, then
//! `Cost` and unknown cost when the measure can be cost (Q7, Q11).
//! Ranked by the measure (the procedure's order) and re-sortable by
//! pointer through upstream `perform_sort`; nested `Then by` rows label
//! their dimension ("trigger: poll"); every cell exposes its text. The
//! table is a Tab stop and Tab leaves it (D19).

use std::cmp::Ordering;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{App, Context, SharedString, Window, div, px};
use gpui_component::ActiveTheme as _;
use gpui_component::h_flex;
use gpui_component::table::{Column, ColumnSort, TableDelegate, TableState};

use super::format;
use crate::usage_analytics::Dimension;
use crate::usage_analytics::surfaces::{Measure, RequestGroup, RequestTrend};

/// The table shows this many rows before it scrolls (D20's cap).
pub(super) const MAX_VISIBLE_ROWS: usize = 12;

/// The key column never narrows below this; past it, it takes what the
/// figure columns leave so the table spans the band (docs/0082 D10).
const KEY_MIN_W: f32 = 240.;
/// The table's border plus its vertical scrollbar's gutter.
const TABLE_CHROME_W: f32 = 18.;

fn fixed_width(col: Col) -> f32 {
    match col {
        Col::Unknown => 120.,
        Col::ErrorRate => 104.,
        _ => 92.,
    }
}

/// The key column's width in a band `content_w` wide.
fn key_width(content_w: f32, cost_allowed: bool) -> f32 {
    let figures: f32 = columns(cost_allowed)
        .iter()
        .filter(|c| **c != Col::Key)
        .map(|c| fixed_width(*c))
        .sum();
    (content_w - figures - TABLE_CHROME_W).max(KEY_MIN_W)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Key,
    Requests,
    P50,
    P90,
    Max,
    ErrorRate,
    Cost,
    Unknown,
}

const COLS: [Col; 8] = [
    Col::Key,
    Col::Requests,
    Col::P50,
    Col::P90,
    Col::Max,
    Col::ErrorRate,
    Col::Cost,
    Col::Unknown,
];

impl Col {
    fn name(self, group_by: Dimension) -> &'static str {
        match self {
            Col::Key => group_by.label(),
            Col::Requests => "Requests",
            Col::P50 => "p50",
            Col::P90 => "p90",
            Col::Max => "Max",
            Col::ErrorRate => "Error rate",
            Col::Cost => "Cost",
            Col::Unknown => "Unknown cost",
        }
    }

    fn cmp(self, a: &RequestGroup, b: &RequestGroup) -> Ordering {
        match self {
            Col::Key => a.key.cmp(&b.key),
            Col::Requests => a.requests.cmp(&b.requests),
            Col::P50 => a.p50_ms.cmp(&b.p50_ms),
            Col::P90 => a.p90_ms.cmp(&b.p90_ms),
            Col::Max => a.max_ms.cmp(&b.max_ms),
            Col::ErrorRate => a.error_rate_pct.total_cmp(&b.error_rate_pct),
            Col::Cost => a.cost.cmp(&b.cost),
            Col::Unknown => a.unknown.cmp(&b.unknown),
        }
    }
}

/// The column the procedure ranks by: the measure's (by count when
/// cost is refused, as `request_trend` ranks).
fn rank_col(m: Measure, cost_allowed: bool) -> Col {
    match m {
        Measure::Count => Col::Requests,
        Measure::Cost if cost_allowed => Col::Cost,
        Measure::Cost => Col::Requests,
        Measure::P50 => Col::P50,
        Measure::P90 => Col::P90,
        Measure::Max => Col::Max,
        Measure::ErrorRate => Col::ErrorRate,
    }
}

/// The columns a trend shows: cost only where cost is allowed (Q7).
fn columns(cost_allowed: bool) -> &'static [Col] {
    if cost_allowed { &COLS } else { &COLS[..6] }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct FlatRow {
    pub label: String,
    pub nested: bool,
    pub group: RequestGroup,
}

/// Every group, each followed by all its members, both levels in the
/// procedure's order unless `sort` re-orders them.
fn flatten(
    groups: &[RequestGroup],
    then_by: Option<Dimension>,
    sort: Option<(Col, ColumnSort)>,
) -> Vec<FlatRow> {
    let order = |rows: &mut Vec<&RequestGroup>| {
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
    let mut top: Vec<&RequestGroup> = groups.iter().collect();
    order(&mut top);
    let dim = then_by.map_or(String::new(), |d| d.label().to_lowercase());
    let mut out = Vec::new();
    for g in top {
        out.push(FlatRow {
            label: g.key.clone(),
            nested: false,
            group: RequestGroup {
                children: Vec::new(),
                ..g.clone()
            },
        });
        let mut kids: Vec<&RequestGroup> = g.children.iter().collect();
        order(&mut kids);
        out.extend(kids.into_iter().map(|c| FlatRow {
            label: format!("{dim}: {}", c.key),
            nested: true,
            group: c.clone(),
        }));
    }
    out
}

fn cell_text(r: &FlatRow, col: Col) -> String {
    let g = &r.group;
    match col {
        Col::Key => r.label.clone(),
        Col::Requests => format::count(g.requests),
        Col::P50 => format::duration(g.p50_ms.into()),
        Col::P90 => format::duration(g.p90_ms.into()),
        Col::Max => format::duration(g.max_ms.into()),
        Col::ErrorRate => error_rate(g.error_rate_pct),
        Col::Cost => g.cost.map_or_else(|| "—".into(), format::count),
        Col::Unknown => format::count(g.unknown),
    }
}

/// `0%`, `<0.1%`, `2.5%`.
pub(super) fn error_rate(pct: f64) -> String {
    if pct <= 0.0 {
        "0%".into()
    } else if pct < 0.1 {
        "<0.1%".into()
    } else {
        format!("{pct:.1}%")
    }
}

/// The ranking's delegate over one derived [`RequestTrend`].
pub(super) struct RankingTable {
    group_by: Dimension,
    then_by: Option<Dimension>,
    cost_allowed: bool,
    source: Option<Arc<RequestTrend>>,
    sort: Option<(Col, ColumnSort)>,
    rows: Vec<FlatRow>,
    /// The key column's width: what the others leave of the band.
    key_w: f32,
}

impl RankingTable {
    pub(super) fn empty() -> Self {
        RankingTable {
            group_by: Dimension::Operation,
            then_by: None,
            cost_allowed: false,
            source: None,
            sort: None,
            rows: Vec::new(),
            key_w: KEY_MIN_W,
        }
    }

    /// Fit the key column to `content_w`; true when it changed and the
    /// caller must refresh the table so the header re-reads it.
    pub(super) fn fit(&mut self, content_w: f32) -> bool {
        let w = key_width(content_w, self.cost_allowed);
        let changed = (w - self.key_w).abs() >= 1.;
        if changed {
            self.key_w = w;
        }
        changed
    }

    pub(super) fn shows(&self, data: &Arc<RequestTrend>) -> bool {
        self.source.as_ref().is_some_and(|s| Arc::ptr_eq(s, data))
    }

    /// Point the table at a new derivation; the caller refreshes the
    /// table so the header reads the new columns.
    pub(super) fn set(
        &mut self,
        data: Arc<RequestTrend>,
        group_by: Dimension,
        then_by: Option<Dimension>,
    ) {
        let reranked = self
            .source
            .as_ref()
            .is_none_or(|s| s.measure != data.measure);
        if reranked || self.group_by != group_by || self.cost_allowed != data.cost_allowed {
            // A new measure, key column or column set: back to the
            // measure's rank.
            self.sort = None;
        }
        self.group_by = group_by;
        self.then_by = then_by;
        self.cost_allowed = data.cost_allowed;
        self.rows = flatten(&data.groups, then_by, self.sort);
        self.source = Some(data);
    }

    pub(super) fn len(&self) -> usize {
        self.rows.len()
    }

    fn col(&self, ix: usize) -> Option<Col> {
        columns(self.cost_allowed).get(ix).copied()
    }

    /// A column's header state. Unsorted, the procedure's order shows
    /// as the measure's column descending.
    fn sort_of(&self, col: Col) -> ColumnSort {
        let ranked = self
            .source
            .as_ref()
            .map(|s| rank_col(s.measure, s.cost_allowed));
        match self.sort {
            Some((c, dir)) if c == col => dir,
            None if ranked == Some(col) => ColumnSort::Descending,
            _ => ColumnSort::Default,
        }
    }
}

impl TableDelegate for RankingTable {
    fn columns_count(&self, _: &App) -> usize {
        columns(self.cost_allowed).len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let col = self.col(col_ix).unwrap_or(Col::Key);
        let key: SharedString = format!("{col:?}").to_lowercase().into();
        let c = Column::new(key, col.name(self.group_by)).sort(self.sort_of(col));
        match col {
            Col::Key => c.width(px(self.key_w)),
            _ => c.text_right().width(px(fixed_width(col))),
        }
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
        let Some(col) = self.col(col_ix) else { return };
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
        let (Some(r), Some(col)) = (self.rows.get(row_ix), self.col(col_ix)) else {
            return div().into_any_element();
        };
        let muted = cx.theme().muted_foreground;
        let zero = match col {
            Col::ErrorRate => r.group.error_rate_pct <= 0.0,
            Col::Unknown => r.group.unknown == 0,
            _ => false,
        };
        h_flex()
            .size_full()
            .items_center()
            .when(col != Col::Key, |c| c.justify_end())
            .when(col == Col::Key && r.nested, |c| c.pl_4().text_color(muted))
            // Zero counts recede, so the non-zero ones stand out.
            .when(zero, |c| c.text_color(muted))
            .child(cell_text(r, col))
            .into_any_element()
    }

    fn cell_label(&self, row_ix: usize, col_ix: usize, _: &App) -> Option<SharedString> {
        Some(cell_text(self.rows.get(row_ix)?, self.col(col_ix)?).into())
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, cx: &App) -> String {
        self.cell_label(row_ix, col_ix, cx)
            .map(|s| s.to_string())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(key: &str, requests: u64, p90: u32, children: Vec<RequestGroup>) -> RequestGroup {
        RequestGroup {
            key: key.into(),
            requests,
            cost: None,
            unknown: 0,
            p50_ms: p90 / 2,
            p90_ms: p90,
            max_ms: p90 * 2,
            error_rate_pct: 0.0,
            children,
        }
    }

    fn groups() -> Vec<RequestGroup> {
        vec![
            group(
                "pr-detail",
                5,
                1_800,
                vec![
                    group("view-shown", 3, 1_900, vec![]),
                    group("poll", 2, 400, vec![]),
                ],
            ),
            group(
                "notifications",
                40,
                300,
                vec![group("poll", 40, 300, vec![])],
            ),
        ]
    }

    #[test]
    fn nested_rows_follow_their_group_and_name_their_dimension() {
        let flat = flatten(&groups(), Some(Dimension::Trigger), None);
        let labels: Vec<_> = flat.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "pr-detail",
                "trigger: view-shown",
                "trigger: poll",
                "notifications",
                "trigger: poll"
            ]
        );
        let by_requests = flatten(
            &groups(),
            Some(Dimension::Trigger),
            Some((Col::Requests, ColumnSort::Descending)),
        );
        assert_eq!(by_requests[0].label, "notifications");
        assert_eq!(by_requests[2].label, "pr-detail");
    }

    #[test]
    fn cells_expose_the_text_they_draw_and_cost_needs_a_quota_transport() {
        let flat = flatten(&groups(), None, None);
        let texts: Vec<_> = columns(false)
            .iter()
            .map(|c| cell_text(&flat[0], *c))
            .collect();
        assert_eq!(texts, ["pr-detail", "5", "900 ms", "1.8 s", "3.6 s", "0%"]);
        assert_eq!(columns(true).len(), 8);
        assert_eq!(Col::Cost.name(Dimension::Operation), "Cost", "Q11");
        assert_eq!(rank_col(Measure::P90, false), Col::P90);
        assert_eq!(rank_col(Measure::Cost, false), Col::Requests);
        assert_eq!(rank_col(Measure::Cost, true), Col::Cost);
        assert_eq!(error_rate(0.04), "<0.1%");
        assert_eq!(error_rate(2.54), "2.5%");
    }

    #[test]
    fn the_key_column_takes_what_the_figures_leave() {
        // 4 × 92 + 104 + chrome 18 = 490 beside the key.
        assert_eq!(key_width(955., false), 465.);
        // Cost and Unknown cost take 212 more.
        assert_eq!(key_width(955., true), 253.);
        assert_eq!(key_width(600., true), KEY_MIN_W, "never below its floor");
    }
}
