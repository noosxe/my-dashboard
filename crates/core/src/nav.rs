//! Navigation state (docs/0003 §4.3): single source of truth for the shell's
//! chrome — active page, sidebar collapse, breadcrumb crumbs.

use gpui::{App, Context, EventEmitter, SharedString, Window};
use std::sync::Arc;

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
}

impl Default for Navigator {
    fn default() -> Self {
        Self {
            sidebar_collapsed: false,
            page: Page::Home,
            crumbs: Vec::new(),
        }
    }
}

impl EventEmitter<NavEvent> for Navigator {}
impl Navigator {
    /// `sidebar_collapsed` seeds from persisted shell state (docs/0004
    /// §4.3); `default()` keeps `false`.
    pub fn new(sidebar_collapsed: bool) -> Self {
        Self {
            sidebar_collapsed,
            page: Page::Home,
            crumbs: Vec::new(),
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
        cx.notify();
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
}
