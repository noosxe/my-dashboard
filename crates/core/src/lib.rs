//! my-dashboard core: module framework, navigation, home grid, app shell.

pub mod home;
pub mod module;
pub mod nav;
pub mod registry;
pub mod shell;

pub use module::{Module, ModuleEntry, ModuleMeta};
pub use nav::{Crumb, Navigator, Page};
pub use registry::ModuleRegistry;
pub use shell::{DashboardShell, ToggleTitleBar};
