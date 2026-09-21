//! App shell (docs/0003 §4.5): 0002's window chrome + sidebar navigation.
//!
//! Layout mirrors `sidebar_story.rs`: `Sidebar` on the left, content column
//! with `SidebarToggleButton` left of the `Breadcrumb`, page below.

use gpui::{prelude::FluentBuilder as _, *};

use gpui::Decorations;

use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, TitleBar,
    breadcrumb::{Breadcrumb, BreadcrumbItem},
    button::{Button, ButtonVariants},
    h_flex,
    setting::Settings,
    sidebar::{Sidebar, SidebarCollapsible, SidebarHeader, SidebarMenuItem, SidebarToggleButton},
    v_flex,
};

use crate::home;
use crate::module::ModuleIcon;
use crate::nav::{self, CrumbAction, Navigator, Page, Route};
use crate::registry::ModuleRegistry;
use crate::settings as app_settings;
use crate::status;
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
    /// In-flight request tracker (docs/0013 D2/D6) — observed for
    /// status-bar re-renders.
    in_flight: Entity<status::InFlight>,
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
        // History trail keys (docs/0048 D7): global (no key context).
        cx.bind_keys([
            KeyBinding::new("alt-left", nav::Back, None),
            KeyBinding::new("alt-right", nav::Forward, None),
        ]);
        cx.observe(&nav, |_, _, cx| cx.notify()).detach();
        // Status bar source (docs/0013 D6): re-render on every in-flight
        // mutation; the bar reads the aggregated snapshot per frame.
        let in_flight = status::tracker(cx);
        cx.observe(&in_flight, |_, _, cx| cx.notify()).detach();
        let focus_handle = cx.focus_handle();
        // Boot focus: without a focused node, key actions dispatch from
        // the tree root and never reach this shell's handlers
        // (owner-verified bug on Hyprland — a 0002 keybind was dead
        // from first launch).
        window.focus(&focus_handle, cx);
        Self {
            registry,
            nav,
            in_flight,
            focus_handle,
            save: state_path.map(|p| {
                // Registered for the quit-time flush (docs/0045 §9,
                // #283): a quit inside the 500 ms window keeps the last
                // sidebar toggle.
                let save = DebouncedSave::new(p, Duration::from_millis(500));
                save.register_flush_on_quit();
                save
            }),
        }
    }

    /// Snapshot the sidebar toggle and schedule the debounced save.
    fn save_state(&mut self, cx: &Context<Self>) {
        if let Some(save) = &self.save {
            let sidebar_collapsed = self.nav.read(cx).sidebar_collapsed;
            save.schedule(ShellState { sidebar_collapsed }, cx);
        }
    }

    /// Sidebar/crumb navigation (docs/0048 D6): a jump to a different
    /// top-level branch first collapses the abandoned module's open
    /// path (its deep state tears down per the module's own policy),
    /// then `go` records the destination visit. Same-branch jumps
    /// (the module crumb mid-module) skip the collapse — the module's
    /// own `Reentered` handler returns to root (docs/0014 §5 D7).
    fn navigate(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.nav.read(cx).page.clone();
        if let Page::Module(id) = &current {
            if current != page {
                if let Some(nav) = self.registry.get(id).and_then(|e| e.nav.as_ref()) {
                    nav.collapse(window, cx);
                }
            }
        }
        self.nav.update(cx, |nav, cx| nav.go(page, cx));
        self.focus_root(window, cx);
    }

    /// The shell root takes keyboard focus after a page switch: the
    /// abandoned deep view may have held focus and just unmounted, and
    /// gpui dispatches key actions along the focused node's path — a
    /// dead focus falls back to the window root, which is ABOVE this
    /// shell's `on_action` handlers, so global bindings (Alt+←/→) would
    /// silently die (the reported second-press hang).
    fn focus_root(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
    }

    /// Back/forward cursor moves (docs/0048 D5): the Navigator switched
    /// the page already (`back`/`forward` return the route that is now
    /// current); what remains is the module re-activation — `restore`
    /// for a deep place, `collapse` for a module root. Restores are the
    /// same code path as clicking, so 0031 dedup and caches apply.
    fn activate_route(&mut self, route: &Route, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((id, payload)) = &route.module {
            if let Some(nav) = self.registry.get(id).and_then(|e| e.nav.as_ref()) {
                nav.restore(payload, window, cx);
            }
        } else if let Page::Module(id) = &route.page {
            if let Some(nav) = self.registry.get(id).and_then(|e| e.nav.as_ref()) {
                nav.collapse(window, cx);
            }
        }
    }

    // Back/forward refocus the root BEFORE re-activating the landed
    // route: a deep restore that hands focus to its sub-view (the PR
    // page's Esc affordance) steals it back from the root; anything
    // else — module root, repo view, shell pages — keeps the root
    // focused. Either way focus is alive on a node inside the shell.
    fn go_back(&mut self, _: &nav::Back, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(route) = self.nav.update(cx, |nav, cx| nav.back(cx)) {
            self.focus_root(window, cx);
            self.activate_route(&route, window, cx);
        }
    }

    fn go_forward(&mut self, _: &nav::Forward, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(route) = self.nav.update(cx, |nav, cx| nav.forward(cx)) {
            self.focus_root(window, cx);
            self.activate_route(&route, window, cx);
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

        let (can_back, can_forward) = (nav.can_back(), nav.can_forward());
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
            // History trail keys (docs/0048 D7): global bindings dispatch
            // from the focused node upward — the shell's focused root
            // catches them (the boot-focus precedent below).
            // Mouse back/forward buttons (docs/0048 §10): the Linux
            // backend maps them to Navigate(Back|Forward) — X11 calls
            // them 8/9, Wayland BTN_BACK/BTN_SIDE and BTN_FORWARD/
            // BTN_EXTRA — the same two handlers the keyboard drives.
            .on_action(cx.listener(Self::go_back))
            .on_action(cx.listener(Self::go_forward))
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.go_back(&nav::Back, window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Forward),
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.go_forward(&nav::Forward, window, cx);
                }),
            )
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
                // Window-padding wrapper: insets the framed panel holding
                // both main columns (sidebar + content) from the window
                // edges — upstream gpui adds no automatic padding; apps pad
                // themselves (the gpui-component examples do this). The
                // client-decorated TitleBar above stays full-width.
                v_flex().flex_1().min_h_0().p_4().child(
                    // Framed panel: border + radius around the two main
                    // columns AND the status bar below them (docs/0013
                    // D1); overflow_hidden clips all three backgrounds to
                    // the rounded corners.
                    v_flex()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(cx.theme().radius)
                        .overflow_hidden()
                        .child(
                            // The two main columns, unchanged — one row
                            // so the status bar below spans BOTH
                            // (docs/0013 D1).
                            h_flex()
                                .flex_1()
                                .min_h_0()
                                // Sidebar (Q4): wordmark header; flat menu — Home first,
                                // then modules, then the Settings entry (0006 §4.1).
                                // Sidebar children are typed Vec<E>, so a
                                // SidebarGroup would exclude the standalone Home item.
                                .child(
                                    Sidebar::new("app-sidebar")
                                        .collapsible(SidebarCollapsible::Icon)
                                        .collapsed(sidebar_collapsed)
                                        .header(
                                            // Brand, sidebar_story.rs pattern: the
                                            // wordmark's flex_1 absorbs the free space, so
                                            // SidebarHeader's hard-coded justify_between
                                            // keeps it packed left next to the logo; when
                                            // collapsed the icon box shrinks to a bare
                                            // size_4 glyph (rail) instead of clipping.
                                            SidebarHeader::new()
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
                                                        .when(!sidebar_collapsed, |this| {
                                                            this.child(
                                                                Icon::new(
                                                                    IconName::GalleryVerticalEnd,
                                                                )
                                                                .size_4(),
                                                            )
                                                        })
                                                        .when(sidebar_collapsed, |this| {
                                                            this.size_4()
                                                        .bg(cx.theme().transparent)
                                                        .text_color(cx.theme().foreground)
                                                        .child(
                                                            Icon::new(IconName::GalleryVerticalEnd)
                                                                .size_4(),
                                                        )
                                                        }),
                                                )
                                                .when(!sidebar_collapsed, |this| {
                                                    this.child(
                                                        div()
                                                            .flex_1()
                                                            .text_base()
                                                            .overflow_hidden()
                                                            .text_ellipsis()
                                                            .child("Dashboard"),
                                                    )
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
                                                            this.nav.update(cx, |nav, cx| {
                                                                nav.toggle_sidebar(cx)
                                                            });
                                                            this.save_state(cx);
                                                        })),
                                                )
                                                .child(
                                                    h_flex()
                                                        .gap_0p5()
                                                        .pr_1()
                                                        // History trail controls
                                                        // (docs/0048 D7) — ids are
                                                        // bridge-visible
                                                        // (docs/0010 D10).
                                                        .child(
                                                            Button::new("nav-back")
                                                                .debug_selector(|| {
                                                                    "nav-back".to_string()
                                                                })
                                                                .icon(IconName::ArrowLeft)
                                                                .ghost()
                                                                .small()
                                                                .tooltip("Back (Alt+Left)")
                                                                // docs/0049 D1: icon-only buttons get an
                                                                // explicit accessible name.
                                                                .accessibility_label("Back")
                                                                .disabled(!can_back)
                                                                .on_click(cx.listener(
                                                                    |this, _, window, cx| {
                                                                        this.go_back(
                                                                            &nav::Back,
                                                                            window,
                                                                            cx,
                                                                        );
                                                                    },
                                                                )),
                                                        )
                                                        .child(
                                                            Button::new("nav-forward")
                                                                .debug_selector(|| {
                                                                    "nav-forward".to_string()
                                                                })
                                                                .icon(IconName::ArrowRight)
                                                                .ghost()
                                                                .small()
                                                                .tooltip("Forward (Alt+Right)")
                                                                .accessibility_label("Forward")
                                                                .disabled(!can_forward)
                                                                .on_click(cx.listener(
                                                                    |this, _, window, cx| {
                                                                        this.go_forward(
                                                                            &nav::Forward,
                                                                            window,
                                                                            cx,
                                                                        );
                                                                    },
                                                                )),
                                                        ),
                                                )
                                                .child(
                                                    div()
                                                        .min_w_0()
                                                        .flex_1()
                                                        .overflow_hidden()
                                                        .child(
                                                            Breadcrumb::new()
                                                                .child(
                                                                    BreadcrumbItem::new("Home")
                                                                        .on_click(cx.listener(
                                                                            |this, _, window, cx| {
                                                                                this.navigate(
                                                                                    Page::Home,
                                                                                    window,
                                                                                    cx,
                                                                                );
                                                                            },
                                                                        )),
                                                                )
                                                                .when_some(
                                                                    current_module.clone(),
                                                                    |this, m| {
                                                                        this.child(
                                                            BreadcrumbItem::new(m.1).on_click(
                                                                cx.listener(
                                                                    move |this, _, window, cx| {
                                                                        this.navigate(
                                                                            Page::Module(m.0.clone()),
                                                                            window,
                                                                            cx,
                                                                        );
                                                                    },
                                                                ),
                                                            ),
                                                        )
                                                                    },
                                                                )
                                                                .when(
                                                                    page == Page::Settings,
                                                                    |this| {
                                                                        this.child(
                                                                            BreadcrumbItem::new(
                                                                                "Settings",
                                                                            ),
                                                                        )
                                                                    },
                                                                )
                                                                .children(crumbs.iter().map(
                                                                    |(label, on_click)| {
                                                                        let mut item =
                                                                            BreadcrumbItem::new(
                                                                                label.clone(),
                                                                            );
                                                                        if let Some(f) =
                                                                            on_click.clone()
                                                                        {
                                                                            item = item.on_click(
                                                                    move |_, window, cx| {
                                                                        f(window, cx)
                                                                    },
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
                                                    Page::Home => {
                                                        home::render(&self.registry, &self.nav, cx)
                                                            .into_any_element()
                                                    }
                                                    Page::Module(id) => match self.registry.get(id)
                                                    {
                                                        Some(entry) => entry
                                                            .main_page
                                                            .clone()
                                                            .into_any_element(),
                                                        None => div()
                                                            .child("Unknown module")
                                                            .into_any_element(),
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
                                                                (
                                                                    e.meta.icon.clone(),
                                                                    e.meta.title.clone(),
                                                                )
                                                            })
                                                            .collect::<Vec<_>>();
                                                        let mut pages = vec![
                                                            app_settings::appearance_page(cx),
                                                            app_settings::about_page(&modules),
                                                        ];
                                                        pages.extend(
                                                            self.registry
                                                                .settings_pages(window, cx),
                                                        );
                                                        div()
                                                            .size_full()
                                                            .child(
                                                                Settings::new("app-settings")
                                                                    .pages(pages),
                                                            )
                                                            .into_any_element()
                                                    }
                                                }),
                                        ),
                                ),
                        )
                        .child(
                            // Status bar (docs/0013 D1/D5): app chrome
                            // below both columns — the right region lists
                            // modules with requests in flight, empty when
                            // idle.
                            status::bar(&self.in_flight, &self.registry, cx).flex_shrink_0(),
                        ),
                ),
            )
    }
}

/// Home + module main-page entries as one flat sidebar menu, with the
/// Settings entry (docs/0006 §4.1) as the last item after the modules
/// (see the shell render note: `Sidebar` children are `Vec<E>`, so mixing
/// a standalone menu with a labeled group is not possible).
fn nav_menu_items(
    registry: &ModuleRegistry,
    page: &Page,
    cx: &mut Context<DashboardShell>,
) -> Vec<SidebarMenuItem> {
    let home = SidebarMenuItem::new("Home")
        .icon(IconName::LayoutDashboard)
        .active(*page == Page::Home)
        .on_click(cx.listener(|this, _, window, cx| {
            this.navigate(Page::Home, window, cx);
        }));

    let modules = registry.entries().iter().map(|entry| {
        let id = entry.meta.id.clone();
        let active = *page == Page::Module(id.clone());
        let mut item = SidebarMenuItem::new(entry.meta.title.clone())
            .active(active)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.navigate(Page::Module(id.clone()), window, cx);
            }));
        if let Some(icon) = &entry.meta.icon {
            item = item.icon(match icon {
                ModuleIcon::BuiltIn(name) => name.clone().into(),
                ModuleIcon::Custom(path) => Icon::empty().path(path.clone()),
            });
        }
        item
    });

    let settings = SidebarMenuItem::new("Settings")
        .icon(IconName::Settings)
        .active(*page == Page::Settings)
        .on_click(cx.listener(|this, _, window, cx| {
            this.navigate(Page::Settings, window, cx);
        }));

    std::iter::once(home)
        .chain(modules)
        .chain(std::iter::once(settings))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    use gpui::{
        App, AppContext as _, Context, InteractiveElement as _, IntoElement, Modifiers,
        MouseButton, NavigationDirection, ParentElement as _, Render, TestAppContext,
        VisualTestContext, Window, div, point, px,
    };

    use super::DashboardShell;
    use crate::ShellState;
    use crate::module::{Module, ModuleEntry, ModuleMeta, ModuleNav};
    use crate::nav::{Navigator, Page, Route};
    use crate::paths::AppPaths;
    use crate::registry::ModuleRegistry;

    // docs/0048 D3/D6 shell contract, with a minimal fake module:
    // cross-branch `navigate` collapses the abandoned module, Back
    // restores its deep place through the recorded payload.

    thread_local! {
        static LOG: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    }

    // The deep view's focus handle: painted while present, so a Back
    // that collapses it mirrors the PR page's focus-steal-then-unmount
    // (the reported Alt+← dead second press).
    thread_local! {
        static DEEP: RefCell<Option<gpui::FocusHandle>> = const { RefCell::new(None) };
    }

    struct FakePage;

    impl Render for FakePage {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            match DEEP.with(|deep| deep.borrow().clone()) {
                Some(handle) => div()
                    .id("fake-page")
                    .child(div().id("fake-deep").track_focus(&handle)),
                None => div().id("fake-page"),
            }
        }
    }

    struct FakeNav;

    impl FakeNav {
        fn push(line: String) {
            LOG.with(|log| log.borrow_mut().push(line));
        }
    }

    impl ModuleNav for FakeNav {
        fn restore(
            &self,
            payload: &Arc<dyn std::any::Any + Send + Sync>,
            _window: &mut Window,
            _cx: &mut App,
        ) {
            let place = payload.downcast_ref::<String>().cloned();
            Self::push(format!("restore:{}", place.as_deref().unwrap_or("?")));
        }

        fn collapse(&self, _window: &mut Window, _cx: &mut App) {
            DEEP.with(|deep| *deep.borrow_mut() = None);
            Self::push("collapse".into());
        }
    }

    struct FakeModule;

    impl Module for FakeModule {
        fn meta() -> ModuleMeta {
            ModuleMeta {
                id: "fake".into(),
                title: "Fake".into(),
                icon: None,
            }
        }

        fn init(
            _nav: &gpui::Entity<Navigator>,
            _window: &mut Window,
            _paths: &AppPaths,
            cx: &mut App,
        ) -> ModuleEntry {
            let nav = Arc::new(FakeNav);
            ModuleEntry {
                meta: Self::meta(),
                overview: cx.new(|_| FakePage).into(),
                main_page: cx.new(|_| FakePage).into(),
                settings: None,
                nav: Some(nav),
            }
        }
    }

    #[gpui::test]
    fn navigate_collapses_abandoned_module_and_back_restores(cx: &mut TestAppContext) {
        LOG.with(|log| log.borrow_mut().clear());
        cx.update(|cx| {
            gpui_component::theme::init(cx);
            crate::settings::init(None, None, cx);
            crate::status::init(cx);
        });
        let nav = cx.update(|cx| cx.new(|_| Navigator::default()));
        let (shell, vcx) = {
            let nav = nav.clone();
            cx.add_window_view(|window, cx| {
                let registry =
                    ModuleRegistry::new().with::<FakeModule>(&nav, window, AppPaths::init(), cx);
                DashboardShell::new(registry, nav, ShellState::default(), None, window, cx)
            })
        };

        // The fake module is deep somewhere (its own visit).
        vcx.update(|_, cx| {
            nav.update(cx, |nav, cx| {
                // The module is on screen: its root, then a deep place.
                nav.go(Page::Module("fake".into()), cx);
                nav.visit(
                    Route::module_place("fake", "deep:x", Arc::new("x".to_string())),
                    cx,
                );
            });
        });
        assert!(
            vcx.debug_bounds("nav-back").is_some(),
            "trail controls render"
        );

        // Cross-branch jump through the sidebar path: the abandoned
        // module collapses, then Settings is recorded (docs/0048 D6).
        vcx.update(|window, cx| {
            shell.update(cx, |this, cx| this.navigate(Page::Settings, window, cx));
        });
        let log = LOG.with(|log| log.borrow().clone());
        assert_eq!(log, vec!["collapse"], "cross-branch jump collapses");
        vcx.update(|_, cx| {
            nav.read_with(cx, |nav, _| assert!(matches!(nav.page, Page::Settings)));
        });

        // Back re-activates the deep place with its payload (D5).
        vcx.update(|window, cx| {
            shell.update(cx, |this, cx| this.go_back(&crate::nav::Back, window, cx));
        });
        let log = LOG.with(|log| log.borrow().clone());
        assert_eq!(
            log,
            vec!["collapse", "restore:x"],
            "Back restores the deep leaf"
        );

        vcx.update(|_, cx| {
            nav.read_with(cx, |nav, _| {
                assert!(matches!(nav.page, Page::Module(ref id) if id.as_ref() == "fake"));
            });
        });
    }

    /// The reported hang (Alt+← dead on the second press): the deep view
    /// (PR page) steals focus when opened; Back #1 collapses it, so the
    /// focused node unmounts. Press #2 must still dispatch — through the
    /// real keystroke pipeline, not the handler method the button uses.
    #[gpui::test]
    fn back_keystroke_survives_the_deep_view_unmounting(cx: &mut TestAppContext) {
        LOG.with(|log| log.borrow_mut().clear());
        cx.update(|cx| {
            gpui_component::theme::init(cx);
            crate::settings::init(None, None, cx);
            crate::status::init(cx);
        });
        let nav = cx.update(|cx| cx.new(|_| Navigator::default()));
        let (shell, vcx) = {
            let nav = nav.clone();
            cx.add_window_view(|window, cx| {
                let registry =
                    ModuleRegistry::new().with::<FakeModule>(&nav, window, AppPaths::init(), cx);
                DashboardShell::new(registry, nav, ShellState::default(), None, window, cx)
            })
        };

        // The user's flow: module root, then a deep place whose view
        // paints a focusable child and steals focus onto it.
        vcx.update(|window, cx| {
            let handle = cx.focus_handle();
            DEEP.with(|deep| *deep.borrow_mut() = Some(handle.clone()));
            nav.update(cx, |nav, cx| {
                nav.go(Page::Module("fake".into()), cx);
                nav.visit(
                    Route::module_place("fake", "deep:x", Arc::new("x".to_string())),
                    cx,
                );
            });
            window.focus(&handle, cx);
        });

        // Back #1 (keyboard): lands on the module root and collapses the
        // deep view — its focused child unmounts with the collapse.
        vcx.simulate_keystrokes("alt-left");
        let log = LOG.with(|log| log.borrow().clone());
        assert_eq!(log, vec!["collapse"], "keystroke #1 collapsed");
        vcx.update(|_, cx| {
            nav.read_with(cx, |nav, _| {
                assert!(matches!(nav.page, Page::Module(ref id) if id.as_ref() == "fake"));
            });
        });
        // The repaint that drops the unmounted deep view.
        vcx.update(|window, cx| window.draw(cx).clear(cx));

        // Back #2 (keyboard): the previously focused node is gone from
        // the frame — dispatch must still reach the shell (→ Home).
        vcx.simulate_keystrokes("alt-left");
        vcx.update(|_, cx| {
            nav.read_with(cx, |nav, _| {
                assert!(
                    matches!(nav.page, Page::Home),
                    "second Alt+← must land on Home, got {:?}",
                    nav.page
                );
            });
        });
        let _ = &shell;
    }

    // #309 (docs/0048 §10): the mouse back/forward buttons must drive
    // the same trail handlers Alt+←/→ do — dispatched through the real
    // input pipeline like the keystroke test above.
    #[gpui::test]
    fn mouse_back_forward_buttons_drive_the_trail(cx: &mut TestAppContext) {
        LOG.with(|log| log.borrow_mut().clear());
        cx.update(|cx| {
            gpui_component::theme::init(cx);
            crate::settings::init(None, None, cx);
            crate::status::init(cx);
        });
        let nav = cx.update(|cx| cx.new(|_| Navigator::default()));
        let (shell, vcx) = {
            let nav = nav.clone();
            cx.add_window_view(|window, cx| {
                let registry =
                    ModuleRegistry::new().with::<FakeModule>(&nav, window, AppPaths::init(), cx);
                DashboardShell::new(registry, nav, ShellState::default(), None, window, cx)
            })
        };

        // Home → module root → deep place.
        vcx.update(|window, cx| {
            let handle = cx.focus_handle();
            DEEP.with(|deep| *deep.borrow_mut() = Some(handle.clone()));
            nav.update(cx, |nav, cx| {
                nav.go(Page::Module("fake".into()), cx);
                nav.visit(
                    Route::module_place("fake", "deep:x", Arc::new("x".to_string())),
                    cx,
                );
            });
            window.focus(&handle, cx);
        });

        let press = |vcx: &mut VisualTestContext, dir: NavigationDirection| {
            vcx.simulate_mouse_down(
                point(px(0.), px(0.)),
                MouseButton::Navigate(dir),
                Modifiers::default(),
            );
        };

        // Mouse back #1: collapses the deep view, lands on the module root.
        press(vcx, NavigationDirection::Back);
        vcx.update(|window, cx| window.draw(cx).clear(cx));
        let log = LOG.with(|log| log.borrow().clone());
        assert_eq!(log, vec!["collapse"], "mouse back #1 collapsed");

        // Mouse back #2: Home — then forward walks back out.
        press(vcx, NavigationDirection::Back);
        vcx.update(|_, cx| {
            nav.read_with(cx, |nav, _| {
                assert!(
                    matches!(nav.page, Page::Home),
                    "second mouse back must land on Home, got {:?}",
                    nav.page
                );
            });
        });
        press(vcx, NavigationDirection::Forward);
        vcx.update(|_, cx| {
            nav.read_with(cx, |nav, _| {
                assert!(
                    matches!(nav.page, Page::Module(ref id) if id.as_ref() == "fake"),
                    "mouse forward must restore the module root"
                );
            });
        });
        let _ = &shell;
    }
}
