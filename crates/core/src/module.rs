//! The module contract (docs/0003 §4.1).
//!
//! A module is a domain unit: a service layer, a single state entity, and two
//! presentation faces (overview card + main page) that both observe the state.

use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AnyView, App, SharedString, Window};

use gpui_component::{IconName, setting::SettingPage};

use crate::nav::Navigator;
use crate::paths::AppPaths;
/// Sidebar icon source for a module. `BuiltIn` covers gpui-component's fixed
/// `IconName` set; `Custom` names an SVG asset path resolved through the
/// app's `AssetSource` — how brand marks (e.g. GitHub's octicon) reach the
/// sidebar despite not existing in the default set.
#[derive(Clone)]
pub enum ModuleIcon {
    BuiltIn(IconName),
    Custom(SharedString),
}

/// Static identity — known before instantiation; drives sidebar, home grid
/// and navigation keys.
pub struct ModuleMeta {
    /// Stable id ("clock") — used as the `Page::Module` nav key.
    pub id: SharedString,
    /// Display title ("Clock") — sidebar item + breadcrumb.
    pub title: SharedString,
    /// Sidebar icon; `None` renders a label-only item.
    pub icon: Option<ModuleIcon>,
}

/// Builds a module's settings page (docs/0006 §4.3). Called per render of
/// the settings shell, so returned pages always reflect live module state.
/// Returning `None` = nothing to show right now. Built inside a module's
/// `init`, capturing its state entity.
pub type SettingsProvider = Rc<dyn Fn(&mut Window, &mut App) -> Option<SettingPage>>;

/// History support for a module (docs/0048 D3): how the shell
/// re-activates a trail entry the module recorded. The impl captures the
/// module's entities; the payload is whatever the module handed to
/// [`crate::nav::Route::module_place`] — only the owning module ever
/// downcasts it, and a stale payload must degrade to the module root
/// rather than panic.
pub trait ModuleNav: 'static {
    /// Activate a payload previously handed to `visit` — the same code
    /// path as the user clicking (0031 dedup and caches apply).
    fn restore(&self, payload: &Arc<dyn Any + Send + Sync>, window: &mut Window, cx: &mut App);
    /// Tear down to the module root: the LCA collapse of a cross-branch
    /// jump (docs/0048 D6), and the landing for root routes on Back.
    fn collapse(&self, window: &mut Window, cx: &mut App);
}

/// What a module hands to the registry: identity + its two views, both
/// type-erased and both reading the module's single state entity.
pub struct ModuleEntry {
    pub meta: ModuleMeta,
    /// Home-page overview card content.
    pub overview: AnyView,
    /// Navigation destination (the module's main page).
    pub main_page: AnyView,
    /// Settings subpage (docs/0006); `None` = module ships no settings page.
    pub settings: Option<SettingsProvider>,
    /// History support (docs/0048 D3); `None` = module has no trail
    /// places yet — it is never asked to restore or collapse.
    pub nav: Option<Arc<dyn ModuleNav>>,
}

/// Implemented by module crates; registered statically in `main.rs`.
pub trait Module: 'static {
    fn meta() -> ModuleMeta;

    /// Runs once at startup: build the state entity, spawn services
    /// (timers/network — services write only to the state entity), and
    /// return the two views.
    fn init(
        nav: &gpui::Entity<Navigator>,
        window: &mut Window,
        paths: &AppPaths,
        cx: &mut App,
    ) -> ModuleEntry;
}
