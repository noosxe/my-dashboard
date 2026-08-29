//! GitHub module (docs/0005) — first real data module.
//!
//! Layer status (implementation in progress on PR branch):
//! - `auth`  — OAuth device flow state machine + keychain token store (done)
//! - `http`  — injectable transport + ureq client (done)
//! - `api`   — payload structs + endpoint builders (done)
//! - service — poll loops, ETag caches (next)
//! - UI      — two-column page + home card (next; placeholders registered
//!   below so the module is live in the shell from this commit)
//!
//! `ModuleMeta.icon` stays `None` for now: the default icon set has no
//! GitHub mark (docs/0005 §3.4); the card/page will draw the bundled SVG,
//! the sidebar renders label-only until a custom-icon path exists in core.

pub mod api;
pub mod auth;
pub mod http;

use gpui::*;

use gpui_component::ActiveTheme;

use dashboard_core::Module;
use dashboard_core::module::{ModuleEntry, ModuleMeta};
use dashboard_core::nav::Navigator;

pub struct GithubModule;

/// The single state entity — filled in by the service layer; both views
/// observe it.
pub(crate) struct GithubState {}

impl Module for GithubModule {
    fn meta() -> ModuleMeta {
        ModuleMeta {
            id: "github".into(),
            title: "GitHub".into(),
            icon: None,
        }
    }

    fn init(
        _nav: &Entity<Navigator>,
        _window: &mut Window,
        _paths: &dashboard_core::AppPaths,
        cx: &mut App,
    ) -> ModuleEntry {
        let state = cx.new(|_| GithubState {});
        let overview = cx.new(|_| PlaceholderView {
            text: "GitHub card — service layer pending".into(),
        });
        let main_page = cx.new(|_| PlaceholderView {
            text: "GitHub — UI lands with the service layer".into(),
        });
        let _ = state; // consumed by the service layer
        ModuleEntry {
            meta: Self::meta(),
            overview: overview.into(),
            main_page: main_page.into(),
        }
    }
}

/// Temporary stand-in view (removed when card/page land).
struct PlaceholderView {
    text: SharedString,
}

impl Render for PlaceholderView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(cx.theme().muted_foreground)
            .child(self.text.clone())
    }
}
