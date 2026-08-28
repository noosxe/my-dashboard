//! The module contract (docs/0003 §4.1).
//!
//! A module is a domain unit: a service layer, a single state entity, and two
//! presentation faces (overview card + main page) that both observe the state.

use gpui::{AnyView, App, SharedString, Window};

use gpui_component::IconName;

use crate::nav::Navigator;

/// Static identity — known before instantiation; drives sidebar, home grid
/// and navigation keys.
pub struct ModuleMeta {
    /// Stable id ("clock") — used as the `Page::Module` nav key.
    pub id: SharedString,
    /// Display title ("Clock") — sidebar item + breadcrumb.
    pub title: SharedString,
    /// Sidebar icon from the default set; `None` renders a label-only item.
    pub icon: Option<IconName>,
}

/// What a module hands to the registry: identity + its two views, both
/// type-erased and both reading the module's single state entity.
pub struct ModuleEntry {
    pub meta: ModuleMeta,
    /// Home-page overview card content.
    pub overview: AnyView,
    /// Navigation destination (the module's main page).
    pub main_page: AnyView,
}

/// Implemented by module crates; registered statically in `main.rs`.
pub trait Module: 'static {
    fn meta() -> ModuleMeta;

    /// Runs once at startup: build the state entity, spawn services
    /// (timers/network — services write only to the state entity), and
    /// return the two views.
    fn init(nav: &gpui::Entity<Navigator>, window: &mut Window, cx: &mut App) -> ModuleEntry;
}
