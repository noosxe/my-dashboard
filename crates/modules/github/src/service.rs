//! Service layer (docs/0005 Q6, Q7, Q9; docs/0007): one state entity
//! written by a 60-second tick loop.
//!
//! Fetching model (docs/0007 D6): one **global GraphQL snapshot** per
//! bootstrap + every 5th tick (viewer + orgs + all repos with PR/issue
//! data and exact counts), global REST notifications every tick, and a
//! repo-scoped snapshot + one-shot repo notifications when a repo view
//! opens. ETag revalidation survives only on the REST notifications path;
//! last-known caches under the 0004 cache root persist the snapshot
//! (`snapshot-v2`) and notifications. 401/403 handling per the [DG]
//! states matrix.
//!
//! Split of responsibilities:
//! - pure state machinery here (`plan_tick`, `run_spec`, `apply`) and the
//!   401 [`AuthGate`] — unit-tested with a scripted transport, no gpui
//!   involved;
//! - thin gpui orchestration at the bottom (`spawn_tick_loop`,
//!   `run_spec_tracked` with the docs/0028 refresh gate, `start_connect`,
//!   `sign_out_now`) — every state write ends with the 0003 §3.12
//!   unfocused refresh.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::channel::oneshot;
use gpui::AnyWindowHandle;
use gpui::AsyncApp;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use dashboard_core::AppPaths;
use dashboard_core::status;
use dashboard_core::store;
use gpui::App;
use gpui::Entity;

use crate::api::{self, Notification};
use crate::auth::{
    AuthError, PollOutcome, RefreshOutcome, StoredCredential, TokenStore, now_epoch, poll_token,
    refresh, start_device_flow,
};
use crate::gql::{self, Org, Repo, User};
use crate::http::{Http, HttpError, RateLimit, SharedToken, Verb};

/// Tick cadence (docs/0005 Q7): the first beat runs immediately on open;
/// afterwards notifications every tick, the global snapshot every 5th.
pub const TICK: Duration = Duration::from_secs(60);
const SNAPSHOT_EVERY: u64 = 5;
/// Page cap for the REST notifications pagination (docs/0005 §7).
const MAX_PAGES: u32 = 10;
/// Proactive refresh lead (docs/0011 Q7): the tick loop refreshes the
/// token this long before `access_expires_at`.
const REFRESH_LEAD_SECS: u64 = 600;
/// Status-bar module id (docs/0013) — matches `GithubModule::meta().id`.
const MODULE_ID: &str = "github";

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
// Auth gate (docs/0028): single-flight refresh + FIFO retry queue.
// ---------------------------------------------------------------------------

/// What the gate told a 401-ed run (docs/0028 §3.2).
#[derive(Debug)]
pub(crate) enum GateEntry {
    /// You are first: run the refresh, then complete the queue. Your own
    /// oneshot is queue item #1 — await it like any other waiter.
    Lead(oneshot::Receiver<GateOutcome>),
    /// A refresh is in flight — await its outcome.
    Wait(oneshot::Receiver<GateOutcome>),
    /// The token rotated after this run dispatched: re-dispatch with the
    /// current token, nothing spent (docs/0028 Q3).
    RetryFresh,
    /// No refresh possible or worth it — surface the result as-is.
    Surface,
}

/// What a finished refresh told the queue (docs/0028 §3.2).
#[derive(Debug, Clone)]
pub(crate) enum GateOutcome {
    /// New credential installed — every queued run re-dispatches.
    Refreshed,
    /// Terminal auth failure — queued runs surface their original result.
    Surface,
    /// Transient transport failure — queued runs surface a synthesized
    /// network error instead (docs/0028 Q4); the chance is unburnt.
    SurfaceNetwork(String),
}

/// Single-flight 401 gate (docs/0028 §3.2): one refresh at a time, a FIFO
/// queue of 401-ed runs retried after the install, and a generation
/// counter that lets pre-rotation runs retry without spending anything.
/// Plain data + a std Mutex — unit-testable without gpui. Critical
/// sections never span an await: entries take their oneshot receiver out
/// of the lock and await it outside.
pub(crate) struct AuthGate {
    st: std::sync::Mutex<GateState>,
}

struct GateState {
    /// Bumped on every install and sign-out. A 401 from a run dispatched
    /// under an older generation is explained by the rotation — it
    /// re-dispatches without a refresh.
    generation: u64,
    /// A refresh is running; its lead owns completing the queue.
    inflight: bool,
    /// 401-ed runs waiting for the current refresh, FIFO. The lead's own
    /// continuation is item #1 (docs/0028 §1: the original 401 receiver
    /// retries first).
    queue: VecDeque<oneshot::Sender<GateOutcome>>,
    /// The current credential's refresh chance is spent (Reauth /
    /// keychain failure / signed out): every 401 surfaces until the next
    /// install or connect.
    exhausted: bool,
}

impl AuthGate {
    pub(crate) fn new() -> Self {
        Self {
            st: std::sync::Mutex::new(GateState {
                generation: 0,
                inflight: false,
                queue: VecDeque::new(),
                exhausted: false,
            }),
        }
    }

    /// The generation a run dispatched under — captured before the
    /// request so a post-refresh 401 can be attributed to the rotation.
    pub(crate) fn generation(&self) -> u64 {
        self.st.lock().unwrap().generation
    }

    /// Whether the current credential still has its refresh chance.
    pub(crate) fn exhausted(&self) -> bool {
        self.st.lock().unwrap().exhausted
    }

    /// A run received a 401 (docs/0028 §3.2 decision table).
    pub(crate) fn on_401(&self, used_gen: u64, round_spent: bool, has_refresh: bool) -> GateEntry {
        let mut st = self.st.lock().unwrap();
        if st.generation != used_gen {
            // The token died of rotation, not of rejection (docs/0028 Q3).
            return GateEntry::RetryFresh;
        }
        if round_spent || !has_refresh || st.exhausted {
            return GateEntry::Surface;
        }
        st.enter()
    }

    /// The tick loop's proactive entry (docs/0028 §3.4): lead or join the
    /// current refresh; `None` when the credential is spent or absent.
    pub(crate) fn refresh_now(&self, has_refresh: bool) -> Option<GateEntry> {
        let mut st = self.st.lock().unwrap();
        if st.exhausted || !has_refresh {
            return None;
        }
        Some(st.enter())
    }

    /// A credential was installed (connect or refresh): the generation
    /// moves and the chance resets (docs/0028 §3.4).
    pub(crate) fn installed(&self) {
        let mut st = self.st.lock().unwrap();
        st.generation += 1;
        st.exhausted = false;
    }

    /// The credential's chance is spent terminally (Reauth, keychain
    /// failure) — no further refresh attempts until the next install.
    pub(crate) fn spend(&self) {
        self.st.lock().unwrap().exhausted = true;
    }

    /// Sign-out (docs/0028 §3.2): the generation moves, the chance is
    /// spent until the next connect, and any queued runs surface instead
    /// of retrying against cleared slots.
    pub(crate) fn reset(&self) {
        let queue = {
            let mut st = self.st.lock().unwrap();
            st.generation += 1;
            st.exhausted = true;
            std::mem::take(&mut st.queue)
        };
        for tx in queue {
            let _ = tx.send(GateOutcome::Surface);
        }
    }

    /// A finished refresh completes the queue, FIFO (docs/0028 §3.2).
    /// Returns the drained count. Safe after a `reset` race — the queue
    /// is then empty and this is a no-op.
    pub(crate) fn complete(&self, outcome: GateOutcome) -> usize {
        let queue = {
            let mut st = self.st.lock().unwrap();
            st.inflight = false;
            std::mem::take(&mut st.queue)
        };
        let drained = queue.len();
        for tx in queue {
            let _ = tx.send(outcome.clone());
        }
        drained
    }
}

impl GateState {
    /// Shared lead/wait body — the caller holds the lock and has passed
    /// the spent/absent checks.
    fn enter(&mut self) -> GateEntry {
        let (tx, rx) = oneshot::channel();
        if self.inflight {
            self.queue.push_back(tx);
            return GateEntry::Wait(rx);
        }
        self.inflight = true;
        self.queue.push_back(tx); // item #1: the original 401 retries first
        GateEntry::Lead(rx)
    }
}

// ---------------------------------------------------------------------------
// Last-known + ETag caches (Q6) — transient data under the cache root.
// ---------------------------------------------------------------------------

#[derive(Clone)]
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
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
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
            log::warn!("etag cache write failed: {e}");
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
            log::warn!("last-known write failed ({rel}): {e}");
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
    /// The global GraphQL snapshot (docs/0007 D1/D2/D4).
    Snapshot,
    /// Global unread notifications (REST + ETag — the only GraphQL-less
    /// data left, docs/0007 §1).
    Notifications,
    /// Repo-scoped snapshot on view open (docs/0007 D6).
    RepoSnapshot,
    /// Repo-scoped unread notifications — fetched once per open (D6).
    RepoNotifications,
    /// One PR's detail payload (docs/0016 D2) — fetched by the PR page
    /// on open/refresh; the 200 body is parsed page-side, never by
    /// [`GithubState::apply`] (only non-200 results are forwarded there
    /// for the shared auth/rate-limit handling).
    PrDetail,
    /// A PR-page write (docs/0016 §6.2 D2) — the payload says which of
    /// the five mutation documents to post. Same page-owned result
    /// convention as [`Kind::PrDetail`]: the page parses 200 bodies;
    /// only non-200 results are forwarded to [`GithubState::apply`].
    PrMutation(Mutation),
    /// One PR's changed-file list (docs/0016 §6.3 D8) — REST
    /// `GET /repos/{o}/{r}/pulls/{n}/files`, fetched lazily by the PR
    /// page the first time the Files tab opens (and again on refresh
    /// once loaded). Same page-owned result convention as
    /// [`Kind::PrDetail`]: the page parses the stitched 200 body;
    /// non-200 results forward to [`GithubState::apply`].
    PrFiles,
    /// One issue's detail payload (docs/0020 D2) — the issue page's
    /// sibling of [`Kind::PrDetail`], same page-owned result convention:
    /// the page parses 200 bodies; only non-200 results are forwarded to
    /// [`GithubState::apply`] for the shared auth/rate-limit handling.
    IssueDetail,
    /// docs/0022 D5 — the issue-page pickers' repository-level source
    /// pages (labels / assignable users / open milestones), fetched
    /// lazily on first picker open (the files-sidebar `PrFiles`
    /// pattern). Same page-owned result convention as [`Kind::IssueDetail`].
    RepoLabels,
    RepoAssignableUsers,
    RepoMilestones,
    /// One notification thread's payload (docs/0026 D2) — the
    /// notification page's GET of `GET /notifications/threads/{id}`.
    /// Page-owned result convention per [`Kind::IssueDetail`]: the page
    /// parses the 200 body (the schema equals the list item —
    /// docs/0026 §2.1 — so it parses straight into `api::Notification`);
    /// only non-200 results are forwarded to [`GithubState::apply`].
    ThreadDetail,
    /// A notification-page write (docs/0026 D3) — the REST mutation
    /// lane: the verb (PATCH/DELETE) rides the [`ThreadOp`], no body,
    /// success is any 2xx (205/204). The page treats 2xx as settled and
    /// forwards only non-2xx to [`GithubState::apply`] for the shared
    /// auth/rate-limit handling.
    ThreadMutation(ThreadOp),
}

/// The five PR-page mutations (docs/0016 §6.2), each mapping to one
/// GraphQL document — the page builds the input, the transport just
/// swaps the document string (D2: "transport is identical").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mutation {
    Comment,
    Review,
    UpdateDescription,
    Merge,
    Close,
    // 0019 D5: timeline-row writes — one variant per document.
    UpdateComment,
    DeleteComment,
    UpdateReview,
    DeleteReview,
    // 0020 D2: the issue page's two write documents — same transport,
    // different input shapes.
    IssueClose,
    IssueReopen,
    // 0022 D1: the issue page's metadata writes — all four post the
    // shared updateIssue document (`ISSUE_UPDATE_MUTATION`); the
    // variant is the op identity (busy label, log line, gating), not a
    // new document.
    IssueSetTitle,
    IssueSetLabels,
    IssueSetAssignees,
    IssueSetMilestone,
    // 0023 D3/D5: reaction toggles (add/remove share the input shape,
    // two documents) + conversation locking. Same transport, op
    // identity rides the variant.
    AddReaction,
    RemoveReaction,
    LockIssue,
    UnlockIssue,
    // 0024 D5: the creator's own document — the minimum input
    // (repositoryId + title, body optional), one op identity.
    CreateIssue,
    // 0020 D2: the issue page reuses the shared addComment /
    // updateIssue / updateIssueComment / deleteIssueComment documents;
    // the page passes those existing variants unchanged.
}

/// The notification-thread write ops (docs/0026 D3/D8): the module's
/// REST mutation lane — every earlier write is GraphQL POST, `FetchSpec`
/// is GET-only for REST, so the verb rides the op instead. `MarkRead` is
/// `PATCH /notifications/threads/{id}` (→ 205); `MarkDone` (`DELETE`,
/// → 204, removes the thread from the inbox) fires from the
/// notification page and the repo-view rows (#153).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadOp {
    MarkRead,
    MarkDone,
}

#[derive(Clone, Debug)]
pub struct FetchSpec {
    pub kind: Kind,
    pub url: String,
    pub etag: Option<String>,
    /// `owner/repo` for the per-repo kinds.
    pub ident: Option<String>,
    pub max_pages: u32,
    /// GraphQL variables (JSON) — `None` for REST specs.
    pub variables: Option<Value>,
}

/// What a background run hands back to the main thread. `body` is raw
/// JSON — parsed during `apply` so the background side stays type-free.
/// `cursor` is the repos continuation cursor after a capped snapshot run
/// (docs/0007 §5): `None` for REST kinds and completed runs.
pub struct SpecResult {
    pub kind: Kind,
    pub ident: Option<String>,
    pub status: u16,
    pub etag: Option<String>,
    pub rate: Option<RateLimit>,
    pub body: Option<String>,
    pub network: Option<String>,
    pub cursor: Option<String>,
}

/// Run one spec to completion. Sync — runs on the background executor.
pub fn run_spec(http: &dyn Http, spec: &FetchSpec) -> SpecResult {
    match spec.kind {
        Kind::Snapshot => run_snapshot(http, spec),
        Kind::RepoSnapshot => run_repo_snapshot(http, spec),
        Kind::PrDetail => run_pr_detail(http, spec),
        Kind::PrMutation(m) => run_pr_mutation(http, spec, m),
        Kind::IssueDetail => run_issue_detail(http, spec),
        Kind::ThreadDetail => run_thread_detail(http, spec),
        Kind::ThreadMutation(op) => run_thread_mutation(http, spec, op),
        Kind::RepoLabels => run_graphql_fetch(http, spec, gql::REPO_LABELS_QUERY, "repo-labels"),
        Kind::RepoAssignableUsers => run_graphql_fetch(
            http,
            spec,
            gql::REPO_ASSIGNABLE_USERS_QUERY,
            "repo-assignable-users",
        ),
        Kind::RepoMilestones => {
            run_graphql_fetch(http, spec, gql::REPO_MILESTONES_QUERY, "repo-milestones")
        }
        Kind::Notifications | Kind::RepoNotifications | Kind::PrFiles => run_rest(http, spec),
    }
}

/// REST GET with page stitching + page-1 ETag revalidation (docs/0005
/// §3.3) — notifications only now.
fn run_rest(http: &dyn Http, spec: &FetchSpec) -> SpecResult {
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
        log::debug!(
            "fetch {}: page {page} (etag sent: {})",
            spec.kind.kind_name(),
            spec.etag.is_some() && page == 1
        );
        let sent_at = Instant::now();
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
                log::debug!(
                    "fetch {}: page {page} → {} in {}ms",
                    spec.kind.kind_name(),
                    status,
                    sent_at.elapsed().as_millis()
                );
                if status == 304 {
                    log::debug!("fetch {}: unchanged (ETag 304)", spec.kind.kind_name());
                }
                if status != 200 {
                    break;
                }
                let body = result.body.unwrap_or_default();
                let count = serde_json::from_str::<Vec<Value>>(&body)
                    .map(|v| v.len())
                    .unwrap_or(0);
                if count == 0 && !body.trim().starts_with('[') {
                    // Non-array payload (error object) — surfaced below as
                    // a status the caller can act on.
                    log::warn!(
                        "{}: non-array payload at page {page}",
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
        cursor: None,
    }
}

/// The global snapshot (docs/0007 D4/D5): up to `SNAPSHOT_MAX_PAGES`
/// repos pages per run, cursor-following; the pages stitch into one body
/// so `apply` lands the snapshot as a single atomic unit. A run that hits
/// the page cap hands its cursor back for the next snapshot tick.
fn run_snapshot(http: &dyn Http, spec: &FetchSpec) -> SpecResult {
    let mut status = 0u16;
    let mut rate = None;
    let mut network = None;
    let mut raw_first: Option<String> = None;
    let mut datas: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    let mut next_cursor: Option<String> = None;
    let mut cursor = spec
        .variables
        .as_ref()
        .and_then(|v| v.get("cursor"))
        .and_then(|c| c.as_str())
        .map(str::to_string);

    for page_no in 1..=gql::SNAPSHOT_MAX_PAGES {
        let variables = serde_json::json!({ "cursor": cursor });
        log::debug!(
            "fetch snapshot: page {page_no} (cursor: {})",
            cursor.as_deref().unwrap_or("none")
        );
        let sent_at = Instant::now();
        match http.post_graphql(gql::GLOBAL_SNAPSHOT_QUERY, &variables) {
            Err(HttpError(m)) => {
                network = Some(m);
                break;
            }
            Ok(result) => {
                status = result.status;
                rate = result.rate;
                log::debug!(
                    "fetch snapshot: page {page_no} → {} in {}ms",
                    status,
                    sent_at.elapsed().as_millis()
                );
                if status != 200 {
                    break;
                }
                let body = result.body.unwrap_or_default();
                if raw_first.is_none() {
                    raw_first = Some(body.clone());
                }
                let value: Value = match serde_json::from_str(&body) {
                    Ok(v) => v,
                    Err(e) => {
                        log::warn!("snapshot: malformed page: {e}");
                        break;
                    }
                };
                if let Some(errs) = value.get("errors").and_then(|e| e.as_array()) {
                    errors.extend(errs.iter().cloned());
                }
                let Some(data) = value.get("data").filter(|d| !d.is_null()).cloned() else {
                    break;
                };
                let repos = &data["viewer"]["repositories"];
                let has_next = repos["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false);
                next_cursor = has_next
                    .then(|| repos["pageInfo"]["endCursor"].as_str().map(str::to_string))
                    .flatten();
                datas.push(data);
                match &next_cursor {
                    Some(c) => cursor = Some(c.clone()),
                    None => break,
                }
            }
        }
    }

    let body = if network.is_none() && status == 200 {
        Some(stitch_pages(datas, errors, raw_first))
    } else {
        None
    };

    SpecResult {
        kind: spec.kind,
        ident: None,
        status,
        etag: None,
        rate,
        body,
        network,
        cursor: next_cursor,
    }
}

/// Stitch snapshot pages into one body: concat `repositories.nodes` across
/// pages, keep the last page's `pageInfo`, and attach any accumulated
/// `errors[]` so `apply` can log them (docs/0007 §6).
fn stitch_pages(mut datas: Vec<Value>, errors: Vec<Value>, raw_first: Option<String>) -> String {
    if datas.is_empty() {
        // No usable pages — pass the first raw body through so `apply`
        // sees the envelope (typically `data: null` + `errors[]`).
        return raw_first.unwrap_or_else(|| serde_json::json!({ "data": null }).to_string());
    }
    let mut base = serde_json::json!({ "data": datas.remove(0) });
    if datas.is_empty() && errors.is_empty() {
        return base.to_string();
    }
    let data = &mut base["data"];
    let mut nodes = data["viewer"]["repositories"]["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for page in &datas {
        nodes.extend(
            page["viewer"]["repositories"]["nodes"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        );
    }
    if let Some(last) = datas.last() {
        data["viewer"]["repositories"]["pageInfo"] =
            last["viewer"]["repositories"]["pageInfo"].clone();
    }
    data["viewer"]["repositories"]["nodes"] = Value::Array(nodes);
    if !errors.is_empty() {
        base["errors"] = Value::Array(errors);
    }
    base.to_string()
}

/// Repo-scoped snapshot (docs/0007 D6): one GraphQL call, body passthrough.
fn run_repo_snapshot(http: &dyn Http, spec: &FetchSpec) -> SpecResult {
    let mut status = 0u16;
    let mut rate = None;
    let mut network = None;
    let mut body = None;
    let (owner, name) = split_full_name(spec.ident.as_deref().unwrap_or_default());
    let variables = serde_json::json!({ "owner": owner, "name": name });
    log::debug!("fetch repo-snapshot: {}/{}", owner, name);
    let sent_at = Instant::now();
    match http.post_graphql(gql::REPO_SNAPSHOT_QUERY, &variables) {
        Err(HttpError(m)) => network = Some(m),
        Ok(result) => {
            status = result.status;
            rate = result.rate;
            log::debug!(
                "fetch repo-snapshot: → {} in {}ms",
                status,
                sent_at.elapsed().as_millis()
            );
            if status == 200 {
                body = result.body;
            }
        }
    }
    SpecResult {
        kind: spec.kind,
        ident: spec.ident.clone(),
        status,
        etag: None,
        rate,
        body,
        network,
        cursor: None,
    }
}

/// One detail query (docs/0016 D2, docs/0020 D2): a single GraphQL call,
/// body passthrough — shared by the PR and issue detail pages, only the
/// document differs. The page parses the 200 body itself; `ident`
/// carries `owner/repo#number` for logging.
fn run_graphql_fetch(
    http: &dyn Http,
    spec: &FetchSpec,
    doc: &str,
    name: &'static str,
) -> SpecResult {
    let mut status = 0u16;
    let mut rate = None;
    let mut network = None;
    let mut body = None;
    log::debug!("fetch {name}: {}", spec.ident.as_deref().unwrap_or("?"));
    let sent_at = Instant::now();
    match http.post_graphql(doc, spec.variables.as_ref().unwrap_or(&Value::Null)) {
        Err(HttpError(m)) => network = Some(m),
        Ok(result) => {
            status = result.status;
            rate = result.rate;
            log::debug!(
                "fetch {name}: → {} in {}ms",
                status,
                sent_at.elapsed().as_millis()
            );
            if status == 200 {
                body = result.body;
            }
        }
    }
    SpecResult {
        kind: spec.kind,
        ident: spec.ident.clone(),
        status,
        etag: None,
        rate,
        body,
        network,
        cursor: None,
    }
}

fn run_pr_detail(http: &dyn Http, spec: &FetchSpec) -> SpecResult {
    run_graphql_fetch(http, spec, gql::PR_DETAIL_QUERY, "pr-detail")
}

fn run_issue_detail(http: &dyn Http, spec: &FetchSpec) -> SpecResult {
    run_graphql_fetch(http, spec, gql::ISSUE_DETAIL_QUERY, "issue-detail")
}

/// One thread GET (docs/0026 D2): a single REST call, body passthrough
/// on 200 — no pagination (one object, not a list) and no ETag (the
/// page wants freshness on every open). The page parses the 200 body
/// itself; non-200 results forward to [`GithubState::apply`].
fn run_thread_detail(http: &dyn Http, spec: &FetchSpec) -> SpecResult {
    let mut status = 0u16;
    let mut rate = None;
    let mut network = None;
    let mut body = None;
    log::debug!("thread-detail: {}", spec.ident.as_deref().unwrap_or("?"));
    let sent_at = Instant::now();
    match http.request(Verb::Get, &spec.url, None, None) {
        Err(HttpError(m)) => network = Some(m),
        Ok(result) => {
            status = result.status;
            rate = result.rate;
            log::debug!(
                "thread-detail: → {} in {}ms",
                status,
                sent_at.elapsed().as_millis()
            );
            if status == 200 {
                body = result.body;
            }
        }
    }
    SpecResult {
        kind: spec.kind,
        ident: spec.ident.clone(),
        status,
        etag: None,
        rate,
        body,
        network,
        cursor: None,
    }
}

/// The REST mutation lane (docs/0026 D3): the verb rides the op —
/// PATCH for `MarkRead`, DELETE for `MarkDone` — no body, no ETag.
/// Success is any 2xx (205 Reset Content / 204 No Content); the result
/// carries no body because the page only branches on status.
fn run_thread_mutation(http: &dyn Http, spec: &FetchSpec, op: ThreadOp) -> SpecResult {
    let verb = match op {
        ThreadOp::MarkRead => Verb::Patch,
        ThreadOp::MarkDone => Verb::Delete,
    };
    let mut status = 0u16;
    let mut rate = None;
    let mut network = None;
    log::debug!(
        "thread-mutation {:?}: {}",
        op,
        spec.ident.as_deref().unwrap_or("?")
    );
    let sent_at = Instant::now();
    match http.request(verb, &spec.url, None, None) {
        Err(HttpError(m)) => network = Some(m),
        Ok(result) => {
            status = result.status;
            rate = result.rate;
            log::debug!(
                "thread-mutation {:?}: → {} in {}ms",
                op,
                status,
                sent_at.elapsed().as_millis()
            );
        }
    }
    SpecResult {
        kind: spec.kind,
        ident: spec.ident.clone(),
        status,
        etag: None,
        rate,
        body: None,
        network,
        cursor: None,
    }
}

/// One PR-page mutation (docs/0016 §6.2 D2): a single GraphQL call with
/// the document picked by the mutation kind — the same `post_graphql`
/// transport, auth, and token path as every query. The PR page parses
/// the 200 body itself (checking the `errors` array); `ident` carries
/// `owner/repo#number <mutation>` for logging.
fn run_pr_mutation(http: &dyn Http, spec: &FetchSpec, mutation: Mutation) -> SpecResult {
    let doc = match mutation {
        Mutation::Comment => gql::PR_ADD_COMMENT_MUTATION,
        Mutation::Review => gql::PR_ADD_REVIEW_MUTATION,
        Mutation::UpdateDescription => gql::PR_UPDATE_DESCRIPTION_MUTATION,
        Mutation::Merge => gql::PR_MERGE_MUTATION,
        Mutation::Close => gql::PR_CLOSE_MUTATION,
        Mutation::UpdateComment => gql::PR_UPDATE_COMMENT_MUTATION,
        Mutation::DeleteComment => gql::PR_DELETE_COMMENT_MUTATION,
        Mutation::UpdateReview => gql::PR_UPDATE_REVIEW_MUTATION,
        Mutation::DeleteReview => gql::PR_DELETE_REVIEW_MUTATION,
        Mutation::IssueClose => gql::ISSUE_CLOSE_MUTATION,
        Mutation::IssueReopen => gql::ISSUE_REOPEN_MUTATION,
        // 0022 D1: one document, four op identities — the input's
        // single field-group is what distinguishes the writes.
        Mutation::IssueSetTitle => gql::ISSUE_UPDATE_MUTATION,
        Mutation::IssueSetLabels => gql::ISSUE_UPDATE_MUTATION,
        Mutation::IssueSetAssignees => gql::ISSUE_UPDATE_MUTATION,
        Mutation::IssueSetMilestone => gql::ISSUE_UPDATE_MUTATION,
        // 0023 D3/D5: four op identities over two documents — add/remove
        // share the `subjectId`+`content` input, lock/unlock their own.
        Mutation::AddReaction => gql::ADD_REACTION_MUTATION,
        Mutation::RemoveReaction => gql::REMOVE_REACTION_MUTATION,
        Mutation::LockIssue => gql::LOCK_LOCKABLE_MUTATION,
        Mutation::UnlockIssue => gql::UNLOCK_LOCKABLE_MUTATION,
        // 0024 D5: the creator's document.
        Mutation::CreateIssue => gql::CREATE_ISSUE_MUTATION,
    };
    let mut status = 0u16;
    let mut rate = None;
    let mut network = None;
    let mut body = None;
    log::debug!(
        "pr-mutation {:?}: {}",
        mutation,
        spec.ident.as_deref().unwrap_or("?")
    );
    let sent_at = Instant::now();
    match http.post_graphql(doc, spec.variables.as_ref().unwrap_or(&Value::Null)) {
        Err(HttpError(m)) => network = Some(m),
        Ok(result) => {
            status = result.status;
            rate = result.rate;
            log::debug!(
                "pr-mutation {:?}: → {} in {}ms",
                mutation,
                status,
                sent_at.elapsed().as_millis()
            );
            if status == 200 {
                body = result.body;
            }
        }
    }
    SpecResult {
        kind: spec.kind,
        ident: spec.ident.clone(),
        status,
        etag: None,
        rate,
        body,
        network,
        cursor: None,
    }
}

/// One line of a unified-diff `patch` (docs/0016 D8): the variant carries
/// the row's tone, the string is the line's content (sign stripped; the
/// hunk keeps its full `@@ … @@` header) — the renderer colors by variant
/// and re-prepends the sign glyph for +/- rows, never re-parsing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PatchLine {
    Hunk(String),
    Context(String),
    Add(String),
    Del(String),
}

/// Parse a REST `patch` field into rows (docs/0016 D8 — pure, so the
/// render path stays dumb and the parser is table-testable). Tolerant by
/// design: empty input is an empty list; any line that does not start
/// with `@@`, `+` or `-` (including blank context lines) is context.
pub(crate) fn parse_patch(patch: &str) -> Vec<PatchLine> {
    patch
        .lines()
        .map(|line| {
            if line.starts_with("@@") {
                PatchLine::Hunk(line.to_string())
            } else if let Some(rest) = line.strip_prefix('+') {
                PatchLine::Add(rest.to_string())
            } else if let Some(rest) = line.strip_prefix('-') {
                PatchLine::Del(rest.to_string())
            } else {
                PatchLine::Context(line.strip_prefix(' ').unwrap_or(line).to_string())
            }
        })
        .collect()
}

impl Kind {
    fn kind_name(&self) -> &'static str {
        match self {
            Kind::Snapshot => "snapshot",
            Kind::Notifications => "notifications",
            Kind::RepoSnapshot => "repo-snapshot",
            Kind::PrDetail => "pr-detail",
            Kind::PrMutation(_) => "pr-mutation",
            Kind::IssueDetail => "issue-detail",
            Kind::RepoLabels => "repo-labels",
            Kind::RepoAssignableUsers => "repo-assignable-users",
            Kind::RepoMilestones => "repo-milestones",
            Kind::PrFiles => "pr-files",
            Kind::RepoNotifications => "repo-notifs",
            Kind::ThreadDetail => "thread-detail",
            Kind::ThreadMutation(_) => "thread-mutation",
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
    /// Every repo with its open-PR/issue summaries (docs/0007 D2) — the
    /// single source for card counts, repo lists, and repo views.
    pub repos: Vec<Repo>,
    /// Global unread notifications (card count + repo derivation).
    pub notifications: Vec<Notification>,
    /// Selected repo's unread notifications — fetched once at open, then
    /// derived from the global list (docs/0007 D6).
    pub repo_notifications: BTreeMap<String, Vec<Notification>>,
    /// `owner/repo` of the selected repo (session-only, not persisted).
    pub selected: Option<String>,
    /// Repos continuation cursor after a capped snapshot run (docs/0007
    /// §5); `None` = the next snapshot starts from the top.
    pub repo_cursor: Option<String>,
    /// Rate-limit pause: UTC epoch seconds until requests resume (Q9).
    /// When the access token dies (docs/0011 §4.1); `None` = no expiry.
    /// Non-secret, unlike the tokens themselves.
    pub access_expires_at: Option<u64>,
    /// Armed by the proactive trigger (docs/0028 §3.4), drained by the
    /// tick loop in the same tick it armed in. Reactive refresh does not
    /// pass through here — the 401 gate owns it (docs/0028 §3.2).
    pub(crate) pending_refresh: bool,
    /// Single-flight 401 gate: one refresh at a time, FIFO retry queue,
    /// rotation-generation tracking (docs/0028 §3.2).
    pub(crate) gate: Arc<AuthGate>,
    pub rate_limited_until: Option<u64>,
    /// Set when a connect flow should fetch the snapshot on next tick.
    pub needs_bootstrap: bool,
    pub(crate) etags: BTreeMap<String, String>,
    pub(crate) client: Arc<dyn Http>,
    pub(crate) store: Arc<dyn TokenStore>,
    pub(crate) shared_token: SharedToken,
    /// Refresh-token slot (docs/0011 §4.3): same handling rules as the
    /// access token — keychain + slot only, never in state or logs.
    pub(crate) shared_refresh: SharedToken,
    pub(crate) cache: GithubCache,
}

impl GithubState {
    /// Build from the keychain: seeds last-known data (no skeleton flash
    /// for returning users, Q6) and resolves the auth status.
    pub fn new(
        client: Arc<dyn Http>,
        store: Arc<dyn TokenStore>,
        shared_token: SharedToken,
        shared_refresh: SharedToken,
        cache: GithubCache,
    ) -> Self {
        let etags = cache.etags();
        let (auth, access_expires_at) = match store.load() {
            Ok(Some(cred)) => {
                if let Ok(mut slot) = shared_token.write() {
                    *slot = Some(cred.access_token.clone());
                }
                if let Ok(mut slot) = shared_refresh.write() {
                    *slot = cred.refresh_token.clone();
                }
                // Legacy non-expiring credential (docs/0011 §7): works
                // as-is, never refreshed.
                if cred.refresh_token.is_none() {
                    log::debug!("legacy non-expiring token loaded (no refresh)");
                }
                (AuthStatus::Connected, cred.access_expires_at)
            }
            Ok(None) => (AuthStatus::Disconnected, None),
            Err(e) => (AuthStatus::KeychainError(e), None),
        };
        let seeded = auth.connected() || matches!(auth, AuthStatus::TokenRejected);
        let snapshot: Option<gql::SnapshotCache> = seeded
            .then(|| cache.load(gql::SNAPSHOT_CACHE_KEY))
            .flatten();
        Self {
            user: snapshot.as_ref().and_then(|s| s.user.clone()),
            orgs: snapshot
                .as_ref()
                .map(|s| s.orgs.clone())
                .unwrap_or_default(),
            repos: snapshot.map(|s| s.repos).unwrap_or_default(),
            notifications: seeded
                .then(|| cache.load("notifications"))
                .flatten()
                .unwrap_or_default(),
            repo_notifications: BTreeMap::new(),
            selected: None,
            repo_cursor: None,
            rate_limited_until: None,
            access_expires_at,
            pending_refresh: false,
            gate: Arc::new(AuthGate::new()),
            // A keychain-token launch boots straight into the snapshot
            // refresh — the first tick must fire it, not wait for tick 5
            // (docs/0007 §8: cache paint, then the snapshot lands).
            needs_bootstrap: auth.connected(),
            auth,
            etags,
            client,
            store,
            shared_token,
            shared_refresh,
            cache,
        }
    }

    /// Rate-limit / auth gate (Q9): no requests while disconnected, mid
    /// connect-flow, token-rejected, or paused until the window resets.
    fn gated(&self) -> bool {
        // A pending (proactive) refresh defers fetches — they would 401
        // against a dying token (docs/0028 §3.4).
        if self.pending_refresh {
            return true;
        }
        if !self.auth.connected() {
            return true;
        }
        match self.rate_limited_until {
            Some(until) => now_epoch() < until,
            None => false,
        }
    }

    /// Plan this tick's work (docs/0007 D6). Consumes `needs_bootstrap`.
    pub fn plan_tick(&mut self, tick: u64) -> Vec<FetchSpec> {
        // Gate first: a rate-limit pause (or auth loss) defers bootstrap
        // too — the flag survives until the window passes.
        if let Some(until) = self.rate_limited_until {
            if now_epoch() >= until {
                self.rate_limited_until = None;
                log::info!("rate-limit window passed — resuming fetches");
            }
        }
        // Proactive refresh arming (docs/0028 §3.4) — before the gates:
        // the refresh call spends no API budget, so it is armed even
        // during a rate-limit pause. The tick loop drains it in the same
        // tick through the gate. A spent credential (Reauth, keychain
        // failure) never re-arms — the reconnect state owns the session.
        if !self.pending_refresh
            && !self.gate.exhausted()
            && self.has_refresh_token()
            && self
                .access_expires_at
                .is_some_and(|at| now_epoch() + REFRESH_LEAD_SECS >= at)
        {
            log::debug!(
                "token refresh due (expiry epoch {}) — proactive",
                self.access_expires_at.unwrap_or(0)
            );
            self.pending_refresh = true;
        }
        if self.gated() {
            return Vec::new();
        }
        if self.needs_bootstrap {
            self.needs_bootstrap = false;
            return vec![
                self.snapshot_spec(),
                self.spec(Kind::Notifications, api::notifications_url(1), None),
            ];
        }
        if self.gated() {
            return Vec::new();
        }
        let mut specs = vec![self.spec(
            Kind::Notifications,
            api::notifications_url(1),
            Some(MAX_PAGES),
        )];
        if tick % SNAPSHOT_EVERY == 0 {
            specs.push(self.snapshot_spec());
        }
        specs
    }

    /// The global snapshot spec, continuing from `repo_cursor` when a
    /// previous run was capped (docs/0007 §5).
    pub(crate) fn snapshot_spec(&self) -> FetchSpec {
        FetchSpec {
            kind: Kind::Snapshot,
            url: gql::GRAPHQL_ENDPOINT.to_string(),
            etag: None,
            ident: None,
            max_pages: 1,
            variables: Some(serde_json::json!({ "cursor": self.repo_cursor })),
        }
    }

    /// Work fired when a repo view opens (docs/0007 D6): the
    /// edge-freshness repo-scoped snapshot plus the one-shot repo-scoped
    /// notifications fetch. Later ticks update the GroupBox from the
    /// global list instead.
    pub(crate) fn repo_open_specs(&self, full_name: &str) -> Vec<FetchSpec> {
        let (owner, name) = split_full_name(full_name);
        vec![
            FetchSpec {
                kind: Kind::RepoSnapshot,
                url: gql::GRAPHQL_ENDPOINT.to_string(),
                etag: None,
                ident: Some(full_name.to_string()),
                max_pages: 1,
                variables: Some(serde_json::json!({ "owner": owner, "name": name })),
            },
            self.spec_ident(
                Kind::RepoNotifications,
                Some(full_name),
                api::repo_notifications_url(&owner, &name, 1),
                Some(2),
            ),
        ]
    }

    /// Post-write refetch (docs/0026 D3/D8): the global unread list plus
    /// the selected repo's one-shot slice — the marked/done row leaves
    /// both once these land. Host-run on the notification page's
    /// `MarkSettled`/`DoneSettled` events and after a settled row-level
    /// done (the `repo_open_specs` shape minus the repo snapshot, which
    /// the writes do not touch).
    pub(crate) fn mark_refetch_specs(&self) -> Vec<FetchSpec> {
        let mut specs = vec![self.spec(
            Kind::Notifications,
            api::notifications_url(1),
            Some(MAX_PAGES),
        )];
        if let Some(full) = &self.selected {
            let (owner, name) = split_full_name(full);
            specs.push(self.spec_ident(
                Kind::RepoNotifications,
                Some(full),
                api::repo_notifications_url(&owner, &name, 1),
                Some(2),
            ));
        }
        specs
    }

    pub(crate) fn spec(&self, kind: Kind, url: String, max_pages: Option<u32>) -> FetchSpec {
        self.spec_ident(kind, None, url, max_pages)
    }

    /// `spec` with an identity — per-repo kinds key their ETags per repo.
    pub(crate) fn spec_ident(
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
            variables: None,
        }
    }

    /// Apply a parsed global notifications payload to state: replace the
    /// list and re-derive the selected repo's GroupBox slice (docs/0007
    /// D6). Shared by the 200 path and the ETag-304 restore (docs/0005
    /// Q6, #84).
    fn apply_notifications(&mut self, items: Vec<Notification>) {
        let unread = items.iter().filter(|n| n.unread).count();
        self.notifications = items;
        log::debug!("notifications applied: {unread} unread");
        if let Some(sel) = self.selected.clone() {
            let derived: Vec<Notification> = self
                .notifications
                .iter()
                .filter(|n| n.repository.full_name == sel)
                .cloned()
                .collect();
            self.repo_notifications.insert(sel, derived);
        }
    }

    /// Apply a parsed per-repo notifications payload to state. Shared by
    /// the 200 path and the ETag-304 restore (docs/0005 Q6, #84).
    fn apply_repo_notifications(&mut self, id: &str, items: Vec<Notification>) {
        let unread = items.iter().filter(|n| n.unread).count();
        log::debug!("repo-notifications applied: {id}, {unread} unread");
        self.repo_notifications.insert(id.to_string(), items);
    }

    /// Apply a finished spec: parse, update state, store last-known + etag,
    /// handle 401/403 (Q9). Malformed payloads degrade to stale data with a
    /// stderr note — never a dialog.
    pub fn apply(&mut self, result: SpecResult) {
        if let Some(m) = result.network {
            log::warn!("{}: network error: {m}", result.kind.kind_name());
            return;
        }
        match result.status {
            200 => {}
            304 => {
                // ETag match: the last-known payload on disk is exactly
                // what the server would have returned — re-apply it so a
                // fresh session (empty memory, etags.json survived the
                // restart) still paints cached rows (docs/0005 Q6, #84).
                // A warm session re-applies the same generation: a no-op.
                match result.kind {
                    Kind::Notifications => {
                        if let Some(items) = self.cache.load::<Vec<Notification>>("notifications") {
                            log::debug!("notifications: unchanged (ETag 304) — from last-known");
                            self.apply_notifications(items);
                        } else {
                            log::debug!("notifications: unchanged (ETag 304), no last-known");
                        }
                    }
                    Kind::RepoNotifications => match result.ident.as_deref() {
                        Some(id) => {
                            let rel = format!("notifs-{}", slug(id));
                            if let Some(items) = self.cache.load::<Vec<Notification>>(&rel) {
                                log::debug!("repo-notifs: unchanged (ETag 304) — from last-known");
                                self.apply_repo_notifications(id, items);
                            } else {
                                log::debug!("repo-notifs: unchanged (ETag 304), no last-known");
                            }
                        }
                        None => log::warn!("repo-notifs spec without ident"),
                    },
                    _ => {}
                }
                return;
            }
            401 => {
                // The 401 gate already spent its chance before this
                // landed — the lead, the queue and the rotation retry all
                // happen upstream of `apply` (docs/0028 §3.1). A 401 here
                // means reconnect (docs/0005 Q9).
                log::warn!(
                    "{}: token rejected (401) — reconnect required",
                    result.kind.kind_name()
                );
                self.auth = AuthStatus::TokenRejected;
                return;
            }
            _ => {
                if result.rate.is_some_and(|r| r.remaining == 0) {
                    self.rate_limited_until = result.rate.map(|r| r.reset_epoch);
                    log::warn!(
                        "{}: rate-limited (HTTP {}) — paused until epoch {}",
                        result.kind.kind_name(),
                        result.status,
                        self.rate_limited_until.unwrap_or(0)
                    );
                } else {
                    log::warn!("{}: HTTP {}", result.kind.kind_name(), result.status);
                }
                return;
            }
        }
        if result.rate.is_some_and(|r| r.remaining == 0) {
            self.rate_limited_until = result.rate.map(|r| r.reset_epoch);
            log::warn!(
                "{}: rate-limited — paused until epoch {}",
                result.kind.kind_name(),
                self.rate_limited_until.unwrap_or(0)
            );
        }
        let Some(body) = result.body.as_deref() else {
            return;
        };

        match result.kind {
            Kind::Snapshot => {
                match serde_json::from_str::<gql::Envelope<gql::SnapshotData>>(body) {
                    Ok(env) => {
                        for e in &env.errors {
                            log::warn!("snapshot: {}", e.message);
                        }
                        let Some(data) = env.data else {
                            log::warn!("snapshot: data null — keeping last-known");
                            return; // stale data stays (docs/0007 §6)
                        };
                        if let Some(rl) = &data.rate_limit {
                            if rl.remaining <= 0 {
                                self.rate_limited_until = Some(rl.reset_epoch());
                                log::warn!(
                                    "snapshot: GraphQL point budget exhausted — paused until {}",
                                    rl.reset_at
                                );
                            }
                        }
                        // One snapshot, one atomic apply (docs/0007 D4).
                        self.user = Some(User {
                            login: data.viewer.login.clone(),
                            avatar_url: data.viewer.avatar_url.clone(),
                        });
                        self.apply_orgs(gql::orgs_from_values(&data.viewer.organizations.nodes));
                        if let Some(page) = data.viewer.repositories {
                            let completed = result.cursor.is_none();
                            let incoming: Vec<Repo> =
                                page.nodes.iter().filter_map(gql::repo_from_value).collect();
                            self.merge_repos(incoming, completed);
                            self.repo_cursor = result.cursor.clone();
                        }
                        self.persist_snapshot();
                        // One concise landing note — the owner's log
                        // verification point (docs/0007 §8).
                        let open_prs: i64 = self.repos.iter().map(|r| r.prs.total_count).sum();
                        log::info!(
                            "snapshot applied: {} repos, {} open PRs",
                            self.repos.len(),
                            open_prs
                        );
                    }
                    Err(e) => log::warn!("snapshot payload: {e}"),
                }
            }
            Kind::RepoSnapshot => {
                match serde_json::from_str::<gql::Envelope<gql::RepoSnapshotData>>(body) {
                    Ok(env) => {
                        for e in &env.errors {
                            log::warn!("repo-snapshot: {}", e.message);
                        }
                        match env.data.and_then(|d| d.repository) {
                            Some(value) => match gql::repo_from_value(&value) {
                                Some(repo) => {
                                    let prs = repo.prs.total_count;
                                    let ident = result.ident.clone().unwrap_or_default();
                                    self.merge_repos(vec![repo], false);
                                    self.persist_snapshot();
                                    log::info!("repo-snapshot applied: {ident}, {prs} open PRs");
                                }
                                None => {
                                    log::warn!("repo-snapshot: malformed repository node");
                                }
                            },
                            None => {
                                log::warn!("repo-snapshot: repository null — keeping last-known")
                            }
                        }
                    }
                    Err(e) => log::warn!("repo-snapshot payload: {e}"),
                }
            }
            Kind::Notifications => match serde_json::from_str::<Vec<Notification>>(body) {
                Ok(items) => {
                    self.cache.put("notifications", &items);
                    self.apply_notifications(items);
                }
                Err(e) => log::warn!("notifications payload: {e}"),
            },
            Kind::RepoNotifications => match (
                result.ident.as_deref(),
                serde_json::from_str::<Vec<Notification>>(body),
            ) {
                (Some(id), Ok(items)) => {
                    self.cache.put(&format!("notifs-{}", slug(id)), &items);
                    self.apply_repo_notifications(id, items);
                }
                (None, _) => log::warn!("repo-notifs spec without ident"),
                (_, Err(e)) => log::warn!("repo-notifs payload: {e}"),
            },
            // Page-owned kind (docs/0016 D2): the PR page parses its own
            // 200 bodies. Only non-200 results are ever forwarded here,
            // and those return in the status handling above — this arm
            // exists purely for match exhaustiveness.
            Kind::PrDetail
            | Kind::PrMutation(_)
            | Kind::PrFiles
            | Kind::IssueDetail
            | Kind::RepoLabels
            | Kind::RepoAssignableUsers
            | Kind::RepoMilestones
            | Kind::ThreadDetail
            | Kind::ThreadMutation(_) => {}
        }

        // ETag keyed by logical resource (kind + ident), not raw URL, so
        // pagination page-1 etags stay stable. GraphQL kinds have no
        // conditional path — never stored.
        if matches!(result.kind, Kind::Notifications | Kind::RepoNotifications) {
            if let Some(etag) = &result.etag {
                self.etags
                    .insert(etag_key(result.kind, result.ident.as_deref()), etag.clone());
            }
            self.cache.save_etags(&self.etags);
        }
    }

    /// Merge snapshot repos into the list (docs/0007 D4): a completed run
    /// (`prune`) saw the whole list and replaces it; a capped run overlays
    /// its batch and keeps repos from earlier pages. Either way the list
    /// ends in composite last-activity order (issue #35): the connection
    /// comes back `PUSHED_AT DESC`, which ignores PR/issue activity, and
    /// a repo-open refresh must not move the row. Ties break by key so
    /// identical activity is a stable order.
    fn merge_repos(&mut self, incoming: Vec<Repo>, prune: bool) {
        if prune {
            self.repos = incoming;
        } else {
            let keys: std::collections::BTreeSet<String> =
                incoming.iter().map(|r| r.key.clone()).collect();
            let mut merged = incoming;
            let old = std::mem::take(&mut self.repos);
            merged.extend(old.into_iter().filter(|r| !keys.contains(&r.key)));
            self.repos = merged;
        }
        self.repos.sort_by(|a, b| {
            b.last_activity()
                .cmp(&a.last_activity())
                .then_with(|| a.key.cmp(&b.key))
        });
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

    /// Persist the snapshot to `snapshot-v2` (docs/0007 D7) — atomic
    /// writes + quarantine via the 0004 store primitives.
    fn persist_snapshot(&self) {
        let snapshot = gql::SnapshotCache {
            schema: 2,
            user: self.user.clone(),
            orgs: self.orgs.clone(),
            repos: self.repos.clone(),
        };
        self.cache.put(gql::SNAPSHOT_CACHE_KEY, &snapshot);
    }

    /// Card totals (§6.1): unread notifications + Σ open-PR counts + Σ open-
    /// issue counts — exact via `totalCount` (docs/0007 §1, doc 0012 D2).
    pub fn card_counts(&self) -> (usize, usize, usize) {
        let unread = self.notifications.iter().filter(|n| n.unread).count();
        let prs: i64 = self.repos.iter().map(|r| r.prs.total_count).sum();
        let issues: i64 = self.repos.iter().map(|r| r.issues.total_count).sum();
        (unread, prs.max(0) as usize, issues.max(0) as usize)
    }

    /// Refresh-token presence (docs/0028 §3.2) — the gate consults this
    /// via the wrapper; slots only, never the token itself.
    pub(crate) fn has_refresh_token(&self) -> bool {
        self.shared_refresh
            .read()
            .map(|s| s.is_some())
            .unwrap_or(false)
    }

    /// Connect/refresh success path (docs/0011 §4.3): keychain FIRST —
    /// persist-before-publish — then the in-memory slots and state.
    pub(crate) fn install_credential(&mut self, cred: StoredCredential) -> Result<(), String> {
        self.store.save(&cred)?;
        if let Ok(mut slot) = self.shared_token.write() {
            *slot = Some(cred.access_token);
        }
        if let Ok(mut slot) = self.shared_refresh.write() {
            *slot = cred.refresh_token;
        }
        self.access_expires_at = cred.access_expires_at;
        // A new credential moves the gate's generation and resets its
        // chance accounting (docs/0028 §3.4) — also on the reconnect path.
        self.gate.installed();
        self.auth = AuthStatus::Connected;
        self.rate_limited_until = None;
        self.needs_bootstrap = true;
        Ok(())
    }

    /// Drain a finished refresh (docs/0011 §4.3, docs/0028 §3.4): install,
    /// retry, or end the session. Runs on the main thread from the gate
    /// lead; returns the outcome the queue settles on.
    fn apply_refresh(&mut self, outcome: RefreshOutcome) -> GateOutcome {
        match outcome {
            RefreshOutcome::Refreshed(cred) => {
                self.pending_refresh = false;
                if let Err(e) = self.install_credential(cred) {
                    // The old access token is already dead upstream —
                    // terminal for the session (docs/0011 §4.4).
                    log::warn!("token refresh keychain save failed: {e} — reconnect required");
                    self.auth = AuthStatus::KeychainError(e);
                    self.gate.spend();
                    GateOutcome::Surface
                } else {
                    log::info!(
                        "token refreshed — new expiry epoch {}",
                        self.access_expires_at.unwrap_or(0)
                    );
                    GateOutcome::Refreshed
                }
            }
            RefreshOutcome::Transient(m) => {
                // Nothing was invalidated upstream — the flag re-arms the
                // refresh on a later tick; queued runs get a network error
                // now instead of a silent wait (docs/0028 Q4), and the
                // credential's chance stays unburnt.
                log::warn!("token refresh transient failure: {m} — retrying next tick");
                self.pending_refresh = true;
                GateOutcome::SurfaceNetwork(m)
            }
            RefreshOutcome::Reauth(m) => {
                self.pending_refresh = false;
                log::warn!("token refresh failed: {m} — reconnect required");
                self.gate.spend();
                self.auth = AuthStatus::TokenRejected;
                GateOutcome::Surface
            }
        }
    }

    /// Sign-out (Q5): keychain + token slot + caches + data.
    pub fn sign_out(&mut self) {
        let _ = self.store.delete();
        if let Ok(mut slot) = self.shared_token.write() {
            *slot = None;
        }
        if let Ok(mut slot) = self.shared_refresh.write() {
            *slot = None;
        }
        self.access_expires_at = None;
        self.pending_refresh = false;
        // No queued run may retry against cleared slots (docs/0028 §3.2).
        self.gate.reset();
        self.cache.clear();
        self.user = None;
        self.orgs.clear();
        self.repos.clear();
        self.notifications.clear();
        self.repo_notifications.clear();
        self.etags.clear();
        self.selected = None;
        self.repo_cursor = None;
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
        (Kind::Notifications, _) => "notifications".into(),
        (Kind::RepoNotifications, Some(id)) => format!("rn:{id}"),
        (_, None) => "unknown".into(),
        _ => "unknown".into(),
    }
}

// ---------------------------------------------------------------------------
// gpui orchestration — thin; ends every state write with the §3.12 refresh.
// ---------------------------------------------------------------------------

/// Run one spec with the global in-flight tracker around it (docs/0013
/// D4): one `FetchSpec` run = one status-bar unit — spanning the whole
/// run-⟲-refresh-⟲-retry cycle on a 401 (docs/0028 §3.3), so the
/// indicator never flickers across a mid-run refresh. Both runners (the
/// tick loop below and `page.rs::run_specs`) go through here so
/// `begin`/`end` cannot drift apart. The tracker calls marshal through
/// `AsyncApp::update`, whose abort-on-released-app behavior is the same
/// teardown contract the surrounding loops already rely on.
pub(crate) async fn run_spec_tracked(
    state: Entity<GithubState>,
    spec: FetchSpec,
    cx: &mut AsyncApp,
) -> SpecResult {
    let id = cx.update(|cx| status::begin(cx, MODULE_ID.into()));
    let (client, gate) =
        cx.update(|cx| state.update(cx, |s, _| (s.client.clone(), s.gate.clone())));
    let mut used_gen = gate.generation();
    let mut rounds: u32 = 0;
    let result = loop {
        let result = cx
            .background_executor()
            .spawn({
                let client = client.clone();
                let spec = spec.clone();
                async move { run_spec(&*client, &spec) }
            })
            .await;
        if result.network.is_some() || result.status != 401 {
            break result;
        }
        let has_refresh = cx.update(|cx| state.read(cx).has_refresh_token());
        let outcome = match gate.on_401(used_gen, rounds > 0, has_refresh) {
            GateEntry::RetryFresh => {
                // Rotation landed while we were in flight (docs/0028 Q3)
                // — retry against the current token, nothing spent.
                used_gen = gate.generation();
                continue;
            }
            GateEntry::Surface => break result,
            GateEntry::Lead(rx) => {
                log::debug!("{}: 401 — refresh leading", spec.kind.kind_name());
                run_refresh(state.clone(), cx).await;
                rx.await.unwrap_or(GateOutcome::Surface)
            }
            GateEntry::Wait(rx) => {
                log::debug!("{}: 401 — joined in-flight refresh", spec.kind.kind_name());
                rx.await.unwrap_or(GateOutcome::Surface)
            }
        };
        match outcome {
            GateOutcome::Refreshed => {
                used_gen = gate.generation();
                rounds += 1;
            }
            GateOutcome::Surface => break result,
            GateOutcome::SurfaceNetwork(m) => {
                break transient_refresh_result(spec.kind, spec.ident.clone(), m);
            }
        }
    };
    cx.update(|cx| status::end(cx, id));
    result
}

/// Synthesize the network-error result a run surfaces when the refresh
/// failed transiently (docs/0028 Q4): it must land in the existing
/// network-loss paths — stale data, page-local error — never in
/// `TokenRejected` (docs/0011 Q5).
fn transient_refresh_result(kind: Kind, ident: Option<String>, msg: String) -> SpecResult {
    log::warn!("{}: token refresh unavailable: {msg}", kind.kind_name());
    SpecResult {
        kind,
        ident,
        status: 0,
        etag: None,
        rate: None,
        body: None,
        network: Some(format!("token refresh unavailable: {msg}")),
        cursor: None,
    }
}

/// The single refresh procedure (docs/0028 §3.2): the gate lead only.
/// Reads the grant inputs on the main thread, runs the grant on the
/// background executor, applies the outcome on the main thread (install
/// is persist-before-publish, docs/0011 §4.4), then completes the queue.
/// A sign-out or another install that landed mid-refresh drops the
/// result — the slots it would overwrite no longer belong to this
/// session.
async fn run_refresh(state: Entity<GithubState>, cx: &mut AsyncApp) {
    let (gate, client, refresh_token, used_gen) = cx.update(|cx| {
        state.update(cx, |s, _| {
            (
                s.gate.clone(),
                s.client.clone(),
                s.shared_refresh.read().ok().and_then(|t| t.clone()),
                s.gate.generation(),
            )
        })
    });
    let Some(refresh_token) = refresh_token else {
        // Sign-out raced the lead — its reset already drained the queue;
        // the guard's drop completes defensively (a no-op then).
        log::debug!("token refresh skipped — no refresh token in slot");
        return;
    };
    let mut guard = LeadGuard { gate, armed: true };
    let outcome = cx
        .background_executor()
        .spawn(async move { refresh(&*client, &refresh_token) })
        .await;
    if guard.gate.generation() != used_gen {
        log::debug!("token refresh landed after a session change — dropped");
        return; // the guard drains any waiter that raced in
    }
    // Entity observers fire only on cx.notify — a window.refresh repaint
    // never triggers them. The page sync + crumb (docs/0009 D6, docs/0014
    // D1/D6) and the card badge rely on these notifies.
    let gate_outcome = cx.update(|cx| {
        state.update(cx, |s, cx| {
            let o = s.apply_refresh(outcome);
            cx.notify();
            o
        })
    });
    let drained = guard.gate.complete(gate_outcome.clone());
    match gate_outcome {
        GateOutcome::Refreshed => {
            log::info!("refresh ok — retrying {drained} queued request(s)");
        }
        GateOutcome::Surface => {
            log::warn!("refresh reauth — surfacing auth failure to {drained} request(s)");
        }
        GateOutcome::SurfaceNetwork(_) => {
            log::warn!("refresh transient — surfacing network error to {drained} request(s)");
        }
    }
    guard.disarm();
}

/// Drains the queue if its lead is cancelled before completing
/// (docs/0028 §3.2) — a torn-down task must never hang the waiters.
struct LeadGuard {
    gate: Arc<AuthGate>,
    armed: bool,
}

impl LeadGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for LeadGuard {
    fn drop(&mut self) {
        if self.armed {
            let drained = self.gate.complete(GateOutcome::Surface);
            if drained > 0 {
                log::warn!("refresh lead cancelled — surfaced to {drained} request(s)");
            }
        }
    }
}

/// The tick loop's proactive drain (docs/0028 §3.4): lead or join the
/// in-flight refresh through the gate. `None` = spent or absent —
/// nothing to wait for; the next tick re-plans.
async fn drain_refresh(state: &Entity<GithubState>, cx: &mut AsyncApp) -> Option<GateOutcome> {
    let (gate, has_refresh) = cx.update(|cx| {
        let s = state.read(cx);
        (s.gate.clone(), s.has_refresh_token())
    });
    match gate.refresh_now(has_refresh) {
        None => None,
        Some(GateEntry::Lead(rx)) => {
            log::debug!("token refresh drain (proactive — leading)");
            run_refresh(state.clone(), cx).await;
            Some(rx.await.unwrap_or(GateOutcome::Surface))
        }
        Some(GateEntry::Wait(rx)) => {
            log::debug!("token refresh drain (proactive — joining in-flight)");
            Some(rx.await.unwrap_or(GateOutcome::Surface))
        }
        Some(_) => None,
    }
}

/// The PR page's write path (docs/0016 §6.2 D2): one mutation, tracked
/// like a fetch (one status-bar unit, 0013 D4) and on the exact same
/// transport — `post_graphql` with a different document string, the
/// authed client, and page-side non-200 forwarding for the shared
/// 401/403 handling. Retries through the gate like any other run
/// (docs/0028 §3.1): a 401 rejected the write before it executed, so
/// the re-dispatch cannot double-apply.
pub(crate) async fn run_mutation(
    state: Entity<GithubState>,
    mutation: Mutation,
    variables: Value,
    ident: String,
    cx: &mut AsyncApp,
) -> SpecResult {
    let spec = FetchSpec {
        kind: Kind::PrMutation(mutation),
        url: gql::GRAPHQL_ENDPOINT.to_string(),
        etag: None,
        ident: Some(ident),
        max_pages: 1,
        variables: Some(variables),
    };
    run_spec_tracked(state, spec, cx).await
}

/// The 60-second service loop (Q7/Q8): plan → (drain) → background-run →
/// apply. Self-gates on auth state and rate-limit windows; a proactive
/// refresh armed by planning drains in the same tick (docs/0028 §3.4).
pub fn spawn_tick_loop(state: Entity<GithubState>, window: AnyWindowHandle, cx: &mut App) {
    cx.spawn(async move |cx| {
        let mut tick: u64 = 0;
        loop {
            // First beat runs immediately on open — the card must move off
            // its boot-seeded cache within seconds, not after one full
            // TICK sleep. Subsequent beats land on the 60s cadence.
            tick += 1;

            let (mut specs, pending) = cx.update(|cx| {
                state.update(cx, |s, _| {
                    let specs = s.plan_tick(tick);
                    (specs, std::mem::take(&mut s.pending_refresh))
                })
            });
            // Proactive drain (docs/0028 §3.4): planning armed the flag —
            // refresh through the gate now; on success re-plan in this
            // same tick so the bootstrap it arms runs immediately (the
            // old top-of-next-tick drain hop is gone).
            if pending {
                if let Some(GateOutcome::Refreshed) = drain_refresh(&state, cx).await {
                    specs = cx.update(|cx| state.update(cx, |s, _| s.plan_tick(tick)));
                }
            }
            log::trace!(
                "tick {tick}: {}",
                if specs.is_empty() {
                    "gated — nothing planned".to_string()
                } else {
                    specs
                        .iter()
                        .map(|s| s.kind.kind_name())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            );
            for spec in specs {
                let result = run_spec_tracked(state.clone(), spec, cx).await;
                cx.update(|cx| {
                    state.update(cx, |s, cx| {
                        s.apply(result);
                        cx.notify();
                    })
                });
            }
            let _ = window.update(cx, |_, window, _| window.refresh());
            cx.background_executor().timer(TICK).await;
        }
    })
    .detach();
}

/// Device-flow connect (§6.2): request code → show + auto-open browser →
/// poll honoring interval/deadline → install token. Terminal phases
/// (Denied/Expired/Failed) stay visible for the UI's Retry affordance.
pub fn start_connect(state: Entity<GithubState>, window: AnyWindowHandle, cx: &mut App) {
    state.update(cx, |s, cx| {
        s.auth = AuthStatus::ConnectFlow(ConnectPhase::RequestingCode);
        cx.notify();
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

        // Step 2 — show the code, open the verification page once. The
        // user code itself is never logged (docs/0008 D7).
        log::info!(
            "device flow started (approval pending, poll every {}s, expires in {}s)",
            start.interval_secs,
            start.expires_in_secs
        );
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
                PollOutcome::Token(cred) => {
                    log::info!("token acquired — connected");
                    let saved = cx.update(|cx| {
                        state.update(cx, |s, cx| {
                            let saved = s.install_credential(cred);
                            cx.notify();
                            saved
                        })
                    });
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
        state.update(cx, |s, cx| {
            if let ConnectPhase::Failed(why) = &phase {
                log::warn!("connect flow failed: {why}");
            }
            s.auth = AuthStatus::ConnectFlow(phase);
            cx.notify();
        });
    });
    let _ = window.update(cx, |_, window, _| window.refresh());
}

/// Sign-out orchestration (Q5): state wipe + refresh.
pub fn sign_out_now(state: Entity<GithubState>, window: AnyWindowHandle, cx: &mut App) {
    log::info!("signed out — token and caches cleared");
    state.update(cx, |s, cx| {
        s.sign_out();
        cx.notify();
    });
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
    use crate::http::mock::{MockHttp, Recorded};
    use gpui::AppContext as _;
    use std::collections::HashMap;

    fn make_state(http: Arc<MockHttp>) -> GithubState {
        let shared: SharedToken = Arc::new(std::sync::RwLock::new(Some("t0k".into())));
        let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
        let client: Arc<dyn Http> = http;
        GithubState::new(
            client,
            Arc::new(MockTokenStore::with("t0k".into())),
            shared,
            refresh,
            GithubCache::at_tmp("state"),
        )
    }

    fn make_state_at(http: Arc<MockHttp>, cache: GithubCache) -> GithubState {
        let shared: SharedToken = Arc::new(std::sync::RwLock::new(Some("t0k".into())));
        let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
        let client: Arc<dyn Http> = http;
        GithubState::new(
            client,
            Arc::new(MockTokenStore::with("t0k".into())),
            shared,
            refresh,
            cache,
        )
    }

    #[test]
    fn pr_row_mutations_post_their_documents() {
        // docs/0019 D5: each timeline-row mutation posts its own
        // document (identified by operation name) with the page-built
        // variables, on the same transport as every query.
        let cases: Vec<(Mutation, &str, serde_json::Value)> = vec![
            (
                Mutation::UpdateComment,
                "updateIssueComment",
                serde_json::json!({ "input": { "id": "IC_1", "body": "edited" } }),
            ),
            (
                Mutation::DeleteComment,
                "deleteIssueComment",
                serde_json::json!({ "input": { "id": "IC_1" } }),
            ),
            (
                Mutation::UpdateReview,
                "updatePullRequestReview",
                serde_json::json!({
                    "input": { "pullRequestReviewId": "PRR_1", "body": "revised" }
                }),
            ),
            (
                Mutation::DeleteReview,
                "deletePullRequestReview",
                serde_json::json!({ "input": { "pullRequestReviewId": "PRR_1" } }),
            ),
        ];
        for (mutation, op, variables) in cases {
            let http = Arc::new(MockHttp::ok(200, r#"{"data":{}}"#));
            let spec = FetchSpec {
                kind: Kind::PrMutation(mutation),
                url: gql::GRAPHQL_ENDPOINT.into(),
                etag: None,
                ident: Some("o/r#7 probe".into()),
                max_pages: 1,
                variables: Some(variables.clone()),
            };
            let result = run_pr_mutation(http.as_ref(), &spec, mutation);
            assert_eq!(result.status, 200, "{op}");
            let recorded = http.recorded();
            assert_eq!(recorded.len(), 1, "{op}");
            let sent: serde_json::Value =
                serde_json::from_str(recorded[0].body.as_deref().expect("POST body")).unwrap();
            assert!(
                sent["query"].as_str().expect("query").contains(op),
                "{op} missing from its document"
            );
            assert_eq!(sent["variables"], variables, "{op} variables");
        }
    }

    #[test]
    fn issue_mutations_post_their_documents() {
        // docs/0020 D2: the issue page's two write documents ride the
        // same transport — each posts its own operation with the
        // page-built input (close always carries the panel's reason).
        let cases: Vec<(Mutation, &str, serde_json::Value)> = vec![
            (
                Mutation::IssueClose,
                "closeIssue",
                serde_json::json!({ "input": { "issueId": "I_1", "stateReason": "NOT_PLANNED" } }),
            ),
            (
                Mutation::IssueReopen,
                "reopenIssue",
                serde_json::json!({ "input": { "issueId": "I_1" } }),
            ),
            (
                // docs/0022 D1: the metadata writes — one document
                // (updateIssue), one field-group per post.
                Mutation::IssueSetTitle,
                "updateIssue",
                serde_json::json!({ "input": { "id": "I_1", "title": "New" } }),
            ),
            (
                Mutation::IssueSetLabels,
                "updateIssue",
                serde_json::json!({ "input": { "id": "I_1", "labelIds": ["L_1"] } }),
            ),
            (
                Mutation::IssueSetAssignees,
                "updateIssue",
                serde_json::json!({
                    "input": { "id": "I_1", "assigneeIds": ["U_1", "U_2"] }
                }),
            ),
            (
                Mutation::IssueSetMilestone,
                "updateIssue",
                serde_json::json!({ "input": { "id": "I_1", "milestoneId": null } }),
            ),
        ];
        for (mutation, op, variables) in cases {
            let http = Arc::new(MockHttp::ok(200, r#"{"data":{}}"#));
            let spec = FetchSpec {
                kind: Kind::PrMutation(mutation),
                url: gql::GRAPHQL_ENDPOINT.into(),
                etag: None,
                ident: Some("o/r#487 probe".into()),
                max_pages: 1,
                variables: Some(variables.clone()),
            };
            let result = run_pr_mutation(http.as_ref(), &spec, mutation);
            assert_eq!(result.status, 200, "{op}");
            let recorded = http.recorded();
            assert_eq!(recorded.len(), 1, "{op}");
            let sent: serde_json::Value =
                serde_json::from_str(recorded[0].body.as_deref().expect("POST body")).unwrap();
            assert!(
                sent["query"].as_str().expect("query").contains(op),
                "{op} missing from its document"
            );
            assert_eq!(sent["variables"], variables, "{op} variables");
        }
    }

    /// A refresh-capable slot pair, preloaded (docs/0011 tests).
    fn slots_with_refresh() -> (SharedToken, SharedToken) {
        (
            Arc::new(std::sync::RwLock::new(Some("t0k".into()))),
            Arc::new(std::sync::RwLock::new(Some("ghr_r0".into()))),
        )
    }

    fn repo(full_name: &str) -> Repo {
        repo_at(full_name, None)
    }

    fn repo_at(full_name: &str, pushed_at: Option<&str>) -> Repo {
        Repo {
            key: full_name.into(),
            database_id: Some(1),
            name: full_name.split('/').nth(1).unwrap_or(full_name).into(),
            full_name: full_name.into(),
            node_id: Some("R_1".into()),
            has_issues_enabled: true,
            owner: gql::Owner { login: "o".into() },
            description: None,
            private: false,
            pushed_at: pushed_at.map(str::to_string),
            prs: gql::PrSummary {
                total_count: 0,
                nodes: vec![],
            },
            issues: gql::IssueSummary {
                total_count: 0,
                nodes: vec![],
            },
        }
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

    fn gql_ok(body: &str) -> Result<HttpResult, HttpError> {
        Ok(HttpResult {
            status: 200,
            etag: None,
            rate: None,
            body: Some(body.into()),
        })
    }

    fn one_notification() -> &'static str {
        r#"[{"id":"1","unread":true,"subject":{"title":"t","type":"Issue","url":"u"},"repository":{"full_name":"o/r"}}]"#
    }

    // --- GraphQL page/response builders -------------------------------

    fn gql_page(has_next: bool, end_cursor: &str, repos_json: &str) -> String {
        format!(
            r#"{{"data":{{"rateLimit":{{"cost":2,"remaining":4950,"resetAt":"2030-01-01T00:00:00Z"}},
                "viewer":{{"login":"noosxe","avatarUrl":"https://a/me.png",
                "organizations":{{"nodes":[{{"login":"acme","avatarUrl":"https://a/o.png"}}]}},
                "repositories":{{"totalCount":9,
                "pageInfo":{{"hasNextPage":{has_next},"endCursor":"{end_cursor}"}},
                "nodes":[{repos_json}]}}}}}},"errors":[]}}"#
        )
    }

    fn repo_node(full_name: &str, pr_total: i64, prs_json: &str) -> String {
        repo_node_ts(full_name, "2026-09-01T00:00:00Z", pr_total, prs_json, "")
    }

    fn repo_node_ts(
        full_name: &str,
        pushed_at: &str,
        pr_total: i64,
        prs_json: &str,
        issues_json: &str,
    ) -> String {
        format!(
            r#"{{"databaseId":7,"name":"{}","nameWithOwner":"{full_name}",
                "description":null,"isPrivate":false,"pushedAt":"{pushed_at}",
                "owner":{{"login":"o"}},
                "pullRequests":{{"totalCount":{pr_total},"nodes":[{prs_json}]}},
                "issues":{{"totalCount":0,"nodes":[{issues_json}]}}}}"#,
            full_name.split('/').nth(1).unwrap_or(full_name)
        )
    }

    fn pr_node_at(number: i64, updated_at: &str) -> String {
        format!(
            r#"{{"databaseId":{number},"number":{number},"title":"t","isDraft":false,
                "state":"OPEN","author":{{"login":"x","avatarUrl":"a"}},
                "updatedAt":"{updated_at}","url":"https://github.com/o/r/pull/{number}"}}"#
        )
    }

    fn issue_node_at(number: i64, updated_at: &str) -> String {
        format!(
            r#"{{"databaseId":{number},"number":{number},"title":"i",
                "author":{{"login":"x","avatarUrl":"a"}},
                "updatedAt":"{updated_at}","url":"https://github.com/o/r/issues/{number}"}}"#
        )
    }
    fn pr_node(number: i64) -> String {
        format!(
            r#"{{"databaseId":{number},"number":{number},"title":"t","isDraft":false,
                "state":"OPEN","author":{{"login":"x","avatarUrl":"a"}},
                "updatedAt":"u","url":"https://github.com/o/r/pull/{number}"}}"#
        )
    }

    fn snapshot_result(body: &str, cursor: Option<&str>) -> SpecResult {
        SpecResult {
            kind: Kind::Snapshot,
            ident: None,
            status: 200,
            etag: None,
            rate: None,
            body: Some(body.into()),
            network: None,
            cursor: cursor.map(str::to_string),
        }
    }

    // --- Tick planning (docs/0007 D6) ---------------------------------

    #[test]
    fn parse_patch_classifies_every_line_kind() {
        let patch = "@@ -1,3 +1,4 @@\n fn one() {}\n+fn two() {}\n-fn zero() {}\n@@ -10,2 +11,2 @@\n context with no leading space";
        let rows = parse_patch(patch);
        assert_eq!(
            rows,
            vec![
                PatchLine::Hunk("@@ -1,3 +1,4 @@".into()),
                PatchLine::Context("fn one() {}".into()),
                PatchLine::Add("fn two() {}".into()),
                PatchLine::Del("fn zero() {}".into()),
                PatchLine::Hunk("@@ -10,2 +11,2 @@".into()),
                // A context line without the conventional leading
                // space survives verbatim (tolerant, not lossy).
                PatchLine::Context("context with no leading space".into()),
            ]
        );
    }

    #[test]
    fn parse_patch_empty_and_blank_lines() {
        assert!(parse_patch("").is_empty());
        // A blank context line is a Context row, not a dropped one —
        // the row count must match what GitHub served.
        assert_eq!(
            parse_patch("@@ -1,1 +1,2 @@\n \n+added"),
            vec![
                PatchLine::Hunk("@@ -1,1 +1,2 @@".into()),
                PatchLine::Context(String::new()),
                PatchLine::Add("added".into()),
            ]
        );
    }

    #[test]
    fn disconnected_and_rate_limited_are_gated() {
        // No token in the keychain — genuinely disconnected.
        let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
        let client: Arc<dyn Http> = Arc::new(MockHttp::new(vec![]));
        let mut state = GithubState::new(
            client,
            Arc::new(MockTokenStore::empty()),
            shared,
            Arc::new(std::sync::RwLock::new(None)),
            GithubCache::at_tmp("disconnected"),
        );
        assert!(state.plan_tick(1).is_empty());

        state.auth = AuthStatus::Connected;
        state.rate_limited_until = Some(u64::MAX);
        assert!(state.plan_tick(3).is_empty());

        // After the window passes, the tick resumes.
        state.rate_limited_until = Some(0);
        assert!(!state.plan_tick(1).is_empty());
    }
    #[test]
    fn plan_tick_cadence_table() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.auth = AuthStatus::Connected;

        // Bootstrap: snapshot + notifications, then the flag is consumed.
        state.needs_bootstrap = true;
        let specs = state.plan_tick(1);
        assert_eq!(
            specs.iter().map(|s| s.kind).collect::<Vec<_>>(),
            vec![Kind::Snapshot, Kind::Notifications]
        );
        assert!(!state.needs_bootstrap);

        // Ticks 2–4: notifications only.
        for tick in 2..=4 {
            assert_eq!(
                state
                    .plan_tick(tick)
                    .iter()
                    .map(|s| s.kind)
                    .collect::<Vec<_>>(),
                vec![Kind::Notifications]
            );
        }

        // 5th tick adds the snapshot.
        assert_eq!(
            state
                .plan_tick(5)
                .iter()
                .map(|s| s.kind)
                .collect::<Vec<_>>(),
            vec![Kind::Notifications, Kind::Snapshot]
        );

        // No per-tick repo-scoped fetches anymore.
        state.selected = Some("o/r".into());
        let specs = state.plan_tick(6);
        assert!(specs.iter().all(|s| s.kind == Kind::Notifications));
    }

    #[test]
    fn keychain_launch_fires_snapshot_on_first_tick() {
        // Regression (owner report): a launch with an existing keychain
        // token must not wait for tick 5 — old REST-era cache keys are
        // ignored by snapshot-v2, so nothing else populates the UI.
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        assert_eq!(state.auth, AuthStatus::Connected);
        assert!(state.needs_bootstrap);
        let specs = state.plan_tick(1);
        assert!(specs.iter().any(|s| s.kind == Kind::Snapshot));
    }

    #[test]
    fn snapshot_spec_carries_continuation_cursor() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.repo_cursor = Some("c9".into());
        let spec = state.snapshot_spec();
        assert_eq!(spec.variables.unwrap()["cursor"], "c9");

        state.repo_cursor = None;
        let spec = state.snapshot_spec();
        assert!(spec.variables.unwrap()["cursor"].is_null());
    }

    // --- Snapshot run + apply ------------------------------------------

    #[test]
    fn run_snapshot_follows_cursor_and_stitches() {
        let http = Arc::new(MockHttp::new(vec![
            gql_ok(&gql_page(true, "c2", &repo_node("o/a", 1, &pr_node(1)))),
            gql_ok(&gql_page(false, "unused", &repo_node("o/b", 0, ""))),
        ]));
        let state = make_state(http.clone());
        let spec = state.snapshot_spec();

        let result = run_spec(&*http, &spec);
        assert_eq!(result.status, 200);
        assert!(result.cursor.is_none(), "completed run resets cursor");

        // Both requests went to the GraphQL endpoint with the right
        // cursor variables.
        let recorded = http.recorded();
        assert_eq!(recorded.len(), 2);
        assert!(
            recorded[0]
                .body
                .as_ref()
                .unwrap()
                .contains("\"cursor\":null")
        );
        assert!(
            recorded[1]
                .body
                .as_ref()
                .unwrap()
                .contains("\"cursor\":\"c2\"")
        );

        // Stitched body carries both repos in one atomic unit.
        let parsed: Value = serde_json::from_str(result.body.as_deref().unwrap()).unwrap();
        let nodes = parsed["data"]["viewer"]["repositories"]["nodes"]
            .as_array()
            .unwrap();
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn run_snapshot_caps_pages_and_returns_cursor() {
        let page = || gql_ok(&gql_page(true, "cx", &repo_node("o/a", 0, "")));
        let http = Arc::new(MockHttp::new(vec![page(), page(), page(), page()]));
        let state = make_state(http.clone());

        let result = run_spec(&*http, &state.snapshot_spec());
        assert_eq!(http.recorded().len(), gql::SNAPSHOT_MAX_PAGES as usize);
        assert_eq!(result.cursor.as_deref(), Some("cx"));
    }

    #[test]
    fn apply_snapshot_sets_user_orgs_repos_and_prunes() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.auth = AuthStatus::Connected;
        state.repos = vec![repo("o/gone")];

        let body = gql_page(
            false,
            "unused",
            &format!(
                "{},{}",
                repo_node("o/a", 1, &pr_node(1)),
                repo_node("o/b", 0, "")
            ),
        );
        state.apply(snapshot_result(&body, None));

        // User + orgs (personal first, Q10).
        assert_eq!(state.user.as_ref().unwrap().login, "noosxe");
        assert_eq!(state.orgs[0].login, "noosxe");
        assert_eq!(state.orgs[1].login, "acme");

        // Completed run prunes repos absent from the list (docs/0007 D4).
        let names: Vec<&str> = state.repos.iter().map(|r| r.full_name.as_str()).collect();
        assert_eq!(names, vec!["o/a", "o/b"]);

        // PR data lands with composite keys (D3).
        let a = state.repos.iter().find(|r| r.full_name == "o/a").unwrap();
        assert_eq!(a.prs.total_count, 1);
        assert_eq!(a.prs.nodes[0].key, "o/a#1");

        // Persisted: the cache round-trips through snapshot-v2.
        let loaded: gql::SnapshotCache = state
            .cache
            .load(gql::SNAPSHOT_CACHE_KEY)
            .expect("snapshot persisted");
        assert_eq!(loaded.schema, 2);
        assert_eq!(loaded.repos.len(), 2);
        assert_eq!(loaded.user.as_ref().unwrap().login, "noosxe");
    }

    #[test]
    fn apply_snapshot_capped_run_keeps_earlier_repos() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.repos = vec![repo("o/a"), repo("o/b")];

        let body = gql_page(true, "c2", &repo_node("o/c", 0, ""));
        state.apply(snapshot_result(&body, Some("c2")));

        // Continuation batch overlays; earlier pages survive.
        let mut names: Vec<&str> = state.repos.iter().map(|r| r.full_name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["o/a", "o/b", "o/c"]);
        assert_eq!(state.repo_cursor.as_deref(), Some("c2"));
    }

    #[test]
    fn apply_snapshot_partial_errors_still_applies_data() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.repos = vec![repo("o/old")];

        let body = format!(
            r#"{{"data":{{"viewer":{{"login":"noosxe","avatarUrl":"a",
                "organizations":{{"nodes":[]}},
                "repositories":{{"totalCount":1,"pageInfo":{{"hasNextPage":false}},
                "nodes":[{}]}}}}}},"errors":[{{"message":"Some field failed"}}]}}"#,
            repo_node("o/a", 0, "")
        );
        state.apply(snapshot_result(&body, None));
        assert_eq!(state.repos.len(), 1);
        assert_eq!(state.repos[0].full_name, "o/a");
    }

    #[test]
    fn apply_snapshot_data_null_keeps_stale() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.repos = vec![repo("o/a")];
        state.apply(snapshot_result(
            r#"{"data":null,"errors":[{"message":"boom"}]}"#,
            None,
        ));
        assert_eq!(state.repos.len(), 1);
    }

    #[test]
    fn apply_snapshot_rate_limit_zero_gates() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.auth = AuthStatus::Connected;
        let body = r#"{"data":{"rateLimit":{"cost":2,"remaining":0,"resetAt":"2030-01-01T00:00:00Z"},
            "viewer":{"login":"noosxe","avatarUrl":"a","organizations":{"nodes":[]},
            "repositories":{"totalCount":0,"nodes":[]}}}}"#;
        state.apply(snapshot_result(body, None));
        assert_eq!(state.rate_limited_until, Some(1_893_456_000));
        assert!(state.plan_tick(5).is_empty());
    }

    // --- Repo-scoped snapshot -------------------------------------------

    #[test]
    fn repo_open_specs_shape() {
        let state = make_state(Arc::new(MockHttp::new(vec![])));
        let specs = state.repo_open_specs("o/r");
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].kind, Kind::RepoSnapshot);
        assert_eq!(specs[0].variables.as_ref().unwrap()["owner"], "o");
        assert_eq!(specs[0].variables.as_ref().unwrap()["name"], "r");
        assert_eq!(specs[1].kind, Kind::RepoNotifications);
        assert!(specs[1].url.contains("/repos/o/r/notifications"));
    }

    #[test]
    fn apply_repo_snapshot_merges_single_repo() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.repos = vec![repo("o/a"), repo("o/r")];

        let result = SpecResult {
            kind: Kind::RepoSnapshot,
            ident: Some("o/r".into()),
            status: 200,
            etag: None,
            rate: None,
            body: Some(format!(
                r#"{{"data":{{"repository":{}}}}}"#,
                repo_node("o/r", 3, &pr_node(7))
            )),
            network: None,
            cursor: None,
        };
        state.apply(result);

        assert_eq!(state.repos.len(), 2, "merge — no prune, no dupes");
        let r = state.repos.iter().find(|r| r.full_name == "o/r").unwrap();
        assert_eq!(r.prs.total_count, 3);
        assert_eq!(r.prs.nodes[0].key, "o/r#7");
    }

    #[test]
    fn repo_list_orders_by_composite_last_activity() {
        // Issue #35: per repo, the newest of pushed_at / PR / issue
        // activity decides the position — not the connection's
        // PUSHED_AT ordering.
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        let body = gql_page(
            false,
            "x",
            [
                // Pushed in March, nothing else — mid.
                repo_node_ts("o/pushed", "2026-03-01T00:00:00Z", 0, "", ""),
                // Pushed in January, PR updated in May — outranks March.
                repo_node_ts(
                    "o/pr-fresh",
                    "2026-01-01T00:00:00Z",
                    1,
                    &pr_node_at(1, "2026-05-01T00:00:00Z"),
                    "",
                ),
                // Pushed in January, issue updated in June — top.
                repo_node_ts(
                    "o/issue-fresh",
                    "2026-01-01T00:00:00Z",
                    0,
                    "",
                    &issue_node_at(1, "2026-06-01T00:00:00Z"),
                ),
                // January only — bottom.
                repo_node_ts("o/old", "2026-01-01T00:00:00Z", 0, "", ""),
            ]
            .join(",")
            .as_str(),
        );
        state.apply(snapshot_result(&body, None));

        let names: Vec<&str> = state.repos.iter().map(|r| r.full_name.as_str()).collect();
        assert_eq!(names, ["o/issue-fresh", "o/pr-fresh", "o/pushed", "o/old"]);
    }

    #[test]
    fn repo_snapshot_does_not_move_row_when_activity_unchanged() {
        // Issue #35 regression: selecting a repo fired a repo-scoped
        // snapshot whose merge prepended the repo — it jumped to the top
        // even with no new activity. Only a genuinely newer stamp may
        // move it.
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.repos = vec![
            repo_at("o/a", Some("2026-05-01T00:00:00Z")),
            repo_at("o/r", Some("2026-01-01T00:00:00Z")),
        ];

        // Same activity as before: row must stay in place.
        let unchanged = SpecResult {
            kind: Kind::RepoSnapshot,
            ident: Some("o/r".into()),
            status: 200,
            etag: None,
            rate: None,
            body: Some(format!(
                r#"{{"data":{{"repository":{}}}}}"#,
                repo_node_ts("o/r", "2026-01-01T00:00:00Z", 2, "", "")
            )),
            network: None,
            cursor: None,
        };
        state.apply(unchanged);
        assert_eq!(
            state
                .repos
                .iter()
                .map(|r| r.full_name.clone())
                .collect::<Vec<_>>(),
            ["o/a", "o/r"],
            "same activity — no reorder"
        );

        // A genuinely fresher PR moves the repo up.
        let fresher = SpecResult {
            kind: Kind::RepoSnapshot,
            ident: Some("o/r".into()),
            status: 200,
            etag: None,
            rate: None,
            body: Some(format!(
                r#"{{"data":{{"repository":{}}}}}"#,
                repo_node_ts(
                    "o/r",
                    "2026-01-01T00:00:00Z",
                    1,
                    &pr_node_at(9, "2026-09-01T00:00:00Z"),
                    "",
                )
            )),
            network: None,
            cursor: None,
        };
        state.apply(fresher);
        assert_eq!(
            state
                .repos
                .iter()
                .map(|r| r.full_name.clone())
                .collect::<Vec<_>>(),
            ["o/r", "o/a"],
            "newer activity — moves up"
        );
    }

    #[test]
    fn apply_repo_snapshot_null_repository_keeps_stale() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.repos = vec![repo("o/r")];
        let result = SpecResult {
            kind: Kind::RepoSnapshot,
            ident: Some("o/r".into()),
            status: 200,
            etag: None,
            rate: None,
            body: Some(r#"{"data":{"repository":null}}"#.into()),
            network: None,
            cursor: None,
        };
        state.apply(result);
        assert_eq!(state.repos.len(), 1);
        assert_eq!(state.repos[0].prs.total_count, 0);
    }

    // --- Notifications (REST) + derivation ------------------------------

    #[test]
    fn reaction_lock_mutations_post_their_documents() {
        // docs/0023 D3/D5: four op identities over three documents —
        // add/remove share the `subjectId`+`content` input shape, lock
        // and unlock carry only the `lockableId` (v1 omits lockReason).
        let cases: Vec<(Mutation, &str, serde_json::Value)> = vec![
            (
                Mutation::AddReaction,
                "addReaction",
                serde_json::json!({ "input": { "subjectId": "I_1", "content": "HEART" } }),
            ),
            (
                Mutation::RemoveReaction,
                "removeReaction",
                serde_json::json!({ "input": { "subjectId": "C_7", "content": "EYES" } }),
            ),
            (
                Mutation::LockIssue,
                "lockLockable",
                serde_json::json!({ "input": { "lockableId": "I_1" } }),
            ),
            (
                Mutation::UnlockIssue,
                "unlockLockable",
                serde_json::json!({ "input": { "lockableId": "I_1" } }),
            ),
        ];
        for (mutation, op, variables) in cases {
            let http = Arc::new(MockHttp::ok(200, r#"{"data":{}}"#));
            let spec = FetchSpec {
                kind: Kind::PrMutation(mutation),
                url: gql::GRAPHQL_ENDPOINT.into(),
                etag: None,
                ident: Some("o/r#487 probe".into()),
                max_pages: 1,
                variables: Some(variables.clone()),
            };
            let result = run_pr_mutation(http.as_ref(), &spec, mutation);
            assert_eq!(result.status, 200, "{op}");
            let recorded = http.recorded();
            assert_eq!(recorded.len(), 1, "{op}");
            let sent: serde_json::Value =
                serde_json::from_str(recorded[0].body.as_deref().expect("POST body")).unwrap();
            assert!(
                sent["query"].as_str().expect("query").contains(op),
                "{op} missing from its document"
            );
            assert_eq!(sent["variables"], variables, "{op} variables");
        }
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
    fn apply_notifications_derives_selected_repo_group() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.selected = Some("o/r".into());
        let both = r#"[
            {"id":"1","unread":true,"subject":{"title":"t","type":"Issue","url":"u"},
             "repository":{"full_name":"o/r"}},
            {"id":"2","unread":true,"subject":{"title":"t2","type":"Issue","url":"u"},
             "repository":{"full_name":"o/other"}}]"#;
        state.apply(SpecResult {
            kind: Kind::Notifications,
            ident: None,
            status: 200,
            etag: None,
            rate: None,
            body: Some(both.into()),
            network: None,
            cursor: None,
        });
        // Global list holds both; the selected repo's GroupBox derives its
        // slice (docs/0007 D6).
        assert_eq!(state.notifications.len(), 2);
        let derived = state.repo_notifications.get("o/r").unwrap();
        assert_eq!(derived.len(), 1);
        assert_eq!(derived[0].id, "1");
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

    /// #84: etags.json survives the restart but in-memory state does
    /// not — the 304 path must repaint from the last-known payload
    /// (docs/0005 Q6). Simulated with two states sharing one cache root.
    #[test]
    fn apply_304_restores_notifications_from_last_known() {
        let cache = GithubCache::at_tmp("s84-notifs");
        let http = Arc::new(MockHttp::new(vec![ok(one_notification(), "\"etag-1\"")]));
        let mut first = make_state_at(http.clone(), cache.clone());
        first.auth = AuthStatus::Connected;
        first.selected = Some("o/r".into());
        let spec = first.spec(Kind::Notifications, api::notifications_url(1), Some(2));
        first.apply(run_spec(&*http, &spec));
        assert_eq!(first.notifications.len(), 1);

        let mut second = make_state_at(Arc::new(MockHttp::new(vec![])), cache);
        second.selected = Some("o/r".into());
        second.apply(SpecResult {
            kind: Kind::Notifications,
            ident: None,
            status: 304,
            etag: Some("\"etag-1\"".into()),
            rate: None,
            body: None,
            network: None,
            cursor: None,
        });
        assert_eq!(second.notifications.len(), 1);
        assert_eq!(second.notifications[0].id, "1");
        let derived = second.repo_notifications.get("o/r").unwrap();
        assert_eq!(derived.len(), 1);
    }

    #[test]
    fn apply_304_restores_repo_notifications_from_last_known() {
        let cache = GithubCache::at_tmp("s84-repo-notifs");
        let http = Arc::new(MockHttp::new(vec![ok(one_notification(), "\"etag-r\"")]));
        let mut first = make_state_at(http.clone(), cache.clone());
        first.auth = AuthStatus::Connected;
        let spec = first.spec_ident(
            Kind::RepoNotifications,
            Some("o/r"),
            api::repo_notifications_url("o", "r", 1),
            Some(2),
        );
        first.apply(run_spec(&*http, &spec));
        assert_eq!(first.repo_notifications.get("o/r").unwrap().len(), 1);

        let mut second = make_state_at(Arc::new(MockHttp::new(vec![])), cache);
        second.apply(SpecResult {
            kind: Kind::RepoNotifications,
            ident: Some("o/r".into()),
            status: 304,
            etag: Some("\"etag-r\"".into()),
            rate: None,
            body: None,
            network: None,
            cursor: None,
        });
        assert_eq!(second.repo_notifications.get("o/r").unwrap().len(), 1);
    }

    /// No last-known payload on disk (crash between payload and etag
    /// writes, or wiped cache): the 304 degrades to the status quo —
    /// warm state stays, cold state stays empty, never a panic.
    #[test]
    fn apply_304_without_last_known_keeps_state_empty() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.apply(SpecResult {
            kind: Kind::Notifications,
            ident: None,
            status: 304,
            etag: Some("\"etag-x\"".into()),
            rate: None,
            body: None,
            network: None,
            cursor: None,
        });
        assert!(state.notifications.is_empty());
        assert!(state.repo_notifications.is_empty());
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

        let spec = state.spec(Kind::Notifications, api::notifications_url(1), None);
        state.apply(run_spec(&*http, &spec));
        assert_eq!(state.rate_limited_until, Some(9_999_999_999));
        assert!(state.plan_tick(1).is_empty());
    }

    // --- Card + persistence ----------------------------------------------

    #[test]
    fn card_counts_sum_exact_totals() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        let mut big = repo("o/big");
        big.prs.total_count = 250; // >100 open PRs — exact now (docs/0007 §1)
        big.issues.total_count = 300; // >100 open issues — exact too (doc 0012 D2)
        let mut small = repo("o/small");
        small.prs.total_count = 4;
        small.issues.total_count = 7;
        state.repos = vec![big, small];
        state.notifications =
            serde_json::from_str::<Vec<Notification>>(one_notification()).unwrap();
        assert_eq!(state.card_counts(), (1, 254, 307));
        // Clamp parity with the PR sum: a negative SUM can't surface.
        state.repos[1].issues.total_count = -400;
        assert_eq!(state.card_counts().2, 0);
    }

    #[test]
    fn snapshot_v2_seeds_a_new_state() {
        let cache = GithubCache::at_tmp("seed");
        let mut state = make_state_at(Arc::new(MockHttp::new(vec![])), cache.clone());
        state.user = Some(User {
            login: "noosxe".into(),
            avatar_url: "a".into(),
        });
        state.apply_orgs(vec![Org {
            login: "acme".into(),
            avatar_url: "oa".into(),
            description: None,
        }]);
        let mut r = repo("o/a");
        r.prs.total_count = 2;
        state.repos = vec![r];
        state.persist_snapshot();

        // A fresh state over the same cache root seeds instantly (Q6).
        let seeded = make_state_at(Arc::new(MockHttp::new(vec![])), cache);
        assert_eq!(seeded.user.as_ref().unwrap().login, "noosxe");
        assert_eq!(seeded.orgs.len(), 2);
        assert_eq!(seeded.repos.len(), 1);
        assert_eq!(seeded.repos[0].prs.total_count, 2);
    }

    // --- Misc ------------------------------------------------------------

    #[test]
    fn slug_is_filename_safe() {
        assert_eq!(slug("owner/repo.name-x"), "owner__repo.name-x");
        assert_eq!(slug("weird/name~!"), "weird__name__");
    }

    #[test]
    fn age_buckets() {
        assert_eq!(crate::page::relative_age(None), "");
        let now = chrono::Utc::now();
        assert_eq!(crate::page::relative_age(Some(&now.to_rfc3339())), "now");
        let hours_ago = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        assert_eq!(crate::page::relative_age(Some(&hours_ago)), "2h");
    }

    #[test]
    fn full_name_splits_on_first_slash() {
        assert_eq!(split_full_name("o/r"), ("o".to_string(), "r".to_string()));
    }

    #[test]
    fn sign_out_clears_everything() {
        let shared: SharedToken = Arc::new(std::sync::RwLock::new(Some("t0k".into())));
        let refresh: SharedToken = Arc::new(std::sync::RwLock::new(Some("ghr_r".into())));
        let client: Arc<dyn Http> = Arc::new(MockHttp::new(vec![]));
        let mut state = GithubState::new(
            client,
            Arc::new(MockTokenStore::with("t0k".into())),
            shared.clone(),
            refresh.clone(),
            GithubCache::at_tmp("signout"),
        );
        state.auth = AuthStatus::Connected;
        state.repos = vec![repo("o/r")];
        state.repo_cursor = Some("c1".into());
        state.access_expires_at = Some(1_799_999_999);
        state.pending_refresh = true;

        state.sign_out();
        assert_eq!(state.auth, AuthStatus::Disconnected);
        assert!(state.repos.is_empty());
        assert!(state.repo_cursor.is_none());
        assert!(shared.read().unwrap().is_none());
        // docs/0011: the refresh slot and scheduling fields go too.
        assert!(refresh.read().unwrap().is_none());
        assert_eq!(state.access_expires_at, None);
        assert!(!state.pending_refresh);
        // docs/0028: the gate spends the credential until the next connect.
        assert!(state.gate.exhausted());
    }

    // --- Token refresh (docs/0011 §6) ---------------------------------

    fn expiring_cred() -> crate::auth::StoredCredential {
        crate::auth::StoredCredential {
            access_token: "t0k".into(),
            refresh_token: Some("ghr_r0".into()),
            access_expires_at: Some(now_epoch() + 28_800),
        }
    }

    fn make_refresh_state(
        store: Arc<MockTokenStore>,
        slots: (SharedToken, SharedToken),
        tag: &str,
    ) -> GithubState {
        let client: Arc<dyn Http> = Arc::new(MockHttp::new(vec![]));
        GithubState::new(client, store, slots.0, slots.1, GithubCache::at_tmp(tag))
    }

    fn result_401() -> SpecResult {
        SpecResult {
            kind: Kind::Notifications,
            ident: None,
            status: 401,
            etag: None,
            rate: None,
            body: Some(r#"{"message":"Bad credentials"}"#.into()),
            network: None,
            cursor: None,
        }
    }

    #[test]
    fn proactive_refresh_arms_inside_lead_window_and_gates() {
        let mut state = make_refresh_state(
            Arc::new(MockTokenStore::with_credential(expiring_cred())),
            slots_with_refresh(),
            "refresh-lead",
        );
        // 8 h out (≫ 600 s lead): fetches plan normally, nothing armed.
        assert!(!state.plan_tick(1).is_empty());
        assert!(!state.pending_refresh);

        // Inside the lead window: armed + fetches gated.
        state.access_expires_at = Some(now_epoch() + 300);
        assert!(state.plan_tick(2).is_empty());
        assert!(state.pending_refresh);

        // Past expiry arms too (wake-from-suspend / stale boot).
        state.pending_refresh = false;
        state.access_expires_at = Some(now_epoch() - 10);
        assert!(state.plan_tick(3).is_empty());
        assert!(state.pending_refresh);
    }

    #[test]
    fn legacy_token_never_arms_proactive_refresh() {
        // No refresh token in the slot — even bogus expiry metadata
        // must not arm a refresh (docs/0011 §7, legacy row).
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.access_expires_at = Some(now_epoch() - 10);
        assert!(!state.plan_tick(1).is_empty());
        assert!(!state.pending_refresh);
    }

    #[test]
    fn apply_401_rejects_even_when_a_refresh_would_exist() {
        // docs/0028 §3.4: `apply` never refreshes — the gate owns that,
        // upstream of `apply`. A 401 that still reaches `apply` (gate
        // spent / legacy credential) means reconnect, refresh token or
        // not.
        let mut state = make_refresh_state(
            Arc::new(MockTokenStore::with_credential(expiring_cred())),
            slots_with_refresh(),
            "refresh-401",
        );
        state.apply(result_401());
        assert_eq!(state.auth, AuthStatus::TokenRejected);
        assert!(!state.pending_refresh);
        assert!(state.plan_tick(1).is_empty());
    }

    #[test]
    fn apply_refresh_success_installs_rotated_credential() {
        let store = Arc::new(MockTokenStore::with_credential(expiring_cred()));
        let (token_slot, refresh_slot) = slots_with_refresh();
        let mut state = make_refresh_state(
            store.clone(),
            (token_slot.clone(), refresh_slot.clone()),
            "refresh-ok",
        );
        state.pending_refresh = true;
        assert!(!state.gate.exhausted());
        state.needs_bootstrap = false;

        state.apply_refresh(RefreshOutcome::Refreshed(crate::auth::StoredCredential {
            access_token: "t1k".into(),
            refresh_token: Some("ghr_r1".into()),
            access_expires_at: Some(1_799_999_999),
        }));

        assert_eq!(state.auth, AuthStatus::Connected);
        assert!(!state.pending_refresh);
        // The gate's generation moved and the chance is fresh again
        // (docs/0028 §3.4).
        assert!(!state.gate.exhausted());
        assert!(state.needs_bootstrap);
        assert_eq!(state.access_expires_at, Some(1_799_999_999));
        assert_eq!(token_slot.read().unwrap().as_deref(), Some("t1k"));
        assert_eq!(refresh_slot.read().unwrap().as_deref(), Some("ghr_r1"));
        let saved = store.slot.lock().unwrap().clone().unwrap();
        assert_eq!(saved.access_token, "t1k");
        assert_eq!(saved.refresh_token.as_deref(), Some("ghr_r1"));
    }

    #[test]
    fn apply_refresh_reauth_ends_session_and_spends_the_gate() {
        let store = Arc::new(MockTokenStore::with_credential(expiring_cred()));
        let mut state = make_refresh_state(store.clone(), slots_with_refresh(), "refresh-reauth");
        let outcome = state.apply_refresh(RefreshOutcome::Reauth("invalid_grant".into()));
        assert!(matches!(outcome, GateOutcome::Surface));
        assert_eq!(state.auth, AuthStatus::TokenRejected);
        assert!(!state.pending_refresh);
        assert!(state.gate.exhausted());
        // A later 401 stays in the reconnect state; the gate would also
        // decline any further attempt for this credential.
        state.apply(result_401());
        assert_eq!(state.auth, AuthStatus::TokenRejected);
        assert!(state.gate.refresh_now(state.has_refresh_token()).is_none());
        // The stored pair is only removed by sign-out — reconnect offers
        // the device flow (docs/0011 §6.5).
        assert!(store.slot.lock().unwrap().is_some());
    }

    #[test]
    fn apply_refresh_transient_surfaces_network_and_retries_later() {
        let mut state = make_refresh_state(
            Arc::new(MockTokenStore::with_credential(expiring_cred())),
            slots_with_refresh(),
            "refresh-transient",
        );
        let outcome = state.apply_refresh(RefreshOutcome::Transient("dns".into()));
        // Queued runs settle as network errors — never TokenRejected
        // (docs/0028 Q4, docs/0011 Q5).
        assert!(matches!(outcome, GateOutcome::SurfaceNetwork(m) if m == "dns"));
        assert_eq!(state.auth, AuthStatus::Connected);
        assert!(state.pending_refresh); // re-arms the refresh next tick
        assert!(!state.gate.exhausted()); // the chance is not burned
        assert!(state.plan_tick(1).is_empty()); // still gated while pending
    }

    #[test]
    fn refresh_keychain_failure_is_terminal_and_publishes_nothing() {
        // Persist-before-publish (docs/0011 §4.4/§6.9/6.10): a failed
        // keychain save must leave slots and state untouched.
        let (token_slot, refresh_slot) = slots_with_refresh();
        let mut state = make_refresh_state(
            Arc::new(MockTokenStore::failing_save()),
            (token_slot.clone(), refresh_slot.clone()),
            "refresh-keychain",
        );
        state.auth = AuthStatus::Connected;
        state.pending_refresh = true;

        state.apply_refresh(RefreshOutcome::Refreshed(crate::auth::StoredCredential {
            access_token: "t1k".into(),
            refresh_token: Some("ghr_r1".into()),
            access_expires_at: Some(1_799_999_999),
        }));

        assert_eq!(
            state.auth,
            AuthStatus::KeychainError("keychain unavailable (scripted)".into())
        );
        assert!(state.gate.exhausted()); // docs/0028: the chance is spent
        assert_eq!(token_slot.read().unwrap().as_deref(), Some("t0k"));
        assert_eq!(refresh_slot.read().unwrap().as_deref(), Some("ghr_r0"));
        assert_eq!(state.access_expires_at, None);
    }

    #[test]
    fn thread_detail_is_a_single_get_with_body_passthrough() {
        // docs/0026 D2: one REST GET of the thread, no pagination (one
        // object, not a list) — the 200 body flows through untouched for
        // the page to parse.
        let thread = r#"{"id":"19","unread":false,
            "subject":{"title":"PR #1","type":"PullRequest",
                "url":"https://api.github.com/repos/o/r/pulls/1"},
            "repository":{"full_name":"o/r"},
            "reason":"review_requested"}"#;
        let http = Arc::new(MockHttp::ok(200, thread));
        let state = make_state(http.clone());
        let spec = state.spec(Kind::ThreadDetail, crate::api::thread_url("19"), None);
        let result = run_spec(&*http, &spec);
        assert_eq!(result.status, 200);
        assert_eq!(result.body.as_deref(), Some(thread));
        let recorded = http.recorded();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].verb, Verb::Get);
        assert_eq!(recorded[0].url, crate::api::thread_url("19"));
    }

    #[test]
    fn thread_mutations_ride_the_rest_lane_verbs() {
        // docs/0026 D3: the verb is the op — MarkRead is PATCH (205
        // success), MarkDone is DELETE (204). No body, success = 2xx.
        let cases = [
            (ThreadOp::MarkRead, Verb::Patch, 205u16),
            (ThreadOp::MarkDone, Verb::Delete, 204),
        ];
        for (op, verb, status) in cases {
            let http = Arc::new(MockHttp::ok(status, ""));
            let state = make_state(http.clone());
            let spec = state.spec(Kind::ThreadMutation(op), crate::api::thread_url("19"), None);
            let result = run_spec(&*http, &spec);
            assert_eq!(result.status, status);
            assert_eq!(result.body, None, "mutations carry no body");
            let recorded = http.recorded();
            assert_eq!(recorded.len(), 1);
            assert_eq!(recorded[0].verb, verb, "{op:?} verb");
            assert_eq!(recorded[0].body, None, "{op:?} sends no body");
        }
    }

    #[test]
    fn mark_refetch_specs_cover_global_and_selected_repo() {
        // docs/0026 D3: after a settled mark the global unread list plus
        // the selected repo's slice refetch — never the repo snapshot.
        let http = Arc::new(MockHttp::new(vec![]));
        let mut state = make_state(http);
        state.selected = Some("o/r".into());
        let specs = state.mark_refetch_specs();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].kind, Kind::Notifications);
        assert_eq!(specs[1].kind, Kind::RepoNotifications);
        assert_eq!(specs[1].ident.as_deref(), Some("o/r"));
        // No selection: the global list only.
        let http = Arc::new(MockHttp::new(vec![]));
        let mut state = make_state(http);
        state.selected = None;
        assert_eq!(state.mark_refetch_specs().len(), 1);
    }

    // --- Auth gate (docs/0028 §5) -----------------------------------------

    fn gate() -> AuthGate {
        AuthGate::new()
    }

    /// Await a oneshot receiver synchronously (the gate never leaves a
    /// receiver unresolved — every complete/reset/guard path sends).
    fn settle(rx: oneshot::Receiver<GateOutcome>) -> GateOutcome {
        futures::executor::block_on(rx).unwrap_or(GateOutcome::Surface)
    }

    #[test]
    fn gate_single_flights_concurrent_401s() {
        // docs/0028 §5.1: three concurrent 401s produce exactly one
        // refresh — one Lead, two queued waiters.
        let gate = gate();
        assert!(matches!(gate.on_401(0, false, true), GateEntry::Lead(_)));
        assert!(matches!(gate.on_401(0, false, true), GateEntry::Wait(_)));
        assert!(matches!(gate.on_401(0, false, true), GateEntry::Wait(_)));
    }

    #[test]
    fn gate_completes_every_queued_run_and_reopens() {
        // docs/0028 §5.2: the lead's own oneshot is queue item #1; after
        // the drain the gate is reusable (no leak, inflight cleared).
        let gate = gate();
        let GateEntry::Lead(lead_rx) = gate.on_401(0, false, true) else {
            panic!("first 401 must lead");
        };
        let GateEntry::Wait(w1) = gate.on_401(0, false, true) else {
            panic!("second 401 must queue");
        };
        let GateEntry::Wait(w2) = gate.on_401(0, false, true) else {
            panic!("third 401 must queue");
        };
        assert_eq!(gate.complete(GateOutcome::Refreshed), 3);
        assert!(matches!(settle(lead_rx), GateOutcome::Refreshed));
        assert!(matches!(settle(w1), GateOutcome::Refreshed));
        assert!(matches!(settle(w2), GateOutcome::Refreshed));
        // All settled, nothing in flight — the next 401 leads again.
        assert!(matches!(
            gate.on_401(gate.generation(), false, true),
            GateEntry::Lead(_)
        ));
    }

    #[test]
    fn gate_retries_fresh_after_rotation_without_spending() {
        // docs/0028 §5.3: a 401 from a pre-rotation dispatch re-dispatches
        // (RetryFresh) instead of spending the new credential's chance;
        // a post-rotation 401 leads normally; a second round surfaces.
        let gate = gate();
        let used_gen = gate.generation();
        let GateEntry::Lead(_rx) = gate.on_401(used_gen, false, true) else {
            panic!("first 401 must lead");
        };
        gate.complete(GateOutcome::Surface); // the lead finishes (simulated)
        gate.installed(); // rotation lands (gen + 1, chance reset)
        assert!(matches!(
            gate.on_401(used_gen, false, true),
            GateEntry::RetryFresh
        ));
        let new_gen = gate.generation();
        assert!(matches!(
            gate.on_401(new_gen, false, true),
            GateEntry::Lead(_)
        ));
        // The run already consumed its one refresh round (docs/0028 Q2).
        assert!(matches!(
            gate.on_401(new_gen, true, true),
            GateEntry::Surface
        ));
    }

    #[test]
    fn gate_surfaces_when_spent_or_legacy() {
        // docs/0028 §5.7: a legacy credential (no refresh token) and a
        // spent credential both surface immediately; the proactive entry
        // declines the same way.
        let gate = gate();
        assert!(matches!(
            gate.on_401(gate.generation(), false, false),
            GateEntry::Surface
        ));
        gate.spend();
        assert!(matches!(
            gate.on_401(gate.generation(), false, true),
            GateEntry::Surface
        ));
        assert!(gate.refresh_now(true).is_none());
    }

    #[test]
    fn gate_reset_surfaces_waiters_and_blocks_until_install() {
        // docs/0028 §5.8: sign-out drains the queue with Surface and the
        // gate stays spent until an install (reconnect) reopens it.
        let gate = gate();
        let gen_before_reset = gate.generation();
        let GateEntry::Lead(_lead) = gate.on_401(gen_before_reset, false, true) else {
            panic!("first 401 must lead");
        };
        let GateEntry::Wait(waiter) = gate.on_401(gen_before_reset, false, true) else {
            panic!("second 401 must queue");
        };
        gate.reset();
        gate.complete(GateOutcome::Surface); // the abandoned lead finishes
        assert!(matches!(settle(waiter), GateOutcome::Surface));
        assert!(gate.exhausted());
        // A run dispatched before the reset sees only the rotation.
        assert!(matches!(
            gate.on_401(gen_before_reset, false, true),
            GateEntry::RetryFresh
        ));
        // Post-reset dispatches surface; an install reopens the gate.
        assert!(matches!(
            gate.on_401(gate.generation(), false, true),
            GateEntry::Surface
        ));
        gate.installed();
        assert!(!gate.exhausted());
        assert!(matches!(
            gate.on_401(gate.generation(), false, true),
            GateEntry::Lead(_)
        ));
    }

    #[test]
    fn gate_lead_cancelled_drains_the_queue() {
        // docs/0028 §5.12: a lead dropped before completing (task
        // teardown) settles the waiters instead of hanging them.
        let gate = std::sync::Arc::new(AuthGate::new());
        let GateEntry::Lead(_rx) = gate.on_401(0, false, true) else {
            panic!("first 401 must lead");
        };
        let GateEntry::Wait(waiter) = gate.on_401(0, false, true) else {
            panic!("second 401 must queue");
        };
        let guard = LeadGuard {
            gate: std::sync::Arc::clone(&gate),
            armed: true,
        };
        drop(guard); // the lead task was torn down before completing
        assert!(matches!(settle(waiter), GateOutcome::Surface));
        // inflight cleared — the gate is usable again.
        assert!(matches!(
            gate.on_401(gate.generation(), false, true),
            GateEntry::Lead(_)
        ));
    }

    // --- Wrapper integration (docs/0028 §5, async) -------------------------

    /// The rotated-pair refresh grant response (docs/0011 §4.2 wire shape).
    fn refresh_ok_body() -> String {
        r#"{"access_token":"t1k","refresh_token":"ghr_r1","expires_in":28800}"#.into()
    }

    fn api_result(status: u16, body: &str) -> Result<HttpResult, HttpError> {
        Ok(HttpResult {
            status,
            etag: None,
            rate: None,
            body: Some(body.into()),
        })
    }

    fn notifications_spec() -> FetchSpec {
        FetchSpec {
            kind: Kind::Notifications,
            url: api::notifications_url(1),
            etag: None,
            ident: None,
            max_pages: 1,
            variables: None,
        }
    }

    fn gate_state_entity(
        cx: &mut gpui::TestAppContext,
        http: Arc<dyn Http>,
        tag: &str,
    ) -> Entity<GithubState> {
        let (token, refresh) = slots_with_refresh();
        cx.new(|_| {
            GithubState::new(
                http.clone(),
                Arc::new(MockTokenStore::with_credential(expiring_cred())),
                token,
                refresh,
                GithubCache::at_tmp(tag),
            )
        })
    }

    fn run_task(
        cx: &mut gpui::TestAppContext,
        state: &Entity<GithubState>,
    ) -> gpui::Task<SpecResult> {
        cx.spawn({
            let state = state.clone();
            let spec = notifications_spec();
            move |mut cx: gpui::AsyncApp| async move {
                run_spec_tracked(state, spec, &mut cx).await
            }
        })
    }

    #[gpui::test]
    async fn wrapper_retries_the_401d_request_after_refresh(cx: &mut gpui::TestAppContext) {
        // docs/0028 §5: the reactive flow — the 401'd request itself is
        // retried after the refresh + install, no tick, no bootstrap.
        cx.update(dashboard_core::status::init);
        let http = Arc::new(MockHttp::new(vec![
            api_result(401, r#"{"message":"Bad credentials"}"#),
            api_result(200, &refresh_ok_body()),
            api_result(200, "[]"),
        ]));
        let state = gate_state_entity(cx, http.clone(), "gate-retry");

        let result = run_task(cx, &state).await;

        assert_eq!(result.status, 200);
        let recorded = http.recorded();
        assert_eq!(recorded.len(), 3);
        assert_eq!(recorded[1].url, crate::auth::TOKEN_URL); // the grant
        // The rotated pair was installed on the shared slots.
        let (auth, expiry) = cx.update(|cx| {
            let s = state.read(cx);
            (s.auth.clone(), s.access_expires_at)
        });
        assert_eq!(auth, AuthStatus::Connected);
        assert!(expiry.is_some());
    }

    /// URL-routing transport for concurrent tests: each URL pops from its
    /// own script, so task scheduling cannot cross-consume responses
    /// (MockHttp is strictly FIFO).
    struct RoutedHttp {
        routes: std::sync::Mutex<
            HashMap<String, std::collections::VecDeque<Result<HttpResult, HttpError>>>,
        >,
        requests: std::sync::Mutex<Vec<Recorded>>,
    }

    impl RoutedHttp {
        fn new(routes: Vec<(String, Vec<Result<HttpResult, HttpError>>)>) -> Self {
            Self {
                routes: std::sync::Mutex::new(
                    routes
                        .into_iter()
                        .map(|(url, script)| (url.to_string(), script.into()))
                        .collect(),
                ),
                requests: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl Http for RoutedHttp {
        fn request(
            &self,
            verb: Verb,
            url: &str,
            _body: Option<&str>,
            _etag: Option<&str>,
        ) -> Result<HttpResult, HttpError> {
            self.requests.lock().unwrap().push(Recorded {
                verb,
                url: url.into(),
                body: None,
                etag: None,
            });
            self.routes
                .lock()
                .unwrap()
                .get_mut(url)
                .expect("request to an unscripted url")
                .pop_front()
                .expect("script exhausted")
        }

        fn post_graphql(
            &self,
            _query: &str,
            _variables: &serde_json::Value,
        ) -> Result<HttpResult, HttpError> {
            panic!("graphql is not scripted in gate tests")
        }
    }

    #[gpui::test]
    async fn wrapper_single_flights_two_concurrent_401_runs(cx: &mut gpui::TestAppContext) {
        // docs/0028 §5.1: two concurrent 401 runs → exactly one refresh
        // grant (rotation is single-use), both runs settle on their retry.
        // Per-URL scripts keep the test scheduling-insensitive.
        cx.update(dashboard_core::status::init);
        let http = Arc::new(RoutedHttp::new(vec![
            (
                api::notifications_url(1),
                vec![
                    api_result(401, r#"{"message":"Bad credentials"}"#),
                    api_result(200, "[]"),
                ],
            ),
            (
                api::repo_notifications_url("o", "r", 1),
                vec![
                    api_result(401, r#"{"message":"Bad credentials"}"#),
                    api_result(200, "[]"),
                ],
            ),
            (
                crate::auth::TOKEN_URL.to_string(),
                vec![api_result(200, &refresh_ok_body())],
            ),
        ]));
        let state = gate_state_entity(cx, http.clone(), "gate-single");

        let t1 = cx.spawn({
            let state = state.clone();
            let spec = notifications_spec();
            move |mut cx: gpui::AsyncApp| async move {
                run_spec_tracked(state, spec, &mut cx).await
            }
        });
        let t2 = cx.spawn({
            let state = state.clone();
            let spec = FetchSpec {
                kind: Kind::RepoNotifications,
                url: api::repo_notifications_url("o", "r", 1),
                etag: None,
                ident: Some("o/r".into()),
                max_pages: 1,
                variables: None,
            };
            move |mut cx: gpui::AsyncApp| async move {
                run_spec_tracked(state, spec, &mut cx).await
            }
        });
        let (r1, r2) = futures::join!(t1, t2);

        assert_eq!(r1.status, 200);
        assert_eq!(r2.status, 200);
        let grants = http
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.url == crate::auth::TOKEN_URL)
            .count();
        assert_eq!(grants, 1, "exactly one refresh grant");
    }

    #[gpui::test]
    async fn wrapper_surfaces_the_401_when_refresh_reauths(cx: &mut gpui::TestAppContext) {
        // docs/0028 §5.5: a rejected grant surfaces the original 401 and
        // ends the session (TokenRejected); the gate is spent.
        cx.update(dashboard_core::status::init);
        let http = Arc::new(MockHttp::new(vec![
            api_result(401, r#"{"message":"Bad credentials"}"#),
            api_result(200, r#"{"error":"invalid_grant"}"#),
        ]));
        let state = gate_state_entity(cx, http.clone(), "gate-reauth");

        let result = run_task(cx, &state).await;

        assert_eq!(result.status, 401);
        let (auth, exhausted) = cx.update(|cx| {
            let s = state.read(cx);
            (s.auth.clone(), s.gate.exhausted())
        });
        assert_eq!(auth, AuthStatus::TokenRejected);
        assert!(exhausted);
    }

    #[gpui::test]
    async fn wrapper_transient_refresh_surfaces_network_error(cx: &mut gpui::TestAppContext) {
        // docs/0028 §5.6: a transport-failed grant surfaces a synthesized
        // network error — never TokenRejected — and burns nothing.
        cx.update(dashboard_core::status::init);
        let http = Arc::new(MockHttp::new(vec![
            api_result(401, r#"{"message":"Bad credentials"}"#),
            Err(HttpError("dns lookup failed".into())),
        ]));
        let state = gate_state_entity(cx, http.clone(), "gate-transient");

        let result = run_task(cx, &state).await;

        assert_eq!(result.status, 0);
        assert!(
            result
                .network
                .as_deref()
                .is_some_and(|m| m.contains("token refresh unavailable"))
        );
        let (auth, exhausted) = cx.update(|cx| {
            let s = state.read(cx);
            (s.auth.clone(), s.gate.exhausted())
        });
        assert_eq!(auth, AuthStatus::Connected);
        assert!(!exhausted);
    }
}
