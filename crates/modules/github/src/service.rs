//! Service layer (docs/0005 Q6, Q7, Q9): one state entity written by a
//! 60-second tick loop; ETag + last-known caches under the 0004 cache root;
//! 401/403 handling per the [DG] states matrix.
//!
//! Split of responsibilities:
//! - pure state machinery here (`plan_tick`, `run_spec`, `apply`) —
//!   unit-tested with a scripted transport, no gpui involved;
//! - thin gpui orchestration at the bottom (`spawn_tick_loop`,
//!   `start_connect`, `sign_out_now`) — every state write ends with the
//!   0003 §3.12 unfocused refresh.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::AnyWindowHandle;
use serde::Serialize;
use serde::de::DeserializeOwned;

use dashboard_core::AppPaths;
use dashboard_core::store;
use gpui::App;
use gpui::Entity;

use crate::api::{self, Notification, Org, PullRequest, Repo, User};
use crate::auth::{AuthError, PollOutcome, TokenStore, poll_token, start_device_flow};
use crate::http::{Http, HttpError, RateLimit, SharedToken, Verb};

/// Tick cadence (docs/0005 Q7): notifications + selected-repo PRs every
/// tick; PR-count walk every 5th tick; orgs/repos every 10th.
pub const TICK: Duration = Duration::from_secs(60);
const PR_WALK_EVERY: u64 = 5;
const REPO_LIST_EVERY: u64 = 10;
/// Page cap: repos list stops at page 1 (100 repos — doc §10.2); PR walks
/// paginate up to here.
const MAX_PAGES: u32 = 10;
/// Card-walk repo cap (doc §10.2).
const PR_WALK_REPO_CAP: usize = 100;

// ---------------------------------------------------------------------------
// Auth status
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum AuthStatus {
    /// No token in the keychain.
    Disconnected,
    /// A connect flow is (or was last) in progress; terminal phases are
    /// kept for the UI to show with a Retry affordance.
    ConnectFlow(ConnectPhase),
    Connected,
    /// 401 from the API — token revoked; reconnect required (Q9).
    TokenRejected,
    /// The keychain itself is unavailable (Q3: designed error, no fallback).
    KeychainError(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConnectPhase {
    RequestingCode,
    AwaitApproval {
        user_code: String,
        verification_uri: String,
    },
    Denied,
    Expired,
    Failed(String),
}

impl AuthStatus {
    pub fn connected(&self) -> bool {
        matches!(self, AuthStatus::Connected)
    }
}

// ---------------------------------------------------------------------------
// Last-known + ETag caches (Q6) — transient data under the cache root.
// ---------------------------------------------------------------------------

pub struct GithubCache {
    root: PathBuf,
}

impl GithubCache {
    pub fn new(paths: &AppPaths) -> Self {
        Self {
            root: paths.cache_dir().join("github"),
        }
    }

    #[cfg(test)]
    fn at_tmp(tag: &str) -> Self {
        Self {
            root: std::env::temp_dir().join(format!(
                "dash-gh-cache-{tag}-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            )),
        }
    }

    fn etags_path(&self) -> PathBuf {
        self.root.join("etags.json")
    }

    fn last_known_path(&self, rel: &str) -> PathBuf {
        self.root.join("last-known").join(format!("{rel}.json"))
    }

    pub fn etags(&self) -> BTreeMap<String, String> {
        store::load_json(&self.etags_path())
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    fn save_etags(&self, etags: &BTreeMap<String, String>) {
        if let Err(e) = store::save_json_atomic(&self.etags_path(), etags) {
            eprintln!("[github] etag cache write failed: {e}");
        }
    }

    pub fn load<T: DeserializeOwned>(&self, rel: &str) -> Option<T> {
        store::load_json(&self.last_known_path(rel)).ok().flatten()
    }

    pub fn put<T: Serialize>(&self, rel: &str, value: &T) {
        if let Some(parent) = self.last_known_path(rel).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = store::save_json_atomic(&self.last_known_path(rel), value) {
            eprintln!("[github] last-known write failed ({rel}): {e}");
        }
    }

    /// Sign-out wipes everything under the module's cache root (Q5).
    pub fn clear(&self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// `owner/repo` → filename-safe slug (`/` → `__`, anything exotic → `_`).
fn slug(full_name: &str) -> String {
    full_name
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' | '/' => c,
            _ => '_',
        })
        .collect::<String>()
        .replace('/', "__")
}

// ---------------------------------------------------------------------------
// Fetch specs — plain-data work items planned on the main thread, run on
// the background executor, applied back on the main thread.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    User,
    Orgs,
    Repos,
    Notifications,
    /// Selected repo's open PRs (full payloads, shown in the GroupBox).
    SelectedPrs,
    /// One repo's open-PR count for the card (list length only, §6.1).
    PrCount,
}

#[derive(Clone, Debug)]
pub struct FetchSpec {
    pub kind: Kind,
    pub url: String,
    pub etag: Option<String>,
    /// `owner/repo` for the per-repo kinds.
    pub ident: Option<String>,
    pub max_pages: u32,
}

/// What a background run hands back to the main thread. The body is raw
/// JSON — parsed during `apply` so the background side stays type-free.
pub struct SpecResult {
    pub kind: Kind,
    pub ident: Option<String>,
    pub status: u16,
    pub etag: Option<String>,
    pub rate: Option<RateLimit>,
    pub body: Option<String>,
    pub network: Option<String>,
}

/// Run one spec to completion (paginating as needed). Sync — runs on the
/// background executor.
pub fn run_spec(http: &dyn Http, spec: &FetchSpec) -> SpecResult {
    let mut bodies: Vec<String> = Vec::new();
    let mut page: u32 = 1;
    let mut status = 0u16;
    let mut etag = None;
    let mut rate = None;
    let mut network = None;

    loop {
        let page_url = if page == 1 {
            spec.url.clone()
        } else {
            format!("{}&page={page}", spec.url)
        };
        // ETag revalidation applies to page 1 only: 304 there means the
        // whole list is unchanged; mid-walk 304s keep what we collected.
        match http.request(
            Verb::Get,
            &page_url,
            None,
            spec.etag.as_deref().filter(|_| page == 1),
        ) {
            Err(HttpError(m)) => {
                network = Some(m);
                break;
            }
            Ok(result) => {
                status = result.status;
                etag = result.etag;
                rate = result.rate;
                if status != 200 {
                    break;
                }
                let body = result.body.unwrap_or_default();
                if spec.kind == Kind::User {
                    // Single-object payload — no array checks, no pages.
                    bodies.push(body);
                    break;
                }
                let count = serde_json::from_str::<Vec<serde_json::Value>>(&body)
                    .map(|v| v.len())
                    .unwrap_or(0);
                if count == 0 && !body.trim().starts_with('[') {
                    // Non-array payload (error object) — surfaced below as
                    // a status the caller can act on.
                    eprintln!(
                        "[github] {}: non-array payload at page {page}",
                        spec.kind.kind_name()
                    );
                    break;
                }
                let full_page = count == 100;
                bodies.push(body);
                if !full_page || page >= spec.max_pages {
                    break;
                }
                page += 1;
            }
        }
    }

    let body = if network.is_none() && status == 200 {
        Some(match bodies.len() {
            0 => "[]".to_string(),
            1 => bodies.remove(0),
            _ => {
                // Stitch pages: strip each page's outer brackets, join the
                // inner items.
                let items: Vec<String> = bodies
                    .iter()
                    .map(|b| b.trim())
                    .filter(|t| t.len() >= 2)
                    .map(|t| t[1..t.len() - 1].to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                format!("[{}]", items.join(","))
            }
        })
    } else {
        None
    };

    SpecResult {
        kind: spec.kind,
        ident: spec.ident.clone(),
        status,
        etag,
        rate,
        body,
        network,
    }
}

impl Kind {
    fn kind_name(&self) -> &'static str {
        match self {
            Kind::User => "user",
            Kind::Orgs => "orgs",
            Kind::Repos => "repos",
            Kind::Notifications => "notifications",
            Kind::SelectedPrs => "prs",
            Kind::PrCount => "pr-count",
        }
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct GithubState {
    pub auth: AuthStatus,
    pub user: Option<User>,
    /// Org entries for the dropdown; the personal account is first (Q10).
    pub orgs: Vec<Org>,
    pub repos: Vec<Repo>,
    /// Global unread notifications (card count + repo grouping).
    pub notifications: Vec<Notification>,
    /// `owner/repo` → open PR count (card total; PrCount walk).
    pub open_pr_counts: BTreeMap<String, usize>,
    /// Selected repo's open PRs (full payloads for the PR GroupBox).
    pub prs: BTreeMap<String, Vec<PullRequest>>,
    /// `owner/repo` of the selected repo (session-only, not persisted).
    pub selected: Option<String>,
    /// Rate-limit pause: UTC epoch seconds until requests resume (Q9).
    pub rate_limited_until: Option<u64>,
    /// Set when a connect flow should fetch identity + lists on next tick.
    pub needs_bootstrap: bool,

    pub(crate) etags: BTreeMap<String, String>,
    pub(crate) client: Arc<dyn Http>,
    pub(crate) store: Arc<dyn TokenStore>,
    pub(crate) shared_token: SharedToken,
    pub(crate) cache: GithubCache,
}

impl GithubState {
    /// Build from the keychain: seeds last-known data (no skeleton flash
    /// for returning users, Q6) and resolves the auth status.
    pub fn new(
        client: Arc<dyn Http>,
        store: Arc<dyn TokenStore>,
        shared_token: SharedToken,
        cache: GithubCache,
    ) -> Self {
        let etags = cache.etags();
        let auth = match store.load() {
            Ok(Some(token)) => {
                if let Ok(mut slot) = shared_token.write() {
                    *slot = Some(token);
                }
                AuthStatus::Connected
            }
            Ok(None) => AuthStatus::Disconnected,
            Err(e) => AuthStatus::KeychainError(e),
        };
        let seeded = auth.connected() || matches!(auth, AuthStatus::TokenRejected);
        Self {
            user: seeded.then(|| cache.load("user")).flatten(),
            orgs: seeded
                .then(|| cache.load("orgs"))
                .flatten()
                .unwrap_or_default(),
            repos: seeded
                .then(|| cache.load("repos"))
                .flatten()
                .unwrap_or_default(),
            notifications: seeded
                .then(|| cache.load("notifications"))
                .flatten()
                .unwrap_or_default(),
            open_pr_counts: seeded
                .then(|| cache.load("pr-counts"))
                .flatten()
                .unwrap_or_default(),
            prs: BTreeMap::new(),
            selected: None,
            rate_limited_until: None,
            needs_bootstrap: false,
            auth,
            etags,
            client,
            store,
            shared_token,
            cache,
        }
    }

    /// Rate-limit / auth gate (Q9): no requests while disconnected, mid
    /// connect-flow, token-rejected, or paused until the window resets.
    fn gated(&self) -> bool {
        if !self.auth.connected() {
            return true;
        }
        match self.rate_limited_until {
            Some(until) => now_epoch() < until,
            None => false,
        }
    }

    /// Plan this tick's work (Q7). Consumes `needs_bootstrap`.
    pub fn plan_tick(&mut self, tick: u64) -> Vec<FetchSpec> {
        if self.needs_bootstrap && self.auth.connected() {
            self.needs_bootstrap = false;
            return vec![
                self.spec(Kind::User, api::user_url(), None),
                self.spec(Kind::Orgs, api::orgs_url(1), None),
                self.spec(Kind::Repos, api::repos_url(1), None),
                self.spec(Kind::Notifications, api::notifications_url(1), None),
            ];
        }
        if self.gated() {
            return Vec::new();
        }
        let mut specs = Vec::new();
        specs.push(self.spec(
            Kind::Notifications,
            api::notifications_url(1),
            Some(MAX_PAGES),
        ));
        if let Some(full_name) = self.selected.clone() {
            let (owner, repo) = split_full_name(&full_name);
            specs.push(self.spec_ident(
                Kind::SelectedPrs,
                Some(&full_name),
                api::pulls_url(&owner, &repo, 1),
                Some(MAX_PAGES),
            ));
        }
        if tick % PR_WALK_EVERY == 0 {
            for repo in self.repos.iter().take(PR_WALK_REPO_CAP) {
                let full_name = repo.full_name.clone();
                let (owner, name) = split_full_name(&full_name);
                specs.push(self.spec_ident(
                    Kind::PrCount,
                    Some(&full_name),
                    api::pulls_url(&owner, &name, 1),
                    Some(1),
                ));
            }
        }
        if tick % REPO_LIST_EVERY == 0 {
            specs.push(self.spec(Kind::Orgs, api::orgs_url(1), None));
            specs.push(self.spec(Kind::Repos, api::repos_url(1), None));
        }
        specs
    }

    fn spec(&self, kind: Kind, url: String, max_pages: Option<u32>) -> FetchSpec {
        self.spec_ident(kind, None, url, max_pages)
    }

    /// `spec` with an identity — per-repo kinds key their ETags per repo.
    fn spec_ident(
        &self,
        kind: Kind,
        ident: Option<&str>,
        url: String,
        max_pages: Option<u32>,
    ) -> FetchSpec {
        FetchSpec {
            kind,
            etag: self.etags.get(&etag_key(kind, ident)).cloned(),
            url,
            ident: ident.map(str::to_string),
            max_pages: max_pages.unwrap_or(1),
        }
    }

    /// Apply a finished spec: parse, update state, store last-known + etag,
    /// handle 401/403 (Q9). Malformed payloads degrade to stale data with a
    /// stderr note — never a dialog.
    pub fn apply(&mut self, result: SpecResult) {
        if let Some(m) = result.network {
            eprintln!("[github] {}: network error: {m}", result.kind.kind_name());
            return;
        }
        match result.status {
            200 => {}
            304 => return, // unchanged — stale data stays, nothing to do
            401 => {
                self.auth = AuthStatus::TokenRejected;
                return;
            }
            _ => {
                if result.rate.is_some_and(|r| r.remaining == 0) {
                    self.rate_limited_until = result.rate.map(|r| r.reset_epoch);
                }
                eprintln!(
                    "[github] {}: HTTP {}",
                    result.kind.kind_name(),
                    result.status
                );
                return;
            }
        }
        if result.rate.is_some_and(|r| r.remaining == 0) {
            self.rate_limited_until = result.rate.map(|r| r.reset_epoch);
        }
        let Some(body) = result.body.as_deref() else {
            return;
        };

        match result.kind {
            Kind::User => match serde_json::from_str::<User>(body) {
                Ok(v) => {
                    self.cache.put("user", &v);
                    self.user = Some(v);
                }
                Err(e) => eprintln!("[github] user payload: {e}"),
            },
            Kind::Orgs => match serde_json::from_str::<Vec<Org>>(body) {
                Ok(orgs) => {
                    self.apply_orgs(orgs);
                    self.cache.put("orgs", &self.orgs);
                }
                Err(e) => eprintln!("[github] orgs payload: {e}"),
            },
            Kind::Repos => match serde_json::from_str::<Vec<Repo>>(body) {
                Ok(repos) => {
                    self.cache.put("repos", &repos);
                    self.repos = repos;
                }
                Err(e) => eprintln!("[github] repos payload: {e}"),
            },
            Kind::Notifications => match serde_json::from_str::<Vec<Notification>>(body) {
                Ok(items) => {
                    self.cache.put("notifications", &items);
                    self.notifications = items;
                }
                Err(e) => eprintln!("[github] notifications payload: {e}"),
            },
            Kind::SelectedPrs => match (
                result.ident.as_deref(),
                serde_json::from_str::<Vec<PullRequest>>(body),
            ) {
                (Some(id), Ok(prs)) => {
                    self.cache.put(&format!("prs-{}", slug(id)), &prs);
                    self.prs.insert(id.to_string(), prs);
                }
                (None, _) => eprintln!("[github] prs spec without ident"),
                (_, Err(e)) => eprintln!("[github] prs payload: {e}"),
            },
            Kind::PrCount => match (
                result.ident.as_deref(),
                serde_json::from_str::<Vec<PullRequest>>(body),
            ) {
                (Some(id), Ok(prs)) => {
                    let count = prs.len();
                    self.open_pr_counts.insert(id.to_string(), count);
                    self.cache.put("pr-counts", &self.open_pr_counts);
                }
                (None, _) => eprintln!("[github] pr-count spec without ident"),
                (_, Err(e)) => eprintln!("[github] pr-count payload: {e}"),
            },
        }

        // ETag keyed by logical resource (kind + ident), not raw URL, so
        // pagination page-1 etags stay stable.
        if let Some(etag) = &result.etag {
            self.etags
                .insert(etag_key(result.kind, result.ident.as_deref()), etag.clone());
        }
        self.cache.save_etags(&self.etags);
    }

    /// Org list for the dropdown: the personal account first (Q10).
    fn apply_orgs(&mut self, orgs: Vec<Org>) {
        let personal = self.user.as_ref().map(|u| Org {
            login: u.login.clone(),
            avatar_url: u.avatar_url.clone(),
            description: None,
        });
        self.orgs = match personal {
            Some(me) => std::iter::once(me).chain(orgs).collect(),
            None => orgs,
        };
    }

    /// Card totals (§6.1): unread notifications + Σ open PR counts.
    pub fn card_counts(&self) -> (usize, usize) {
        let unread = self.notifications.iter().filter(|n| n.unread).count();
        let prs = self.open_pr_counts.values().sum();
        (unread, prs)
    }

    /// Connect flow success path: store the token everywhere it lives.
    pub(crate) fn install_token(&mut self, token: String) -> Result<(), String> {
        self.store.save(&token)?;
        if let Ok(mut slot) = self.shared_token.write() {
            *slot = Some(token);
        }
        self.auth = AuthStatus::Connected;
        self.rate_limited_until = None;
        self.needs_bootstrap = true;
        Ok(())
    }

    /// Sign-out (Q5): keychain + token slot + caches + data.
    pub fn sign_out(&mut self) {
        let _ = self.store.delete();
        if let Ok(mut slot) = self.shared_token.write() {
            *slot = None;
        }
        self.cache.clear();
        self.user = None;
        self.orgs.clear();
        self.repos.clear();
        self.notifications.clear();
        self.open_pr_counts.clear();
        self.prs.clear();
        self.etags.clear();
        self.selected = None;
        self.rate_limited_until = None;
        self.needs_bootstrap = false;
        self.auth = AuthStatus::Disconnected;
    }
}

fn split_full_name(full_name: &str) -> (String, String) {
    let mut parts = full_name.splitn(2, '/');
    (
        parts.next().unwrap_or_default().to_string(),
        parts.next().unwrap_or_default().to_string(),
    )
}

fn etag_key(kind: Kind, ident: Option<&str>) -> String {
    match (kind, ident) {
        (Kind::User, _) => "user".into(),
        (Kind::Orgs, _) => "orgs".into(),
        (Kind::Repos, _) => "repos".into(),
        (Kind::Notifications, _) => "notifications".into(),
        (Kind::SelectedPrs, Some(id)) => format!("prs:{id}"),
        (Kind::PrCount, Some(id)) => format!("prc:{id}"),
        (_, None) => "unknown".into(),
    }
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// gpui orchestration — thin; ends every state write with the §3.12 refresh.
// ---------------------------------------------------------------------------

/// The 60-second service loop (Q7/Q8): plan → background-run → apply →
/// refresh. Self-gates on auth state and rate-limit windows.
pub fn spawn_tick_loop(state: Entity<GithubState>, window: AnyWindowHandle, cx: &mut App) {
    cx.spawn(async move |cx| {
        let mut tick: u64 = 0;
        loop {
            cx.background_executor().timer(TICK).await;
            tick += 1;

            let (client, specs) =
                cx.update(|cx| state.update(cx, |s, _| (s.client.clone(), s.plan_tick(tick))));

            for spec in specs {
                let client = client.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move { run_spec(&*client, &spec) })
                    .await;
                cx.update(|cx| state.update(cx, |s, _| s.apply(result)));
            }
            let _ = window.update(cx, |_, window, _| window.refresh());
        }
    })
    .detach();
}

/// Device-flow connect (§6.2): request code → show + auto-open browser →
/// poll honoring interval/deadline → install token. Terminal phases
/// (Denied/Expired/Failed) stay visible for the UI's Retry affordance.
pub fn start_connect(state: Entity<GithubState>, window: AnyWindowHandle, cx: &mut App) {
    state.update(cx, |s, _| {
        s.auth = AuthStatus::ConnectFlow(ConnectPhase::RequestingCode);
    });

    cx.spawn(async move |cx| {
        let client = cx.update(|cx| state.update(cx, |s, _| s.client.clone()));

        // Step 1 — device + user codes.
        let auth_client = client.clone();
        let start = cx
            .background_executor()
            .spawn(async move { start_device_flow(&*auth_client) })
            .await;
        let start = match start {
            Ok(s) => s,
            Err(AuthError::Network(m)) | Err(AuthError::Malformed(m)) => {
                set_connect_phase(&state, &window, cx, ConnectPhase::Failed(m));
                return;
            }
        };

        // Step 2 — show the code, open the verification page once.
        cx.update(|cx| {
            state.update(cx, |s, _| {
                s.auth = AuthStatus::ConnectFlow(ConnectPhase::AwaitApproval {
                    user_code: start.user_code.clone(),
                    verification_uri: start.verification_uri.clone(),
                });
            });
            cx.open_url(&start.verification_uri);
        });
        let _ = window.update(cx, |_, window, _| window.refresh());

        // Step 3 — poll until token, expiry, denial or deadline.
        let deadline = Instant::now() + Duration::from_secs(start.expires_in_secs);
        let mut interval = start.interval_secs;
        loop {
            cx.background_executor()
                .timer(Duration::from_secs(interval))
                .await;
            if Instant::now() > deadline {
                set_connect_phase(&state, &window, cx, ConnectPhase::Expired);
                return;
            }
            let outcome = {
                let client = client.clone();
                let device_code = start.device_code.clone();
                cx.background_executor()
                    .spawn(async move { poll_token(&*client, &device_code, interval) })
                    .await
            };
            match outcome {
                PollOutcome::Token(token) => {
                    let saved = cx.update(|cx| state.update(cx, |s, _| s.install_token(token)));
                    match saved {
                        Ok(()) => {}
                        // Keychain save failed — designed error state (Q3).
                        _ => {
                            set_connect_phase(
                                &state,
                                &window,
                                cx,
                                ConnectPhase::Failed(
                                    "could not store the token in the OS keychain".into(),
                                ),
                            );
                            return;
                        }
                    }
                    let _ = window.update(cx, |_, window, _| window.refresh());
                    return;
                }
                PollOutcome::Pending { interval_secs } => interval = interval_secs,
                PollOutcome::SlowDown { interval_secs } => interval = interval_secs,
                PollOutcome::Expired => {
                    set_connect_phase(&state, &window, cx, ConnectPhase::Expired);
                    return;
                }
                PollOutcome::Denied => {
                    set_connect_phase(&state, &window, cx, ConnectPhase::Denied);
                    return;
                }
                PollOutcome::Network(_) => continue, // transient; deadline bounds us
                PollOutcome::Malformed(m) => {
                    set_connect_phase(&state, &window, cx, ConnectPhase::Failed(m));
                    return;
                }
            }
        }
    })
    .detach();
}

fn set_connect_phase(
    state: &Entity<GithubState>,
    window: &AnyWindowHandle,
    cx: &mut gpui::AsyncApp,
    phase: ConnectPhase,
) {
    cx.update(|cx| {
        state.update(cx, |s, _| {
            s.auth = AuthStatus::ConnectFlow(phase);
        });
    });
    let _ = window.update(cx, |_, window, _| window.refresh());
}

/// Sign-out orchestration (Q5): state wipe + refresh.
pub fn sign_out_now(state: Entity<GithubState>, window: AnyWindowHandle, cx: &mut App) {
    state.update(cx, |s, _| s.sign_out());
    let _ = window.update(cx, |_, window, _| window.refresh());
}

// ---------------------------------------------------------------------------
// Tests — pure state machinery against the scripted transport.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::MockTokenStore;
    use crate::http::HttpResult;
    use crate::http::mock::MockHttp;

    fn make_state(http: Arc<MockHttp>) -> GithubState {
        let shared: SharedToken = Arc::new(std::sync::RwLock::new(Some("t0k".into())));
        let client: Arc<dyn Http> = http;
        GithubState::new(
            client,
            Arc::new(MockTokenStore::with("t0k".into())),
            shared,
            GithubCache::at_tmp("state"),
        )
    }

    fn repo(full_name: &str) -> Repo {
        let name = full_name.split('/').nth(1).unwrap_or(full_name);
        serde_json::from_str(&format!(
            r#"{{"id":1,"name":"{name}","full_name":"{full_name}","owner":{{"login":"o","avatar_url":"a"}},"private":false}}"#
        ))
        .unwrap()
    }

    fn ok(body: &str, etag: &str) -> Result<HttpResult, HttpError> {
        Ok(HttpResult {
            status: 200,
            etag: Some(etag.into()),
            rate: Some(RateLimit {
                limit: 5000,
                remaining: 4900,
                reset_epoch: 9_999_999,
            }),
            body: Some(body.into()),
        })
    }

    fn one_notification() -> &'static str {
        r#"[{"id":"1","unread":true,"subject":{"title":"t","type":"Issue","url":"u"},"repository":{"full_name":"o/r"}}]"#
    }

    #[test]
    fn bootstrap_plan_fetches_identity_and_lists_then_clears() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.auth = AuthStatus::Connected;
        state.needs_bootstrap = true;

        let specs = state.plan_tick(1);
        assert_eq!(
            specs.iter().map(|s| s.kind).collect::<Vec<_>>(),
            vec![Kind::User, Kind::Orgs, Kind::Repos, Kind::Notifications]
        );
        // Bootstrap consumed — a later tick plans the regular cadence.
        assert!(state.plan_tick(2).len() <= 2);
    }

    fn make_disconnected() -> GithubState {
        let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
        let client: Arc<dyn Http> = Arc::new(MockHttp::new(vec![]));
        GithubState::new(
            client,
            Arc::new(MockTokenStore::empty()),
            shared,
            GithubCache::at_tmp("disconnected"),
        )
    }

    #[test]
    fn disconnected_and_rate_limited_are_gated() {
        let mut state = make_disconnected();
        assert!(state.plan_tick(1).is_empty());

        state.auth = AuthStatus::Connected;
        state.rate_limited_until = Some(u64::MAX);
        assert!(state.plan_tick(3).is_empty());

        // After the window passes, the tick resumes.
        state.rate_limited_until = Some(0);
        assert!(!state.plan_tick(1).is_empty());
    }

    #[test]
    fn apply_notifications_updates_state_etag_and_cache() {
        let http = Arc::new(MockHttp::new(vec![ok(one_notification(), "\"etag-1\"")]));
        let mut state = make_state(http.clone());
        state.auth = AuthStatus::Connected;

        let spec = state.spec(Kind::Notifications, api::notifications_url(1), Some(2));
        state.apply(run_spec(&*http, &spec));

        assert_eq!(state.notifications.len(), 1);
        assert_eq!(state.etags.get("notifications").unwrap(), "\"etag-1\"");
        // The next conditional fetch carries If-None-Match.
        let spec2 = state.spec(Kind::Notifications, api::notifications_url(1), Some(2));
        assert_eq!(spec2.etag.as_deref(), Some("\"etag-1\""));
    }

    #[test]
    fn apply_304_is_a_noop() {
        let http = Arc::new(MockHttp::new(vec![Ok(HttpResult {
            status: 304,
            etag: Some("\"etag-1\"".into()),
            rate: None,
            body: None,
        })]));
        let mut state = make_state(http.clone());
        state.auth = AuthStatus::Connected;
        state.notifications =
            serde_json::from_str::<Vec<Notification>>(one_notification()).unwrap();

        let spec = state.spec(Kind::Notifications, api::notifications_url(1), Some(2));
        state.apply(run_spec(&*http, &spec));
        assert_eq!(state.notifications.len(), 1);
        assert_eq!(state.notifications[0].id, "1");
    }

    #[test]
    fn apply_401_rejects_token_and_gates() {
        let http = Arc::new(MockHttp::new(vec![Ok(HttpResult {
            status: 401,
            etag: None,
            rate: None,
            body: Some(r#"{"message":"Bad credentials"}"#.into()),
        })]));
        let mut state = make_state(http.clone());
        state.auth = AuthStatus::Connected;

        let spec = state.spec(Kind::Notifications, api::notifications_url(1), Some(2));
        state.apply(run_spec(&*http, &spec));
        assert_eq!(state.auth, AuthStatus::TokenRejected);
        assert!(state.plan_tick(1).is_empty());
    }

    #[test]
    fn apply_403_rate_limited_pauses_until_reset() {
        let http = Arc::new(MockHttp::new(vec![Ok(HttpResult {
            status: 403,
            etag: None,
            rate: Some(RateLimit {
                limit: 5000,
                remaining: 0,
                reset_epoch: 9_999_999_999,
            }),
            body: Some(r#"{"message":"rate limit"}"#.into()),
        })]));
        let mut state = make_state(http.clone());
        state.auth = AuthStatus::Connected;

        let spec = state.spec(Kind::Repos, api::repos_url(1), None);
        state.apply(run_spec(&*http, &spec));
        assert_eq!(state.rate_limited_until, Some(9_999_999_999));
        assert!(state.plan_tick(1).is_empty());
    }

    #[test]
    fn pr_count_and_selected_prs_paginate() {
        let full_page = format!(
            "[{}]",
            (0..100)
                .map(|i| format!(
                    r#"{{"id":{i},"number":{i},"title":"t","draft":false,"user":{{"login":"x","avatar_url":"a"}},"html_url":"u"}}"#
                ))
                .collect::<Vec<_>>()
                .join(",")
        );
        let http = Arc::new(MockHttp::new(vec![
            // PrCount: one short page.
            ok(
                r#"[{"id":1,"number":1,"title":"a","draft":false,"user":{"login":"x","avatar_url":"a"},"html_url":"u"}]"#,
                "\"p1\"",
            ),
            // SelectedPrs: full page then short page.
            ok(&full_page, "\"p2a\""),
            ok(
                r#"[{"id":500,"number":101,"title":"last","draft":true,"user":{"login":"x","avatar_url":"a"},"html_url":"u"}]"#,
                "\"p2b\"",
            ),
        ]));
        let mut state = make_state(http.clone());
        state.auth = AuthStatus::Connected;
        state.repos = vec![repo("o/r")];

        state.apply(run_spec(
            &*http,
            &state.spec_ident(
                Kind::PrCount,
                Some("o/r"),
                api::pulls_url("o", "r", 1),
                Some(1),
            ),
        ));
        assert_eq!(state.open_pr_counts.get("o/r"), Some(&1));

        state.apply(run_spec(
            &*http,
            &state.spec_ident(
                Kind::SelectedPrs,
                Some("o/r"),
                api::pulls_url("o", "r", 1),
                Some(10),
            ),
        ));
        assert_eq!(state.prs.get("o/r").unwrap().len(), 101);
        assert!(state.prs.get("o/r").unwrap()[100].draft);
    }

    #[test]
    fn orgs_list_has_the_personal_account_first() {
        let http = Arc::new(MockHttp::new(vec![
            ok(r#"{"login":"noosxe","avatar_url":"a"}"#, "\"u\""),
            ok(r#"[{"login":"acme","avatar_url":"a"}]"#, "\"o\""),
        ]));
        let mut state = make_state(http.clone());
        state.auth = AuthStatus::Connected;

        state.apply(run_spec(
            &*http,
            &state.spec(Kind::User, api::user_url(), None),
        ));
        state.apply(run_spec(
            &*http,
            &state.spec(Kind::Orgs, api::orgs_url(1), None),
        ));

        assert_eq!(state.orgs[0].login, "noosxe");
        assert_eq!(state.orgs[1].login, "acme");
    }

    #[test]
    fn card_counts_sum_unread_and_pr_counts() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.open_pr_counts.insert("o/a".into(), 3);
        state.open_pr_counts.insert("o/b".into(), 4);
        state.notifications =
            serde_json::from_str::<Vec<Notification>>(one_notification()).unwrap();
        assert_eq!(state.card_counts(), (1, 7));
    }

    #[test]
    fn slug_is_filename_safe() {
        assert_eq!(slug("owner/repo.name-x"), "owner__repo.name-x");
        assert_eq!(slug("weird/name~!"), "weird__name__");
    }

    #[test]
    fn sign_out_clears_everything() {
        let shared: SharedToken = Arc::new(std::sync::RwLock::new(Some("t0k".into())));
        let client: Arc<dyn Http> = Arc::new(MockHttp::new(vec![]));
        let mut state = GithubState::new(
            client,
            Arc::new(MockTokenStore::with("t0k".into())),
            shared.clone(),
            GithubCache::at_tmp("signout"),
        );
        state.auth = AuthStatus::Connected;
        state.repos = vec![repo("o/r")];

        state.sign_out();
        assert_eq!(state.auth, AuthStatus::Disconnected);
        assert!(state.repos.is_empty());
        assert!(shared.read().unwrap().is_none());
    }
}
