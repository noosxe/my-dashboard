//! Home page (docs/0003 §4.4): responsive grid of module overview cards.
//!
//! Responsiveness without `min_w` (gpui has none): `flex_wrap` + fixed-width
//! cards — columns adapt to whatever width the tiling WM gives the window.

use gpui::{prelude::FluentBuilder as _, *};

use gpui_component::{ActiveTheme, Icon, h_flex};

use crate::module::{ModuleEntry, ModuleIcon};
use crate::nav::{Navigator, Page};
use crate::registry::ModuleRegistry;
use crate::ui::{CardVariant, card};

/// Card frame (Q2): fixed width + wrap; height keeps the grid regular.
pub const CARD_SIZE: (f32, f32) = (320., 200.);

pub fn render(
    registry: &ModuleRegistry,
    nav: &Entity<Navigator>,
    cx: &mut App,
) -> impl IntoElement {
    let cards: Vec<_> = registry
        .entries()
        .iter()
        .map(|entry| card_tile(entry, nav.clone(), cx))
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

fn card_tile(entry: &ModuleEntry, nav: Entity<Navigator>, cx: &App) -> impl IntoElement {
    let id = entry.meta.id.clone();
    let title = entry.meta.title.clone();
    let icon = entry.meta.icon.as_ref().map(|i| match i {
        ModuleIcon::BuiltIn(name) => name.clone().into(),
        ModuleIcon::Custom(path) => Icon::empty().path(path.clone()),
    });
    let overview = entry.overview.clone();

    card(cx, CardVariant::Interactive)
        .id(ElementId::Name(id.clone()))
        .w(px(CARD_SIZE.0))
        .h(px(CARD_SIZE.1))
        .flex_shrink_0()
        .gap_2()
        .overflow_hidden()
        // docs/0049 D1: module cards are list items that open the
        // module — id + role + explicit name.
        .role(accesskit::Role::ListItem)
        .aria_label(format!("Open {title} module"))
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .flex_shrink_0()
                .when_some(icon, |this, icon| this.child(Icon::new(icon)))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(title),
                ),
        )
        .child(div().flex_1().min_h_0().child(overview))
        .on_click(move |_, _window, cx| {
            nav.update(cx, |nav, cx| nav.go(Page::Module(id.clone()), cx));
        })
}
