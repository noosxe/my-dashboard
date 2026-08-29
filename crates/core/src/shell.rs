//! App shell (docs/0003 §4.5): 0002's window chrome + sidebar navigation.
//!
//! Layout mirrors `sidebar_story.rs`: `Sidebar` on the left, content column
//! with `SidebarToggleButton` left of the `Breadcrumb`, page below.

use gpui::{prelude::FluentBuilder as _, *};

use gpui_component::{
    ActiveTheme, Icon, IconName, TitleBar,
    breadcrumb::{Breadcrumb, BreadcrumbItem},
    button::{Button, ButtonVariants},
    h_flex,
    sidebar::{Sidebar, SidebarCollapsible, SidebarMenuItem, SidebarToggleButton},
    v_flex,
};

use crate::home;
use crate::nav::{CrumbAction, Navigator, Page};
use crate::registry::ModuleRegistry;
use crate::store::DebouncedSave;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

actions!(dashboard, [ToggleTitleBar]);

/// Default chrome state (0002 D7): titlebar-less on Linux, full bar elsewhere.
#[cfg(target_os = "linux")]
const DEFAULT_TITLEBAR: bool = false;
#[cfg(not(target_os = "linux"))]
const DEFAULT_TITLEBAR: bool = true;

/// Persisted shell UI state (docs/0004 §4.3) → `<state>/shell.json`.
/// `show_titlebar: None` (absent/older file) keeps the OS default from
/// 0002 D7.
#[derive(Serialize, Deserialize, Default)]
pub struct ShellState {
    pub sidebar_collapsed: bool,
    pub show_titlebar: Option<bool>,
}

pub struct DashboardShell {
    registry: ModuleRegistry,
    nav: Entity<Navigator>,
    show_titlebar: bool,
    save: Option<DebouncedSave>,
}

impl DashboardShell {
    /// `state` seeds the toggles (docs/0004 §4.3); `state_path` arms
    /// debounced persistence (`None` disables saving — tests).
    pub fn new(
        registry: ModuleRegistry,
        nav: Entity<Navigator>,
        state: ShellState,
        state_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&nav, |_, _, cx| cx.notify()).detach();
        Self {
            registry,
            nav,
            show_titlebar: state.show_titlebar.unwrap_or(DEFAULT_TITLEBAR),
            save: state_path.map(|p| DebouncedSave::new(p, Duration::from_millis(500))),
        }
    }

    /// Snapshot both toggles and schedule the debounced save.
    fn save_state(&mut self, cx: &Context<Self>) {
        if let Some(save) = &self.save {
            let sidebar_collapsed = self.nav.read(cx).sidebar_collapsed;
            save.schedule(
                ShellState {
                    sidebar_collapsed,
                    show_titlebar: Some(self.show_titlebar),
                },
                cx,
            );
        }
    }

    fn toggle_titlebar(
        &mut self,
        _: &ToggleTitleBar,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_titlebar = !self.show_titlebar;
        self.save_state(cx);
        cx.notify();
    }
}

impl Render for DashboardShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Snapshot nav state (crumbs hold Arc'd closures — cloneable).
        let nav = self.nav.read(cx);
        let page = nav.page.clone();
        let sidebar_collapsed = nav.sidebar_collapsed;
        let crumbs: Vec<(SharedString, Option<CrumbAction>)> = nav
            .crumbs()
            .iter()
            .map(|c| (c.label.clone(), c.on_click.clone()))
            .collect();

        let current_module = match &page {
            Page::Module(id) => self
                .registry
                .get(id)
                .map(|e| (e.meta.id.clone(), e.meta.title.clone())),
            Page::Home => None,
        };

        v_flex()
            .size_full()
            .key_context("Dashboard")
            .on_action(cx.listener(Self::toggle_titlebar))
            .when(self.show_titlebar, |this| {
                this.child(
                    // 0002: custom per-OS adaptive title bar, reserved right slot.
                    TitleBar::new().child(
                        h_flex()
                            .w_full()
                            .pr_2()
                            .justify_between()
                            .child("Dashboard")
                            .child(div()),
                    ),
                )
            })
            .child(
                h_flex()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    // Sidebar (Q4): wordmark header; flat menu — Home first,
                    // then modules (Sidebar children are typed Vec<E>, so a
                    // SidebarGroup would exclude the standalone Home item).
                    .child(
                        Sidebar::new("app-sidebar")
                            .collapsible(SidebarCollapsible::Icon)
                            .collapsed(sidebar_collapsed)
                            .header(
                                // Brand: icon box always; wordmark only when
                                // expanded (in the icon rail the text would clip).
                                // Plain div, not SidebarHeader: that component
                                // hard-codes justify_between + p_2 + a hover
                                // highlight (and its hover can't be overridden —
                                // gpui panics on double hover registration).
                                h_flex()
                                    .items_center()
                                    .gap_2()
                                    .p_2()
                                    .w_full()
                                    .when(sidebar_collapsed, |this| this.p_0())
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded(cx.theme().radius)
                                            .bg(cx.theme().primary)
                                            .text_color(cx.theme().primary_foreground)
                                            .size_8()
                                            .flex_shrink_0()
                                            .child(
                                                Icon::new(IconName::GalleryVerticalEnd).size_4(),
                                            ),
                                    )
                                    .when(!sidebar_collapsed, |this| {
                                        this.child(div().text_size(px(15.)).child("Dashboard"))
                                    }),
                            )
                            .children(nav_menu_items(&self.registry, &page, cx)),
                    )
                    // Content column: chrome row (toggle + breadcrumb) + page.
                    .child(
                        // Content column: mirrors sidebar_story.rs — h_full +
                        // overflow_hidden so nothing below can push the
                        // chrome row around.
                        v_flex()
                            .h_full()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_hidden()
                            .child(
                                // Chrome row: fixed height, pinned to the
                                // top; breadcrumb truncates instead of wrapping.
                                h_flex()
                                    .id("chrome-row")
                                    .h_9()
                                    .items_center()
                                    .gap_2()
                                    .px_2()
                                    .flex_shrink_0()
                                    .border_b_1()
                                    .border_color(cx.theme().border)
                                    .overflow_hidden()
                                    .child(
                                        SidebarToggleButton::new()
                                            .collapsed(sidebar_collapsed)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.nav
                                                    .update(cx, |nav, cx| nav.toggle_sidebar(cx));
                                                this.save_state(cx);
                                            })),
                                    )
                                    .child(
                                        div().min_w_0().flex_1().overflow_hidden().child(
                                            Breadcrumb::new()
                                                .child(BreadcrumbItem::new("Home").on_click(
                                                    cx.listener(|this, _, _, cx| {
                                                        this.nav.update(cx, |nav, cx| {
                                                            nav.go(Page::Home, cx)
                                                        });
                                                    }),
                                                ))
                                                .when_some(current_module.clone(), |this, m| {
                                                    this.child(BreadcrumbItem::new(m.1).on_click(
                                                        cx.listener(move |this, _, _, cx| {
                                                            this.nav.update(cx, |nav, cx| {
                                                                nav.go(
                                                                    Page::Module(m.0.clone()),
                                                                    cx,
                                                                )
                                                            });
                                                        }),
                                                    ))
                                                })
                                                .children(crumbs.iter().map(
                                                    |(label, on_click)| {
                                                        let mut item =
                                                            BreadcrumbItem::new(label.clone());
                                                        if let Some(f) = on_click.clone() {
                                                            item = item.on_click(
                                                                move |_, window, cx| f(window, cx),
                                                            );
                                                        }
                                                        item
                                                    },
                                                )),
                                        ),
                                    ),
                            )
                            .child(
                                // Page host: bounded flex column that fills the
                                // rest and clips (modules scroll internally).
                                v_flex()
                                    .id("page")
                                    .flex_1()
                                    .min_h_0()
                                    .w_full()
                                    .overflow_hidden()
                                    .child(match &page {
                                        Page::Home => home::render(&self.registry, &self.nav, cx)
                                            .into_any_element(),
                                        Page::Module(id) => match self.registry.get(id) {
                                            Some(entry) => {
                                                entry.main_page.clone().into_any_element()
                                            }
                                            None => {
                                                div().child("Unknown module").into_any_element()
                                            }
                                        },
                                    }),
                            ),
                    )
                    // Titlebar-less mode (0002 D7): ghost close, now with the
                    // [DG]-required tooltip (Q7).
                    .when(!self.show_titlebar, |this| {
                        this.child(
                            div().absolute().top_1().right_1().child(
                                Button::new("close")
                                    .ghost()
                                    .icon(IconName::Close)
                                    .tooltip("Close")
                                    .on_click(|_, window, _| window.remove_window()),
                            ),
                        )
                    }),
            )
    }
}

/// Home + module main-page entries as one flat sidebar menu (see the shell
/// render note: `Sidebar` children are `Vec<E>`, so mixing a standalone menu
/// with a labeled group is not possible).
fn nav_menu_items(
    registry: &ModuleRegistry,
    page: &Page,
    cx: &mut Context<DashboardShell>,
) -> Vec<SidebarMenuItem> {
    let home = SidebarMenuItem::new("Home")
        .icon(IconName::LayoutDashboard)
        .active(*page == Page::Home)
        .on_click(cx.listener(|this, _, _, cx| {
            this.nav.update(cx, |nav, cx| nav.go(Page::Home, cx));
        }));

    let modules = registry.entries().iter().map(|entry| {
        let id = entry.meta.id.clone();
        let active = *page == Page::Module(id.clone());
        let mut item = SidebarMenuItem::new(entry.meta.title.clone())
            .active(active)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.nav
                    .update(cx, |nav, cx| nav.go(Page::Module(id.clone()), cx));
            }));
        if let Some(icon) = entry.meta.icon.clone() {
            item = item.icon(icon);
        }
        item
    });

    std::iter::once(home).chain(modules).collect()
}
