//! Spec persistence (docs/0064 D8): `state/favicon.json` — the whole
//! [`DesignSpec`], written through core's trailing-edge
//! [`DebouncedSave`] (the drafts pattern, docs/0045). User preference,
//! not log data — deliberately off the app-DB rail (0063 §9).

use std::path::PathBuf;

use dashboard_core::paths::AppPaths;
use dashboard_core::store::{DebouncedSave, load_json};
use gpui::App;

use crate::spec::DesignSpec;

/// The house debounce delay (shell.json / settings.json / drafts
/// precedent).
const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);

/// The module's single-file store.
pub struct SpecStore {
    path: PathBuf,
    save: DebouncedSave,
}

impl SpecStore {
    pub fn new(paths: &AppPaths) -> Self {
        Self::at(paths.state_dir().join("favicon.json"), true)
    }

    /// Test seam (drafts precedent): an isolated root that never joins
    /// the quit-time flush sweep.
    pub fn at_tmp(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("favicon-state-{tag}"));
        let _ = std::fs::create_dir_all(&dir);
        Self::at(dir.join("favicon.json"), false)
    }

    fn at(path: PathBuf, flush_on_quit: bool) -> Self {
        let save = DebouncedSave::new(path.clone(), DEBOUNCE);
        if flush_on_quit {
            save.register_flush_on_quit();
        }
        Self { path, save }
    }

    /// The saved design, or the default when nothing/stale/broken is on
    /// disk (the 0062 log-and-default failure policy).
    pub fn load(&self) -> DesignSpec {
        load_json::<DesignSpec>(&self.path)
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Schedule (or reschedule) the debounced save.
    pub fn schedule(&self, spec: &DesignSpec, cx: &App) {
        self.save.schedule(spec.clone(), cx);
    }

    /// On-disk path — test seam.
    #[cfg(test)]
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_default_on_missing() {
        let store = SpecStore::at_tmp("roundtrip");
        // Nothing on disk: the default design, never an error.
        assert_eq!(store.load(), DesignSpec::default());

        let spec = DesignSpec {
            app_name: "Dashboard".into(),
            export_dir: Some("/tmp/icons".into()),
            ..DesignSpec::default()
        };
        dashboard_core::store::save_json_atomic(store.path(), &spec).unwrap();
        assert_eq!(store.load(), spec);

        // A stale v1 file (missing every later field) still loads.
        std::fs::write(store.path(), "{}").unwrap();
        assert_eq!(store.load(), DesignSpec::default());

        let _ = std::fs::remove_dir_all(store.path().parent().unwrap());
    }
}
