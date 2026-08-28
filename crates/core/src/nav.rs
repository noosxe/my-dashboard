//! Navigation state (docs/0003 §4.3): single source of truth for the shell's
//! chrome — active page, sidebar collapse, breadcrumb crumbs.

use std::sync::Arc;

use gpui::{App, Context, SharedString, Window};

/// Top-level destination. Sub-pages live inside a module's main page and are
/// surfaced via [`Navigator::set_crumbs`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    Home,
    Module(SharedString),
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

impl Navigator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Switch top-level page (clears crumbs — sub-pages do not survive
    /// leaving their module).
    pub fn go(&mut self, page: Page, cx: &mut Context<Self>) {
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
