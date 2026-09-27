//! my-dashboard core: module framework, navigation, home grid, app shell.

pub mod data_color;
pub mod freshness;
pub mod home;
pub mod logging;
pub mod module;
pub mod nav;
pub mod paths;
pub mod registry;
pub mod settings;
pub mod shell;
pub mod status;
pub mod store;
pub mod toasts;
pub mod usage_analytics;
pub mod usage_records;
pub mod usage_sqlite;

pub use module::{Module, ModuleEntry, ModuleMeta};
pub use nav::{Crumb, Navigator, Page};
pub use paths::AppPaths;
pub use registry::ModuleRegistry;
pub use shell::{DashboardShell, ShellState};
pub use store::{DebouncedSave, load_json, save_json_atomic};
