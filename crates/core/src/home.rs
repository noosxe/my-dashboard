//! Home page (docs/0003 §4.4): responsive grid of module overview cards.
//!
//! Responsiveness without `min_w` (gpui has none): `flex_wrap` + fixed-width
//! cards — columns adapt to whatever width the tiling WM gives the window.

use gpui::{prelude::FluentBuilder as _, *};

use gpui_component::{ActiveTheme, Icon, h_flex, v_flex};

use crate::module::{ModuleEntry, ModuleIcon};
use crate::nav::{Navigator, Page};
use crate::registry::ModuleRegistry;

/// Card frame (Q2): fixed width + wrap; height keeps the grid regular.
pub const CARD_SIZE: (f32, f32) = (320., 200.);

pub fn render(
    registry: &ModuleRegistry,
    nav: &Entity<Navigator>,
    cx: &mut App,
) -> impl IntoElement {
    // Snapshot theme colors first (Copy) so the card builder needs no `cx`
    // — a `FnMut` map closure can't hand out borrowed `&mut App`.
    let theme = cx.theme();
    let colors = CardColors {
        border: theme.border,
        muted: theme.muted_foreground,
        hover_bg: theme.secondary,
        bg: theme.background,
    };

    let cards: Vec<_> = registry
        .entries()
        .iter()
        .map(|entry| card(entry, nav.clone(), colors))
        .collect();

    h_flex()
        .id("home-grid")
        .flex_wrap()
        // h_flex() defaults to items_center — cards must start at the top.
        .items_start()
        // Multi-line wrapping: lines themselves are placed by align-content
        // (default Stretch distributes leftover space) — pack them from top.
        .content_start()
        .gap_4()
        .p_4()
        .flex_1()
        .min_h_0()
        // Scroll (not push) when wrapped cards exceed the viewport.
        .overflow_y_scroll()
        .children(cards)
}

#[derive(Clone, Copy)]
struct CardColors {
    border: Hsla,
    muted: Hsla,
    hover_bg: Hsla,
    bg: Hsla,
}

fn card(entry: &ModuleEntry, nav: Entity<Navigator>, colors: CardColors) -> impl IntoElement {
    let id = entry.meta.id.clone();
    let title = entry.meta.title.clone();
    let icon = entry.meta.icon.as_ref().map(|i| match i {
        ModuleIcon::BuiltIn(name) => name.clone().into(),
        ModuleIcon::Custom(path) => Icon::empty().path(path.clone()),
    });
    let overview = entry.overview.clone();

    v_flex()
        .id(ElementId::Name(id.clone()))
        .w(px(CARD_SIZE.0))
        .h(px(CARD_SIZE.1))
        .flex_shrink_0()
        .border_1()
        .border_color(colors.border)
        .rounded_lg()
        .bg(colors.bg)
        .p_3()
        .gap_2()
        .overflow_hidden()
        .hover(|this| this.bg(colors.hover_bg))
        .cursor_pointer()
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .flex_shrink_0()
                .when_some(icon, |this, icon| this.child(Icon::new(icon)))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(colors.muted)
                        .child(title),
                ),
        )
        .child(div().flex_1().min_h_0().child(overview))
        .on_click(move |_, _window, cx| {
            nav.update(cx, |nav, cx| nav.go(Page::Module(id.clone()), cx));
        })
}
