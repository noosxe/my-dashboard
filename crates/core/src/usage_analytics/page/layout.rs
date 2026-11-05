//! The page frame (docs/0082 D3, D20): a fixed header over one page
//! scroll, both capped at ~1200 px and centred. Views size to their
//! content inside the scroll; none scrolls on its own.

use gpui::prelude::*;
use gpui::{AnyElement, App, ScrollHandle, Stateful, div, px};
use gpui_component::{ActiveTheme, v_flex};

use crate::ui::page_canvas;

/// The content cap (D20): wide windows don't stretch the charts.
pub(super) const CONTENT_MAX_W: f32 = 1200.0;

/// The page: `header` fixed, `body` in the one scroll, tracked by
/// `scroll` so the request log can fill its viewport (D10, Q8).
pub(super) fn frame(
    header: impl IntoElement,
    body: AnyElement,
    scroll: &ScrollHandle,
    cx: &App,
) -> Stateful<gpui::Div> {
    v_flex()
        .id("usage-page")
        .size_full()
        .min_w_0()
        .child(
            div()
                .id("usage-header")
                .flex_none()
                .w_full()
                .border_b_1()
                .border_color(cx.theme().border)
                .px_4()
                .py_3()
                .child(capped(v_flex().gap_2().child(header))),
        )
        .child(
            div()
                .id("usage-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(scroll)
                .child(capped(page_canvas()).child(body)),
        )
}

fn capped(el: gpui::Div) -> gpui::Div {
    el.w_full().max_w(px(CONTENT_MAX_W)).mx_auto()
}
