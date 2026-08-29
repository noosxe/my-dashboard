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
pub mod service;

use std::sync::Arc;

use gpui::*;

use gpui_component::ActiveTheme;

use dashboard_core::Module;
use dashboard_core::module::{ModuleEntry, ModuleMeta};
use dashboard_core::nav::Navigator;

use crate::auth::{KeyringTokenStore, TokenStore};
use crate::http::{Http, SharedToken, UreqClient};
use crate::service::{GithubCache, GithubState, spawn_tick_loop};

pub struct GithubModule;

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
        window: &mut Window,
        paths: &dashboard_core::AppPaths,
        cx: &mut App,
    ) -> ModuleEntry {
        let window_handle = window.window_handle();

        // Keychain-backed token slot + shared client + cache root.
        let store: Arc<dyn TokenStore> = Arc::new(KeyringTokenStore);
        let shared_token: SharedToken = Arc::new(std::sync::RwLock::new(None));
        let client: Arc<dyn Http> = Arc::new(UreqClient::new(shared_token.clone()));
        let cache = GithubCache::new(paths);

        // The single state entity; seeds from last-known caches (Q6) and
        // resolves auth from the keychain.
        let state = cx.new(|_| GithubState::new(client, store, shared_token, cache));

        // 60 s service loop — self-gates on auth/rate-limit state and
        // refreshes per tick (0003 §3.12 pattern).
        spawn_tick_loop(state.clone(), window_handle, cx);

        // Placeholder faces — replaced by card.rs/page.rs in the UI layer;
        // they hold the state entity alive in the meantime.
        let overview = cx.new(|_| StubView {
            state: state.clone(),
            text: "GitHub card — UI lands next".into(),
        });
        let main_page = cx.new(|_| StubView {
            state: state.clone(),
            text: "GitHub — UI lands next".into(),
        });
        ModuleEntry {
            meta: Self::meta(),
            overview: overview.into(),
            main_page: main_page.into(),
        }
    }
}

/// Temporary stand-in view (replaced by card.rs/page.rs); holds the state
/// entity alive.
struct StubView {
    #[allow(dead_code)] // lifecycle only — real views observe this entity
    state: Entity<service::GithubState>,
    text: SharedString,
}

impl Render for StubView {
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
