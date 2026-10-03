//! Favicon generator module (docs/0064) — the first producer module:
//! an offline tool that turns a glyph, emoji, image or SVG into a
//! platform-correct favicon/app-icon set. Everything rides house rails:
//! the module registry, `state/favicon.json` via the core store, the
//! toast hub, the usage rail (`Transport::Local`, D9), and the debug
//! bridge ids (D10).

pub mod export;
pub mod render;
pub mod spec;
pub mod state;

mod card;
mod page;

use dashboard_core::Module;
use dashboard_core::module::{ModuleEntry, ModuleIcon, ModuleMeta};
use dashboard_core::nav::Navigator;
use dashboard_core::paths::AppPaths;
use dashboard_core::usage_records::Recorder;
use gpui::{App, AppContext as _, Entity, Window};

pub struct FaviconModule;

impl Module for FaviconModule {
    fn meta() -> ModuleMeta {
        ModuleMeta {
            id: "favicon".into(),
            title: "Favicon".into(),
            icon: Some(ModuleIcon::BuiltIn(gpui_component::IconName::Palette)),
        }
    }

    fn init(
        nav: &Entity<Navigator>,
        window: &mut Window,
        paths: &AppPaths,
        cx: &mut App,
    ) -> ModuleEntry {
        // Toast source (docs/0062 D2): every favicon raise attributes
        // here; the settings page can mute it by id.
        dashboard_core::toasts::register_source("favicon", "Favicon", cx);

        // The single state entity (docs/0003 §4.1): the spec store +
        // the shared model. The recorder rides the app-wide sink
        // (docs/0053 D3) with the module id baked in.
        let store = state::SpecStore::new(paths);
        let recorder = Recorder::new("favicon", dashboard_core::usage_records::sink(cx));
        let model = cx.new(|cx| page::FaviconModel::new(store, recorder, window, cx));
        // Initial preview build (the entity exists now).
        model.update(cx, |m, cx| m.boot(cx));

        let overview = cx.new(|cx| card::FaviconCard::new(model.clone(), nav.clone(), cx));
        let main_page = cx.new(|cx| page::FaviconPage::new(model.clone(), window, cx));

        ModuleEntry {
            meta: Self::meta(),
            overview: overview.into(),
            main_page: main_page.into(),
            // No settings subpage and no nav-trail places in v1 (D1):
            // a single-page surface.
            settings: None,
            nav: None,
            status_item: None,
        }
    }
}
