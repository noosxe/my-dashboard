//! Shared page-scaffold helpers — one owner for the canvas contract and
//! the type tiers pages compose from (#487, #488).

use gpui::ParentElement as _;
use gpui::Styled as _;
use gpui::{Div, FontWeight, SharedString, div};
use gpui_component::v_flex;

/// The page canvas: the single owner of the content inset (`p_4`) and the
/// stack rhythm (`gap_3`). Page roots start here instead of repeating the
/// `v_flex().p_4().gap_3()` recipe — one place to tune the inset for the
/// whole app, and no second padding layer stacked on the shell's framed
/// panel.
///
/// Top-level page roots add their own sizing and scroll (`.size_full()`,
/// `.overflow_y_scroll()`): layout varies per page, the inset does not.
/// The home module grid (`home.rs`) is the one deliberate exception — a
/// wrapping card grid, not a stack, so it keeps its own `p_4().gap_4()`.
pub fn page_canvas() -> Div {
    v_flex().min_w_0().p_4().gap_3()
}

/// The page-title tier: `text_lg` + semibold. The single level for page
/// identity titles — issue/PR/notification subject titles, creator and
/// generator headers, module landing headers. Chain `.id(...)`,
/// `.text_color(...)`, and behavior on top for title-styled links and
/// controls.
pub fn page_title(text: impl Into<SharedString>) -> Div {
    div()
        .text_lg()
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

/// The section-header tier: `text_sm` + semibold — box and block headings.
/// Full foreground color; chain `.text_color(...)` for status-tinted
/// headers. List-item titles stay regular-weight/MEDIUM: item emphasis is
/// a different job from a heading.
pub fn section_header(text: impl Into<SharedString>) -> Div {
    div()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}
