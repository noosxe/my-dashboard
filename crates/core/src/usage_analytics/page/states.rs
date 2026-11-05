//! The page-level states (docs/0082 D18): loading while the background
//! read is in flight (D11 gate), the page-level Empty when the store
//! holds nothing, the degraded-store chip, and the placeholder the
//! views show until #697 and #698 fill them.

use gpui::prelude::*;
use gpui::{App, div, px};
use gpui_component::{ActiveTheme, Icon, Sizable as _, h_flex, skeleton::Skeleton, v_flex};

use super::UsageView;
use crate::ui::empty_state;

/// The loading skeleton's fixed geometry (docs/0002 D10: canvas sizes
/// are exempt from the rem scale and therefore named).
const LOADING_ROW_H: f32 = 28.0; // the query-bar row
const LOADING_BAR_W: f32 = 320.0; // ditto, its width
const LOADING_CARD_H: f32 = 120.0; // the card row
const LOADING_CHART_H: f32 = 220.0; // the chart block

/// D18's degraded-store words (0057 D6, 0063 F4).
pub(super) const DEGRADED_COPY: &str =
    "History paused: storage unavailable, showing this session only";

/// The views' body until their surfaces land (#697 Budget, #698
/// Requests): a neutral D18-style Empty, not a promise.
pub(super) const REBUILDING_COPY: &str = "This view is being rebuilt.";

/// The page-level Empty (D18): the store holds no records at all, so
/// "widen the range" would be wrong advice. Worded per view.
pub(super) fn page_empty_copy(view: UsageView) -> &'static str {
    match view {
        UsageView::Budget => {
            "No GitHub responses yet. Open a GitHub page; spend, history and avoidable cost appear after the first response."
        }
        UsageView::Requests => {
            "No requests recorded yet. Open a GitHub page; latency, errors and the request log appear after the first request."
        }
    }
}

/// D18 loading: skeleton blocks in the view body's shape.
pub(super) fn loading() -> impl IntoElement {
    v_flex()
        .id("usage-loading")
        .role(gpui::accesskit::Role::ProgressIndicator)
        .aria_label("Reading usage history")
        .gap_3()
        .child(Skeleton::new().h(px(LOADING_ROW_H)).w(px(LOADING_BAR_W)))
        .child(
            h_flex()
                .gap_3()
                .child(Skeleton::new().flex_1().h(px(LOADING_CARD_H)))
                .child(Skeleton::new().flex_1().h(px(LOADING_CARD_H))),
        )
        .child(Skeleton::new().w_full().h(px(LOADING_CHART_H)))
}

/// D18 Empty: one page-level message, no query bar.
pub(super) fn page_empty(view: UsageView, cx: &App) -> impl IntoElement {
    let copy = page_empty_copy(view);
    empty_state(copy, cx)
        .id("usage-empty")
        .role(gpui::accesskit::Role::Label)
        .aria_label(copy)
}

/// A view's placeholder body, under the view's own id.
pub(super) fn rebuilding(id: &'static str, cx: &App) -> impl IntoElement {
    empty_state(REBUILDING_COPY, cx)
        .id(id)
        .role(gpui::accesskit::Role::Label)
        .aria_label(REBUILDING_COPY)
}

/// The degraded chip on the honesty line (D18/D5): the icon carries the
/// state colour, the words stay in `foreground`.
pub(super) fn degraded_chip(cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    h_flex()
        .gap_1()
        .items_center()
        .text_color(theme.foreground)
        .child(
            Icon::empty()
                .path("icons/database.svg")
                .small()
                .text_color(theme.warning),
        )
        .child(div().child(DEGRADED_COPY))
}
