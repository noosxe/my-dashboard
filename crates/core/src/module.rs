//! The module contract (docs/0003 §4.1).
//!
//! A module is a domain unit: a service layer, a single state entity, and two
//! presentation faces (overview card + main page) that both observe the state.

use std::rc::Rc;

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
