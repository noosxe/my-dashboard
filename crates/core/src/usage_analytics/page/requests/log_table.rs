//! The request log's table (docs/0082 D10 band 5, D19): every request
//! in the selection, one row each. Columns: time · op · target ·
//! surface · trigger · status · cost · duration · queue · size.
//!
//! Upstream's `DataTable` draws its rows through `uniform_list`, so only
//! the rows on screen are built and formatted: a 90-day log of ~10⁵
//! rows costs nothing per frame beyond the visible ones. Sorting is the
//! calculator's (`request_log`, ties newest first): a header click hands
//! the new [`LogSort`] to the page, which re-derives the log off the UI
//! thread (D11) and points this table at the result. The table is not a
//! Tab stop until #711 is fixed (D19); every cell exposes its text.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{App, Context, SharedString, WeakEntity, Window, div, px};
use gpui_component::ActiveTheme as _;
use gpui_component::h_flex;
use gpui_component::table::{Column, ColumnSort, TableDelegate, TableState};

use super::format;
use crate::usage_analytics::page::UsagePage;
use crate::usage_analytics::surfaces::{
    CostCell, LogRow, LogSort, LogSortKey, RequestLog, StatusClass,
};

/// The columns in order, each with the calculator's sort key.
const COLS: [(LogSortKey, &str, f32); 10] = [
    (LogSortKey::Time, "Time", 136.),
    (LogSortKey::Operation, "Operation", 150.),
    (LogSortKey::Target, "Target", 170.),
    (LogSortKey::Surface, "Surface", 112.),
    (LogSortKey::Trigger, "Trigger", 104.),
    (LogSortKey::Status, "Status", 150.),
    (LogSortKey::Cost, "Cost", 104.),
    (LogSortKey::Duration, "Duration", 92.),
    (LogSortKey::Queue, "Queue", 80.),
    (LogSortKey::Size, "Size", 72.),
];

/// The sort a header click asks for: upstream cycles a column Default →
/// Descending → Ascending → Default, and Default is newest first.
pub(super) fn sort_for(col_ix: usize, sort: ColumnSort) -> LogSort {
    match (COLS.get(col_ix), sort) {
        (Some((key, ..)), ColumnSort::Descending) => LogSort {
            key: *key,
            descending: true,
        },
        (Some((key, ..)), ColumnSort::Ascending) => LogSort {
            key: *key,
            descending: false,
        },
        _ => LogSort::default(),
    }
}

/// One cell's text, as drawn and as read (D19).
pub(super) fn cell_text(r: &LogRow, col_ix: usize, as_of_ms: u64, tz_offset_ms: i64) -> String {
    let ms = |v: Option<u32>| v.map_or_else(|| "—".into(), |v| format::duration(v.into()));
    match COLS.get(col_ix).map(|c| c.0) {
        Some(LogSortKey::Time) => format::log_time(r.ts_ms, as_of_ms, tz_offset_ms),
        Some(LogSortKey::Operation) => r.op.clone(),
        Some(LogSortKey::Target) => r.target.clone().unwrap_or_else(|| "—".into()),
        Some(LogSortKey::Surface) => r.surface.clone(),
        Some(LogSortKey::Trigger) => r.trigger.clone(),
        Some(LogSortKey::Status) => format::status(r.status, r.status_class),
        Some(LogSortKey::Cost) => format::cost_cell(r.cost),
        Some(LogSortKey::Duration) => format::duration(r.duration_ms.into()),
        Some(LogSortKey::Queue) => ms(r.queue_ms),
        Some(LogSortKey::Size) => r
            .resp_bytes
            .map_or_else(|| "—".into(), |b| format::bytes(b.into())),
        None => String::new(),
    }
}

pub(super) struct LogTable {
    page: WeakEntity<UsagePage>,
    source: Option<Arc<RequestLog>>,
    as_of_ms: u64,
    tz_offset_ms: i64,
}

impl LogTable {
    pub(super) fn new(page: WeakEntity<UsagePage>) -> Self {
        LogTable {
            page,
            source: None,
            as_of_ms: 0,
            tz_offset_ms: 0,
        }
    }

    pub(super) fn shows(&self, data: &Arc<RequestLog>) -> bool {
        self.source.as_ref().is_some_and(|s| Arc::ptr_eq(s, data))
    }

    pub(super) fn set(&mut self, data: Arc<RequestLog>, as_of_ms: u64, tz_offset_ms: i64) {
        self.as_of_ms = as_of_ms;
        self.tz_offset_ms = tz_offset_ms;
        self.source = Some(data);
    }

    fn row(&self, ix: usize) -> Option<&LogRow> {
        self.source.as_ref()?.rows.get(ix)
    }

    fn sort(&self) -> LogSort {
        self.source
            .as_ref()
            .map_or_else(LogSort::default, |s| s.sort)
    }
}

impl TableDelegate for LogTable {
    fn columns_count(&self, _: &App) -> usize {
        COLS.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.source.as_ref().map_or(0, |s| s.rows.len())
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let (key, name, width) = COLS.get(col_ix).copied().unwrap_or(COLS[0]);
        let s = self.sort();
        // Newest first is the default: the Time column shows it.
        let sort = match (s.key == key, s.descending) {
            (true, true) => ColumnSort::Descending,
            (true, false) => ColumnSort::Ascending,
            (false, _) => ColumnSort::Default,
        };
        let c = Column::new(SharedString::from(name.to_lowercase()), name)
            .width(px(width))
            .sort(sort);
        let right = matches!(
            key,
            LogSortKey::Cost | LogSortKey::Duration | LogSortKey::Queue | LogSortKey::Size
        );
        if right { c.text_right() } else { c }
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let next = sort_for(col_ix, sort);
        let page = self.page.clone();
        // Outside the table's own update: the page re-derives the log
        // (D11) and points this table at it when it lands.
        cx.defer(move |cx| {
            page.update(cx, |p, cx| p.set_log_sort(next, cx)).ok();
        });
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(r) = self.row(row_ix) else {
            return div().into_any_element();
        };
        let key = COLS.get(col_ix).map(|c| c.0);
        let text = cell_text(r, col_ix, self.as_of_ms, self.tz_offset_ms);
        let muted = cx.theme().muted_foreground;
        // Errors keep `foreground` (Q1: colour is never the only signal,
        // and the reason is spelled out); dashes, 304s and free or
        // unpriced cost recede.
        let recede = match key {
            Some(LogSortKey::Target) => true,
            Some(LogSortKey::Status) => r.status_class == StatusClass::NotModified,
            Some(LogSortKey::Cost) => {
                !matches!(r.cost, CostCell::Requests(1..) | CostCell::Points(1..))
            }
            _ => text == "—",
        };
        h_flex()
            .size_full()
            .items_center()
            .overflow_hidden()
            .when(
                matches!(
                    key,
                    Some(
                        LogSortKey::Cost
                            | LogSortKey::Duration
                            | LogSortKey::Queue
                            | LogSortKey::Size
                    )
                ),
                |c| c.justify_end(),
            )
            .when(recede, |c| c.text_color(muted))
            .child(div().truncate().child(text))
            .into_any_element()
    }

    fn cell_label(&self, row_ix: usize, col_ix: usize, _: &App) -> Option<SharedString> {
        let r = self.row(row_ix)?;
        Some(cell_text(r, col_ix, self.as_of_ms, self.tz_offset_ms).into())
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
    use crate::usage_records::Transport;

    fn row() -> LogRow {
        LogRow {
            seq: 1,
            ts_ms: 1_793_891_490_000,
            module: "github".into(),
            transport: Transport::Graphql,
            op: "repo-pulls".into(),
            target: Some("fixture-user/compass".into()),
            surface: "repo-view".into(),
            trigger: "view-shown".into(),
            status: 200,
            status_class: StatusClass::Success,
            cost: CostCell::Points(11),
            duration_ms: 362,
            queue_ms: None,
            resp_bytes: Some(38_900),
        }
    }

    #[test]
    fn cells_read_as_drawn() {
        let as_of = 1_793_891_520_000;
        let texts: Vec<_> = (0..COLS.len())
            .map(|i| cell_text(&row(), i, as_of, 0))
            .collect();
        assert_eq!(
            texts,
            [
                "15:11:30",
                "repo-pulls",
                "fixture-user/compass",
                "repo-view",
                "view-shown",
                "200 OK",
                "11 points",
                "362 ms",
                "—",
                "38 KB"
            ]
        );
    }

    #[test]
    fn a_header_click_maps_to_the_calculators_sort() {
        assert_eq!(
            sort_for(7, ColumnSort::Descending),
            LogSort {
                key: LogSortKey::Duration,
                descending: true
            }
        );
        assert_eq!(
            sort_for(0, ColumnSort::Ascending),
            LogSort {
                key: LogSortKey::Time,
                descending: false
            }
        );
        assert_eq!(sort_for(3, ColumnSort::Default), LogSort::default());
    }
}
