//! OS-appropriate storage roots (docs/0004 §4.1).
//!
//! Linux: the four XDG roots (`$XDG_{CONFIG,DATA,STATE,CACHE}_HOME`).
//! macOS: native locations — config/data in `~/Library/Application
//! Support`, cache in `~/Library/Caches`; state has no native slot
//! (`dirs::state_dir()` is `None` there), so it falls back to the data
//! root (owner decision 0004 Q2 — native locations, not Zed's
//! `~/.local/state` deviation).
//!
//! `DASHBOARD_STATE_DIR` redirects all roots under itself
//! (`{root}/{config,data,state,cache,runtime}`) for tests, dev runs and
//! Nix isolation — same shape as Zed's `set_custom_data_dir`.
//!
//! The runtime root (docs/0010 D2) is the volatile per-user slot for the
//! debug bridge's socket + token: `$XDG_RUNTIME_DIR/my-dashboard` on Linux,
//! with the state root as fallback when the session has no runtime dir.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const APP_DIR_NAME: &str = "my-dashboard";
const STATE_DIR_ENV: &str = "DASHBOARD_STATE_DIR";

pub struct AppPaths {
    config: PathBuf,
    data: PathBuf,
    state: PathBuf,
    cache: PathBuf,
    runtime: PathBuf,
}

impl AppPaths {
    /// Resolve once at startup, before any entity is built. Panics only
    /// if the OS cannot answer where the user's home is — same stance
    /// as Zed's `crates/paths`.
    pub fn init() -> &'static AppPaths {
        static PATHS: OnceLock<AppPaths> = OnceLock::new();
        PATHS.get_or_init(|| {
            let override_dir = std::env::var_os(STATE_DIR_ENV).filter(|v| !v.is_empty());
            Self::resolve(override_dir.map(PathBuf::from))
        })
    }

    /// Pure core (unit-testable without touching process env).
    fn resolve(custom_root: Option<PathBuf>) -> AppPaths {
        if let Some(root) = custom_root {
            return AppPaths {
                config: root.join("config"),
                data: root.join("data"),
                state: root.join("state"),
                cache: root.join("cache"),
                runtime: root.join("runtime"),
            };
        }

        #[cfg(target_os = "macos")]
        {
            // Native mapping (0004 Q2): Application Support holds both
            // config and data; state has no macOS convention → data root.
            let app_support = dirs::data_local_dir()
                .expect("failed to resolve ~/Library/Application Support")
                .join(APP_DIR_NAME);
            let cache = dirs::cache_dir()
                .expect("failed to resolve ~/Library/Caches")
                .join(APP_DIR_NAME);
            AppPaths {
                config: app_support.clone(),
                data: app_support.clone(),
                state: app_support.clone(),
                cache,
                // No macOS runtime convention either — state-root fallback
                // (docs/0010 D2; macOS support is deferred anyway).
                runtime: app_support.join("runtime"),
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            // XDG platforms (Linux; sane stand-ins anywhere else if ever
            // compiled). `state_dir()` is Linux-only in `dirs`, hence the
            // data-root fallback for the non-macOS/non-XDG remainder.
            let state_root = dirs::state_dir()
                .or_else(dirs::data_local_dir)
                .expect("failed to resolve XDG state home");
            let state = state_root.join(APP_DIR_NAME);
            // Volatile per-user slot (docs/0010 D2): XDG runtime dir when the
            // session provides one (SSH sessions sometimes don't), state
            // root otherwise.
            let runtime = std::env::var_os("XDG_RUNTIME_DIR")
                .filter(|v| !v.is_empty())
                .map(|v| PathBuf::from(v).join(APP_DIR_NAME))
                .unwrap_or_else(|| state.join("runtime"));
            AppPaths {
                config: dirs::config_dir()
                    .expect("failed to resolve XDG config home")
                    .join(APP_DIR_NAME),
                data: dirs::data_local_dir()
                    .expect("failed to resolve XDG data home")
                    .join(APP_DIR_NAME),
                state,
                cache: dirs::cache_dir()
                    .expect("failed to resolve XDG cache home")
                    .join(APP_DIR_NAME),
                runtime,
            }
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config
    }

    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    /// Log directory `<data>/logs` (docs/0008 D4) — app-level diagnostics,
    /// not module state and not re-derivable cache; bounded by the rotation
    /// cap. Created by the logging init (flexi_logger `FileSpec` creates the
    /// directory on demand).
    pub fn logs_dir(&self) -> PathBuf {
        self.data.join("logs")
    }

    pub fn state_dir(&self) -> &Path {
        &self.state
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }

    /// Volatile per-user root for the debug bridge socket + token
    /// (docs/0010 D2). Created by the debug server on demand.
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime
    }

    /// App settings file (`<state>/settings.json`, docs/0006 §4.4).
    pub fn settings_file(&self) -> PathBuf {
        self.state.join("settings.json")
    }

    /// User theme files (`<config>/themes/`, docs/0025 D1). Created on
    /// demand by the theme watcher.
    pub fn themes_dir(&self) -> PathBuf {
        self.config.join("themes")
    }

    /// Shell UI state file (`<state>/shell.json`, docs/0004 §4.3).
    pub fn shell_state_file(&self) -> PathBuf {
        self.state.join("shell.json")
    }
    /// Per-module data directory `<data>/modules/<sanitized-id>/`,
    /// created on demand. Ids are stable module keys, not user input,
    /// but sanitize anyway so a bad id can never escape the root.
    pub fn module_data_dir(&self, id: &str) -> PathBuf {
        let dir = self.data.join("modules").join(sanitize_id(id));
        if let Err(err) = std::fs::create_dir_all(&dir) {
            log::warn!("failed to create module data dir {}: {err}", dir.display());
        }
        dir
    }

    /// Per-module settings directory `<state>/modules/<sanitized-id>/`
    /// (docs/0006 Q3) — settings are user state, not data; mirror of
    /// [`Self::module_data_dir`], created on demand.
    pub fn module_state_dir(&self, id: &str) -> PathBuf {
        let dir = self.state.join("modules").join(sanitize_id(id));
        if let Err(err) = std::fs::create_dir_all(&dir) {
            log::warn!("failed to create module state dir {}: {err}", dir.display());
        }
        dir
    }
}

/// `[A-Za-z0-9-]`; everything else collapses to `-`.
fn sanitize_id(id: &str) -> String {
    let sanitized: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "module".into()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_redirects_all_roots() {
        let paths = AppPaths::resolve(Some(PathBuf::from("/tmp/dash-test-root")));
        assert_eq!(paths.config_dir(), Path::new("/tmp/dash-test-root/config"));
        assert_eq!(paths.data_dir(), Path::new("/tmp/dash-test-root/data"));
        assert_eq!(paths.state_dir(), Path::new("/tmp/dash-test-root/state"));
        assert_eq!(paths.cache_dir(), Path::new("/tmp/dash-test-root/cache"));
        assert_eq!(
            paths.runtime_dir(),
            Path::new("/tmp/dash-test-root/runtime")
        );
    }

    #[test]
    fn logs_dir_lives_under_data_root() {
        // docs/0008 D4: app-level diagnostics, deliberately under data (not
        // state or cache — logs are neither user state nor re-derivable).
        let paths = AppPaths::resolve(Some(PathBuf::from("/tmp/dash-test-root")));
        assert_eq!(paths.logs_dir(), Path::new("/tmp/dash-test-root/data/logs"));
    }

    #[test]
    fn shell_state_file_lives_in_state_root() {
        let paths = AppPaths::resolve(Some(PathBuf::from("/tmp/dash-test-root")));
        assert_eq!(
            paths.shell_state_file(),
            Path::new("/tmp/dash-test-root/state/shell.json")
        );
    }

    #[test]
    fn settings_file_lives_in_state_root() {
        let paths = AppPaths::resolve(Some(PathBuf::from("/tmp/dash-test-root")));
        assert_eq!(
            paths.settings_file(),
            Path::new("/tmp/dash-test-root/state/settings.json")
        );
    }

    #[test]
    fn module_state_dir_mirrors_data_dir_layout() {
        let root = std::env::temp_dir().join(format!(
            "dash-paths-test-module-state-{}",
            std::process::id()
        ));
        let paths = AppPaths::resolve(Some(root.clone()));
        let dir = paths.module_state_dir("../notes evil/..");
        assert!(dir.starts_with(root.join("state/modules")));
        assert_eq!(
            dir.file_name().unwrap().to_str().unwrap(),
            "---notes-evil---"
        );
        assert!(dir.is_dir(), "module state dir must be created on demand");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn module_data_dir_is_sanitized_and_created() {
        let root = std::env::temp_dir().join(format!(
            "dash-paths-test-module-data-{}",
            std::process::id()
        ));
        let paths = AppPaths::resolve(Some(root.clone()));
        let dir = paths.module_data_dir("../notes evil/..");
        assert!(dir.starts_with(root.join("data/modules")));
        assert_eq!(
            dir.file_name().unwrap().to_str().unwrap(),
            "---notes-evil---"
        );
        assert!(dir.is_dir(), "module data dir must be created on demand");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn empty_id_sanitizes_to_module() {
        assert_eq!(sanitize_id(""), "module");
        assert_eq!(sanitize_id("---"), "---");
    }
}
