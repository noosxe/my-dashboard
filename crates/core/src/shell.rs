//! App shell (docs/0003 §4.5): 0002's window chrome + sidebar navigation.
//!
//! Layout mirrors `sidebar_story.rs`: `Sidebar` on the left, content column
//! with `SidebarToggleButton` left of the `Breadcrumb`, page below.

use gpui::{prelude::FluentBuilder as _, *};

use gpui::Decorations;
use std::rc::Rc;

use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, TitleBar, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    notification::{Notification, NotificationDelivery},
    setting::{SelectIndex, Settings},
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
use crate::toasts;
use crate::ui::{CardVariant, card};
use crate::usage_analytics;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Persisted shell UI state (docs/0004 §4.3) → `<state>/shell.json`.
/// (Chrome itself is no longer persisted — see the render-time
/// decoration logic below; older files with a `show_titlebar` field
/// load fine, serde ignores unknown fields.)
#[derive(Serialize, Deserialize, Default)]
#[non_exhaustive]
pub struct ShellState {
    pub sidebar_collapsed: bool,
    /// Files-tab sidebar width in px (docs/0069 D4). Older files lack
    /// it → the pre-seam default; restored clamped into the declared
    /// range via `panes::seed`.
    #[serde(default = "default_files_sidebar_px")]
    pub files_sidebar_px: f32,
}

/// Pre-seam sidebar width (docs/0069 D3) — also the serde default.
fn default_files_sidebar_px() -> f32 {
    crate::panes::FILES_SIDEBAR_DEFAULT
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
    /// Usage analytics page (docs/0054 D1): created on first
    /// navigation — see the nav observer in [`Self::new`] — and kept
    /// for the session so its frozen `as_of` survives re-entry (D10).
    usage_page: Option<Entity<usage_analytics::UsagePage>>,
    /// About dialog visibility (docs/0002 D12): opened from the macOS
    /// app menu; session-only — never persisted to shell.json.
    about_open: bool,
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
        // Pane widths (docs/0069 D4): a seam resize settled (drag end or
        // keyboard step) — funnel it into the same debounced save as
        // every other shell state.
        cx.observe_global::<crate::panes::PaneWidths>(|this, cx| {
            this.save_state(cx);
        })
        .detach();
        cx.observe_in(&nav, window, |this, nav, window, cx| {
            // Usage page (docs/0054 D1/D10): the entity is created at
            // FIRST navigation, not app start — its `as_of` freeze and
            // one bounded read belong to first open. Later re-entries
            // keep the frozen state; only the page's refresh button
            // moves it. `observe_in` carries the window the page's
            // Selects and table entity need exactly once, at creation.
            if nav.read(cx).page == Page::Usage && this.usage_page.is_none() {
                this.usage_page = Some(cx.new(|cx| usage_analytics::UsagePage::new(window, cx)));
            }
            cx.notify();
        })
        .detach();
        // Status bar source (docs/0013 D6): re-render on every in-flight
        // mutation; the bar reads the aggregated snapshot per frame.
        let in_flight = status::tracker(cx);
        cx.observe(&in_flight, |_, _, cx| cx.notify()).detach();
        // Toast forwarding (docs/0058 D2/D5/D6): the hub is core-global;
        // this shell is the only place that touches the UI layer. One
        // window-aware observation forwards staged toasts; a 1 s pump
        // drains the cross-thread spill (spill pushes cannot notify —
        // they come from non-UI threads like the usage writer).
        let toasts_hub = toasts::hub(cx);
        cx.observe_in(&toasts_hub, window, |this, hub, window, cx| {
            this.forward_toasts(&hub, window, cx);
            // Hub mutations also change the badge/panel (docs/0059 D3), so
            // the shell must re-render its bar.
            cx.notify();
        })
        .detach();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                // Err → the shell (and its window) is gone; stop pumping.
                if this
                    .update(cx, |_, cx| {
                        toasts::hub(cx).update(cx, |hub, cx| {
                            hub.drain_spill(cx);
                        });
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
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
            usage_page: None,
            about_open: false,
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

    /// Drain the hub's staged toasts into the notification layer (docs/
    /// 0058 D1/D5). The shell never renders toasts itself — `Root` owns
    /// the layer, the z-order, and the lifecycle.
    fn forward_toasts(
        &mut self,
        hub: &Entity<toasts::ToastHub>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let staged = hub.update(cx, |hub, _| hub.take_pending());
        if staged.is_empty() {
            return;
        }
        // docs/0060 D2: one activation read per drain — the whole batch
        // shares the window state at drain time.
        let active = window.is_window_active();
        // 0.7.0: notifications moved off `Root`'s API onto `WindowExt`
        // (component window_ext.rs) — Root still owns the layer, we just
        // address it through the window.
        for toast in staged {
            window.push_notification(toast.into_notification(hub, active), cx);
        }
    }

    /// Snapshot the sidebar toggle and the pane widths (docs/0069 D4)
    /// and schedule the debounced save.
    fn save_state(&mut self, cx: &Context<Self>) {
        if let Some(save) = &self.save {
            let sidebar_collapsed = self.nav.read(cx).sidebar_collapsed;
            let files_sidebar_px = cx
                .try_global::<crate::panes::PaneWidths>()
                .map(|panes| panes.files_sidebar_px)
                .unwrap_or(crate::panes::FILES_SIDEBAR_DEFAULT);
            save.schedule(
                ShellState {
                    sidebar_collapsed,
                    files_sidebar_px,
                },
                cx,
            );
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
    /// About dialog card width — widget geometry is a named constant
    /// (docs/0002 D9 exemption); spacing goes through the scale.
    const ABOUT_CARD_WIDTH: f32 = 360.;

    /// macOS About menu item (docs/0002 D12): opens the About dialog.
    /// Session-only state — never persisted to shell.json.
    pub fn open_about(&mut self, cx: &mut Context<Self>) {
        self.about_open = true;
        cx.notify();
    }

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

/// Click handler for a [`ShellCrumb`]; the `Rc` twin of [`CrumbAction`]
/// (which is `Arc`-based for cross-entity sharing).
type ShellCrumbAction = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// One chrome-row breadcrumb crumb (#522): rendered locally instead of
/// via upstream `Breadcrumb`, whose `BreadcrumbItem` self-roles but never
/// lands an accessible name — the text child is its own text node
/// (docs/0049), so every link crumb FAILs the audit's role-label rule.
/// Local crumbs carry ids, roles, and explicit names (docs/0010 D10).
struct ShellCrumb {
    id: SharedString,
    label: SharedString,
    /// `None` renders a non-clickable landmark: the current location is
    /// never a link.
    on_click: Option<ShellCrumbAction>,
}

fn shell_crumb(crumb: ShellCrumb, is_last: bool, muted: Hsla, foreground: Hsla) -> AnyElement {
    let ShellCrumb {
        id,
        label,
        on_click,
    } = crumb;
    let mut item = div()
        .id(ElementId::Name(id))
        .role(if on_click.is_some() {
            accesskit::Role::Link
        } else {
            accesskit::Role::ListItem
        })
        // docs/0010 D10: names are explicit; text children are not names.
        .aria_label(label.clone())
        .text_color(if is_last { foreground } else { muted })
        .child(label);
    if let Some(on_click) = on_click {
        item = item
            .cursor_pointer()
            .on_click(move |event, window, cx| on_click(event, window, cx));
    }
    item.into_any_element()
}

/// The crumb row itself: upstream-Breadcrumb-identical visuals (muted
/// text, foreground on the last crumb, chevron separators).
fn crumb_row(items: Vec<ShellCrumb>, muted: Hsla, foreground: Hsla) -> AnyElement {
    let count = items.len();
    let mut row = h_flex().gap_1p5().text_sm().text_color(muted);
    for (ix, crumb) in items.into_iter().enumerate() {
        let is_last = ix + 1 == count;
        row = row.child(shell_crumb(crumb, is_last, muted, foreground));
        if !is_last {
            row = row.child(
                Icon::new(IconName::ChevronRight)
                    .text_color(muted)
                    .size_3p5(),
            );
        }
    }
    row.into_any_element()
}

impl Render for DashboardShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Chrome is decoration-driven (owner decision, 0004 PR #13 —
        // supersedes 0002 D7's runtime toggle): if the window is
        // client-decorated the app owns the whole chrome (full title
        // bar; gpui-component renders min/max/close there); if the
        // server owns decorations we draw nothing on Linux — KWin/X11
        // WM bars provide the controls (D11 adds the macOS exception:
        // its "server" decoration is traffic lights only, no bar, so
        // the app draws the strip there), and on decoration-less
        // servers (Hyprland) window management stays keybind territory.
        // Resolved per render: the Wayland decoration answer arrives
        // asynchronously with the first configure, which triggers a
        // redraw, so this tracks the live state with no observer wiring.
        let client_decorated = matches!(window.window_decorations(), Decorations::Client { .. });
        // macOS draws the strip (D11) — except in fullscreen: there the
        // OS overlays its own bar (menu + traffic lights) when you
        // hover the top screen edge, so ours is redundant vertical
        // space. is_fullscreen() re-evaluates per render; the
        // fullscreen transition resizes the window, forcing the
        // re-render — the same mechanism upstream's TitleBar relies on
        // for its fullscreen padding tweak. Linux keeps the strip in
        // fullscreen: its min/max/close live there and no OS overlay
        // replaces them.
        let draw_titlebar =
            client_decorated || (cfg!(target_os = "macos") && !window.is_fullscreen());

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
            Page::Home | Page::Settings | Page::Usage => None,
        };

        let (muted_fg, crumb_fg) = (cx.theme().muted_foreground, cx.theme().foreground);
        // Chrome-row crumbs (#522): id'd, role'd, explicitly labeled — see
        // `ShellCrumb`. The current location is a landmark, not a link.
        let home_click = (page != Page::Home).then(|| {
            Rc::new(cx.listener(|this, _, window, cx| {
                this.navigate(Page::Home, window, cx);
            })) as ShellCrumbAction
        });
        let mut crumb_items = vec![ShellCrumb {
            id: "crumb-home".into(),
            label: "Home".into(),
            on_click: home_click,
        }];
        if let Some((id, title)) = current_module.clone() {
            let on_click = (page != Page::Module(id.clone())).then(|| {
                Rc::new(cx.listener(move |this, _, window, cx| {
                    this.navigate(Page::Module(id.clone()), window, cx);
                })) as ShellCrumbAction
            });
            crumb_items.push(ShellCrumb {
                id: "crumb-module".into(),
                label: title,
                on_click,
            });
        }
        match &page {
            Page::Settings => crumb_items.push(ShellCrumb {
                id: "crumb-settings".into(),
                label: "Settings".into(),
                on_click: None,
            }),
            Page::Usage => crumb_items.push(ShellCrumb {
                id: "crumb-usage".into(),
                label: "Usage".into(),
                on_click: None,
            }),
            Page::Home | Page::Module(_) => {}
        }
        for (ix, (label, action)) in crumbs.iter().enumerate() {
            let on_click = action.clone().map(|action| {
                Rc::new(move |_: &ClickEvent, window: &mut Window, cx: &mut App| action(window, cx))
                    as ShellCrumbAction
            });
            crumb_items.push(ShellCrumb {
                id: format!("crumb-trail-{ix}").into(),
                label: label.clone(),
                on_click,
            });
        }
        // About dialog copy (docs/0002 D12): same source as the
        // settings About page (docs/0006 §4.4).
        let (app_name, app_version) = crate::settings::app_info();
        v_flex()
            .id("dashboard-shell")
            .relative()
            .track_focus(&self.focus_handle)
                // #360: a focused element needs a role to paint an
                // a11y node — id + track_focus alone is the
                // "focus without node" gap class (the walk and
                // assistive tech both land here blind otherwise).
                .role(accesskit::Role::Group)
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
            .when(draw_titlebar, |this| {
                this.child(
                    // Custom title bar; gpui-component draws min/max/
                    // close at the right edge on client-decorated Linux
                    // and no buttons on macOS (traffic lights are native).
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
                // themselves (the gpui-component examples do this). The
                // title bar above stays full-width.
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
                                                        .accessibility_label("Toggle sidebar")
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
                                                            crumb_row(
                                                                crumb_items,
                                                                muted_fg,
                                                                crumb_fg,
                                                            )
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
                                                    Page::Usage => match self
                                                        .usage_page
                                                        .clone()
                                                    {
                                                        // Created by the nav
                                                        // observer on first
                                                        // open (docs/0054
                                                        // D1/D10); the None
                                                        // arm is transient
                                                        // at most one frame.
                                                        Some(page) => page
                                                            .into_any_element(),
                                                        None => div()
                                                            .into_any_element(),
                                                    },
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
                                                            app_settings::notifications_page(cx),
                                                            app_settings::about_page(&modules),
                                                        ];
                                                        // Notifications sits at index 1 above; the
                                                        // deep-link preselect (docs/0062 D1/D8)
                                                        // consumes the panel gear's one-shot
                                                        // request. Upstream keyed state keeps a
                                                        // remembered selection once the user
                                                        // has browsed — the default applies only
                                                        // to a fresh subnav.
                                                        let deep_link_notifications =
                                                            app_settings::take_notifications_settings_request(cx);
                                                        pages.extend(
                                                            self.registry
                                                                .settings_pages(window, cx),
                                                        );
                                                        div()
                                                            .size_full()
                                                            .child(
                                                                {
                                                                    let mut settings =
                                                                        Settings::new(
                                                                            "app-settings",
                                                                        )
                                                                        .pages(pages);
                                                                    if deep_link_notifications {
                                                                        settings = settings
                                                                            .default_selected_index(SelectIndex {
                                                                                page_ix: 1, // Notifications (vec above)
                                                                                group_ix: None,
                                                                            });
                                                                    }
                                                                    settings
                                                                }
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
                            // modules with requests in flight, plus
                            // module-contributed items (docs/0050 D5,
                            // scoped by each module to the active
                            // page) — empty when idle.
                            status::bar(
                                &self.in_flight,
                                &self.registry,
                                &self.nav,
                                &self.nav.read(cx).page,
                                cx,
                            )
                            .flex_shrink_0(),
                        ),
                ),
            )
            // About dialog (docs/0002 D12): backdrop + card overlay.
            // Theme tokens only (clippy.toml D5 — no literal colors);
            // backdrop click and the Close button both dismiss; the
            // card stops propagation so its own clicks don't close.
            .when(self.about_open, |this| {
                this.child(
                    div()
                        .id("about-overlay")
                        .role(accesskit::Role::Group)
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(cx.theme().overlay)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                this.about_open = false;
                                cx.notify();
                            }),
                        )
                        .child(
                            card(cx, CardVariant::Quiet)
                                .w(px(Self::ABOUT_CARD_WIDTH))
                                .gap_3()
                                .p_4()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|_, _: &MouseDownEvent, _, cx| {
                                        cx.stop_propagation();
                                    }),
                                )
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(app_name.to_string()),
                                )
                                .child(format!("Version {}", app_version))
                                .child(
                                    Button::new("about-close")
                                        .label("Close")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.about_open = false;
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
            })
    }
}

/// Home + module main-page entries as one flat sidebar menu, then the
/// Usage entry (docs/0054 D2), with the Settings entry (docs/0006 §4.1)
/// as the last item (see the shell render note: `Sidebar` children are
/// `Vec<E>`, so mixing a standalone menu with a labeled group is not
/// possible).
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

    let usage = SidebarMenuItem::new("Usage")
        .icon(IconName::ChartPie)
        .active(*page == Page::Usage)
        .on_click(cx.listener(|this, _, window, cx| {
            this.navigate(Page::Usage, window, cx);
        }));

    let settings = SidebarMenuItem::new("Settings")
        .icon(IconName::Settings)
        .active(*page == Page::Settings)
        .on_click(cx.listener(|this, _, window, cx| {
            this.navigate(Page::Settings, window, cx);
        }));

    std::iter::once(home)
        .chain(modules)
        .chain(std::iter::once(usage))
        .chain(std::iter::once(settings))
        .collect()
}

/// Marker scoping hub-raised dedupe ids (docs/0058 D4): every hub toast
/// shares this TypeId, so its keys can't collide with ids other code
/// registers on the notification layer.
struct HubToast;

impl toasts::Toast {
    /// D3's mapping: severity constructor per level, `Error` sticky, the
    /// dedupe key as the upstream id. docs/0060 D2/D3 add the OS-delivery
    /// gate and click-marks-read wiring on keyed raises. `ToastAction`
    /// is carried but not wired yet — docs/0058 §10 defers action wiring
    /// until a raise site needs one (upstream's `.action()` would also
    /// force sticky).
    fn into_notification(self, hub: &Entity<toasts::ToastHub>, active: bool) -> Notification {
        let delivery = os_delivery(active);
        if delivery.includes_system() {
            // docs/0060 D5: a denied or unavailable authorization drops
            // the post silently — this line is the only trace it fired.
            log::info!(
                "posting os notification key={:?} level={:?} (docs/0060 D5)",
                self.dedupe_key,
                self.level
            );
        }
        let mut note = match self.level {
            toasts::Level::Info => Notification::info(self.message),
            toasts::Level::Success => Notification::success(self.message),
            toasts::Level::Warning => Notification::warning(self.message),
            toasts::Level::Error => Notification::error(self.message).autohide(false),
        };
        if let Some(title) = self.title {
            note = note.title(title);
        }
        if let Some(key) = self.dedupe_key {
            // docs/0060 D3/F9: the key gives OS posts replace-not-stack
            // semantics upstream, and a banner (or toast) click is a
            // human action — it marks that incident read (docs/0059 D2).
            // Unkeyed raises keep today's behavior: read via the panel.
            let hub = hub.clone();
            note = note.id1::<HubToast>(key.clone()).on_click(move |_, _, cx| {
                hub.update(cx, |hub, cx| hub.mark_read_by_key(&key, cx));
            });
        }
        note.delivery(delivery)
    }
}

/// docs/0060 D2: the activation gate — a hub raise reaches the OS
/// notification center only while the owner is not looking at the app.
fn deliver_to_os(window_active: bool) -> bool {
    !window_active
}

/// docs/0060 D1: the gate compiles to `InApp` off macOS — behavior there
/// is byte-identical to docs/0058 (Windows needs identity plumbing we
/// don't run; Linux can't retract, breaking docs/0059 D2's ✕-retracts).
fn os_delivery(window_active: bool) -> NotificationDelivery {
    if cfg!(target_os = "macos") && deliver_to_os(window_active) {
        NotificationDelivery::InAppAndSystem
    } else {
        NotificationDelivery::InApp
    }
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

    use super::{DashboardShell, deliver_to_os, os_delivery};
    use crate::ShellState;
    use crate::module::{Module, ModuleEntry, ModuleMeta, ModuleNav};
    use crate::nav::{Navigator, Page, Route};
    use crate::paths::AppPaths;
    use crate::registry::ModuleRegistry;
    use gpui_component::{
        Root, WindowExt,
        notification::{Notification, NotificationDelivery},
    };

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
                status_item: Some(Arc::new(|page: &crate::nav::Page, _cx: &App| {
                    // docs/0050 D5: the provider owns its visibility —
                    // only the fake module's own page sees the item.
                    matches!(page, crate::nav::Page::Module(id) if id.as_ref() == "fake")
                        .then(|| gpui::div().into_any_element())
                })),
            }
        }
    }

    #[gpui::test]
    fn status_items_are_scoped_by_their_providers(cx: &mut TestAppContext) {
        // docs/0050 D5: the registry collects module-contributed items in
        // registration order; each provider decides visibility from the
        // active page (FakeModule's shows only on its own page).
        let registry_cell = std::cell::RefCell::new(None);
        let _view = cx.add_window_view(|window, cx| {
            gpui_component::theme::init(cx);
            let nav = cx.new(|_| Navigator::default());
            let registry =
                ModuleRegistry::new().with::<FakeModule>(&nav, window, AppPaths::init(), cx);
            *registry_cell.borrow_mut() = Some(registry);
            FakePage
        });
        let Some(registry) = registry_cell.into_inner() else {
            panic!("registry never built");
        };
        cx.update(|cx| {
            let home = crate::nav::Page::Home;
            assert_eq!(registry.status_items(&home, cx).len(), 0);
            assert_eq!(
                registry.status_items(&crate::nav::Page::Settings, cx).len(),
                0
            );
            let fake = crate::nav::Page::Module("fake".into());
            assert_eq!(registry.status_items(&fake, cx).len(), 1);
        });
    }
    #[gpui::test]
    fn navigate_collapses_abandoned_module_and_back_restores(cx: &mut TestAppContext) {
        LOG.with(|log| log.borrow_mut().clear());
        cx.update(|cx| {
            gpui_component::theme::init(cx);
            crate::settings::init(None, None, cx);
            crate::status::init(cx);
            crate::toasts::init(cx);
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
            crate::toasts::init(cx);
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
            crate::toasts::init(cx);
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
    /// T1 spike (docs/0059 §6/D2): upstream fires `on_close` on the
    /// autohide path too — its doc says "closed by any means" and every
    /// close path funnels through `complete_close`. That is exactly why
    /// the hub uses panel-managed read state (docs/0059 D2): a dismissal
    /// callback cannot tell a human ✕ from a timeout, and only human
    /// actions may mark read. This test is the rev-bump canary: if a
    /// future rev ever stops firing `on_close` on autohide, this fails
    /// and the exact human-✕→read mapping becomes available.
    #[gpui::test]
    fn autohide_fires_on_close_upstream_canary(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::settings::init(None, None, cx);
            crate::status::init(cx);
            crate::toasts::init(cx);
        });
        let nav = cx.update(|cx| cx.new(|_| Navigator::default()));
        let (_root, vcx) = {
            let nav = nav.clone();
            cx.add_window_view(|window, cx| {
                let registry =
                    ModuleRegistry::new().with::<FakeModule>(&nav, window, AppPaths::init(), cx);
                // Mirror main.rs: the shell view is wrapped in Root — the
                // forward path's `WindowExt` notification layer needs it.
                let shell = cx.new(|cx| {
                    DashboardShell::new(registry, nav, ShellState::default(), None, window, cx)
                });
                Root::new(shell, window, cx)
            })
        };

        let fired = Rc::new(std::cell::Cell::new(false));
        let flag = fired.clone();
        vcx.update(|window, cx| {
            window.push_notification(
                Notification::info("t1 autohide").on_close(move |_, _| flag.set(true)),
                cx,
            );
        });
        cx.run_until_parked();

        // The 5 s autohide plus the exit transition, with slack.
        cx.background_executor
            .advance_clock(std::time::Duration::from_secs(7));
        cx.run_until_parked();
        assert!(
            fired.get(),
            "upstream on_close must fire on autohide — if this ever fails, docs/0059 D2 can upgrade to the exact human-✕→read mapping"
        );
    }

    /// docs/0059 D3/D4: the rightmost bar slot is the notification
    /// indicator; opening the panel marks everything read (badge clears
    /// at open) and freezes the open-time snapshot so the list doesn't
    /// vanish mid-read.
    #[gpui::test]
    fn notification_indicator_opens_panel_and_marks_all_read(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::settings::init(None, None, cx);
            crate::status::init(cx);
            crate::toasts::init(cx);
        });
        let nav = cx.update(|cx| cx.new(|_| Navigator::default()));
        let (_root, vcx) = {
            let nav = nav.clone();
            cx.add_window_view(|window, cx| {
                let registry =
                    ModuleRegistry::new().with::<FakeModule>(&nav, window, AppPaths::init(), cx);
                let shell = cx.new(|cx| {
                    DashboardShell::new(registry, nav, ShellState::default(), None, window, cx)
                });
                Root::new(shell, window, cx)
            })
        };

        // Bare bell at zero unread (the badge number hides itself).
        assert!(
            vcx.debug_bounds("status-notifications").is_some(),
            "the bell trigger renders in the bar"
        );

        // Two raises land in the unread log.
        vcx.update(|_, cx| {
            crate::toasts::info(cx, "first");
            crate::toasts::error(cx, "second");
        });
        vcx.update(|_, cx| {
            let hub = crate::toasts::hub(cx);
            assert_eq!(hub.read(cx).unread_count(), 2);
        });
        vcx.update(|window, cx| window.draw(cx).clear(cx));

        // Clicking the bell opens the panel: badge clears at open, the
        // open-time snapshot stays listed.
        let bell = vcx
            .debug_bounds("status-notifications")
            .expect("bell bounds");
        vcx.simulate_click(bell.center(), Modifiers::default());
        vcx.update(|window, cx| window.draw(cx).clear(cx));
        vcx.update(|_, cx| {
            let hub = crate::toasts::hub(cx);
            assert_eq!(hub.read(cx).unread_count(), 0, "opening marks all read");
            assert!(hub.read(cx).panel_open());
            assert_eq!(hub.read(cx).panel_snapshot().len(), 2);
        });
        assert!(
            vcx.debug_bounds("notification-panel").is_some(),
            "the panel renders with the snapshot rows"
        );
    }

    /// docs/0060 D2: the gate is a pure function of window activation —
    /// focused raises stay in-app, unfocused ones also reach the OS
    /// notification center.
    #[test]
    fn os_delivery_gate_follows_window_activation() {
        assert!(!deliver_to_os(true));
        assert!(deliver_to_os(false));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn os_delivery_maps_the_gate_on_macos() {
        assert_eq!(os_delivery(true), NotificationDelivery::InApp);
        assert_eq!(os_delivery(false), NotificationDelivery::InAppAndSystem);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn os_delivery_stays_in_app_off_macos() {
        assert_eq!(os_delivery(true), NotificationDelivery::InApp);
        assert_eq!(os_delivery(false), NotificationDelivery::InApp);
    }

    /// docs/0060 D3 acceptance: a keyed raise drained while the window is
    /// unfocused lands in the OS notification center (macOS only; the
    /// test platform never activates windows, so the gate is open), and
    /// the banner click must mark the hub entry read. Mirrors upstream's
    /// notification.rs system tests: identity first, then replay the
    /// platform response with the shown notification's tag.
    #[cfg(target_os = "macos")]
    #[gpui::test]
    fn os_banner_click_marks_keyed_hub_entry_read(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::settings::init(None, None, cx);
            crate::status::init(cx);
            crate::toasts::init(cx);
            cx.set_app_identity("com.noosxe.my-dashboard", "My Dashboard");
        });
        let nav = cx.update(|cx| cx.new(|_| Navigator::default()));
        let (_root, vcx) = {
            let nav = nav.clone();
            cx.add_window_view(|window, cx| {
                let registry =
                    ModuleRegistry::new().with::<FakeModule>(&nav, window, AppPaths::init(), cx);
                let shell = cx.new(|cx| {
                    DashboardShell::new(registry, nav, ShellState::default(), None, window, cx)
                });
                Root::new(shell, window, cx)
            })
        };

        // Unfocused drain: the test window never activates, so the gate
        // (docs/0060 D2) is open and the post goes to the OS center.
        vcx.update(|_, cx| {
            crate::toasts::raise(
                cx,
                crate::toasts::Toast::new(crate::toasts::Level::Warning, "token expired")
                    .title("GitHub")
                    .dedupe_key("github-auth-expired"),
            );
        });
        vcx.update(|window, cx| window.draw(cx).clear(cx));

        let shown = vcx.shown_system_notifications();
        assert_eq!(shown.len(), 1, "the unfocused drain must post once");
        let tag = shown[0].tag.clone();

        vcx.simulate_system_notification_response(gpui::SystemNotificationResponse {
            tag,
            action_id: None,
        });
        vcx.run_until_parked();

        vcx.update(|_, cx| {
            let hub = crate::toasts::hub(cx);
            let entries = hub.read(cx).log();
            assert_eq!(entries.len(), 1);
            assert!(
                entries[0].read,
                "a banner click is a human action (docs/0059 D2 / 0060 D3)"
            );
        });
    }
}
