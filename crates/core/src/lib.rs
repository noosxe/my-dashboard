//! my-dashboard core: module framework, navigation, home grid, app shell.

pub mod home;
pub mod module;
pub mod nav;
pub mod paths;
pub mod registry;
pub mod shell;
pub mod store;

pub use module::{Module, ModuleEntry, ModuleMeta};
pub use nav::{Crumb, Navigator, Page};
pub use paths::AppPaths;
pub use registry::ModuleRegistry;
pub use shell::{DashboardShell, ShellState, ToggleTitleBar};
pub use store::{DebouncedSave, load_json, save_json_atomic};
