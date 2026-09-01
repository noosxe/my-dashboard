//! GitHub module (docs/0005) — first real data module.
//!
//! Layers: `auth` (device flow + keychain), `http` (injectable transport +
//! ureq client), `api` (payloads + endpoints), `service` (state entity,
//! tick loop, caches, connect orchestration), `card`/`page` (the two
//! faces). `ModuleMeta.icon` is `Some(ModuleIcon::Custom("icons/github.svg"))`:
//! core supports custom sidebar icons via asset paths, so the GitHub mark
//! renders in the sidebar, the home card and the module page alike — one
//! bundled SVG, served by the app's asset source.
//! Deep gpui builder trees in page.rs need macro-expansion headroom.
#![recursion_limit = "512"]
pub mod api;
pub mod auth;
pub mod card;
pub mod gql;
pub mod http;
pub mod page;
pub mod service;
pub mod settings;

use std::rc::Rc;
use std::sync::Arc;

use gpui::*;

use dashboard_core::Module;
use dashboard_core::module::{ModuleEntry, ModuleIcon, ModuleMeta, SettingsProvider};
use dashboard_core::nav::Navigator;

use crate::auth::{KeyringTokenStore, TokenStore};
use crate::card::GITHUB_MARK;
use crate::http::{Http, SharedToken, UreqClient};
use crate::service::{GithubCache, GithubState, spawn_tick_loop};

pub struct GithubModule;

impl Module for GithubModule {
    fn meta() -> ModuleMeta {
        ModuleMeta {
            id: "github".into(),
            title: "GitHub".into(),
            icon: Some(ModuleIcon::Custom(GITHUB_MARK.into())),
        }
    }

    fn init(
        _nav: &Entity<Navigator>,
        window: &mut Window,
        paths: &dashboard_core::AppPaths,
        cx: &mut App,
    ) -> ModuleEntry {
        let window_handle = window.window_handle();

        // Keychain-backed token slot + shared client + cache root (0005 Q3/Q6).
        let store: Arc<dyn TokenStore> = Arc::new(KeyringTokenStore);
        let shared_token: SharedToken = Arc::new(std::sync::RwLock::new(None));
        let client: Arc<dyn Http> = Arc::new(UreqClient::new(shared_token.clone()));
        let cache = GithubCache::new(paths);

        // The single state entity — seeds from last-known caches and
        // resolves auth from the keychain.
        let state = cx.new(|_| GithubState::new(client, store, shared_token, cache));

        // 60 s service loop — self-gates on auth/rate-limit state and
        // refreshes per tick (0003 §3.12 pattern, docs/0005 Q8).
        spawn_tick_loop(state.clone(), window_handle, cx);

        let overview = cx.new(|cx| card::GithubCard::new(state.clone(), _nav.clone(), cx));
        let main_page = cx.new(|cx| page::GithubPage::new(state.clone(), cx));

        // Settings subpage (docs/0006 §4.3): the provider captures the
        // state entity and rebuilds the page per settings render, so
        // values are always live.
        let settings_state = state.clone();
        let settings: SettingsProvider =
            Rc::new(move |_window, cx| Some(settings::page(&settings_state, cx)));

        ModuleEntry {
            meta: Self::meta(),
            overview: overview.into(),
            main_page: main_page.into(),
            settings: Some(settings),
        }
    }
}
