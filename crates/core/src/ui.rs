//! Shared page-scaffold helpers — one owner for the canvas contract and
//! the type tiers pages compose from (#487, #488).

use gpui::InteractiveElement as _;
use gpui::ParentElement as _;
use gpui::Styled as _;
use gpui::{App, Div, FontWeight, SharedString, div};
use gpui_component::{ActiveTheme as _, v_flex};

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

/// How a card is framed. One construction for the app's box language:
/// `border_1` + the `border` token, the `radius_lg` semantic tier, the
/// page background, and an owned `p_3` inset (docs/0002 D14).
///
/// The line against [`gpui_component::group_box::GroupBox`]: a **titled**
/// container is a GroupBox (PR-page boxes); an **untitled** tile is a
/// card. A tinted/filled variant is deliberately absent until a surface
/// needs one — the page background is the only fill so cards also work
/// over the about overlay's backdrop.
pub enum CardVariant {
    /// Static tile: framed, no affordances (stat tiles, dialogs).
    Quiet,
    /// Clickable tile: [`CardVariant::Quiet`] plus the one hover wash
    /// (`theme.secondary`) and the pointer cursor. The caller keeps the
    /// `id`, role/name, and `on_click` — semantics stay at the surface.
    Interactive,
}

/// The card tile (docs/0002 D14). Chain sizing, `id`, a11y, and content
/// on top; do not re-add frame, radius, fill, padding, hover, or cursor —
/// that is the drift this replaces (#489, #491's card half).
pub fn card(cx: &App, variant: CardVariant) -> Div {
    let theme = cx.theme();
    let base = v_flex()
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius_lg)
        .bg(theme.background)
        .p_3();
    match variant {
        CardVariant::Quiet => base,
        CardVariant::Interactive => {
            let wash = theme.secondary;
            base.hover(move |this| this.bg(wash)).cursor_pointer()
        }
    }
}

/// The empty state (docs/0002 D15): centered muted text with breathing
/// room — "nothing here" is not an error and not a skeleton. Chain
/// sizing (`.flex_1().min_h_0()` fills a tab body), `.id(...)` for a
/// probe handle, and `.into_any_element()` where the container wants it.
pub fn empty_state(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .w_full()
        .py_4()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}
