//! App shell (docs/0003 §4.5): 0002's window chrome + sidebar navigation.
//!
//! Layout mirrors `sidebar_story.rs`: `Sidebar` on the left, content column
//! with `SidebarToggleButton` left of the `Breadcrumb`, page below.

use gpui::{prelude::FluentBuilder as _, *};

use gpui::Decorations;

use gpui_component::{
    ActiveTheme, Collapsible as _, Icon, IconName, Selectable as _, TitleBar,
    breadcrumb::{Breadcrumb, BreadcrumbItem},
    button::{Button, ButtonVariants as _},
    h_flex,
    setting::Settings,
    sidebar::{Sidebar, SidebarCollapsible, SidebarFooter, SidebarMenuItem, SidebarToggleButton},
    v_flex,
};

use crate::home;
use crate::module::ModuleIcon;
use crate::nav::{CrumbAction, Navigator, Page};
use crate::registry::ModuleRegistry;
use crate::settings as app_settings;
use crate::store::DebouncedSave;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Persisted shell UI state (docs/0004 §4.3) → `<state>/shell.json`.
/// (Chrome itself is no longer persisted — see the render-time
/// decoration logic below; older files with a `show_titlebar` field
/// load fine, serde ignores unknown fields.)
#[derive(Serialize, Deserialize, Default)]
pub struct ShellState {
    pub sidebar_collapsed: bool,
}

pub struct DashboardShell {
    registry: ModuleRegistry,
    nav: Entity<Navigator>,
    /// Grabs keyboard focus at boot so window-level key bindings
    /// (`ctrl-shift-t`) dispatch through this node — gpui bubbles key
    /// actions from the focused node UP, and with nothing focused the
    /// dispatch starts at the tree root, skipping every `on_action`
    /// below it.
    focus_handle: FocusHandle,
    save: Option<DebouncedSave>,
}

impl DashboardShell {
    /// `state` seeds the sidebar toggle (docs/0004 §4.3); `state_path`
    /// arms debounced persistence (`None` disables saving — tests).
    pub fn new(
        registry: ModuleRegistry,
        nav: Entity<Navigator>,
        _state: ShellState,
        state_path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&nav, |_, _, cx| cx.notify()).detach();
        let focus_handle = cx.focus_handle();
        // Boot focus: without a focused node, key actions dispatch from
        // the tree root and never reach this shell's handlers
        // (owner-verified bug on Hyprland — a 0002 keybind was dead
        // from first launch).
        window.focus(&focus_handle, cx);
        Self {
            registry,
            nav,
            focus_handle,
            save: state_path.map(|p| DebouncedSave::new(p, Duration::from_millis(500))),
        }
    }

    /// Snapshot the sidebar toggle and schedule the debounced save.
    fn save_state(&mut self, cx: &Context<Self>) {
        if let Some(save) = &self.save {
            let sidebar_collapsed = self.nav.read(cx).sidebar_collapsed;
            save.schedule(ShellState { sidebar_collapsed }, cx);
        }
    }
}

impl Render for DashboardShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Chrome is decoration-driven (owner decision, 0004 PR #13 —
        // supersedes 0002 D7's runtime toggle): if the window is
        // client-decorated the app owns the whole chrome (full title
        // bar; gpui-component renders min/max/close there); if the
        // server owns decorations we draw nothing — KWin/X11/native
        // macOS bars provide controls, and on decoration-less servers
        // (Hyprland) window management stays keybind territory.
        // Resolved per render: the Wayland decoration answer arrives
        // asynchronously with the first configure, which triggers a
        // redraw, so this tracks the live state with no observer wiring.
        let client_decorated = matches!(window.window_decorations(), Decorations::Client { .. });

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
            Page::Home | Page::Settings => None,
        };

        v_flex()
            .id("dashboard-shell")
            .track_focus(&self.focus_handle)
            .size_full()
            .when(client_decorated, |this| {
                this.child(
                    // Custom title bar; gpui-component draws min/max/
                    // close at the right edge on client-decorated Linux.
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
                            .children(nav_menu_items(&self.registry, &page, cx))
                            // Settings entry point (docs/0006 §4.1): gear in
                            // the sidebar footer, present in expanded and
                            // icon-rail modes; selected state tracks the page.
                            .footer(
                                SidebarFooter::new()
                                    .selected(page == Page::Settings)
                                    .collapsed(sidebar_collapsed)
                                    .child(
                                        h_flex().w_full().justify_center().child(
                                            Button::new("open-settings")
                                                .ghost()
                                                .icon(IconName::Settings)
                                                .when(!sidebar_collapsed, |b| b.label("Settings"))
                                                .tooltip("Settings")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.nav.update(cx, |nav, cx| {
                                                        nav.go(Page::Settings, cx)
                                                    });
                                                })),
                                        ),
                                    ),
                            ),
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
                                                .when(page == Page::Settings, |this| {
                                                    this.child(BreadcrumbItem::new("Settings"))
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
                                        // Settings (docs/0006 §4.2): app pages
                                        // (core-owned) + module pages, fed to the
                                        // library's `Settings` shell. Pages rebuilt
                                        // per render from live state (the upstream
                                        // story's intended pattern).
                                        Page::Settings => {
                                            let modules = self
                                                .registry
                                                .entries()
                                                .iter()
                                                .map(|e| {
                                                    (e.meta.icon.clone(), e.meta.title.clone())
                                                })
                                                .collect::<Vec<_>>();
                                            let mut pages = vec![
                                                app_settings::appearance_page(cx),
                                                app_settings::about_page(&modules),
                                            ];
                                            pages.extend(self.registry.settings_pages(window, cx));
                                            div()
                                                .size_full()
                                                .child(Settings::new("app-settings").pages(pages))
                                                .into_any_element()
                                        }
                                    }),
                            ),
                    ),
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
        if let Some(icon) = &entry.meta.icon {
            item = item.icon(match icon {
                ModuleIcon::BuiltIn(name) => name.clone().into(),
                ModuleIcon::Custom(path) => Icon::empty().path(path.clone()),
            });
        }
        item
    });

    std::iter::once(home).chain(modules).collect()
}
