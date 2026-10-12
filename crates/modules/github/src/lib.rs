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
pub mod agent_ops;
pub mod api;
pub mod auth;
pub mod card;
pub mod drafts;
pub mod files;
mod filters;
pub mod gql;
pub mod http;
pub mod issue_page;
pub mod markdown;
pub mod new_issue_page;
pub mod new_pr_page;
pub mod notification_page;
pub mod page;
mod pickers;
pub mod pr_page;
pub mod service;
pub mod settings;
pub mod usage;

use std::rc::Rc;
use std::sync::Arc;

use dashboard_core::usage_records::Recorder;
use gpui::*;

use dashboard_core::Module;
use dashboard_core::module::{ModuleEntry, ModuleIcon, ModuleMeta, SettingsProvider, StatusItemFn};
use dashboard_core::nav::{Navigator, Page};

use crate::auth::{KeyringTokenStore, TokenStore};
use crate::card::GITHUB_MARK;
use crate::http::{Http, SharedToken, UreqClient};
use crate::service::{GithubCache, GithubState, spawn_tick_loop};

pub struct GithubModule;

impl Module for GithubModule {
    fn meta() -> ModuleMeta {
        ModuleMeta::new(
            "github",
            "GitHub",
            Some(ModuleIcon::Custom(GITHUB_MARK.into())),
        )
    }

    fn init(
        nav: &Entity<Navigator>,
        window: &mut Window,
        paths: &dashboard_core::AppPaths,
        cx: &mut App,
    ) -> ModuleEntry {
        let window_handle = window.window_handle();

        // Esc closes the open PR sub-page (docs/0016 D1): scoped to the
        // PR page's key context so it never fires elsewhere; the PR page
        // Esc closes the open sub-page (docs/0016 D1, docs/0020 D1):
        // scoped to each page's key context so it never fires elsewhere;
        // the page focus sits inside its context while it owns the
        // module content.
        cx.bind_keys([
            KeyBinding::new("esc", pr_page::DismissPrPage, Some("GithubPrPage")),
            KeyBinding::new("esc", issue_page::DismissIssuePage, Some("GithubIssuePage")),
            KeyBinding::new(
                "esc",
                notification_page::DismissNotificationPage,
                Some("GithubNotificationPage"),
            ),
            KeyBinding::new(
                "esc",
                new_issue_page::DismissNewIssuePage,
                Some("GithubNewIssuePage"),
            ),
            KeyBinding::new(
                "esc",
                new_pr_page::DismissNewPrPage,
                Some("GithubNewPrPage"),
            ),
        ]);
        // Notification source label (docs/0062 D2): every github raise
        // attributes here; the settings page mutes by this id.
        dashboard_core::toasts::register_source("github", "GitHub", cx);
        // Keychain-backed token slot + shared client + cache root (0005 Q3/Q6).
        let store: Arc<dyn TokenStore> = Arc::new(KeyringTokenStore);
        let shared_token: SharedToken = Arc::new(std::sync::RwLock::new(None));
        // Refresh-token slot (docs/0011 §4.3) — same handling rules as
        // the access-token slot.
        let shared_refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
        // The counting seam (docs/0050 D2): one decorator around the
        // single client sees 100% of GitHub-bound traffic — all three
        // transports and the auth flows borrow `state.client`, so no
        // call site changes.
        let usage = usage::new_ledger();
        let client: Arc<dyn Http> = usage::CountingHttp::wrap(
            Arc::new(UreqClient::new(shared_token.clone())),
            usage.clone(),
        );
        let cache = GithubCache::new(paths);

        // The single state entity — seeds from last-known caches and
        // resolves auth from the keychain.
        let drafts = drafts::DraftStore::new(dashboard_core::paths::AppPaths::init());
        // docs/0053 D3: the app-wide sink from `usage_records::init`.
        let recorder = Recorder::new("github", dashboard_core::usage_records::sink(cx));
        let state = cx.new(move |_| {
            GithubState::with_recorder(
                client,
                store,
                shared_token,
                shared_refresh,
                cache,
                drafts,
                // docs/0053 D3: the module-global sink from
                // `usage_records::init` (app `main`).
                recorder,
            )
        });
        // Swap the constructor's fresh book for the wrapper's (docs/
        // 0050 D2) — before any traffic can flow: the tick loop and
        // the views below spawn after this line.
        state.update(cx, |s, _| s.usage = usage.clone());

        // 10 s service loop (docs/0038 D7) — self-gates on
        // auth/rate-limit state and refreshes per tick (0003 §3.12
        // pattern, docs/0005 Q8); the lanes decide what actually fires.
        spawn_tick_loop(state.clone(), window_handle, cx);

        // Agent data bridge (docs/0068 D3, #468): the module's read-only
        // ops — zero-network-flight reads of held state. Resolution is
        // per request, so registering here is enough regardless of the
        // bridge's startup order.
        dashboard_agent::register_ops(cx, Arc::new(agent_ops::GithubAgentOps::new(state.clone())));

        let overview = cx.new(|cx| card::GithubCard::new(state.clone(), nav.clone(), window, cx));
        let main_page = cx.new(|cx| page::GithubPage::new(state.clone(), nav.clone(), window, cx));

        // Settings subpage (docs/0006 §4.3): the provider captures the
        // state entity and rebuilds the page per settings render, so
        // values are always live.
        let settings_state = state.clone();
        let settings: SettingsProvider =
            Rc::new(move |_window, cx| Some(settings::page(&settings_state, cx)));

        // History support (docs/0048 D3): restore re-clicks through the
        // page's own open/select handlers; collapse is the existing
        // return-to-root. Downgrade before the entry takes the view.
        let nav_support = Arc::new(page::GithubNav {
            page: main_page.downgrade(),
        });
        // Footer chip (docs/0050 D5): one view for the app's lifetime;
        // the status-item closure scopes it to the GitHub module pages
        // (the provider-decides-visibility contract).
        let usage_chip =
            cx.new(|cx| usage::UsageChip::new(state.clone(), usage.clone(), nav.clone(), cx));
        let status_item: StatusItemFn = Arc::new(move |page, _cx| match page {
            Page::Module(id) if id.as_ref() == "github" => {
                Some(usage_chip.clone().into_any_element())
            }
            _ => None,
        });
        ModuleEntry::new(
            Self::meta(),
            overview.into(),
            main_page.into(),
            Some(settings),
            Some(nav_support),
            Some(status_item),
        )
    }
}
