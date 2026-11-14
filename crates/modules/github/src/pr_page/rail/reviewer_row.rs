//! One reviewer row (docs/0083 D8): avatar, name, and the review state
//! right-aligned as icon + word, with the tone on the icon only.

use gpui::{
    AnyElement, Hsla, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable as _, avatar::Avatar, h_flex, tooltip::Tooltip,
};

use super::metrics::ROW_H;
use super::reviewers::{ReviewState, ReviewerRow};

/// The vendored octicon `comment-16`, served by the app's asset source
/// like the pencil.
const COMMENT: &str = "icons/comment.svg";

/// The state's glyph and its tone (muted unless the state decides).
fn glyph(state: ReviewState, cx: &gpui::App) -> (Icon, Hsla) {
    let theme = cx.theme();
    match state {
        ReviewState::Approved => (Icon::new(IconName::CircleCheck), theme.success),
        ReviewState::Changes => (Icon::new(IconName::CircleX), theme.danger),
        ReviewState::Commented => (Icon::empty().path(COMMENT), theme.muted_foreground),
        ReviewState::Pending => (Icon::new(IconName::Dash), theme.muted_foreground),
        ReviewState::Dismissed => (Icon::new(IconName::Minus), theme.muted_foreground),
    }
}

/// Teams get a square avatar and the placeholder when they have no
/// image; people and bots keep the round one.
fn avatar(row: &ReviewerRow) -> Avatar {
    let base = match &row.avatar_url {
        Some(url) => Avatar::new().src(url.clone()),
        None => Avatar::new().name(row.name.clone()),
    };
    if row.kind.is_team() {
        base.rounded(px(4.0))
    } else {
        base
    }
}

pub(super) fn reviewer_row(key: String, row: &ReviewerRow, cx: &gpui::App) -> AnyElement {
    let tip = row.name.clone();
    let spoken = format!("{}, {}", row.name, row.state.spoken());
    let (icon, tone) = glyph(row.state, cx);
    h_flex()
        .id(gpui::SharedString::from(key.clone()))
        .debug_selector(move || key.clone())
        .role(gpui::accesskit::Role::Label)
        .aria_label(spoken)
        .h(px(ROW_H))
        .gap_2()
        .items_center()
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .child(avatar(row).xsmall())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_sm()
                .child(row.name.clone()),
        )
        .child(
            h_flex()
                .flex_shrink_0()
                .gap_1()
                .items_center()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(icon.xsmall().text_color(tone))
                .child(row.state.word()),
        )
        .into_any_element()
}
