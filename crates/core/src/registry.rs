//! Compile-time module registry (docs/0003 §4.2). The crate graph is the
//! plugin system; this is just the ordered, indexed list built at startup.

use std::collections::HashMap;

use gpui::{App, SharedString, Window};

use gpui_component::setting::SettingPage;

use crate::nav::Navigator;
use crate::paths::AppPaths;
use crate::{Module, ModuleEntry};
pub struct ModuleRegistry {
    entries: Vec<ModuleEntry>,
    index: HashMap<SharedString, usize>,
}

impl Default for ModuleRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ModuleRegistry {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Builder-style registration; call order = sidebar + home-grid order.
    pub fn with<M: Module>(
        mut self,
        nav: &gpui::Entity<Navigator>,
        window: &mut Window,
        paths: &AppPaths,
        cx: &mut App,
    ) -> Self {
        let entry = M::init(nav, window, paths, cx);
        self.index.insert(entry.meta.id.clone(), self.entries.len());
        self.entries.push(entry);
        self
    }

    pub fn entries(&self) -> &[ModuleEntry] {
        &self.entries
    }

    pub fn get(&self, id: &SharedString) -> Option<&ModuleEntry> {
        self.index.get(id).map(|&ix| &self.entries[ix])
    }

    /// Settings subpages from modules that opted in (docs/0006 §4.3),
    /// in registration order (= sidebar order). Providers run per call
    /// and must be cheap — one entity read each.
    pub fn settings_pages(&self, window: &mut Window, cx: &mut App) -> Vec<SettingPage> {
        self.entries
            .iter()
            .filter_map(|entry| {
                entry
                    .settings
                    .as_ref()
                    .and_then(|provider| provider(window, cx))
            })
            .collect()
    }
}
