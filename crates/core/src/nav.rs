//! Navigation state (docs/0003 §4.3): single source of truth for the shell's
//! chrome — active page, sidebar collapse, breadcrumb crumbs.

use gpui::{App, Context, EventEmitter, SharedString, Window, actions};
use gpui_base::History;
use std::any::Any;
use std::sync::Arc;

actions!(
    nav,
    [
        /// Navigates one entry back on the visit trail (docs/0048 D5).
        Back,
        /// Navigates one entry forward on the visit trail.
        Forward,
    ]
);

/// Why the navigator touched the chrome. Emitted by [`Navigator::go`]
/// when the target page is already on screen (docs/0014 §5 D7): nav
/// state itself has nothing to change, but the module owning that
/// page can answer the re-entry by returning to its root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavEvent {
    /// `go` targeted the page that was already current — the module
    /// crumb clicked from inside the module.
    Reentered,
}

/// Top-level destination. Sub-pages live inside a module's main page and are
/// surfaced via [`Navigator::set_crumbs`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    Home,
    Module(SharedString),
    /// App settings (docs/0006) — hosted by the shell, subpage selection
    /// lives inside the `Settings` component, not in nav state.
    Settings,
}

/// One position on the visit trail (docs/0048 D1): the top-level page,
/// plus for module pages the owning module and its opaque position
/// payload. Only the owning module ever downcasts the payload (entries
/// carry the module id), so nav state stays module-agnostic and nothing
/// needs serialization — the trail is session-only.
#[derive(Clone)]
pub struct Route {
    /// Stable dedup identity (docs/0048 D4): shell pages derive it from
    /// the page; modules hand theirs to [`Route::module_place`].
    pub(crate) key: SharedString,
    pub page: Page,
    pub(crate) module: Option<(SharedString, Arc<dyn Any + Send + Sync>)>,
}

impl Route {
    pub fn home() -> Self {
        Self {
            key: "page:home".into(),
            page: Page::Home,
            module: None,
        }
    }

    pub fn settings() -> Self {
        Self {
            key: "page:settings".into(),
            page: Page::Settings,
            module: None,
        }
    }

    /// A module's root (its plain landing view) — no payload to restore.
    pub fn module_root(id: impl Into<SharedString>) -> Self {
        let id = id.into();
        Self {
            key: format!("mod:{id}").into(),
            page: Page::Module(id),
            module: None,
        }
    }

    /// A deep module place. `key` is the module's own identity for the
    /// place (`"repo:acme/one"`); the trail namespaces it with the module
    /// id so different modules can't collide.
    pub fn module_place(
        id: impl Into<SharedString>,
        key: impl Into<SharedString>,
        payload: Arc<dyn Any + Send + Sync>,
    ) -> Self {
        let id = id.into();
        Self {
            key: format!("{}:{}", id, key.into()).into(),
            page: Page::Module(id.clone()),
            module: Some((id, payload)),
        }
    }

    fn for_page(page: &Page) -> Self {
        match page {
            Page::Home => Self::home(),
            Page::Settings => Self::settings(),
            Page::Module(id) => Self::module_root(id.clone()),
        }
    }
}

/// Back-navigation callback stored in a [`Crumb`] (`Arc` so the shell can
/// clone it into render closures each frame).
pub type CrumbAction = Arc<dyn Fn(&mut Window, &mut App)>;

/// One breadcrumb entry reported by a module.
pub struct Crumb {
    pub label: SharedString,
    /// Module-provided back-nav; `None` renders a non-clickable crumb.
    pub on_click: Option<CrumbAction>,
}

impl Crumb {
    /// Non-clickable informational crumb.
    pub fn label(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            on_click: None,
        }
    }
}

/// Owned by the shell; modules get a clone in [`crate::Module::init`].
pub struct Navigator {
    pub sidebar_collapsed: bool,
    pub page: Page,
    crumbs: Vec<Crumb>,
    /// The visit trail (docs/0048 D1) — seeded with Home, capped at 200
    /// entries (D8). Not persisted: back/forward is a session concern.
    history: History<Route>,
}

/// Trail capacity (docs/0048 D8): generous for a session, trivial
/// memory — routes are small descriptors.
const HISTORY_CAP: usize = 200;

impl Default for Navigator {
    fn default() -> Self {
        Self::new(false)
    }
}

impl EventEmitter<NavEvent> for Navigator {}
impl Navigator {
    /// `sidebar_collapsed` seeds from persisted shell state (docs/0004
    /// §4.3); `default()` keeps `false`.
    pub fn new(sidebar_collapsed: bool) -> Self {
        // Seeded with Home (docs/0048 D1): launch is a position too.
        let mut history = History::new().max_entries(HISTORY_CAP);
        history.push(Route::home());
        Self {
            sidebar_collapsed,
            page: Page::Home,
            crumbs: Vec::new(),
            history,
        }
    }

    /// Switch top-level page (clears crumbs — sub-pages do not survive
    /// leaving their module). A `go` targeting the page already current
    /// changes nothing in nav state but emits [`NavEvent::Reentered`] —
    /// the module crumb clicked from inside the module is a "back to
    /// the module root" request the owning module answers (docs/0014
    /// §5 D7, issue #100).
    pub fn go(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.page == page {
            cx.emit(NavEvent::Reentered);
        }
        self.page = page;
        self.crumbs.clear();
        self.visit(Route::for_page(&self.page), cx);
    }

    /// Records a user-driven visit (docs/0048 D3/D4). A visit whose key
    /// equals the trail head's is ignored entirely — no push, and no
    /// spurious forward-tail truncation. Any other visit pushes, which
    /// truncates the forward tail (the browser rule, `History::push`).
    /// Module places reach this through the module page's own
    /// place-change handlers; the shell reaches it through `go`.
    pub fn visit(&mut self, route: Route, cx: &mut Context<Self>) {
        if self
            .history
            .current()
            .is_some_and(|head| head.key == route.key)
        {
            return;
        }
        self.history.push(route);
        cx.notify();
    }

    /// Moves the cursor one entry back and returns the route that is now
    /// current (docs/0048 D5) — the caller re-activates it: the page is
    /// already switched here, the shell then runs the owning module's
    /// restore. `None` at the trail's start.
    pub fn back(&mut self, cx: &mut Context<Self>) -> Option<Route> {
        let route = self.history.back()?;
        self.page = route.page.clone();
        self.crumbs.clear();
        cx.notify();
        Some(route)
    }

    /// The `back` mirror (docs/0048 D5): cursor forward one entry, page
    /// switched, route returned for the module restore.
    pub fn forward(&mut self, cx: &mut Context<Self>) -> Option<Route> {
        let route = self.history.forward()?;
        self.page = route.page.clone();
        self.crumbs.clear();
        cx.notify();
        Some(route)
    }

    pub fn can_back(&self) -> bool {
        self.history.can_back()
    }

    pub fn can_forward(&self) -> bool {
        self.history.can_forward()
    }

    /// Replace the breadcrumb crumbs for the current module page.
    pub fn set_crumbs(&mut self, crumbs: Vec<Crumb>, cx: &mut Context<Self>) {
        self.crumbs = crumbs;
        cx.notify();
    }

    pub fn crumbs(&self) -> &[Crumb] {
        &self.crumbs
    }

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        cx.notify();
    }
}

impl Navigator {
    /// The trail head's key (docs/0048) — crumbs' sibling identity, and
    /// the observable the history tests assert on.
    pub fn history_entries_last_key(&self) -> String {
        self.history
            .current()
            .map(|r| r.key.to_string())
            .unwrap_or_default()
    }
}

impl Route {
    /// The route's dedup identity (docs/0048 D4).
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Read-only payload access for restore callers that re-dispatch
    /// through [`ModuleNav::restore`] (the shell holds the whole route).
    pub fn payload(&self) -> &Arc<dyn Any + Send + Sync> {
        self.module
            .as_ref()
            .map(|(_, payload)| payload)
            .expect("route carries a module payload")
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};

    use super::{NavEvent, Navigator, Page};

    /// Subscription holder — collects `NavEvent`s the way a module page
    /// would receive them (docs/0014 §5 D7).
    struct Recorder {
        events: Vec<NavEvent>,
    }

    impl Recorder {
        fn new(nav: &gpui::Entity<Navigator>, cx: &mut gpui::Context<Self>) -> Self {
            cx.subscribe(nav, |this, _, event: &NavEvent, _| {
                this.events.push(event.clone());
            })
            .detach();
            Self { events: Vec::new() }
        }
    }

    #[gpui::test]
    fn go_emits_reentered_only_when_target_is_current(cx: &mut TestAppContext) {
        let (nav, rec) = cx.update(|cx| {
            let nav = cx.new(|_| Navigator::default());
            let rec = cx.new(|cx| Recorder::new(&nav, cx));
            (nav, rec)
        });
        let github = Page::Module("github".into());
        // Page switch from Home: no event.
        cx.update(|cx| nav.update(cx, |nav, cx| nav.go(github.clone(), cx)));
        // Same-page go (module crumb clicked mid-module): Reentered.
        cx.update(|cx| nav.update(cx, |nav, cx| nav.go(github.clone(), cx)));
        // Leaving the module: a page switch, not a re-entry.
        cx.update(|cx| nav.update(cx, |nav, cx| nav.go(Page::Home, cx)));
        // Crumb writes never re-enter.
        cx.update(|cx| {
            nav.update(cx, |nav, cx| nav.set_crumbs(Vec::new(), cx));
        });
        let events = cx.update(|cx| rec.read(cx).events.clone());
        assert_eq!(
            events,
            vec![NavEvent::Reentered],
            "exactly the same-page go emits Reentered"
        );
    }

    // --- Visit trail (docs/0048) ----------------------------------------

    use crate::nav::Route;
    use std::sync::Arc;

    fn place(key: &str) -> Route {
        Route::module_place("fake", key, Arc::new(key.to_string()))
    }

    #[gpui::test]
    fn go_records_pages_and_visit_dedups_by_key(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let nav = cx.new(|_| Navigator::default());
            nav.update(cx, |nav, _| {
                assert_eq!(nav.history.entries().len(), 1, "seeded with Home");
            });
            nav.update(cx, |nav, cx| nav.go(Page::Module("fake".into()), cx));
            nav.update(cx, |nav, cx| nav.visit(place("repo:a"), cx));
            // Same-key visit: ignored entirely — no push, and (docs/0048
            // D4) no spurious forward-tail truncation.
            nav.update(cx, |nav, cx| nav.visit(place("repo:a"), cx));
            nav.update(cx, |nav, _| {
                let keys: Vec<_> = nav.history.entries().map(|r| r.key.clone()).collect();
                assert_eq!(keys, ["page:home", "mod:fake", "fake:repo:a"]);
            });
        });
    }

    #[gpui::test]
    fn back_forward_move_cursor_and_switch_page(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let nav = cx.new(|_| Navigator::default());
            nav.update(cx, |nav, cx| nav.go(Page::Module("fake".into()), cx));
            nav.update(cx, |nav, cx| nav.visit(place("repo:a"), cx));
            nav.update(cx, |nav, cx| nav.visit(place("repo:b"), cx));

            // back/forward return the route they LAND on (the one the
            // shell re-activates), not the one left behind.
            let back = nav.update(cx, |nav, cx| nav.back(cx)).unwrap();
            assert_eq!(back.key, "fake:repo:a");
            nav.read_with(cx, |nav, _| {
                assert!(matches!(nav.page, Page::Module(_)));
                assert!(nav.can_back() && nav.can_forward());
            });

            let back2 = nav.update(cx, |nav, cx| nav.back(cx)).unwrap();
            assert_eq!(back2.key, "mod:fake");
            let fwd = nav.update(cx, |nav, cx| nav.forward(cx)).unwrap();
            assert_eq!(fwd.key, "fake:repo:a");

            // Cursor exhausted both ways.
            nav.update(cx, |nav, cx| {
                nav.forward(cx).unwrap();
                assert!(nav.forward(cx).is_none());
                nav.back(cx).unwrap();
                nav.back(cx).unwrap();
                nav.back(cx).unwrap();
                assert!(nav.back(cx).is_none());
            });
        });
    }

    #[gpui::test]
    fn visit_after_back_truncates_forward(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let nav = cx.new(|_| Navigator::default());
            nav.update(cx, |nav, cx| nav.go(Page::Module("fake".into()), cx));
            nav.update(cx, |nav, cx| nav.visit(place("a"), cx));
            nav.update(cx, |nav, cx| nav.visit(place("b"), cx));
            nav.update(cx, |nav, cx| {
                nav.back(cx).unwrap();
                assert_eq!(nav.history.forward_entries().count(), 1);
            });
            // A fresh visit while mid-trail: the forward branch dies.
            nav.update(cx, |nav, cx| nav.visit(place("c"), cx));
            nav.update(cx, |nav, _| {
                assert_eq!(nav.history.forward_entries().count(), 0);
                let keys: Vec<_> = nav.history.entries().map(|r| r.key.clone()).collect();
                assert_eq!(keys, ["page:home", "mod:fake", "fake:a", "fake:c"]);
            });
        });
    }

    #[gpui::test]
    fn trail_caps_at_200(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let nav = cx.new(|_| Navigator::default());
            nav.update(cx, |nav, cx| {
                for i in 0..250 {
                    nav.visit(place(&format!("p{i}")), cx);
                }
            });
            nav.read_with(cx, |nav, _| {
                assert_eq!(nav.history.entries().len(), 200);
            });
        });
    }
}
