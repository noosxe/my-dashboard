//! Band 5 (docs/0082 D10, D20, Q8): the request log, always last. It
//! fills the page scroll's viewport: page scroll brings it to the top,
//! then the log scrolls its own (virtualized) rows, and nothing sits
//! below it. It replaces the old top-20 "slowest flights" list: sorting
//! by Duration gives that view.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Pixels, div, px};
use gpui_component::table::DataTable;
use gpui_component::{ActiveTheme, Sizable as _};

use super::format;
use super::query::Controls;
use crate::usage_analytics::surfaces::{LogSort, LogSortKey, RequestLog};

/// What the band keeps above the table inside the viewport: the title
/// row, the gap, and the page canvas's inset (docs/0002 D10).
const CHROME_H: f32 = 72.0;
/// The log's height before the page scroll has been laid out once, and
/// its floor in a very short window.
const FALLBACK_H: f32 = 480.0;
const MIN_H: f32 = 240.0;

/// The table's height in a page-scroll viewport `viewport_h` tall.
pub(super) fn table_height(viewport_h: f32) -> f32 {
    if viewport_h <= 0.0 {
        FALLBACK_H
    } else {
        (viewport_h - CHROME_H).max(MIN_H)
    }
}

/// The header's meta line: the count and the order, with the hint to
/// the old "slowest requests" view.
pub(super) fn meta(log: &RequestLog) -> String {
    let order = match log.sort {
        s if s == LogSort::default() => "newest first · sort by Duration for the slowest".into(),
        LogSort {
            key: LogSortKey::Duration,
            descending: true,
        } => "slowest first".into(),
        LogSort { key, descending } => format!(
            "by {} {}",
            column_word(key),
            if descending {
                "descending"
            } else {
                "ascending"
            }
        ),
    };
    format!("{} · {order}", format::requests(log.rows.len() as u64))
}

fn column_word(k: LogSortKey) -> &'static str {
    match k {
        LogSortKey::Time => "time",
        LogSortKey::Operation => "operation",
        LogSortKey::Target => "target",
        LogSortKey::Surface => "surface",
        LogSortKey::Trigger => "trigger",
        LogSortKey::Status => "status",
        LogSortKey::Cost => "cost",
        LogSortKey::Duration => "duration",
        LogSortKey::Queue => "queue",
        LogSortKey::Size => "size",
    }
}

/// Point the table at `log` if it isn't already (a new derivation
/// landed), back at its first row.
pub(super) fn sync_table(
    c: &Controls,
    log: &Arc<RequestLog>,
    as_of_ms: u64,
    tz_offset_ms: i64,
    content_w: f32,
    cx: &mut App,
) {
    let fresh = !c.log.read(cx).delegate().shows(log);
    c.log.update(cx, |t, cx| {
        if fresh {
            t.delegate_mut().set(log.clone(), as_of_ms, tz_offset_ms);
        }
        // A new log starts at its first row; a resize keeps the place.
        if t.delegate_mut().fit(content_w) || fresh {
            t.refresh(cx);
        }
        if fresh {
            t.scroll_to_row(0, cx);
        }
    });
}

pub(super) fn meta_line(text: String, cx: &App) -> impl IntoElement {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// The table at the viewport's height.
pub(super) fn table(c: &Controls, viewport_h: Pixels) -> AnyElement {
    div()
        .id("usage-log-table")
        .w_full()
        .h(px(table_height(viewport_h.as_f32())))
        .child(DataTable::new(&c.log).small())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_analytics::surfaces::Bounds;

    fn log(sort: LogSort) -> RequestLog {
        RequestLog {
            bounds: Bounds {
                from_ms: 0,
                to_ms: 1,
            },
            sort,
            rows: Vec::new(),
        }
    }

    #[test]
    fn the_meta_line_names_the_order() {
        assert_eq!(
            meta(&log(LogSort::default())),
            "0 requests · newest first · sort by Duration for the slowest"
        );
        let slowest = LogSort {
            key: LogSortKey::Duration,
            descending: true,
        };
        assert_eq!(meta(&log(slowest)), "0 requests · slowest first");
        let by_op = LogSort {
            key: LogSortKey::Operation,
            descending: false,
        };
        assert_eq!(meta(&log(by_op)), "0 requests · by operation ascending");
    }

    #[test]
    fn the_log_fills_the_viewport_with_a_floor() {
        assert_eq!(table_height(700.0), 628.0);
        assert_eq!(table_height(0.0), FALLBACK_H);
        assert_eq!(table_height(200.0), MIN_H);
    }
}
