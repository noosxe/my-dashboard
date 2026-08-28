//! Notes sample module (docs/0003 §5.6).
//!
//! Proves the sub-page path: the main page manages its own `Screen` state and
//! reports breadcrumb crumbs (with back-nav closures) to the `Navigator`.

use std::sync::Arc;

use gpui::*;

use gpui_component::{ActiveTheme, IconName, button::Button, v_flex};

use dashboard_core::Module;
use dashboard_core::module::{ModuleEntry, ModuleMeta};
use dashboard_core::nav::{Crumb, Navigator};

pub struct NotesModule;

impl Module for NotesModule {
    fn meta() -> ModuleMeta {
        ModuleMeta {
            id: "notes".into(),
            title: "Notes".into(),
            icon: Some(IconName::BookOpen),
        }
    }

    fn init(nav: &Entity<Navigator>, _window: &mut Window, cx: &mut App) -> ModuleEntry {
        ModuleEntry {
            meta: Self::meta(),
            overview: cx.new(|_| NotesOverview).into(),
            main_page: cx
                .new(|_| NotesPage {
                    nav: nav.clone(),
                    screen: Screen::Main,
                })
                .into(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Main,
    Details,
}

struct NotesOverview;

impl Render for NotesOverview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().size_full().gap_2().justify_center().child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child("Scratch pad — open for the full notes list."),
        )
    }
}

struct NotesPage {
    nav: Entity<Navigator>,
    screen: Screen,
}

impl NotesPage {
    fn show_main(&mut self, cx: &mut Context<Self>) {
        if self.screen == Screen::Main {
            return;
        }
        self.screen = Screen::Main;
        self.nav.update(cx, |nav, cx| nav.set_crumbs(vec![], cx));
        cx.notify();
    }

    fn show_details(&mut self, cx: &mut Context<Self>) {
        if self.screen == Screen::Details {
            return;
        }
        self.screen = Screen::Details;
        let me = cx.entity();
        self.nav.update(cx, |nav, cx| {
            nav.set_crumbs(
                vec![Crumb {
                    label: "Details".into(),
                    on_click: Some(Arc::new(move |_, cx| {
                        me.update(cx, |page, cx| page.show_main(cx));
                    })),
                }],
                cx,
            );
        });
        cx.notify();
    }
}

impl Render for NotesPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.screen {
            Screen::Main => v_flex()
                .p_4()
                .gap_3()
                .child(div().text_size(px(18.)).child("Notes"))
                .child(div().child("Nothing here yet — this module exists to prove sub-pages."))
                .child(
                    Button::new("open-details")
                        .label("View details")
                        .on_click(cx.listener(|this, _, _, cx| this.show_details(cx))),
                ),
            Screen::Details => v_flex()
                .p_4()
                .gap_3()
                .child(div().text_size(px(18.)).child("Notes — details"))
                .child(div().child(
                    "You navigated into a sub-page: the breadcrumb grew a \
                         Details crumb, and clicking it (or Notes) brings you back.",
                ))
                .child(
                    Button::new("back-to-main")
                        .label("Back")
                        .on_click(cx.listener(|this, _, _, cx| this.show_main(cx))),
                ),
        }
    }
}
