//! Service layer (docs/0005 Q6, Q7, Q9; docs/0007): one state entity
//! written by a 60-second tick loop.
//!
//! Fetching model (docs/0007 D6): one **global GraphQL snapshot** per
//! bootstrap + every 5th tick (viewer + orgs + all repos with PR/issue
//! data and exact counts), global REST notifications every tick, and a
//! repo-scoped snapshot + one-shot repo notifications when a repo view
//! opens. ETag revalidation survives only on the REST notifications path;
//! last-known caches under the 0004 cache root persist the snapshot
//! (`snapshot-v3`) and notifications. 401/403 handling per the [DG]
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

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
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
use dashboard_core::app_db::{SQLiteStore, StoredDocument};
use dashboard_core::freshness::Freshness;
use dashboard_core::status;
use dashboard_core::store;
use dashboard_core::usage_records::{MemorySink, RECORD_CAP, Recorder};
use gpui::App;
use gpui::Entity;

use crate::api::{self, Notification};
use crate::auth::{
    AuthError, PollOutcome, RefreshOutcome, StoredCredential, TokenStore, now_epoch, poll_token,
    refresh, start_device_flow,
};
use crate::gql::{self, Org, Repo, User};
use crate::http::{Http, HttpError, RateLimit, SharedToken, Verb};
use crate::usage::{
    Origin, RecordingHttp, Surface, Trigger, UsageLedger, parse_rate_limit_body, recording_for_auth,
};
use dashboard_core::usage_records::Transport;

/// Tick cadence (docs/0052 D1): a 10 s wake for the non-content duties
/// only — pause clearing, token arming, the free usage probe, the
/// bootstrap arm. No lane fetches because this timer elapsed. The
/// first beat still runs immediately on open (docs/0005 Q7).
pub const TICK: Duration = Duration::from_secs(10);
/// docs/0080 D3: the unfocused-poll floor — while the window is
/// inactive, the notifications lane (the one docs/0080 D1 carve-out
/// to "nothing fetches on a timer") waits at least this long between
/// landings. Worst-case detection ≈ floor + one tick.
pub(crate) const UNFOCUSED_POLL_MIN_AGE: Duration = Duration::from_secs(60);
/// docs/0080 D4: per-landing announce cap — the first three new
/// threads raise individually, the remainder collapse into one
/// `+N more` summary toast so a backlog can never stack banners.
pub(crate) const ANNOUNCE_CAP: usize = 3;
/// docs/0052 D4: the refocus floor — on window activation every
/// visible lane younger than this skips, older fires. Kept from
/// docs/0032 D1: rapid alt-tabbing must not become a cadence.
pub(crate) const FOCUS_MIN_AGE: Duration = Duration::from_secs(15);
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
    pub(crate) fn at_tmp(tag: &str) -> Self {
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

    pub(crate) fn last_known_path(&self, rel: &str) -> PathBuf {
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

    /// Age of a last-known blob — its mtime, which the atomic-rename
    /// write (`store::save_json_atomic`) keeps equal to the last `put`
    /// (docs/0034 D1). `None`: no blob (first run — nothing seeded,
    /// nothing to mark).
    pub fn age(&self, rel: &str) -> Option<Duration> {
        std::fs::metadata(self.last_known_path(rel))
            .and_then(|m| m.modified())
            .ok()?
            .elapsed()
            .ok()
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

/// Wall-clock UTC epoch ms — the persisted stamp form (docs/0072 D8).
pub(crate) fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Wall-clock ms for a landed stamp (docs/0072 D8) — the persisted
/// form of an `Instant`, reconstructable only as an age.
fn epoch_ms_of(at: Instant) -> u64 {
    let age = at.elapsed();
    now_epoch_ms().saturating_sub(age.as_millis() as u64)
}

/// The restore-side inverse: a wall-clock age becomes an `Instant` on
/// this boot's clock. `checked_sub` fails when the age predates the
/// process (only reachable inside the restore window) — clamping to
/// now is the intended reading there ("worth showing fresh").
fn instant_from_epoch(landed_ms: u64) -> Instant {
    let age = Duration::from_millis(now_epoch_ms().saturating_sub(landed_ms));
    Instant::now().checked_sub(age).unwrap_or_else(Instant::now)
}

/// Parse a row's detail layer for restore: a missing stamp (the void
/// case), an over-TTL age, or an unparseable body all restore
/// summary-only — a detail is shown only when it can be honestly
/// stamped.
fn restore_layers<T: serde::de::DeserializeOwned>(
    doc: &StoredDocument,
    detail_young: bool,
    repo: &str,
    number: i64,
) -> (Option<T>, Option<Instant>) {
    let (Some(json), Some(at_ms)) = (&doc.detail_json, doc.detail_at_ms) else {
        return (None, None);
    };
    if !detail_young {
        return (None, None);
    }
    match serde_json::from_str::<T>(json) {
        Ok(detail) => (Some(detail), Some(instant_from_epoch(at_ms))),
        Err(e) => {
            log::warn!("doc-store: {repo}#{number} detail failed to parse, summary-only ({e})");
            (None, None)
        }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// Home-card totals (docs/0051 D2) — totals-only repos page, fired
    /// when the home card is shown.
    CardTotals,
    /// Session inventory (docs/0051 D3) — viewer identity, orgs, repo
    /// metadata; once per app launch (re-armed on (re)connect).
    SessionInventory,
    /// Global unread notifications (REST + ETag — the only GraphQL-less
    /// data left, docs/0007 §1).
    Notifications,
    /// Repo-scoped open-PR page on the repo view's Open PRs tab
    /// (docs/0077 D2) — one collection per tab, view-relevant.
    RepoPulls,
    /// Repo-scoped open-issue page — the Open issues tab's lane
    /// (docs/0077 D2).
    RepoIssues,
    /// Repo-scoped unread notifications — the Notifications tab's lane
    /// (docs/0077 D5; was "fetched once per open", docs/0007 D6).
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
    PrFiles,
    /// One completed job's log text (docs/0039 D2) — the four-call
    /// resolution chain's last hop, fetched lazily when a check row is
    /// expanded. Same page-owned result convention as [`Kind::PrFiles`]:
    /// the page parses the 200 body (plain text — nothing for serde);
    /// only non-200 results are forwarded to [`GithubState::apply`]. The
    /// freshness system deliberately does not engage: a completed job's
    /// log is immutable, so there is nothing to re-fetch.
    JobLog,
    /// non-200 results forward to [`GithubState::apply`].
    /// One PR's review threads (docs/0030 D1) — one-shot GraphQL
    /// (`PR_THREADS_QUERY`), fetched lazily by the PR page the first
    /// time the Files tab opens (and again on refresh once loaded, and
    /// after thread mutations). Same page-owned result convention as
    /// [`Kind::PrDetail`].
    PrThreads,
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
    /// docs/0044 D1/D4 — the PR creator's branch source page, fetched
    /// lazily on creator open (the picker-source pattern). Stateless
    /// GraphQL lane, same page-owned result convention.
    RepoBranches,
    /// docs/0046 D1 — the issue creator's Markdown-template source,
    /// fetched on creator open (the [`Kind::RepoBranches`] mirror).
    /// Stateless GraphQL lane, page-owned result.
    RepoIssueTemplates,
    /// docs/0046 §2 — the PR creator's template body: one GraphQL read
    /// of `Repository.pullRequestTemplates` on creator open (the
    /// [`Kind::RepoIssueTemplates`] mirror; body only, title typed).
    /// Stateless lane, page-owned result.
    RepoPrTemplates,
    /// docs/0046 D5 — `blank_issues_enabled` for the issue creator:
    /// one Contents-API GET of `.github/ISSUE_TEMPLATE/config.yml`, the
    /// [`Kind::JobLog`] transport (plain one-shot GET, body passed
    /// through; the page parses + decodes). Never etag-stamped —
    /// re-fetches every creator open.
    RepoTemplatesConfig,
    /// docs/0047 D2 — the home lane: one GraphQL search page of the
    /// selected account's open PRs (`org:`/`user:` scope in the query,
    /// ident `"scope:login"`). State-stamped like the snapshot lanes:
    /// results land on [`GithubState::home_lanes`] keyed by account, so a
    /// switch shows the cached snapshot immediately and refetches. No
    /// etag — GraphQL has no conditional path; refreshes are full
    /// first-page fetches deduped by the docs/0031 guard. "Load more"
    /// rides the same kind: its spec carries `$after`, and the run arm
    /// copies it into [`SpecResult::cursor`] so `apply` knows to append
    /// rather than replace.
    AccountPullRequests,
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
    /// The usage-calculator's free reconcile probe (docs/0050 D3):
    /// `GET /rate_limit` — costs nothing and does not count against
    /// the primary budget. Armed by the tick lane when the ledger's
    /// snapshot is missing or past its window (never during an active
    /// pause — the probe heals *after* the pause clears); the body's
    /// `resources.core` is folded into the usage book by
    /// [`GithubState::apply`], on top of whatever headers the wrapper
    /// already harvested.
    RateLimit,
    /// docs/0066 D2 — the gated-runs sweep: one REST GET of
    /// `actions/runs?branch=…&event=pull_request&status=action_required`,
    /// client-filtered by head SHA (branch names collide across forks).
    /// Page-owned result convention ([`Kind::PrDetail`]): the page parses
    /// the 200 body; only non-200 results forward to
    /// [`GithubState::apply`].
    GatedRuns,
    /// docs/0066 D4 — one gated run's gate probe: `GET
    /// …/runs/{id}/pending_deployments`. Non-empty ⇒ environment gate
    /// (required reviewers); empty ⇒ fork/external-contributor gate.
    /// Same page-owned convention.
    GateProbe,
    /// docs/0066 D4 — a gated-run write (the REST mutation lane, per
    /// [`Kind::ThreadMutation`]): `POST …/runs/{id}/approve` (fork gate —
    /// no body, expect 201) or `POST …/runs/{id}/pending_deployments`
    /// (environment gate — the JSON body rides [`FetchSpec::variables`],
    /// this lane's kind-specific carrier). Success is any 2xx; the page
    /// treats those as settled and forwards only failures for the shared
    /// auth/rate-limit handling.
    RunGateReview,
}

/// docs/0047 D2 — which account qualifier a home snapshot was fetched
/// with: `user:` scopes to repos owned by the personal account, `org:`
/// to an organization's. Rides the FetchSpec ident (`"scope:login"`) and
/// keys [`GithubState::home_lanes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum HomeScope {
    User,
    Org,
}

impl HomeScope {
    /// The GitHub search qualifier prefix (docs/0047 §2).
    pub(crate) fn qualifier(self) -> &'static str {
        match self {
            HomeScope::User => "user",
            HomeScope::Org => "org",
        }
    }

    /// Parse the ident encoding back (docs/0047 D2) — `None` for a
    /// malformed ident (logged by the caller).
    pub(crate) fn from_ident(ident: &str) -> Option<(HomeScope, &str)> {
        let (scope, login) = ident.split_once(':')?;
        let scope = match scope {
            "user" => HomeScope::User,
            "org" => HomeScope::Org,
            _ => return None,
        };
        Some((scope, login))
    }
}

/// docs/0047 D2 + docs/0073 D2 — one account's landed home lane: what
/// is *not* PR data. `order` is the server's key order (which
/// documents the lane shows, in order); the rows themselves live only
/// in `pulls` and the view derives them (`GithubState::lane_view`,
/// docs/0073 D3) — a stored row copy here is what let a confirmed
/// merge go unnoticed until the next landing (#561).
#[derive(Clone)]
pub(crate) struct HomeLane {
    pub order: Vec<PrKey>,
    /// `issueCount` — the full match count, cap or no cap.
    pub total_count: i64,
    /// `!hasNextPage` from the last landing — also true at the search
    /// cap (the server ends the chain cleanly there, docs/0047 §2).
    pub complete: bool,
    /// The cursor the next "Load more" page resumes from; `None` when
    /// complete or when the last page carried no cursor.
    pub cursor: Option<String>,
}

/// docs/0073 D3 — the Open PRs view's per-frame derivation: server key
/// order ⊗ documents, filtered to OPEN. A transient view
/// materialization, never stored — the fact lives only in the entry.
pub(crate) struct LaneView {
    pub rows: Vec<gql::PullRequest>,
    pub total_count: i64,
    pub complete: bool,
    pub has_cursor: bool,
}

/// The five PR-page mutations (docs/0016 §6.2), each mapping to one
/// GraphQL document — the page builds the input, the transport just
/// swaps the document string (D2: "transport is identical").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mutation {
    Comment,
    Review,
    UpdateDescription,
    Merge,
    // docs/0067 D3: the PR page's branch update — one document
    // (updatePullRequestBranch); MERGE | REBASE ride the input, the
    // variant is the op identity (busy mark, log line) as usual.
    UpdateBranch,
    Close,
    // 0019 D5: timeline-row writes — one variant per document.
    UpdateComment,
    DeleteComment,
    UpdateReview,
    DeleteReview,
    // #212: review-thread comments — edit (note the input's
    // `pullRequestReviewCommentId` id field) and delete.
    UpdateReviewComment,
    DeleteReviewComment,
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
    // docs/0043 D1: the PR page's metadata writes — labels/assignees/
    // milestone share the updatePullRequest document, reviewers their
    // own (requestReviews, `union: true` replace). Same op-identity
    // pattern as the 0022 quartet above.
    PrSetLabels,
    PrSetAssignees,
    PrSetMilestone,
    PrSetReviewers,
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
    // docs/0044 D5: the PR creator's write — one document, minimum
    // input (repositoryId, base/head ref names, title, body, draft).
    CreatePullRequest,
    // 0030 D7: the Files-tab review writes — one variant per document,
    // except ReviewBatch: the batch rides the existing addPullRequestReview
    // document (the same input, plus its `threads` array).
    ReviewThread,
    ReviewThreadReply,
    ReviewBatch,
    ThreadResolve,
    ThreadUnresolve,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ThreadOp {
    MarkRead,
    MarkDone,
}

/// Per-key in-flight guard identity for read dispatches (docs/0031 D1):
/// `Kind` discriminates every read kind; `ident` carries the target
/// (`owner/repo#number` for page kinds, `owner/repo` for repo kinds,
/// `None` for globals) — §3 of the doc verified the pair is unique per
/// read target. Mutation kinds never enter the guard: they bypass it
/// entirely (D1).
pub(crate) type InFlightKey = (Kind, Option<String>);

/// Shared fetch lifecycle for page surfaces (docs/0031 D3/D4): the
/// record carries the last good payload and when it landed;
/// `in_flight` stays orthogonal so "show data while a refetch runs"
/// needs no extra state. `fail` never drops a landed payload — the
/// error becomes `last_error` instead (stale-on-error, 0005's
/// stale-data-over-error principle); only a failure with nothing
/// landed is terminal. `fetched_at` is apply-time and session-local:
// The backoff constants and the policy tables live in
// `dashboard_core::freshness` (docs/0037 D2) — this record delegates.
/// no TTL policy lives here (#193 owns that decision).
pub(crate) struct Fetch<T> {
    status: FetchStatus<T>,
    pub(crate) in_flight: bool,
    pub(crate) last_error: Option<String>,
    /// When the last dispatch was stamped (`begin()`), successful or
    /// not — the backoff clock for a failing lane (docs/0036 D2).
    /// `pub(crate)` like `in_flight`/`last_error`: tests backdate the
    /// clock instead of waiting out real delays.
    pub(crate) attempted_at: Option<Instant>,
    /// Consecutive failed dispatches; zeroed by any `land()` (docs/0036
    /// D2).
    pub(crate) fail_streak: u32,
}

enum FetchStatus<T> {
    Idle,
    // Read only through `Fetch::fetched_at`/`age` — the freshness
    // consumers (#192/#193) land after the record itself (docs/0031 D4),
    // so the lib target sees no reader yet.
    #[allow(dead_code)]
    Ready {
        data: T,
        fetched_at: Instant,
    },
    Failed {
        error: String,
    },
}

impl<T> Default for Fetch<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Fetch<T> {
    pub(crate) const fn new() -> Self {
        Self {
            status: FetchStatus::Idle,
            in_flight: false,
            last_error: None,
            attempted_at: None,
            fail_streak: 0,
        }
    }

    /// A landed record without a fetch — fixtures and header paint.
    #[cfg(test)]
    pub(crate) fn ready(data: T) -> Self {
        let mut f = Self::new();
        f.land(data);
        f
    }

    pub(crate) fn begin(&mut self) {
        self.in_flight = true;
        self.attempted_at = Some(Instant::now());
    }

    pub(crate) fn land(&mut self, data: T) {
        self.in_flight = false;
        self.last_error = None;
        self.fail_streak = 0;
        self.status = FetchStatus::Ready {
            data,
            fetched_at: Instant::now(),
        };
    }

    /// docs/0038 D4: a conditional 304 lands as an unchanged payload —
    /// freshness advances, the failure streak resets, nothing re-parses
    /// or re-renders. An `Idle` record stays `Idle` (spec builders only
    /// send the etag when the lane holds data, so this cannot strand a
    /// cold session).
    pub(crate) fn land_unchanged(&mut self) {
        self.in_flight = false;
        self.last_error = None;
        self.fail_streak = 0;
        if let FetchStatus::Ready { fetched_at, .. } = &mut self.status {
            *fetched_at = Instant::now();
        }
    }

    /// With landed data the record keeps it and surfaces `last_error`;
    /// without, the failure is terminal.
    pub(crate) fn fail(&mut self, error: String) {
        self.in_flight = false;
        self.fail_streak = self.fail_streak.saturating_add(1);
        match self.status {
            FetchStatus::Ready { .. } => self.last_error = Some(error),
            _ => self.status = FetchStatus::Failed { error },
        }
    }

    pub(crate) fn data(&self) -> Option<&T> {
        match &self.status {
            FetchStatus::Ready { data, .. } => Some(data),
            _ => None,
        }
    }

    // Test/mutation-surface convenience (docs/0031 §8).
    #[allow(dead_code)]
    pub(crate) fn data_mut(&mut self) -> Option<&mut T> {
        match &mut self.status {
            FetchStatus::Ready { data, .. } => Some(data),
            _ => None,
        }
    }

    pub(crate) fn is_idle(&self) -> bool {
        matches!(self.status, FetchStatus::Idle)
    }

    pub(crate) fn is_ready(&self) -> bool {
        matches!(self.status, FetchStatus::Ready { .. })
    }

    /// First-load spinner: in flight with nothing landed yet.
    pub(crate) fn is_loading(&self) -> bool {
        self.in_flight && !self.is_ready()
    }

    /// Terminal failure (nothing ever landed), or a failed refresh of
    /// landed data.
    pub(crate) fn load_error(&self) -> Option<&str> {
        match &self.status {
            FetchStatus::Failed { error } => Some(error),
            _ => self.last_error.as_deref(),
        }
    }

    pub(crate) fn fetched_at(&self) -> Option<Instant> {
        match &self.status {
            FetchStatus::Ready { fetched_at, .. } => Some(*fetched_at),
            _ => None,
        }
    }

    pub(crate) fn age(&self) -> Option<Duration> {
        self.fetched_at().map(|t| t.elapsed())
    }

    /// The 0032 refresh policy (D2): a lane wants a re-dispatch iff it
    /// is not in flight and either failed (self-heal — `Failed` and
    /// failed-refreshes-over-data both count via `load_error`) or holds
    /// landed data at least `min_age` old. Untouched (`Idle`) lanes
    /// never qualify, so a background beat cannot fetch a surface the
    /// user has not opened.
    pub(crate) fn needs_refresh(&self, min_age: Duration) -> bool {
        dashboard_core::freshness::standard_needs_refresh(
            self.age(),
            self.is_loading(),
            self.load_error().is_some(),
            min_age,
        )
    }

    /// docs/0036 D2: the second dispatch gate — a lane whose last
    /// attempt failed waits `backoff_delay(fail_streak)` (stale-at-`>=`,
    /// docs/0032's boundary) before re-firing.
    pub(crate) fn backoff_elapsed(&self) -> bool {
        dashboard_core::freshness::standard_backoff_elapsed(self.fail_streak, self.attempted_at)
    }
}

/// docs/0037 D2: the record implements the core freshness contract —
/// the policy is core's standard one, accessed through the same
/// inherent methods the policy tables pin.
impl<T> Freshness for Fetch<T> {
    fn age(&self) -> Option<Duration> {
        Fetch::age(self)
    }

    fn is_loading(&self) -> bool {
        Fetch::is_loading(self)
    }

    fn wants_refresh(&self, min_age: Duration) -> bool {
        self.needs_refresh(min_age) && self.backoff_elapsed()
    }
}

/// Which repo-view tab a lane serves (docs/0077 D1/D4). `Ord` orders the
/// `(repo, lane)` map keys; the order is arbitrary but stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RepoLane {
    Pulls,
    Issues,
    Notifications,
}

impl RepoLane {
    /// The log token for this lane.
    pub(crate) fn slug(self) -> &'static str {
        match self {
            RepoLane::Pulls => "pulls",
            RepoLane::Issues => "issues",
            RepoLane::Notifications => "notifs",
        }
    }
}

/// docs/0037 D6: the list-surface clock — a `rows_landed` lookup
/// wrapped up for the repo page's freshness cluster. `is_loading` is
/// always false (rows land in bulk, no per-page spinner);
/// never-landed means refreshable (the 0035 gate) but renders no
/// label (docs/0033 D2's honest blank).
pub(crate) struct LandedClock(pub(crate) Option<Instant>);

impl Freshness for LandedClock {
    fn age(&self) -> Option<Duration> {
        self.0.map(|t| t.elapsed())
    }

    fn is_loading(&self) -> bool {
        false
    }

    fn wants_refresh(&self, min_age: Duration) -> bool {
        match self.0 {
            None => true,
            Some(t) => t.elapsed() >= min_age,
        }
    }
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
    /// docs/0053 D2: where this dispatch comes from. Required — no
    /// default; the compiler forces every dispatch site to answer.
    pub(crate) origin: Origin,
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
/// `dispatched_at` (docs/0054 D14) is the spec's dispatch instant —
/// stamped where [`run_spec_tracked`] hands the spec to the executor,
/// so the record's queue wait covers real backlog, not our own
/// pre-flight work alone.
pub fn run_spec(
    http: &dyn Http,
    recorder: &Recorder,
    spec: &FetchSpec,
    dispatched_at: Instant,
) -> SpecResult {
    // docs/0053 D4: the per-run recording view — every flight through
    // any runner emits one record. `Kind::RateLimit` is the free probe.
    let rec = RecordingHttp::new(
        http,
        recorder,
        spec.origin,
        spec.kind.kind_name(),
        spec.ident.as_deref(),
        spec.kind == Kind::RateLimit,
        Some(dispatched_at),
    );
    match spec.kind {
        Kind::CardTotals => run_paged_view(&rec, spec, gql::CARD_TOTALS_QUERY),
        Kind::SessionInventory => run_paged_view(&rec, spec, gql::SESSION_INVENTORY_QUERY),
        Kind::RepoPulls => run_graphql_fetch(&rec, spec, gql::REPO_PULLS_QUERY, "repo-pulls"),
        Kind::RepoIssues => run_graphql_fetch(&rec, spec, gql::REPO_ISSUES_QUERY, "repo-issues"),
        Kind::PrDetail => run_pr_detail(&rec, spec),
        Kind::PrMutation(m) => run_pr_mutation(&rec, spec, m),
        Kind::IssueDetail => run_issue_detail(&rec, spec),
        Kind::ThreadDetail => run_thread_detail(&rec, spec),
        Kind::ThreadMutation(op) => run_thread_mutation(&rec, spec, op),
        Kind::RunGateReview => run_gate_review(&rec, spec),
        Kind::RepoLabels => run_graphql_fetch(&rec, spec, gql::REPO_LABELS_QUERY, "repo-labels"),
        Kind::RepoAssignableUsers => run_graphql_fetch(
            http,
            spec,
            gql::REPO_ASSIGNABLE_USERS_QUERY,
            "repo-assignable-users",
        ),
        Kind::RepoMilestones => {
            run_graphql_fetch(&rec, spec, gql::REPO_MILESTONES_QUERY, "repo-milestones")
        }
        Kind::RepoBranches => {
            run_graphql_fetch(&rec, spec, gql::REPO_BRANCHES_QUERY, "repo-branches")
        }
        Kind::PrThreads => run_graphql_fetch(&rec, spec, gql::PR_THREADS_QUERY, "pr-threads"),
        Kind::RepoIssueTemplates => run_graphql_fetch(
            http,
            spec,
            gql::REPO_ISSUE_TEMPLATES_QUERY,
            "repo-issue-templates",
        ),
        Kind::RepoTemplatesConfig => run_plain_get(&rec, spec),
        Kind::RepoPrTemplates => run_graphql_fetch(
            http,
            spec,
            gql::REPO_PR_TEMPLATES_QUERY,
            "repo-pr-templates",
        ),
        Kind::Notifications | Kind::RepoNotifications | Kind::PrFiles => run_rest(&rec, spec),
        Kind::JobLog | Kind::GatedRuns | Kind::GateProbe | Kind::RateLimit => {
            run_plain_get(&rec, spec)
        }
        Kind::AccountPullRequests => run_home_prs(&rec, spec),
    }
}

/// One plain GET — no paging, no JSON expectations (docs/0039 D2): the
/// job-log transport, reused by the issue creator's Contents-API config
/// fetch (docs/0046 D5 — the page parses the JSON body itself). ureq
/// follows the 302 to the 1-minute signed URL
/// (its default redirect policy never forwards the authorization header
/// cross-host), so the response this returns is the final 200 with the
/// log text. A 200 body is returned as-is; the page truncates.
fn run_plain_get(http: &dyn Http, spec: &FetchSpec) -> SpecResult {
    let mut status = 0u16;
    let mut rate = None;
    let mut network = None;
    let mut body = None;
    let sent_at = Instant::now();
    match http.request(Verb::Get, &spec.url, None, None) {
        Err(HttpError(m)) => network = Some(m),
        Ok(result) => {
            status = result.status;
            rate = result.rate;
            body = result.body;
            log::debug!(
                "fetch {}: → {status} in {}ms",
                spec.kind.kind_name(),
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
        body: if network.is_none() && status == 200 {
            body
        } else {
            None
        },
        network,
        cursor: None,
    }
}

/// One gated-run write (docs/0066 D4): `POST …/runs/{id}/approve` (fork
/// gate — no body, expect 201) or `POST …/runs/{id}/pending_deployments`
/// (environment gate — the JSON body rides [`FetchSpec::variables`], the
/// lane's kind-specific carrier). Success is any 2xx; the body is empty
/// upstream and never surfaced.
fn run_gate_review(http: &dyn Http, spec: &FetchSpec) -> SpecResult {
    let mut status = 0u16;
    let mut rate = None;
    let mut network = None;
    let sent_at = Instant::now();
    let body = spec.variables.as_ref().map(Value::to_string);
    match http.request(Verb::Post, &spec.url, body.as_deref(), None) {
        Err(HttpError(m)) => network = Some(m),
        Ok(result) => {
            status = result.status;
            rate = result.rate;
            log::debug!(
                "gate-review {}: → {status} in {}ms",
                spec.kind.kind_name(),
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

/// The paged view queries (docs/0051 D2/D3): up to `VIEW_MAX_PAGES`
/// repos pages per run, cursor-following; the pages stitch into one
/// body so `apply` lands the run as a single atomic unit. Hitting the
/// page cap logs a warning — the landed data is partial (D9).
fn run_paged_view(http: &dyn Http, spec: &FetchSpec, query: &str) -> SpecResult {
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

    for page_no in 1..=gql::VIEW_MAX_PAGES {
        let variables = serde_json::json!({ "cursor": cursor });
        let kind = spec.kind.kind_name();
        log::debug!(
            "fetch {kind}: page {page_no} (cursor: {})",
            cursor.as_deref().unwrap_or("none")
        );
        let sent_at = Instant::now();
        match http.post_graphql(query, &variables) {
            Err(HttpError(m)) => {
                network = Some(m);
                break;
            }
            Ok(result) => {
                status = result.status;
                rate = result.rate;
                log::debug!(
                    "fetch {kind}: page {page_no} → {} in {}ms",
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

    // D9 honesty: a cursor that survived the loop means the page cap
    // cut the run short — the landed data is partial.
    if next_cursor.is_some() {
        log::warn!(
            "{}: page cap hit at {} — data is partial",
            spec.kind.kind_name(),
            gql::VIEW_MAX_PAGES
        );
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
        Mutation::UpdateBranch => gql::PR_UPDATE_BRANCH_MUTATION,
        Mutation::Close => gql::PR_CLOSE_MUTATION,
        Mutation::UpdateComment => gql::PR_UPDATE_COMMENT_MUTATION,
        Mutation::DeleteComment => gql::PR_DELETE_COMMENT_MUTATION,
        Mutation::UpdateReview => gql::PR_UPDATE_REVIEW_MUTATION,
        Mutation::DeleteReview => gql::PR_DELETE_REVIEW_MUTATION,
        Mutation::UpdateReviewComment => gql::PR_UPDATE_REVIEW_COMMENT_MUTATION,
        Mutation::DeleteReviewComment => gql::PR_DELETE_REVIEW_COMMENT_MUTATION,
        Mutation::IssueClose => gql::ISSUE_CLOSE_MUTATION,
        Mutation::IssueReopen => gql::ISSUE_REOPEN_MUTATION,
        // 0022 D1: one document, four op identities — the input's
        // single field-group is what distinguishes the writes.
        Mutation::IssueSetTitle => gql::ISSUE_UPDATE_MUTATION,
        Mutation::IssueSetLabels => gql::ISSUE_UPDATE_MUTATION,
        Mutation::IssueSetAssignees => gql::ISSUE_UPDATE_MUTATION,
        Mutation::IssueSetMilestone => gql::ISSUE_UPDATE_MUTATION,
        // docs/0043 D1: one document for the metadata trio, one for
        // review requests.
        Mutation::PrSetLabels => gql::PR_UPDATE_META_MUTATION,
        Mutation::PrSetAssignees => gql::PR_UPDATE_META_MUTATION,
        Mutation::PrSetMilestone => gql::PR_UPDATE_META_MUTATION,
        Mutation::PrSetReviewers => gql::PR_REQUEST_REVIEWS_MUTATION,
        // 0023 D3/D5: four op identities over two documents — add/remove
        // share the `subjectId`+`content` input, lock/unlock their own.
        Mutation::AddReaction => gql::ADD_REACTION_MUTATION,
        Mutation::RemoveReaction => gql::REMOVE_REACTION_MUTATION,
        Mutation::LockIssue => gql::LOCK_LOCKABLE_MUTATION,
        Mutation::UnlockIssue => gql::UNLOCK_LOCKABLE_MUTATION,
        // 0024 D5: the creator's document.
        Mutation::CreateIssue => gql::CREATE_ISSUE_MUTATION,
        Mutation::CreatePullRequest => gql::CREATE_PR_MUTATION,
        // 0030 D7: the Files-tab review writes. ReviewBatch reuses the
        // addPullRequestReview document — the draft batch is the same
        // input plus its `threads` array (D4).
        Mutation::ReviewThread => gql::PR_ADD_REVIEW_THREAD_MUTATION,
        Mutation::ReviewThreadReply => gql::PR_ADD_REVIEW_COMMENT_MUTATION,
        Mutation::ReviewBatch => gql::PR_ADD_REVIEW_MUTATION,
        Mutation::ThreadResolve => gql::PR_RESOLVE_THREAD_MUTATION,
        Mutation::ThreadUnresolve => gql::PR_UNRESOLVE_THREAD_MUTATION,
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

/// One classified diff row plus its file line numbers on both sides
/// (docs/0030 D2). `old`/`new` are `None` on hunk headers, on any row
/// before a parseable hunk header, and on the side a row does not touch
/// (an Add has no old line, a Del no new line).
/// FNV-1a over the patch text (docs/0040 D5): the diff-version key for
/// refresh gating (docs/0070 D4) and memo invalidation. Relocated from the
/// retired `syntax` module — the hash is model-level, not presentation.
pub(crate) fn hash_patch(patch: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in patch.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[derive(Clone, Debug, PartialEq, Eq)]

pub(crate) struct AnchoredLine {
    pub(crate) line: PatchLine,
    pub(crate) old: Option<u32>,
    pub(crate) new: Option<u32>,
}

/// The `@@ -o,s +n,s @@` header's start lines (docs/0030 D2). `None` on
/// anything that does not parse — the walk then emits unanchored rows
/// until the next valid header (parse_patch's tolerant precedent).
fn hunk_starts(header: &str) -> Option<(u32, u32)> {
    let rest = header.strip_prefix("@@ -")?;
    let plus_at = rest.find(" +")?;
    // Counts are optional in unified-diff headers (`@@ -1 +1 @@` for
    // single-line hunks) — split on both separators, not just the comma.
    let old: u32 = rest[..plus_at].split([',', ' ']).next()?.parse().ok()?;
    let new: u32 = rest[plus_at + 1..]
        .strip_prefix('+')?
        .split([',', ' '])
        .next()?
        .parse()
        .ok()?;
    Some((old, new))
}

/// Walk a REST `patch` and compute each row's file line numbers on both
/// sides (docs/0030 D2 — the anchor surface review threads hang on).
/// Pure: hunk headers seed the counters, Context advances both, Add
/// advances new, Del advances old. `\\ No newline` markers and rows
/// without a valid header anchor at `None` and advance nothing.
pub(crate) fn anchor_patch(patch: &str) -> Vec<AnchoredLine> {
    let (mut old, mut new) = (0u32, 0u32);
    let mut out = Vec::new();
    for raw in patch.lines() {
        if raw.starts_with("@@") {
            match hunk_starts(raw) {
                Some((o, n)) => {
                    old = o;
                    new = n;
                }
                None => {
                    // Unparseable header: no anchor until the next one.
                    (old, new) = (0, 0);
                }
            }
            out.push(AnchoredLine {
                line: PatchLine::Hunk(raw.to_string()),
                old: None,
                new: None,
            });
            continue;
        }
        if raw.starts_with('\\') {
            // `\ No newline at end of file` — renders as a context row,
            // but it is a marker, not a file line: no number, no advance.
            out.push(AnchoredLine {
                line: PatchLine::Context(raw.to_string()),
                old: None,
                new: None,
            });
            continue;
        }
        let line = if let Some(rest) = raw.strip_prefix('+') {
            let n = (new != 0).then_some(new);
            new = new.saturating_add(1);
            out.push(AnchoredLine {
                line: PatchLine::Add(rest.to_string()),
                old: None,
                new: n,
            });
            continue;
        } else if let Some(rest) = raw.strip_prefix('-') {
            let o = (old != 0).then_some(old);
            old = old.saturating_add(1);
            out.push(AnchoredLine {
                line: PatchLine::Del(rest.to_string()),
                old: o,
                new: None,
            });
            continue;
        } else {
            PatchLine::Context(raw.strip_prefix(' ').unwrap_or(raw).to_string())
        };
        let (o, n) = ((old != 0).then_some(old), (new != 0).then_some(new));
        old = old.saturating_add(1);
        new = new.saturating_add(1);
        out.push(AnchoredLine {
            line,
            old: o,
            new: n,
        });
    }
    out
}

impl Kind {
    /// docs/0058 §13: the lanes whose results the service state itself
    /// consumes (inventory, notifications, snapshot, card totals, home
    /// PRs, the free probe) — including when a visible surface
    /// triggered them. The offline toast's transition detector keys on
    /// these; page-owned lanes surface failures inline where the user
    /// is looking and stay log-only.
    pub(crate) fn is_service_lane(&self) -> bool {
        matches!(
            self,
            Kind::CardTotals
                | Kind::SessionInventory
                | Kind::Notifications
                | Kind::RepoPulls
                | Kind::RepoIssues
                | Kind::RepoNotifications
                | Kind::AccountPullRequests
                | Kind::RateLimit
        )
    }
    fn kind_name(&self) -> &'static str {
        match self {
            Kind::CardTotals => "card-totals",
            Kind::SessionInventory => "session-inventory",
            Kind::Notifications => "notifications",
            Kind::RepoPulls => "repo-pulls",
            Kind::RepoIssues => "repo-issues",
            Kind::PrDetail => "pr-detail",
            Kind::PrMutation(_) => "pr-mutation",
            Kind::IssueDetail => "issue-detail",
            Kind::RepoLabels => "repo-labels",
            Kind::RepoAssignableUsers => "repo-assignable-users",
            Kind::RepoMilestones => "repo-milestones",
            Kind::RepoBranches => "repo-branches",
            Kind::RepoIssueTemplates => "repo-issue-templates",
            Kind::RepoPrTemplates => "repo-pr-templates",
            Kind::RepoTemplatesConfig => "repo-templates-config",
            Kind::AccountPullRequests => "home-prs",
            Kind::PrFiles => "pr-files",
            Kind::JobLog => "check-log",
            Kind::PrThreads => "pr-threads",
            Kind::RepoNotifications => "repo-notifs",
            Kind::ThreadDetail => "thread-detail",
            Kind::ThreadMutation(_) => "thread-mutation",
            Kind::GatedRuns => "gated-runs",
            Kind::GateProbe => "gate-probe",
            Kind::RunGateReview => "gate-review",
            Kind::RateLimit => "rate-limit",
        }
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// The card's totals lane payload (docs/0051 D2): summed open PRs and
/// issues from the totals-only query, plus the in-session landing
/// clock. Never persisted — a fresh launch re-fetches (~2 points).
#[derive(Clone, Debug)]
pub struct CardTotals {
    pub open_prs: i64,
    pub open_issues: i64,
    pub landed: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ErrorScope {
    /// The conversation column — above the comment box.
    Timeline,
    /// The merge/close rail panel.
    Panel,
    /// The issue page's state panel (docs/0020 D5) — the PR page never
    /// produces it, the issue page never produces Panel.
    Rail,
    /// The diff pane (docs/0030 D7) — above the diff body; only the
    /// Files-tab review writes produce it.
    Files,
}

/// The last mutation failure — kept until the next attempt or success, so
/// it survives the post-mutation refetch that repaints everything else.
#[derive(Clone, Debug)]
pub(crate) struct MutationError {
    pub(crate) scope: ErrorScope,
    pub(crate) message: String,
}

// ---------------------------------------------------------------------------
// PR document store (docs/0072 D1) — one shared cache keyed `(repo, number)`:
// every list lane lands summaries, the PR page's query lands details, and
// optimistic mutations (D4) stage on the entry they belong to.
// ---------------------------------------------------------------------------

/// docs/0072 D9 — a detail older than this revalidates in the background
/// on reopen (stale-while-revalidate); younger paints as-is.
pub(crate) const PR_DETAIL_TTL: Duration = Duration::from_secs(5 * 60);

/// docs/0072 D4 — how long a pending op may wait for its confirmation
/// before the store unblocks the document. Generous: real merges land in
/// seconds; this catches dead pages and silent transports, not slow ones.
pub(crate) const PR_PENDING_MAX_AGE: Duration = Duration::from_secs(120);

/// Branch-update failure copy (docs/0067 D6): stable-fragment match
/// with the generic fallback carrying the upstream message — never raw
/// prose in the panel, never guessed data in the message. Lives beside
/// the store since docs/0072: the session-scoped resolver maps it.
pub(crate) fn update_branch_error_copy(message: &str) -> String {
    let m = message.to_ascii_lowercase();
    if m.contains("expectedheadoid")
        || (m.contains("head") && m.contains("match"))
        || (m.contains("branch") && m.contains("moved"))
        || (m.contains("branch") && m.contains("changed"))
    {
        "The branch moved while you were viewing it — refreshing.".into()
    } else if m.contains("conflict") {
        "GitHub can't update this branch — the merge/rebase conflicts must be resolved first."
            .into()
    } else if m.contains("permission")
        || m.contains("authorized")
        || m.contains("not allowed")
        || m.contains("protected")
        || m.contains("protection")
        || m.contains("forbidden")
    {
        "GitHub refused: your token can't update this branch (repository permissions or branch protection)."
            .into()
    } else {
        format!("GitHub couldn't update the branch: {message}")
    }
}

/// docs/0072 D9 — detail bodies are the heavy part: details evict
/// oldest-landed past this many documents (owner-set for #556: 1024 —
/// the working set, mirrored by the persisted table's prune). Summaries
/// track the home list and are naturally bounded by it.
pub(crate) const PR_STORE_DETAIL_CAP: usize = 1024;

/// docs/0072 D8 (#556) — the restore staleness bar: a detail whose age
/// at boot is past this is not restored (the row still restores its
/// summary). The in-session gates keep their own TTLs — this only
/// decides whether the disk copy is worth showing at all. Owner-tuned;
/// revisit if it feels wrong.
pub(crate) const RESTORE_DETAIL_TTL: Duration = Duration::from_secs(60 * 60);

/// docs/0072 D1 — identity of a stored PR document. The key *is* the
/// identity the async guides demand: results can only ever land on their
/// own document (D6).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PrKey {
    pub(crate) repo: String,
    pub(crate) number: i64,
}

impl PrKey {
    pub(crate) fn new(repo: impl Into<String>, number: i64) -> Self {
        Self {
            repo: repo.into(),
            number,
        }
    }

    /// `"owner/name#123"` → key; `None` when the row key is malformed.
    pub(crate) fn from_row_key(key: &str) -> Option<Self> {
        let (repo, number) = key.split_once('#')?;
        Some(PrKey::new(repo, number.parse().ok()?))
    }
}

/// docs/0072 D1/D4 — the optimistic writes. Stages 1–2 shipped the
/// header ops; stage 3 (#554) moved the conversation writes onto the
/// same machinery: comment/review submit, description edit, the
/// 0019-D5 timeline-row writes, and the 0030-D7 Files-tab writes.
/// Only merge/close patch their post-state into the document; every
/// other kind's confirmation patches nothing (the new or edited row's
/// payload is only learnable by refetch), so it voids the cached
/// detail layer instead — the refetch lands the row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PendingKind {
    Merge,
    Close,
    UpdateBranch,
    Comment,
    Review,
    Description,
    // 0019 D5: timeline-row writes.
    CommentEdit,
    CommentDelete,
    ReviewEdit,
    ReviewDelete,
    // 0030 D7: Files-tab writes.
    LineComment,
    ReviewReply,
    ReviewBatch,
}

impl PendingKind {
    /// The muted pill / header label while the op flies (D4).
    pub(crate) fn label(self) -> &'static str {
        match self {
            PendingKind::Merge => "merging…",
            PendingKind::Close => "closing…",
            PendingKind::UpdateBranch => "updating branch…",
            PendingKind::Comment => "commenting…",
            PendingKind::Review => "submitting review…",
            PendingKind::Description => "saving description…",
            PendingKind::CommentEdit => "editing comment…",
            PendingKind::CommentDelete => "deleting comment…",
            PendingKind::ReviewEdit => "editing review…",
            PendingKind::ReviewDelete => "deleting review…",
            PendingKind::LineComment => "adding line comment…",
            PendingKind::ReviewReply => "replying…",
            PendingKind::ReviewBatch => "adding to review…",
        }
    }

    /// The completion toast's verb (D7): "Merged #482 · owner/repo".
    pub(crate) fn toast_verb(self) -> &'static str {
        match self {
            PendingKind::Merge => "Merged",
            PendingKind::Close => "Closed",
            PendingKind::UpdateBranch => "Updated branch for",
            PendingKind::Comment => "Comment added",
            PendingKind::Review => "Review submitted",
            PendingKind::Description => "Description updated",
            PendingKind::CommentEdit => "Comment updated",
            PendingKind::CommentDelete => "Comment deleted",
            PendingKind::ReviewEdit => "Review updated",
            PendingKind::ReviewDelete => "Review deleted",
            PendingKind::LineComment => "Line comment added",
            PendingKind::ReviewReply => "Reply added",
            PendingKind::ReviewBatch => "Added to review",
        }
    }

    /// The failure toast's noun (D7): "Merge failed · #482 owner/repo".
    pub(crate) fn toast_noun(self) -> &'static str {
        match self {
            PendingKind::Merge => "Merge",
            PendingKind::Close => "Close",
            PendingKind::UpdateBranch => "Branch update",
            PendingKind::Comment => "Comment",
            PendingKind::Review => "Review",
            PendingKind::Description => "Description update",
            PendingKind::CommentEdit => "Comment edit",
            PendingKind::CommentDelete => "Comment delete",
            PendingKind::ReviewEdit => "Review edit",
            PendingKind::ReviewDelete => "Review delete",
            PendingKind::LineComment => "Line comment",
            PendingKind::ReviewReply => "Reply",
            PendingKind::ReviewBatch => "Review batch",
        }
    }

    /// The toast dedupe key's slug (D7).
    fn toast_slug(self) -> &'static str {
        match self {
            PendingKind::Merge => "merged",
            PendingKind::Close => "closed",
            PendingKind::UpdateBranch => "branch-updated",
            PendingKind::Comment => "commented",
            PendingKind::Review => "review-submitted",
            PendingKind::Description => "description-updated",
            PendingKind::CommentEdit => "comment-edited",
            PendingKind::CommentDelete => "comment-deleted",
            PendingKind::ReviewEdit => "review-edited",
            PendingKind::ReviewDelete => "review-deleted",
            PendingKind::LineComment => "line-commented",
            PendingKind::ReviewReply => "replied",
            PendingKind::ReviewBatch => "review-batched",
        }
    }

    /// Where a failure renders inline — the documented placement each
    /// op already had as a page-modal op (docs/0016 D5, 0019 D5, 0030
    /// D7), kept through the migration.
    pub(crate) fn error_scope(self) -> ErrorScope {
        match self {
            PendingKind::Merge | PendingKind::Close | PendingKind::UpdateBranch => {
                ErrorScope::Panel
            }
            PendingKind::Comment
            | PendingKind::Review
            | PendingKind::Description
            | PendingKind::CommentEdit
            | PendingKind::CommentDelete
            | PendingKind::ReviewEdit
            | PendingKind::ReviewDelete => ErrorScope::Timeline,
            PendingKind::LineComment | PendingKind::ReviewReply | PendingKind::ReviewBatch => {
                ErrorScope::Files
            }
        }
    }
}

/// docs/0072 §6 stage 4 (#555) — the issue page's optimistic writes,
/// the [`PendingKind`] mirror: every [`crate::issue_page::IssueBusyOp`]
/// this store replaced, one variant each. Copy, scopes and toasts are
/// the page-modal ops' own (docs/0020 D5, 0022 D2, 0023 D3/D5), kept
/// through the migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IssuePendingKind {
    Comment,
    Description,
    CommentEdit,
    CommentDelete,
    Close,
    Reopen,
    SetTitle,
    SetLabels,
    SetAssignees,
    SetMilestone,
    ToggleReaction,
    ToggleLock,
}

impl IssuePendingKind {
    /// The muted pill / header label while the op flies (D4).
    pub(crate) fn label(self) -> &'static str {
        match self {
            IssuePendingKind::Comment => "commenting…",
            IssuePendingKind::Description => "saving description…",
            IssuePendingKind::CommentEdit => "editing comment…",
            IssuePendingKind::CommentDelete => "deleting comment…",
            IssuePendingKind::Close => "closing…",
            IssuePendingKind::Reopen => "reopening…",
            IssuePendingKind::SetTitle => "saving title…",
            IssuePendingKind::SetLabels => "saving labels…",
            IssuePendingKind::SetAssignees => "saving assignees…",
            IssuePendingKind::SetMilestone => "saving milestone…",
            IssuePendingKind::ToggleReaction => "updating reaction…",
            IssuePendingKind::ToggleLock => "updating lock…",
        }
    }

    /// The completion toast's verb (D7): "Closed #12 · owner/repo".
    pub(crate) fn toast_verb(self) -> &'static str {
        match self {
            IssuePendingKind::Comment => "Comment added",
            IssuePendingKind::Description => "Description updated",
            IssuePendingKind::CommentEdit => "Comment updated",
            IssuePendingKind::CommentDelete => "Comment deleted",
            IssuePendingKind::Close => "Closed",
            IssuePendingKind::Reopen => "Reopened",
            IssuePendingKind::SetTitle => "Title updated",
            IssuePendingKind::SetLabels => "Labels updated",
            IssuePendingKind::SetAssignees => "Assignees updated",
            IssuePendingKind::SetMilestone => "Milestone updated",
            IssuePendingKind::ToggleReaction => "Reaction updated",
            IssuePendingKind::ToggleLock => "Lock updated",
        }
    }

    /// The failure toast's noun (D7).
    pub(crate) fn toast_noun(self) -> &'static str {
        match self {
            IssuePendingKind::Comment => "Comment",
            IssuePendingKind::Description => "Description update",
            IssuePendingKind::CommentEdit => "Comment edit",
            IssuePendingKind::CommentDelete => "Comment delete",
            IssuePendingKind::Close => "Close",
            IssuePendingKind::Reopen => "Reopen",
            IssuePendingKind::SetTitle => "Title update",
            IssuePendingKind::SetLabels => "Labels update",
            IssuePendingKind::SetAssignees => "Assignees update",
            IssuePendingKind::SetMilestone => "Milestone update",
            IssuePendingKind::ToggleReaction => "Reaction toggle",
            IssuePendingKind::ToggleLock => "Lock toggle",
        }
    }

    /// The toast dedupe key's slug (D7).
    fn toast_slug(self) -> &'static str {
        match self {
            IssuePendingKind::Comment => "commented",
            IssuePendingKind::Description => "description-updated",
            IssuePendingKind::CommentEdit => "comment-edited",
            IssuePendingKind::CommentDelete => "comment-deleted",
            IssuePendingKind::Close => "closed",
            IssuePendingKind::Reopen => "reopened",
            IssuePendingKind::SetTitle => "title-updated",
            IssuePendingKind::SetLabels => "labels-updated",
            IssuePendingKind::SetAssignees => "assignees-updated",
            IssuePendingKind::SetMilestone => "milestone-updated",
            IssuePendingKind::ToggleReaction => "reaction-updated",
            IssuePendingKind::ToggleLock => "lock-updated",
        }
    }

    /// Where a failure renders inline — the issue page's own two
    /// scopes (docs/0020 D5): the conversation column, the rail panel.
    pub(crate) fn error_scope(self) -> ErrorScope {
        match self {
            IssuePendingKind::Comment
            | IssuePendingKind::Description
            | IssuePendingKind::CommentEdit
            | IssuePendingKind::CommentDelete
            | IssuePendingKind::ToggleReaction => ErrorScope::Timeline,
            IssuePendingKind::Close
            | IssuePendingKind::Reopen
            | IssuePendingKind::ToggleLock
            | IssuePendingKind::SetTitle
            | IssuePendingKind::SetLabels
            | IssuePendingKind::SetAssignees
            | IssuePendingKind::SetMilestone => ErrorScope::Rail,
        }
    }
}

/// docs/0072 D1 (stage 4, #555) — one issue document: the summary
/// layer (the repo snapshot's open-issue rows land it) plus the detail
/// layer (the issue page's query lands it), the same freshness
/// machinery as a [`PrEntry`] minus the PR-only bits (no rollup —
/// issues carry no checks signal). Session-only — never persisted (D8).
#[derive(Clone, Debug)]
pub(crate) struct IssueEntry {
    pub(crate) summary: gql::Issue,
    pub(crate) detail: Option<gql::IssueDetail>,
    pub(crate) summary_at: Instant,
    pub(crate) detail_at: Option<Instant>,
    pub(crate) books: DetailBooks,
    pub(crate) pending: Option<PendingOp<IssuePendingKind>>,
    /// The last optimistic-write failure, rendered inline in its scope.
    pub(crate) error: Option<MutationError>,
}

impl IssueEntry {
    /// The same SWR gate as a [`PrEntry`] minus the rollup mark.
    pub(crate) fn detail_fresh(&self) -> bool {
        !self.books.in_flight
            && self
                .detail_at
                .is_some_and(|at| at.elapsed() < PR_DETAIL_TTL)
    }
}

/// docs/0072 D1/D6 — a store-scoped optimistic write in flight. `token`
/// is store-issued; a confirmation applies only on token match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingOp<K> {
    pub(crate) kind: K,
    pub(crate) token: u64,
    pub(crate) started_at: Instant,
}

/// The detail read's books (docs/0072 D2) — the page-local `Fetch`
/// record's failure/flight shape, store-owned so the page stays a reader.
#[derive(Clone, Debug, Default)]
pub(crate) struct DetailBooks {
    pub(crate) in_flight: bool,
    pub(crate) last_error: Option<String>,
    pub(crate) attempted_at: Option<Instant>,
    pub(crate) fail_streak: u32,
}

/// docs/0072 D1 — one PR document: the summary layer (list lanes land
/// it) plus the detail layer (the PR page's query lands it), freshness
/// stamps on both, and the pending state between them. Session-only —
/// never persisted (D8).
#[derive(Clone, Debug)]
pub(crate) struct PrEntry {
    pub(crate) summary: gql::PullRequest,
    pub(crate) detail: Option<gql::PrDetail>,
    pub(crate) summary_at: Instant,
    pub(crate) detail_at: Option<Instant>,
    pub(crate) books: DetailBooks,
    pub(crate) pending: Option<PendingOp<PendingKind>>,
    /// The last optimistic-write failure, rendered inline in its scope
    /// (docs/0016 D5 placement — docs/0072 D4).
    pub(crate) error: Option<MutationError>,
    /// docs/0072 D3 — a summary landing moved the rollup under the
    /// cached detail (the checks signal is a shared fact): the cached
    /// detail is behind the fresher layer until the next read lands.
    /// Set by `land_summary`, cleared by `land_detail`.
    pub(crate) rollup_dirty: bool,
}

impl PrEntry {
    /// docs/0072 D3 — the stale-while-revalidate skip: a young,
    /// already-landed detail need not revalidate. Never-landed and
    /// in-flight lanes always (re)fetch; a failed lane refetches too —
    /// its `detail_at` stayed `None`, so the young-detail rule misses.
    /// A drift mark (the summary layer moved the rollup) also voids the
    /// skip: the two layers must never report different check states.
    pub(crate) fn detail_fresh(&self) -> bool {
        !self.books.in_flight
            && !self.rollup_dirty
            && self
                .detail_at
                .is_some_and(|at| at.elapsed() < PR_DETAIL_TTL)
    }
}

/// docs/0072 D2 — a document's detail-layer freshness view: the
/// `Freshness` contract (docs/0037 D2) over the store's own stamps, so
/// the page's freshness row and refresh gates read the store like any
/// other lane. `None` entry = never-landed (the `LandedClock` rule:
/// refreshable, no age label).
pub(crate) struct DetailClock<'a>(pub(crate) Option<&'a PrEntry>);

/// docs/0072 stage 4 (#555) — the issue document's detail-layer
/// freshness view: the [`DetailClock`] contract over an
/// [`IssueEntry`] (no rollup mark to honor).
pub(crate) struct IssueDetailClock<'a>(pub(crate) Option<&'a IssueEntry>);

impl Freshness for IssueDetailClock<'_> {
    fn age(&self) -> Option<Duration> {
        self.0.and_then(|e| e.detail_at).map(|at| at.elapsed())
    }

    fn is_loading(&self) -> bool {
        self.0.is_some_and(|e| e.books.in_flight)
    }

    fn wants_refresh(&self, min_age: Duration) -> bool {
        let Some(entry) = self.0 else {
            return true;
        };
        dashboard_core::freshness::standard_needs_refresh(
            entry.detail_at.map(|at| at.elapsed()),
            entry.books.in_flight,
            entry.books.last_error.is_some(),
            min_age,
        ) && dashboard_core::freshness::standard_backoff_elapsed(
            entry.books.fail_streak,
            entry.books.attempted_at,
        )
    }
}

impl Freshness for DetailClock<'_> {
    fn age(&self) -> Option<Duration> {
        self.0.and_then(|e| e.detail_at).map(|at| at.elapsed())
    }

    fn is_loading(&self) -> bool {
        self.0.is_some_and(|e| e.books.in_flight)
    }

    fn wants_refresh(&self, min_age: Duration) -> bool {
        let Some(entry) = self.0 else {
            return true;
        };
        dashboard_core::freshness::standard_needs_refresh(
            entry.detail_at.map(|at| at.elapsed()),
            entry.books.in_flight,
            entry.books.last_error.is_some(),
            min_age,
        ) && dashboard_core::freshness::standard_backoff_elapsed(
            entry.books.fail_streak,
            entry.books.attempted_at,
        )
    }
}
pub struct GithubState {
    pub auth: AuthStatus,
    pub user: Option<User>,
    /// Org entries for the dropdown; the personal account is first (Q10).
    pub orgs: Vec<Org>,
    /// Repo inventory (docs/0051 D3) — metadata only; `prs`/`issues`
    /// summaries arrive empty and the lazy repo-scope query fills them
    /// on view open (docs/0007 D6).
    pub repos: Vec<Repo>,
    /// Global unread notifications (card count + repo derivation).
    pub notifications: Vec<Notification>,
    /// Selected repo's unread notifications — fetched once at open, then
    /// derived from the global list (docs/0007 D6).
    pub repo_notifications: BTreeMap<String, Vec<Notification>>,
    /// `owner/repo` of the selected repo (session-only, not persisted).
    pub selected: Option<String>,
    /// The card's totals lane (docs/0051 D2): summed open PRs/issues
    /// from `CardTotals`, `None` until the first landing (skeleton then).
    pub card_totals: Option<CardTotals>,
    /// Rate-limit pause: UTC epoch seconds until requests resume (Q9).
    pub rate_limited_until: Option<u64>,
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
    /// docs/0053 D3/D4: the per-module usage-records writer. Test
    /// states get a private `MemorySink`; the app's state (lib.rs)
    /// shares the app-wide sink from `usage_records::init`.
    pub(crate) recorder: Recorder,
    /// Set when a connect flow should fetch the session inventory and
    pub needs_bootstrap: bool,
    /// docs/0034 D2 — disk-seeded cache ages, read at construction.
    /// A landed sync clears them: from there the session records own
    /// freshness (docs/0031/0032) and the staleness chip goes away.
    pub snapshot_age: Option<Duration>,
    pub notifications_age: Option<Duration>,
    /// Rows landed at, per repo full name (docs/0035 D1): both lanes
    /// that write a repo's rows bump its entry — the repo-scoped
    /// snapshot (its ident) and the global snapshot (every repo it
    /// carries). In-session freshness only — never persisted, never
    /// seeded; a missing entry means "never landed this session".
    pub(crate) rows_landed: BTreeMap<(String, RepoLane), Instant>,
    /// docs/0036 D3: the repo-rows lane has no `Fetch` record (results
    /// land through `apply`), so its backoff lives here — bumped by the
    /// page's `run_specs` landing arm for the repo-lane kinds.
    pub(crate) rows_fail_streak: u32,
    pub(crate) rows_attempted_at: Option<Instant>,
    /// docs/0052 D5: the global notifications lane's clocks — the
    /// landed stamp age-gates the view-shown/refocus triggers' specs,
    /// the 0036 pair backs off a failing lane. The lane has no `Fetch`
    /// record; its books live here beside the repo-rows pair's.
    pub(crate) notifications_landed: Option<Instant>,
    pub(crate) notifications_fail_streak: u32,
    pub(crate) notifications_attempted_at: Option<Instant>,
    /// docs/0047 D2 — the home lane's snapshots, keyed `(login, scope)`:
    /// one entry per account this session, so switching the selector
    /// repaints the cached list instantly (D9 stale-while-revalidate)
    /// while the refetch flies. Session-only — never persisted, never
    /// seeded; docs/0034 disk-seeding stays a snapshot/notifications
    /// affair.
    pub(crate) home_lanes: BTreeMap<(String, HomeScope), HomeLane>,
    /// The home lane's landed clock (the notifications-lane pattern,
    /// docs/0038 D7): the age gate reads it; backoff books sit beside it.
    pub(crate) home_landed: Option<Instant>,
    pub(crate) home_fail_streak: u32,
    pub(crate) home_attempted_at: Option<Instant>,
    /// The home lane's last failed attempt (docs/0047 D3): rendered as
    /// error + Retry when neither snapshot nor union has rows to show;
    /// cleared on the next landing.
    pub(crate) home_error: Option<String>,
    /// docs/0072 D1 — the PR document store: one entry per `(repo,
    /// number)`; every list lane lands summaries, the PR page's query
    /// lands details, optimistic mutations stage on the entry.
    /// Persisted write-through to `app_db` (D8, #556); details evict
    /// past [`PR_STORE_DETAIL_CAP`] (D9); sign-out clears it.
    pub(crate) pulls: BTreeMap<PrKey, PrEntry>,
    /// docs/0072 §6 stage 4 (#555) — issue documents, the same
    /// `(repo, number)` identity in its own map (the payload types
    /// differ; the machinery is the same shape). Persisted like
    /// `pulls` (D8, #556).
    pub(crate) issues: BTreeMap<PrKey, IssueEntry>,
    /// docs/0072 D6 — store-issued pending-op tokens; the identity a
    /// late confirmation must carry to apply.
    pub(crate) next_pending_token: u64,
    /// docs/0072 D8 (#556) — the document store's write-through sink.
    /// `None` (tests, or a degraded/unpublished DB) runs the store
    /// session-scoped, exactly pre-#556; pending ops, books, and
    /// mutation errors stay memory-only regardless (D8).
    pub(crate) db: Option<Arc<SQLiteStore>>,
    /// docs/0058 §13 adoption: alerts staged by the cx-free result
    /// sinks (`apply`, `apply_refresh`), drained and raised on the
    /// main thread by the tick loop / refresh lead. Bounded by
    /// construction — only transition edges stage (the FSM flags
    /// below) and the tick drains every beat.
    pub(crate) pending_alerts: Vec<dashboard_core::toasts::Toast>,
    /// docs/0080 D2: the focus gate for the unfocused-poll arm —
    /// `true` until an activation observer reports deactivation. Not
    /// the deleted docs/0052 `focus_engaged`: it gates a single arm
    /// and prices nothing.
    pub(crate) window_active: bool,
    /// docs/0080 D5: thread ids that have landed **while unread** this
    /// session; the first landing after launch seeds silently (the
    /// `notification_seen_seeded` flag), later landings announce the
    /// unread diff. In-memory on purpose — restart forgets (docs/0080
    /// §8 non-goal).
    pub(crate) notification_seen: HashSet<String>,
    /// docs/0080 D5: `false` until the first notifications landing of
    /// the session; that landing records the seen-set without
    /// announcing (no launch-time banner storm for threads that
    /// arrived while the app was closed).
    pub(crate) notification_seen_seeded: bool,
    /// Transport-health FSM (transition-only, docs/0058 §13): `true`
    /// from the first service-lane network failure until the next
    /// landed response. One offline toast per incident; recovery is
    /// log-only.
    pub(crate) net_down: bool,
    /// Auth-alert latch: `true` from the first terminal-auth
    /// transition of an incident until
    /// [`GithubState::install_credential`].
    pub(crate) auth_alert_open: bool,
    pub(crate) etags: BTreeMap<String, String>,
    pub(crate) client: Arc<dyn Http>,
    /// API-usage ledger (docs/0050 D3): shared with the `CountingHttp`
    /// wrapper installed at `lib.rs` init — the wrapper writes on the
    /// background request path, the chip and the probe lane read. The
    /// constructor's fresh book is swapped for the wrapper's in `init`
    /// before any traffic can flow (the tick loop and views spawn
    /// after the swap).
    pub(crate) usage: UsageLedger,
    pub(crate) store: Arc<dyn TokenStore>,
    pub(crate) shared_token: SharedToken,
    /// Refresh-token slot (docs/0011 §4.3): same handling rules as the
    /// access token — keychain + slot only, never in state or logs.
    pub(crate) shared_refresh: SharedToken,
    pub(crate) cache: GithubCache,
    /// Draft persistence root (docs/0045 D8): one small JSON file per
    /// composer context under the module data root; pages build their
    /// handles from it.
    pub(crate) drafts: crate::drafts::DraftStore,
    /// Per-key in-flight guard for read dispatches (docs/0031 D1/D2):
    /// armed at dispatch, cleared when the result lands. A dispatch
    /// finding its key armed is skipped, never queued.
    pub(crate) in_flight: BTreeSet<InFlightKey>,
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
        drafts: crate::drafts::DraftStore,
    ) -> Self {
        // Tests ride a private capped sink (docs/0053 §5: the default
        // sink is the wide-test storage); the app injects the global.
        Self::with_recorder(
            client,
            store,
            shared_token,
            shared_refresh,
            cache,
            drafts,
            Recorder::new(MODULE_ID, Arc::new(MemorySink::new(RECORD_CAP))),
            None,
        )
    }

    /// `new` with an injected records recorder (docs/0053 D3) — the
    /// app's `lib.rs` passes the global-sink recorder — and the
    /// document store's write-through sink (docs/0072 D8, #556): the
    /// app passes `app_db::global(cx)`; `None` runs session-scoped.
    /// The sink doubles as the restore source: documents seed here,
    /// the one blocking read on this path (the docs/0054 §8 precedent
    /// the notification restore already sets).
    #[allow(clippy::too_many_arguments)]
    pub fn with_recorder(
        client: Arc<dyn Http>,
        store: Arc<dyn TokenStore>,
        shared_token: SharedToken,
        shared_refresh: SharedToken,
        cache: GithubCache,
        drafts: crate::drafts::DraftStore,
        recorder: Recorder,
        db: Option<Arc<SQLiteStore>>,
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
        let mut state = Self {
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
            snapshot_age: seeded.then(|| cache.age(gql::SNAPSHOT_CACHE_KEY)).flatten(),
            notifications_age: seeded.then(|| cache.age("notifications")).flatten(),
            repo_notifications: BTreeMap::new(),
            selected: None,
            card_totals: None,
            rate_limited_until: None,
            access_expires_at,
            pending_refresh: false,
            gate: Arc::new(AuthGate::new()),
            recorder,
            // A keychain-token launch boots straight into the snapshot
            // refresh — the first tick must fire it, not wait for tick 5
            // (docs/0007 §8: cache paint, then the snapshot lands).
            needs_bootstrap: auth.connected(),
            auth,
            etags,
            in_flight: BTreeSet::new(),
            rows_landed: BTreeMap::new(),
            rows_fail_streak: 0,
            rows_attempted_at: None,
            notifications_landed: None,
            notifications_fail_streak: 0,
            notifications_attempted_at: None,
            home_lanes: BTreeMap::new(),
            home_landed: None,
            home_fail_streak: 0,
            home_attempted_at: None,
            home_error: None,
            pulls: BTreeMap::new(),
            issues: BTreeMap::new(),
            next_pending_token: 0,
            db,
            client,
            usage: crate::usage::new_ledger(),
            store,
            shared_token,
            shared_refresh,
            cache,
            drafts,
            pending_alerts: Vec::new(),
            net_down: false,
            auth_alert_open: false,
            window_active: true,
            notification_seen: HashSet::new(),
            notification_seen_seeded: false,
        };
        // docs/0072 D8 (#556): seed the store from the persisted
        // documents. A sign-out-then-connect cycle within one process
        // re-seeds through the same path (the new connect rebuilds the
        // state); a degraded or missing sink seeds nothing.
        state.seed_documents();
        state
    }

    /// Arm the guard for a read dispatch — `false` = an identical read
    /// is already in flight and the dispatch must be skipped.
    pub(crate) fn in_flight_arm(&mut self, key: InFlightKey) -> bool {
        self.in_flight.insert(key)
    }

    pub(crate) fn in_flight_clear(&mut self, key: &InFlightKey) {
        self.in_flight.remove(key);
    }

    /// Rate-limit / auth gate (Q9): no requests while disconnected, mid
    /// connect-flow, token-rejected, or paused until the window resets.
    pub(crate) fn gated(&self) -> bool {
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
    pub fn plan_tick(&mut self, _tick: u64) -> Vec<FetchSpec> {
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
        let mut specs = Vec::new();
        // docs/0050 D3 — the free reconcile probe: GET /rate_limit
        // costs nothing and does not count against the primary
        // budget, so a stale window (or one never sampled this
        // session) heals without waiting for real traffic. Armed
        // only on ungated ticks — an active pause holds the book's
        // last snapshot until the pause clears, then this fires on
        // the same tick that cleared it (the snapshot rolled past
        // `reset_at` with it).
        {
            let mut book = self.usage.lock().unwrap();
            let now = now_epoch();
            if book.probe_due(now) {
                book.mark_probe(now);
                drop(book);
                specs.push(self.spec(
                    Kind::RateLimit,
                    api::rate_limit_url(),
                    None,
                    // docs/0053 D2: the free probe is tick-origin.
                    Origin::new(Surface::ServiceTick, Trigger::Probe),
                ));
            }
        }
        if self.needs_bootstrap {
            self.needs_bootstrap = false;
            specs.push(
                self.session_inventory_spec(Origin::new(Surface::ServiceTick, Trigger::Bootstrap)),
            );
            specs.push(self.spec(
                Kind::Notifications,
                api::notifications_url(1),
                Some(MAX_PAGES),
                Origin::new(Surface::ServiceTick, Trigger::Bootstrap),
            ));
            return specs;
        }
        if self.gated() {
            return specs;
        }
        // docs/0052 D1: the tick plans no content lanes —
        // notifications moved to the view-shown/refocus/manual
        // triggers (docs/0052 D5, `notifications_spec_if_stale`).
        // docs/0080 D1/D3 — the single carve-out: while the window is
        // inactive, the notifications lane may poll at the 60 s floor,
        // through the same gate (pause, backoff, age) as every other
        // trigger. Focused ticks stay content-silent.
        if !self.window_active {
            if let Some(spec) = self.notifications_spec_if_stale(
                UNFOCUSED_POLL_MIN_AGE,
                Origin::new(Surface::ServiceTick, Trigger::Poll),
            ) {
                specs.push(spec);
            }
        }
        specs
    }

    /// docs/0052 D5: the global notifications lane's trigger gate —
    /// the view-shown and refocus call sites ask for the spec; `None`
    /// means skip (paused, backed off per docs/0036, or younger than
    /// the caller's floor). The tick's old cadence arm, extracted.
    pub(crate) fn notifications_spec_if_stale(
        &self,
        min_age: Duration,
        origin: Origin,
    ) -> Option<FetchSpec> {
        if self.gated() {
            return None;
        }
        let backed_off = self.notifications_attempted_at.is_some_and(|at| {
            self.notifications_fail_streak > 0
                && at.elapsed()
                    < dashboard_core::freshness::backoff_delay(self.notifications_fail_streak)
        });
        if backed_off {
            log::debug!("notifications: backed off — the lane waits");
            return None;
        }
        self.notifications_landed
            .is_none_or(|at| at.elapsed() >= min_age)
            .then(|| {
                self.spec(
                    Kind::Notifications,
                    api::notifications_url(1),
                    Some(MAX_PAGES),
                    origin,
                )
            })
    }

    /// The session inventory spec (docs/0051 D3): identity + orgs + repo
    /// metadata, paged to exhaustion by the runner (10-page cap, D9).
    pub(crate) fn session_inventory_spec(&self, origin: Origin) -> FetchSpec {
        FetchSpec {
            origin,
            kind: Kind::SessionInventory,
            url: gql::GRAPHQL_ENDPOINT.to_string(),
            etag: None,
            ident: Some("session".to_string()),
            max_pages: 1,
            variables: Some(serde_json::json!({ "cursor": null })),
        }
    }

    /// The home-card totals spec (docs/0051 D2): fired whenever the
    /// home card is shown; totals-only, paged to exhaustion (D9).
    pub(crate) fn card_totals_spec(&self, origin: Origin) -> FetchSpec {
        FetchSpec {
            origin,
            kind: Kind::CardTotals,
            url: gql::GRAPHQL_ENDPOINT.to_string(),
            etag: None,
            ident: Some("card-totals".to_string()),
            max_pages: 1,
            variables: Some(serde_json::json!({ "cursor": null })),
        }
    }

    /// One repo-view tab's fetch (docs/0077 D3): the lane's single
    /// spec — `RepoPulls`/`RepoIssues` GraphQL for the row tabs, the
    /// ident-keyed `RepoNotifications` REST call for the notifications
    /// tab. A tab switch offers this spec; the freshness gate below
    /// (and the docs/0031 dedup) decide whether it flies.
    pub(crate) fn repo_lane_spec(
        &self,
        full_name: &str,
        lane: RepoLane,
        origin: Origin,
    ) -> FetchSpec {
        let (owner, name) = split_full_name(full_name);
        match lane {
            RepoLane::Pulls | RepoLane::Issues => FetchSpec {
                origin,
                kind: match lane {
                    RepoLane::Pulls => Kind::RepoPulls,
                    _ => Kind::RepoIssues,
                },
                url: gql::GRAPHQL_ENDPOINT.to_string(),
                etag: None,
                ident: Some(full_name.to_string()),
                max_pages: 1,
                variables: Some(serde_json::json!({ "owner": owner, "name": name })),
            },
            RepoLane::Notifications => self.spec_ident(
                Kind::RepoNotifications,
                Some(full_name),
                api::repo_notifications_url(&owner, &name, 1),
                Some(2),
                origin,
            ),
        }
    }

    /// Heartbeat/tab-entry refresh for one repo-view lane (docs/0077
    /// D3, amending docs/0035 D2): the lane's spec when that lane's
    /// landed clock is at least `min_age` old — the same stale-at-`>=`
    /// boundary as `Fetch::needs_refresh` (docs/0032). `None` while
    /// fresh, so a beat or a re-entry stays a no-op; the ident-keyed
    /// dispatch dedups against a just-fired fetch (docs/0031). Backoff
    /// stays per-repo across the three lanes (docs/0077 D4 — failures
    /// are connectivity-shaped).
    pub(crate) fn repo_lane_refresh_specs(
        &self,
        full_name: &str,
        lane: RepoLane,
        min_age: Duration,
        origin: Origin,
    ) -> Option<FetchSpec> {
        if let Some(attempted_at) = self.rows_attempted_at {
            if self.rows_fail_streak > 0
                && attempted_at.elapsed()
                    < dashboard_core::freshness::backoff_delay(self.rows_fail_streak)
            {
                return None;
            }
        }
        if self
            .rows_landed
            .get(&(full_name.to_string(), lane))
            .is_some_and(|t| t.elapsed() < min_age)
        {
            return None;
        }
        Some(self.repo_lane_spec(full_name, lane, origin))
    }

    /// The agent surface's tab-agnostic view of "this repo's rows
    /// landed" (docs/0077 D4): any lane's clock, aged by the newest —
    /// the agent ops surface has no tab context.
    pub(crate) fn repo_rows_landed_any(&self, full_name: &str) -> Option<Instant> {
        [RepoLane::Pulls, RepoLane::Issues, RepoLane::Notifications]
            .iter()
            .filter_map(|lane| self.rows_landed.get(&(full_name.to_string(), *lane)))
            .copied()
            .max()
    }

    /// docs/0077 D3 — post-write invalidation: a settled repo-view
    /// mutation drops all three lane clocks so the next view-shown or
    /// beat refetches the active one (correctness over frugality).
    pub(crate) fn invalidate_repo_lanes(&mut self, full_name: &str) {
        for lane in [RepoLane::Pulls, RepoLane::Issues, RepoLane::Notifications] {
            self.rows_landed.remove(&(full_name.to_string(), lane));
        }
    }

    /// docs/0036 D3: the repo-rows backoff bookkeeping, called from the
    /// page's `run_specs` landing arm for the repo-lane kinds — the
    /// page owns the dispatch, the state owns the clock.
    pub(crate) fn bump_rows_backoff(&mut self, ok: bool) {
        self.rows_attempted_at = Some(Instant::now());
        if ok {
            self.rows_fail_streak = 0;
        } else {
            self.rows_fail_streak = self.rows_fail_streak.saturating_add(1);
        }
    }
    // ------------------------------------------------------------------
    // Home lane (docs/0047): spec builders + lane clocks. The page owns
    // the dispatch (the repo-rows pattern, docs/0036 D3); the state owns
    // the clocks.
    // ------------------------------------------------------------------

    /// The home lane's first page (docs/0047 D3) — fired on module open
    /// and on account switch. Ident encodes `scope:login`; no etag
    /// (GraphQL has none).
    pub(crate) fn home_open_spec(
        &self,
        login: &str,
        scope: HomeScope,
        origin: Origin,
    ) -> FetchSpec {
        FetchSpec {
            origin,
            kind: Kind::AccountPullRequests,
            url: gql::GRAPHQL_ENDPOINT.to_string(),
            etag: None,
            ident: Some(format!("{}:{}", scope.qualifier(), login)),
            max_pages: 1,
            variables: Some(serde_json::json!({
                "query": format!(
                    "is:pr is:open sort:updated-desc {}:{login}",
                    scope.qualifier()
                ),
                "after": null,
            })),
        }
    }

    /// The next home page (docs/0047 D4): same ident (the docs/0031 dedup
    /// key is per kind+ident, so a load-more collides with a concurrent
    /// refresh and skips — the beat retries when stale), `$after` set; the
    /// run arm lifts it into `SpecResult::cursor` so `apply` appends.
    pub(crate) fn home_more_spec(&self, login: &str, scope: HomeScope, after: &str) -> FetchSpec {
        // The Load-More button — always manual (docs/0047 D4).
        let mut spec = self.home_open_spec(
            login,
            scope,
            Origin::new(Surface::ModuleHome, Trigger::Manual),
        );
        let _ = &mut spec;
        if let Some(vars) = spec.variables.as_mut() {
            vars["after"] = serde_json::Value::String(after.to_string());
        }
        spec
    }

    /// Heartbeat refresh for the home view (docs/0047 D3, the
    /// [`Self::repo_rows_refresh_specs`] shape): backoff first, then the
    /// landed-clock age gate. `None` while fresh or backed off — the beat
    /// stays a no-op.
    pub(crate) fn home_refresh_specs(
        &self,
        login: &str,
        scope: HomeScope,
        min_age: Duration,
        origin: Origin,
    ) -> Option<Vec<FetchSpec>> {
        if let Some(attempted_at) = self.home_attempted_at {
            if self.home_fail_streak > 0
                && attempted_at.elapsed()
                    < dashboard_core::freshness::backoff_delay(self.home_fail_streak)
            {
                return None;
            }
        }
        if self.home_landed.is_some_and(|t| t.elapsed() < min_age) {
            return None;
        }
        Some(vec![self.home_open_spec(login, scope, origin)])
    }

    /// docs/0047: the home lane's backoff bookkeeping — the
    /// [`Self::bump_rows_backoff`] pattern, called from the page's
    /// `run_specs` landing arm for [`Kind::AccountPullRequests`].
    pub(crate) fn bump_home_backoff(&mut self, ok: bool) {
        self.home_attempted_at = Some(Instant::now());
        if ok {
            self.home_fail_streak = 0;
        } else {
            self.home_fail_streak = self.home_fail_streak.saturating_add(1);
        }
    }

    /// Post-write refetch (docs/0026 D3/D8): the global unread list plus
    /// the selected repo's one-shot slice — the marked/done row leaves
    /// both once these land. Host-run on the notification page's
    /// `MarkSettled`/`DoneSettled` events and after a settled row-level
    /// done (the notifications lanes only — the row tabs' clocks are
    /// [`Self::invalidate_repo_lanes`]'s job post-write, docs/0077 D3).
    pub(crate) fn mark_refetch_specs(&self, surface: Surface) -> Vec<FetchSpec> {
        let origin = Origin::new(surface, Trigger::MutationRefetch);
        let mut specs = vec![self.spec(
            Kind::Notifications,
            api::notifications_url(1),
            Some(MAX_PAGES),
            origin,
        )];
        if let Some(full) = &self.selected {
            let (owner, name) = split_full_name(full);
            specs.push(self.spec_ident(
                Kind::RepoNotifications,
                Some(full),
                api::repo_notifications_url(&owner, &name, 1),
                Some(2),
                origin,
            ));
        }
        specs
    }

    /// docs/0038 D4: the stored conditional-revalidation etag for a
    /// kind, if any — page-owned spec builders attach it only when the
    /// lane already holds data: a cold session with a disk etag must
    /// fetch a full 200 (a 304 against an empty record would strand it
    /// with nothing to keep).
    pub(crate) fn etag_for(&self, kind: Kind, ident: Option<&str>) -> Option<String> {
        self.etags.get(&etag_key(kind, ident)).cloned()
    }

    pub(crate) fn spec(
        &self,
        kind: Kind,
        url: String,
        max_pages: Option<u32>,
        origin: Origin,
    ) -> FetchSpec {
        self.spec_ident(kind, None, url, max_pages, origin)
    }

    /// `spec` with an identity — per-repo kinds key their ETags per repo.
    pub(crate) fn spec_ident(
        &self,
        kind: Kind,
        ident: Option<&str>,
        url: String,
        max_pages: Option<u32>,
        origin: Origin,
    ) -> FetchSpec {
        FetchSpec {
            origin,
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
        // docs/0080 D5/D6: the announce diff runs in the shared apply
        // path — every landing origin (poll, refocus, view-shown,
        // manual) announces identically; docs/0060 prices the channel
        // (OS banner while unfocused, quiet in-app toast while
        // focused). The 304 restore replays the cached rows — ids
        // already in the seen-set — so a 304 never announces.
        self.announce_new_threads(&items);
        self.notifications = items;
        // A landed sync owns freshness now (docs/0034 D2) — the ETag-304
        // path counts: the server just confirmed the blob is current.
        self.notifications_age = None;
        // docs/0038 D7: the engaged-tier landed clock (the age gate
        // reads it; the 304 restore lands here too).
        self.notifications_landed = Some(Instant::now());
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

    /// docs/0080 D4/D5: stage announce toasts for threads this session
    /// has not landed unread before — per-thread `github` Info for the
    /// first [`ANNOUNCE_CAP`] of a landing, one `+N` summary for the
    /// rest, so a backlog can never stack dozens of banners. The
    /// first landing after launch seeds silently (`D5`): threads that
    /// arrived while the app was closed live in the inbox. Keyed
    /// dedupe (`docs/0058 D4`) makes repeats refresh, and the
    /// docs/0062 mute on the `github` source silences the whole path.
    /// Click-through is deferred to #603 (docs/0058 §10 action
    /// wiring + a nav bridge do not exist; the toast is informational).
    fn announce_new_threads(&mut self, items: &[Notification]) {
        let new_threads: Vec<&Notification> = items
            .iter()
            .filter(|n| n.unread && !self.notification_seen.contains(&n.id))
            .collect();
        for n in items.iter().filter(|n| n.unread) {
            self.notification_seen.insert(n.id.clone());
        }
        if !self.notification_seen_seeded {
            self.notification_seen_seeded = true;
            return;
        }
        if new_threads.is_empty() {
            return;
        }
        let head = new_threads.len().min(ANNOUNCE_CAP);
        for n in &new_threads[..head] {
            self.pending_alerts.push(
                dashboard_core::toasts::Toast::new(
                    dashboard_core::toasts::Level::Info,
                    n.subject.title.clone(),
                )
                .title(n.repository.full_name.clone())
                .source("github")
                .dedupe_key(format!("github-notif-{}", n.id)),
            );
        }
        let rest = new_threads.len() - head;
        if rest > 0 {
            self.pending_alerts.push(
                dashboard_core::toasts::Toast::new(
                    dashboard_core::toasts::Level::Info,
                    format!("+{rest} more GitHub notifications"),
                )
                .title("GitHub")
                .source("github")
                .dedupe_key("github-notif-summary"),
            );
        }
    }
    /// Apply a parsed per-repo notifications payload to state. Shared by
    /// the 200 path and the ETag-304 restore (docs/0005 Q6, #84).
    fn apply_repo_notifications(&mut self, id: &str, items: Vec<Notification>) {
        let unread = items.iter().filter(|n| n.unread).count();
        log::debug!("repo-notifications applied: {id}, {unread} unread");
        self.repo_notifications.insert(id.to_string(), items);
        // docs/0077 D4 — the Notifications tab's own lane clock (its
        // skeleton, freshness label, and heartbeat gate read it).
        self.rows_landed
            .insert((id.to_string(), RepoLane::Notifications), Instant::now());
    }

    /// Apply a finished spec: parse, update state, store last-known + etag,
    /// handle 401/403 (Q9). Malformed payloads degrade to stale data with a
    /// stderr note — never a dialog.
    /// The auth incident's single toast (docs/0058 §13): the first
    /// terminal transition raises "reconnect required"; repeats — the
    /// drained waiters' follow-up 401s, a second Surface — stay
    /// log-only. [`GithubState::install_credential`] re-arms for the
    /// next incident.
    fn note_auth_failure(&mut self) {
        if self.auth_alert_open {
            return;
        }
        self.auth_alert_open = true;
        self.pending_alerts.push(
            dashboard_core::toasts::Toast::new(
                dashboard_core::toasts::Level::Error,
                "GitHub session expired — reconnect required.",
            )
            .source("github")
            .dedupe_key("github-auth-expired"),
        );
    }

    /// Drain staged alerts — main-thread callers holding `&mut App`
    /// (the tick loop, the refresh lead); each toast raises through
    /// the core free-fn API at the call site (docs/0058 D2/D6).
    pub(crate) fn take_pending_alerts(&mut self) -> Vec<dashboard_core::toasts::Toast> {
        std::mem::take(&mut self.pending_alerts)
    }

    // --- PR document store (docs/0072) -----------------------------------

    /// The stored document for a PR, if any (docs/0072 D1).
    pub(crate) fn pr_entry(&self, repo: &str, number: i64) -> Option<&PrEntry> {
        self.pulls.get(&PrKey::new(repo, number))
    }

    /// The kind of optimistic write in flight on a document, if any
    /// (docs/0072 D4).
    pub(crate) fn pr_pending(&self, repo: &str, number: i64) -> Option<PendingKind> {
        self.pulls
            .get(&PrKey::new(repo, number))
            .and_then(|e| e.pending.as_ref().map(|op| op.kind))
    }

    /// docs/0073 D3 — the Open PRs selector: the lane's server key
    /// order ⊗ the documents, filtered to OPEN at read time. Missing
    /// entries (evicted, signed out) are skipped; `None` = the lane
    /// never landed (the view owns skeletons). The rows are a per-frame
    /// view materialization — they cannot go stale between frames,
    /// because they are rebuilt from the store every single time.
    pub(crate) fn lane_view(&self, login: &str, scope: HomeScope) -> Option<LaneView> {
        let lane = self.home_lanes.get(&(login.to_string(), scope))?;
        let rows = lane
            .order
            .iter()
            .filter_map(|key| self.pulls.get(key))
            .filter(|e| e.summary.state.as_deref() == Some("OPEN"))
            .map(|e| e.summary.clone())
            .collect();
        Some(LaneView {
            rows,
            total_count: lane.total_count,
            complete: lane.complete,
            has_cursor: lane.cursor.is_some(),
        })
    }

    /// docs/0073 D3 (stage 2) — the repo view's derived PR rows: the
    /// snapshot's held rows ⊗ the documents (the entry wins — it
    /// carries confirmations and fresher landings), filtered to OPEN.
    /// The snapshot itself is the repo domain's persisted cache
    /// (docs/0007 D6); the overlay makes the entry authoritative at
    /// read time, so a confirmed merge/close leaves the list the
    /// moment it confirms — the #561 semantics, repo-view edition.
    /// Restored-from-disk snapshots (empty `pulls`) fall back to the
    /// snapshot row — stale-by-cache honesty.
    pub(crate) fn repo_pr_rows(&self, repo: &gql::Repo) -> Vec<gql::PullRequest> {
        repo.prs
            .nodes
            .iter()
            .map(|row| {
                // Unparseable key or no document → the snapshot row
                // itself (restored-from-disk honesty, docs/0007 D6).
                PrKey::from_row_key(&row.key)
                    .and_then(|key| self.pulls.get(&key))
                    .map_or_else(|| row.clone(), |entry| entry.summary.clone())
            })
            .filter(|row| row.state.as_deref() == Some("OPEN"))
            .collect()
    }

    // ---- docs/0072 D8 (#556): persistence — write-through + restore ----
    //
    // Every store mutation that changes a document's *content* (summary
    // land, detail land, patch confirmation, freshness void) writes the
    // row through; books/pending/errors stay session-only. The sink is
    // buffered (the writer thread commits), so none of this ever blocks
    // the caller.

    /// Land one summary row into its document (docs/0072 D2): stamped
    /// once, here. An existing document keeps its detail, books and
    /// pending state — a summary refresh never resets them.
    /// Serialize one PR document's current layers into a row upsert.
    fn persist_pull(&self, repo: &str, number: i64) {
        let Some(db) = &self.db else { return };
        let Some(entry) = self.pulls.get(&PrKey::new(repo, number)) else {
            return;
        };
        self.put_document(
            db,
            "pr",
            repo,
            number,
            &entry.summary,
            entry.summary_at,
            entry.detail.as_ref(),
            entry.detail_at,
        );
    }

    /// The issue twin.
    fn persist_issue(&self, repo: &str, number: i64) {
        let Some(db) = &self.db else { return };
        let Some(entry) = self.issues.get(&PrKey::new(repo, number)) else {
            return;
        };
        self.put_document(
            db,
            "issue",
            repo,
            number,
            &entry.summary,
            entry.summary_at,
            entry.detail.as_ref(),
            entry.detail_at,
        );
    }

    // One row per call needs both layers plus their stamps — the shape
    // is the persisted row, not an accident of growth.
    #[allow(clippy::too_many_arguments)]
    fn put_document<S: serde::Serialize, D: serde::Serialize>(
        &self,
        db: &Arc<SQLiteStore>,
        kind: &str,
        repo: &str,
        number: i64,
        summary: &S,
        summary_at: Instant,
        detail: Option<&D>,
        detail_at: Option<Instant>,
    ) {
        let summary_json = match serde_json::to_string(summary) {
            Ok(json) => json,
            Err(e) => {
                log::warn!("doc-store: {kind} {repo}#{number} summary serialize failed ({e})");
                return;
            }
        };
        // The persisted detail layer exists iff it carries a live
        // stamp: a voided (stamp-less) body must not read as fresh on
        // restore (docs/0072 D8), so it persists summary-only.
        let (detail_json, detail_at_ms) = match (detail, detail_at) {
            (Some(d), Some(at)) => match serde_json::to_string(d) {
                Ok(json) => (Some(json), Some(epoch_ms_of(at))),
                Err(e) => {
                    // A layer that cannot serialize is a bug, not a
                    // state to persist: land the summary alone.
                    log::warn!("doc-store: {kind} {repo}#{number} detail serialize failed ({e})");
                    (None, None)
                }
            },
            _ => (None, None),
        };
        db.put_document(StoredDocument {
            kind: kind.to_string(),
            repo: repo.to_string(),
            number,
            summary_at_ms: epoch_ms_of(summary_at),
            detail_at_ms,
            summary_json,
            detail_json,
        });
    }

    /// Boot restore (docs/0072 D8): parse each row with the live
    /// parser — a schema skew reads as a skipped row, never a panic —
    /// and reconstruct the freshness stamps from wall-clock ages. A
    /// detail older than [`RESTORE_DETAIL_TTL`] is not worth showing:
    /// the row restores summary-only. Books/pending/error start
    /// clean (session state); `rollup_dirty` starts false (the
    /// session relearns it on the first summary landing).
    fn seed_documents(&mut self) {
        let Some(docs) = self.db.as_ref().map(|db| db.load_documents()) else {
            return;
        };
        if docs.is_empty() {
            return;
        }
        let now_ms = now_epoch_ms();
        let mut seeded = 0usize;
        for doc in docs {
            let detail_young = doc.detail_at_ms.is_some_and(|at| {
                now_ms.saturating_sub(at) < RESTORE_DETAIL_TTL.as_millis() as u64
            });
            let ok = match doc.kind.as_str() {
                "pr" => self.seed_pull(&doc, detail_young),
                "issue" => self.seed_issue(&doc, detail_young),
                other => {
                    log::warn!(
                        "doc-store: unknown kind {other:?} for {repo}#{n}, skipped",
                        repo = doc.repo,
                        n = doc.number
                    );
                    false
                }
            };
            seeded += usize::from(ok);
        }
        if seeded > 0 {
            log::info!("doc-store: restored {seeded} document(s) from app_db");
        }
    }

    fn seed_pull(&mut self, doc: &StoredDocument, detail_young: bool) -> bool {
        let Ok(summary) = serde_json::from_str::<gql::PullRequest>(&doc.summary_json) else {
            log::warn!(
                "doc-store: pr {repo}#{n} failed to parse, skipped",
                repo = doc.repo,
                n = doc.number
            );
            return false;
        };
        let (detail, detail_at) =
            restore_layers::<gql::PrDetail>(doc, detail_young, doc.repo.as_str(), doc.number);
        let key = PrKey::new(doc.repo.clone(), doc.number);
        self.pulls.insert(
            key,
            PrEntry {
                summary,
                detail,
                summary_at: instant_from_epoch(doc.summary_at_ms),
                detail_at,
                books: DetailBooks::default(),
                pending: None,
                error: None,
                rollup_dirty: false,
            },
        );
        true
    }

    fn seed_issue(&mut self, doc: &StoredDocument, detail_young: bool) -> bool {
        let Ok(summary) = serde_json::from_str::<gql::Issue>(&doc.summary_json) else {
            log::warn!(
                "doc-store: issue {repo}#{n} failed to parse, skipped",
                repo = doc.repo,
                n = doc.number
            );
            return false;
        };
        let (detail, detail_at) =
            restore_layers::<gql::IssueDetail>(doc, detail_young, doc.repo.as_str(), doc.number);
        let key = PrKey::new(doc.repo.clone(), doc.number);
        self.issues.insert(
            key,
            IssueEntry {
                summary,
                detail,
                summary_at: instant_from_epoch(doc.summary_at_ms),
                detail_at,
                books: DetailBooks::default(),
                pending: None,
                error: None,
            },
        );
        true
    }

    pub(crate) fn land_summary(&mut self, repo: &str, row: &gql::PullRequest) {
        let key = PrKey::new(repo, row.number);
        if let Some(entry) = self.pulls.get_mut(&key) {
            // docs/0072 D3 — the rollup is a shared fact: a landing that
            // moves it marks the cached detail behind the fresher layer
            // (the page revalidates; the row shows the fresh layer now).
            // Unchanged landings — the common tick case — never mark,
            // and only `land_detail` clears the mark.
            if entry.summary.status != row.status && entry.detail.is_some() {
                entry.rollup_dirty = true;
            }
            entry.summary = row.clone();
            entry.summary_at = Instant::now();
            self.persist_pull(repo, row.number);
            return;
        }
        self.pulls.insert(
            key,
            PrEntry {
                summary: row.clone(),
                detail: None,
                summary_at: Instant::now(),
                detail_at: None,
                books: DetailBooks::default(),
                pending: None,
                error: None,
                rollup_dirty: false,
            },
        );
        self.persist_pull(repo, row.number);
    }

    /// Land a parsed detail into its document (docs/0072 D2/D3): the
    /// detail layer stamps, the read books close (any land resets the
    /// failure streak), and the cap prunes (D9). The open flow always
    /// lands the summary first, so a document-less landing is a late
    /// result after sign-out or eviction — dropped.
    pub(crate) fn land_detail(&mut self, repo: &str, number: i64, detail: gql::PrDetail) {
        let key = PrKey::new(repo, number);
        let Some(entry) = self.pulls.get_mut(&key) else {
            log::debug!(
                "pr-store: detail for unknown document {}#{n}, dropped",
                repo,
                n = number
            );
            return;
        };
        // docs/0072 D3 — the rollup is a shared fact, propagated both
        // ways: the fresh detail's rollup updates the summary layer on
        // the spot (the home row's CI glyph reads it — no 60 s wait for
        // the next home tick), and the catch-up clears any drift mark a
        // summary landing left. A degraded detail rollup (None) never
        // blanks a good row glyph — the next home landing settles it.
        if let Some(rollup) = &detail.rollup_state {
            entry.summary.status = Some(rollup.clone());
        }
        entry.rollup_dirty = false;
        entry.detail = Some(detail);
        entry.detail_at = Some(Instant::now());
        entry.books.in_flight = false;
        entry.books.last_error = None;
        entry.books.fail_streak = 0;
        self.evict_details();
        self.persist_pull(repo, number);
    }

    /// Arm the detail read on a document (docs/0072 D2): the dispatch
    /// dedup stays the shared `in_flight` guard; this is the entry's
    /// own flight record, which the page renders.
    pub(crate) fn begin_detail_read(&mut self, repo: &str, number: i64) {
        if let Some(entry) = self.pulls.get_mut(&PrKey::new(repo, number)) {
            entry.books.in_flight = true;
            entry.books.attempted_at = Some(Instant::now());
        }
    }

    /// Fail the detail read (docs/0072 D3): the entry keeps any previous
    /// detail — stale-while-revalidate — so the error surface only shows
    /// when there is nothing to paint.
    pub(crate) fn fail_detail_read(&mut self, repo: &str, number: i64, error: String) {
        if let Some(entry) = self.pulls.get_mut(&PrKey::new(repo, number)) {
            entry.books.in_flight = false;
            entry.books.last_error = Some(error);
            entry.books.fail_streak += 1;
        }
    }

    /// Arm a document's optimistic write (docs/0072 D4/D5/D6): the
    /// pending op lives on the entry, so every surface rendering it —
    /// home row pill, header — repaints from the store. One write per
    /// document (D5); the returned token must ride the confirmation
    /// (D6). `None` when there is no document or one is already in
    /// flight.
    pub(crate) fn begin_pending(
        &mut self,
        repo: &str,
        number: i64,
        kind: PendingKind,
    ) -> Option<u64> {
        let entry = self.pulls.get_mut(&PrKey::new(repo, number))?;
        if entry.pending.is_some() {
            return None;
        }
        self.next_pending_token += 1;
        let token = self.next_pending_token;
        entry.pending = Some(PendingOp {
            kind,
            token,
            started_at: Instant::now(),
        });
        // A fresh attempt clears the last failure (docs/0016 D4).
        entry.error = None;
        Some(token)
    }

    /// Confirm a pending op (docs/0072 D4/D6): apply the confirmed
    /// fields to both layers, raise the completion toast, clear the
    /// pending state. A token mismatch is refused — stale work never
    /// applies (D6). Returns whether it applied.
    pub(crate) fn resolve_pending(&mut self, repo: &str, number: i64, token: u64) -> bool {
        let key = PrKey::new(repo, number);
        let Some(entry) = self.pulls.get_mut(&key) else {
            return false;
        };
        let Some(op) = &entry.pending else {
            return false;
        };
        if op.token != token {
            log::warn!("pr-store: stale confirmation {repo}#{number} (token {token})");
            return false;
        }
        let kind = op.kind;
        entry.pending = None;
        let confirmed = match kind {
            PendingKind::Merge => Some("MERGED"),
            PendingKind::Close => Some("CLOSED"),
            // Everything else: nothing honest to patch — the post-state
            // (a new comment row, an edited body, the new head OID) is
            // only learnable by refetch (docs/0072 D4).
            PendingKind::UpdateBranch
            | PendingKind::Comment
            | PendingKind::Review
            | PendingKind::Description
            | PendingKind::CommentEdit
            | PendingKind::CommentDelete
            | PendingKind::ReviewEdit
            | PendingKind::ReviewDelete
            | PendingKind::LineComment
            | PendingKind::ReviewReply
            | PendingKind::ReviewBatch => None,
        };
        if let Some(state) = confirmed {
            entry.summary.state = Some(state.to_string());
            if let Some(detail) = &mut entry.detail {
                detail.state = Some(state.to_string());
            }
            // docs/0072 D4 + docs/0073 D6 — the confirmed state is a
            // shared fact: it lives once, on the entry, and every
            // surface reading the entry (open list, page header, merge
            // rail) is correct by construction — the open list derives
            // from this entry at read time, so the merge/close shows up
            // the moment it confirms. No lane patch needed: lanes hold
            // no rows (docs/0073 D2).
        } else {
            // Stage 3 (#554): an unpatchable confirmation changed facts
            // the cached detail cannot know. Void the layer's freshness
            // so the next open revalidates immediately instead of
            // serving a document that predates the confirmed write (the
            // SWR skip keys on a young `detail_at`). An open page
            // refetches through its arming hook; this covers the page
            // that died mid-flight (D4's session-scoped resolver).
            // Persisted (D8): the row keeps the stale detail body but
            // loses its stamp, so a restore cannot claim freshness the
            // confirmed write invalidated.
            entry.detail_at = None;
        }
        let patched = confirmed.is_some();
        self.pending_alerts.push(
            dashboard_core::toasts::Toast::new(
                dashboard_core::toasts::Level::Success,
                format!("{} #{number} · {repo}", kind.toast_verb()),
            )
            .source("github")
            .dedupe_key(format!("pr-{}:{repo}#{number}", kind.toast_slug())),
        );
        self.persist_pull(repo, number);
        let _ = patched;
        true
    }

    /// Fail a pending op (docs/0072 D4/D6/D7): the document keeps its
    /// prior state (nothing beyond the pending flag was optimistically
    /// mutated — rollback is clearing the flag), the failure records on
    /// the entry in its inline scope (docs/0016 D5 placement), and a
    /// warning toast raises.
    pub(crate) fn fail_pending(
        &mut self,
        repo: &str,
        number: i64,
        token: u64,
        message: String,
    ) -> bool {
        let key = PrKey::new(repo, number);
        let Some(entry) = self.pulls.get_mut(&key) else {
            return false;
        };
        let Some(op) = &entry.pending else {
            return false;
        };
        if op.token != token {
            log::warn!("pr-store: stale failure {repo}#{number} (token {token})");
            return false;
        }
        let kind = op.kind;
        entry.pending = None;
        entry.error = Some(MutationError {
            scope: kind.error_scope(),
            message: message.clone(),
        });
        self.pending_alerts.push(
            dashboard_core::toasts::Toast::new(
                dashboard_core::toasts::Level::Warning,
                format!(
                    "{noun} failed · #{number} {repo} — {message}",
                    noun = kind.toast_noun()
                ),
            )
            .source("github")
            .dedupe_key(format!("pr-{}-failed:{repo}#{number}", kind.toast_slug())),
        );
        true
    }

    /// The store op came back (docs/0072 D4/D6) — **session-scoped**: the
    /// resolver runs against the state entity, never the page that armed
    /// the op, so closing or navigating away mid-flight cannot orphan a
    /// pending document. Classification is the standard one: transport
    /// failures, non-200 statuses (which still forward to [`Self::apply`]
    /// — the shared 401 gate and rate-limit pause run exactly as for
    /// every other fetch) and GraphQL `errors` all fail the op; a clean
    /// 200 confirms. Returns whether the op **confirmed** — not whether
    /// the resolution applied (a refused token or a recorded failure
    /// both return false; only a clean success returns true).
    pub(crate) fn apply_store_mutation_result(
        &mut self,
        repo: &str,
        number: i64,
        kind: PendingKind,
        token: u64,
        result: SpecResult,
    ) -> bool {
        let mut failed: Option<String> = None;
        if let Some(m) = &result.network {
            failed = Some(format!("Network error: {m}"));
        } else if result.status != 200 {
            let status = result.status;
            self.apply(result);
            failed = Some(format!("GitHub answered HTTP {status}"));
        } else if let Some(message) = result.body.as_deref().and_then(|body| {
            serde_json::from_str::<gql::Envelope<serde_json::Value>>(body)
                .ok()
                .and_then(|env| env.errors.first().map(|e| e.message.clone()))
        }) {
            log::warn!("pr-mutation {repo}#{number} {kind:?}: {message}");
            failed = Some(message);
        }
        match failed {
            Some(message) => {
                // docs/0067 D6: branch-update failures get the mapped
                // copy — the raw upstream prose stays in the log above.
                let message = if kind == PendingKind::UpdateBranch {
                    update_branch_error_copy(&message)
                } else {
                    message
                };
                self.fail_pending(repo, number, token, message);
                false
            }
            None => self.resolve_pending(repo, number, token),
        }
    }

    /// docs/0072 D4 — unblock documents whose confirmation never landed:
    /// the arming page died mid-flight, or GitHub went silent. The
    /// document returns to its prior state and the inline surface
    /// explains on next visit; **no toast** — an expiry does not know
    /// that the op failed upstream, and a "failed" toast would lie. A
    /// late confirmation is refused by its gone token (D6). Returns how
    /// many documents were unblocked (the tick loop notifies on >0).
    pub(crate) fn expire_stale_pending(&mut self) -> usize {
        let now = Instant::now();
        let mut stale: Vec<(PrKey, PendingKind, u64)> = Vec::new();
        for (key, entry) in &self.pulls {
            if let Some(op) = &entry.pending {
                if now.duration_since(op.started_at) > PR_PENDING_MAX_AGE {
                    stale.push((key.clone(), op.kind, op.token));
                }
            }
        }
        for (key, kind, token) in &stale {
            let Some(entry) = self.pulls.get_mut(key) else {
                continue;
            };
            let armed = entry.pending.as_ref().is_some_and(|op| op.token == *token);
            if !armed {
                continue;
            }
            entry.pending = None;
            entry.error = Some(MutationError {
                scope: kind.error_scope(),
                message: "Still in flight upstream — check the PR on github.com".into(),
            });
            log::warn!(
                "pr-store: pending {kind:?} on {}#{} expired unconfirmed",
                key.repo,
                key.number
            );
        }
        stale.len()
    }

    /// docs/0072 D9 — cap the detail layer: evict oldest-landed details
    /// The issue document, read-side (docs/0072 stage 4).
    pub(crate) fn issue_entry(&self, repo: &str, number: i64) -> Option<&IssueEntry> {
        self.issues.get(&PrKey::new(repo, number))
    }

    /// The issue document's optimistic write in flight, if any (D4).
    pub(crate) fn issue_pending(&self, repo: &str, number: i64) -> Option<IssuePendingKind> {
        self.issues
            .get(&PrKey::new(repo, number))
            .and_then(|e| e.pending.as_ref().map(|op| op.kind))
    }

    /// Land one issue summary row into its document (docs/0072 D2,
    /// stage 4): the repo snapshot's open-issue rows seed the layer.
    /// Snapshot rows carry no `state` (the query asks for open-only),
    /// so a landing never moves a patched or detail-landed state —
    /// state changes come from confirmations and detail landings only.
    pub(crate) fn land_issue_summary(&mut self, repo: &str, row: &gql::Issue) {
        let key = PrKey::new(repo, row.number);
        if let Some(entry) = self.issues.get_mut(&key) {
            let state = entry.summary.state.clone();
            entry.summary = row.clone();
            entry.summary.state = state.or(row.state.clone());
            entry.summary_at = Instant::now();
        } else {
            self.issues.insert(
                key,
                IssueEntry {
                    summary: row.clone(),
                    detail: None,
                    summary_at: Instant::now(),
                    detail_at: None,
                    books: DetailBooks::default(),
                    pending: None,
                    error: None,
                },
            );
        }
        self.persist_issue(repo, row.number);
    }

    /// Land a parsed issue detail into its document (docs/0072 D2/D3,
    /// stage 4): the detail layer stamps, the read books close, the
    /// cap prunes. The document-less landing (late result after
    /// sign-out or eviction) is dropped, like the PR side. The state
    /// is a shared fact: the fresh detail's state updates the summary
    /// layer on the spot (a reopen/close is visible in the repo view
    /// without waiting for the next snapshot).
    pub(crate) fn land_issue_detail(&mut self, repo: &str, number: i64, detail: gql::IssueDetail) {
        let key = PrKey::new(repo, number);
        let Some(entry) = self.issues.get_mut(&key) else {
            log::debug!(
                "issue-store: detail for unknown document {}#{n}, dropped",
                repo,
                n = number
            );
            return;
        };
        entry.summary.state = detail.state.clone();
        // The detail is the freshest read the store holds (docs/0020:
        // one query on open, no cache) — its label set supersedes the
        // summary's. Without this a row's chips stay frozen at the
        // document's seed snapshot even after the user has seen the
        // fresh labels on the page (docs/0072 stage 4, amended).
        entry.summary.labels = Some(gql::LabelSummary {
            total_count: detail.labels_total,
            nodes: detail
                .labels
                .iter()
                .map(|l| gql::LabelRef {
                    name: l.name.clone(),
                    color: l.color.clone(),
                })
                .collect(),
        });
        entry.detail = Some(detail);
        entry.detail_at = Some(Instant::now());
        entry.books.in_flight = false;
        entry.books.last_error = None;
        entry.books.fail_streak = 0;
        self.evict_details();
        self.persist_issue(repo, number);
    }

    /// The user acknowledged a failure (the page-modal era cleared the
    /// page slot on cancel): the entry's inline error goes.
    pub(crate) fn clear_issue_error(&mut self, repo: &str, number: i64) {
        if let Some(entry) = self.issues.get_mut(&PrKey::new(repo, number)) {
            entry.error = None;
        }
    }

    /// Arm the issue detail read on a document (docs/0072 D2).
    pub(crate) fn begin_issue_detail_read(&mut self, repo: &str, number: i64) {
        if let Some(entry) = self.issues.get_mut(&PrKey::new(repo, number)) {
            entry.books.in_flight = true;
            entry.books.attempted_at = Some(Instant::now());
        }
    }

    /// Fail the issue detail read (docs/0072 D3): the entry keeps any
    /// previous detail — stale-while-revalidate.
    pub(crate) fn fail_issue_detail_read(&mut self, repo: &str, number: i64, error: String) {
        if let Some(entry) = self.issues.get_mut(&PrKey::new(repo, number)) {
            entry.books.in_flight = false;
            entry.books.last_error = Some(error);
            entry.books.fail_streak += 1;
        }
    }

    /// Arm an issue document's optimistic write (docs/0072 D4/D5/D6):
    /// one write per document; the token must ride the confirmation.
    pub(crate) fn begin_issue_pending(
        &mut self,
        repo: &str,
        number: i64,
        kind: IssuePendingKind,
    ) -> Option<u64> {
        let entry = self.issues.get_mut(&PrKey::new(repo, number))?;
        if entry.pending.is_some() {
            return None;
        }
        self.next_pending_token += 1;
        let token = self.next_pending_token;
        entry.pending = Some(PendingOp {
            kind,
            token,
            started_at: Instant::now(),
        });
        // A fresh attempt clears the last failure (docs/0016 D4).
        entry.error = None;
        Some(token)
    }

    /// Confirm an issue pending op (docs/0072 D4/D6, stage 4): close
    /// and reopen patch their post-state into both layers — the one
    /// field the payload speaks verbatim; everything else voids the
    /// cached detail's freshness exactly like the PR side (a new
    /// comment row or an edited body is only learnable by refetch).
    pub(crate) fn resolve_issue_pending(&mut self, repo: &str, number: i64, token: u64) -> bool {
        let key = PrKey::new(repo, number);
        let Some(entry) = self.issues.get_mut(&key) else {
            return false;
        };
        let Some(op) = &entry.pending else {
            return false;
        };
        if op.token != token {
            log::warn!("issue-store: stale confirmation {repo}#{number} (token {token})");
            return false;
        }
        let kind = op.kind;
        entry.pending = None;
        let confirmed = match kind {
            IssuePendingKind::Close => Some("CLOSED"),
            IssuePendingKind::Reopen => Some("OPEN"),
            // Nothing honest to patch — refetch decides (docs/0072 D4).
            IssuePendingKind::Comment
            | IssuePendingKind::Description
            | IssuePendingKind::CommentEdit
            | IssuePendingKind::CommentDelete
            | IssuePendingKind::SetTitle
            | IssuePendingKind::SetLabels
            | IssuePendingKind::SetAssignees
            | IssuePendingKind::SetMilestone
            | IssuePendingKind::ToggleReaction
            | IssuePendingKind::ToggleLock => None,
        };
        if let Some(state) = confirmed {
            entry.summary.state = Some(state.to_string());
            if let Some(detail) = &mut entry.detail {
                detail.state = Some(state.to_string());
            }
        } else {
            // The unpatchable confirmation changed facts the cached
            // detail cannot know: void the freshness so the next open
            // revalidates (the SWR skip keys on a young `detail_at`).
            // Persisted (D8): stale body kept, stamp lost, so a
            // restore cannot claim freshness the write invalidated.
            entry.detail_at = None;
        }
        self.pending_alerts.push(
            dashboard_core::toasts::Toast::new(
                dashboard_core::toasts::Level::Success,
                format!("{} #{number} · {repo}", kind.toast_verb()),
            )
            .source("github")
            .dedupe_key(format!("issue-{}:{repo}#{number}", kind.toast_slug())),
        );
        self.persist_issue(repo, number);
        true
    }

    /// Fail an issue pending op (docs/0072 D4/D6/D7): prior document,
    /// inline error in the op's scope, warning toast.
    pub(crate) fn fail_issue_pending(
        &mut self,
        repo: &str,
        number: i64,
        token: u64,
        message: String,
    ) -> bool {
        let key = PrKey::new(repo, number);
        let Some(entry) = self.issues.get_mut(&key) else {
            return false;
        };
        let Some(op) = &entry.pending else {
            return false;
        };
        if op.token != token {
            log::warn!("issue-store: stale failure {repo}#{number} (token {token})");
            return false;
        }
        let kind = op.kind;
        entry.pending = None;
        entry.error = Some(MutationError {
            scope: kind.error_scope(),
            message: message.clone(),
        });
        self.pending_alerts.push(
            dashboard_core::toasts::Toast::new(
                dashboard_core::toasts::Level::Warning,
                format!(
                    "{noun} failed · #{number} {repo} — {message}",
                    noun = kind.toast_noun()
                ),
            )
            .source("github")
            .dedupe_key(format!(
                "issue-{}-failed:{repo}#{number}",
                kind.toast_slug()
            )),
        );
        true
    }

    /// The issue store op came back (docs/0072 D4/D6, stage 4) — the
    /// same session-scoped resolution and classification as the PR
    /// side; see [`Self::apply_store_mutation_result`].
    pub(crate) fn apply_issue_mutation_result(
        &mut self,
        repo: &str,
        number: i64,
        kind: IssuePendingKind,
        token: u64,
        result: SpecResult,
    ) -> bool {
        let mut failed: Option<String> = None;
        if let Some(m) = &result.network {
            failed = Some(format!("Network error: {m}"));
        } else if result.status != 200 {
            let status = result.status;
            self.apply(result);
            failed = Some(format!("GitHub answered HTTP {status}"));
        } else if let Some(message) = result.body.as_deref().and_then(|body| {
            serde_json::from_str::<gql::Envelope<serde_json::Value>>(body)
                .ok()
                .and_then(|env| env.errors.first().map(|e| e.message.clone()))
        }) {
            log::warn!("issue-mutation {repo}#{number} {kind:?}: {message}");
            failed = Some(message);
        }
        match failed {
            // The failure recorded (false = the op did not confirm —
            // the PR side's contract, kept verbatim).
            Some(message) => {
                self.fail_issue_pending(repo, number, token, message);
                false
            }
            None => self.resolve_issue_pending(repo, number, token),
        }
    }

    /// The repo view's derived issue rows (docs/0073 D3, stage 4): the
    /// snapshot's held rows ⊗ the documents (the entry wins), filtered
    /// to not-closed. Snapshot rows carry no state, so the filter is
    /// `!= CLOSED` — a stateless row shows; only a confirmed close (or
    /// a closed detail landing) drops it.
    pub(crate) fn repo_issue_rows(&self, repo: &gql::Repo) -> Vec<gql::Issue> {
        repo.issues
            .nodes
            .iter()
            .map(|row| {
                PrKey::from_row_key(&row.key)
                    .and_then(|key| self.issues.get(&key))
                    .map_or_else(|| row.clone(), |entry| entry.summary.clone())
            })
            .filter(|row| row.state.as_deref() != Some("CLOSED"))
            .collect()
    }

    /// past [`PR_STORE_DETAIL_CAP`]. Pending entries never evict (D6);
    /// details landed inside the TTL window are skipped in the first
    /// pass (a page the user is actively reading). When everything is
    /// protected, the fallback evicts past protection anyway — a
    /// bounded map beats unbounded memory.
    fn evict_details(&mut self) {
        // Stage 4 (#555): one store-wide cap across both domains —
        // candidates are `(key, is_issue)` pairs; the rest is the
        // original two-pass shape (TTL-protected first, oldest last).
        let mut by_age: Vec<(PrKey, bool)> = Vec::new();
        for (k, e) in &self.pulls {
            if e.detail.is_some() {
                by_age.push((k.clone(), false));
            }
        }
        for (k, e) in &self.issues {
            if e.detail.is_some() {
                by_age.push((k.clone(), true));
            }
        }
        if by_age.len() <= PR_STORE_DETAIL_CAP {
            return;
        }
        by_age.sort_by_key(|(k, is_issue)| {
            if *is_issue {
                self.issues[k].detail_at
            } else {
                self.pulls[k].detail_at
            }
        });
        let now = Instant::now();
        let mut excess = by_age.len() - PR_STORE_DETAIL_CAP;
        for (key, is_issue) in &by_age {
            if excess == 0 {
                break;
            }
            let (pending, at) = if *is_issue {
                let e = &self.issues[key];
                (e.pending.is_some(), e.detail_at)
            } else {
                let e = &self.pulls[key];
                (e.pending.is_some(), e.detail_at)
            };
            if pending || at.is_some_and(|a| now.duration_since(a) < PR_DETAIL_TTL) {
                continue;
            }
            self.drop_detail(key, *is_issue);
            excess -= 1;
        }
        for (key, is_issue) in &by_age {
            if excess == 0 {
                break;
            }
            let (pending, has_detail) = if *is_issue {
                let e = &self.issues[key];
                (e.pending.is_some(), e.detail.is_some())
            } else {
                let e = &self.pulls[key];
                (e.pending.is_some(), e.detail.is_some())
            };
            if pending || !has_detail {
                continue;
            }
            self.drop_detail(key, *is_issue);
            excess -= 1;
        }
    }

    fn drop_detail(&mut self, key: &PrKey, is_issue: bool) {
        if is_issue {
            if let Some(e) = self.issues.get_mut(key) {
                e.detail = None;
                e.detail_at = None;
                e.books = DetailBooks::default();
            }
            return;
        }
        if let Some(e) = self.pulls.get_mut(key) {
            e.detail = None;
            e.detail_at = None;
            e.books = DetailBooks::default();
        }
    }

    pub fn apply(&mut self, result: SpecResult) {
        // Inactive-session guard (#168): fetches launched before a
        // sign-out (or before any session exists) keep flying; their
        // results must not repopulate state — a snapshot would even
        // resurrect the just-deleted cache dir via `persist_snapshot`.
        // `TokenRejected` still applies: that user is signed in with a
        // rejected credential (docs/0005 Q9) and legitimate 200s can land
        // around a failed refresh (docs/0028) — only a signed-out or
        // never-connected session is dead.
        if !self.auth.connected() && !matches!(self.auth, AuthStatus::TokenRejected) {
            log::debug!(
                "{}: dropping fetch result — session inactive",
                result.kind.kind_name()
            );
            return;
        }
        // docs/0038 D7: the global notifications lane's backoff books
        // (the 0036 pattern — the lane has no Fetch record). A 304
        // counts as success: the server just confirmed the blob is
        // current, and the restore path below stamps the landed clock.
        if result.kind == Kind::Notifications {
            let ok = result.network.is_none()
                && (result.status == 304 || (200..300).contains(&result.status));
            self.notifications_attempted_at = Some(Instant::now());
            if ok {
                self.notifications_fail_streak = 0;
            } else {
                self.notifications_fail_streak += 1;
            }
        }
        // docs/0047: the home lane's books — same shape. A failure also
        // records `home_error` (D3: rendered as error + Retry when nothing
        // else has rows); a success clears it and stamps the landed clock
        // on the landing path below.
        if result.kind == Kind::AccountPullRequests {
            let ok = result.network.is_none() && (200..300).contains(&result.status);
            self.bump_home_backoff(ok);
            if ok {
                self.home_error = None;
            } else {
                self.home_error = Some(
                    result
                        .network
                        .clone()
                        .unwrap_or_else(|| format!("HTTP {}", result.status)),
                );
            }
        }
        if let Some(m) = result.network {
            log::warn!("{}: network error: {m}", result.kind.kind_name());
            // Transition-only (docs/0058 §13): the first transport-level
            // failure on a service lane raises one offline toast per
            // incident; repeats stay log-only, recovery heals silently.
            if result.kind.is_service_lane() && !self.net_down {
                self.net_down = true;
                self.pending_alerts.push(
                    dashboard_core::toasts::Toast::new(
                        dashboard_core::toasts::Level::Warning,
                        "GitHub is unreachable — retrying in the background.",
                    )
                    .source("github")
                    .dedupe_key("github-poll-offline"),
                );
            }
            return;
        }
        // Recovery edge (docs/0058 §13): any landed transport response on
        // a service lane — 200, 304, even a 4xx/5xx the server answered —
        // heals the offline incident. Log-only: D9 keeps toasts for what
        // needs action, and nothing does once lanes land again.
        if result.kind.is_service_lane() && self.net_down {
            self.net_down = false;
            log::info!("github: connection recovered — service lanes land again");
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
                self.note_auth_failure();
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
            Kind::RateLimit => {
                // docs/0050 D3 — fold the free probe's body:
                // `resources.core` only. `resources.graphql` is a
                // phantom on this endpoint (§2, measured live during
                // #323 review) — the GraphQL book reconciles from
                // GraphQL traffic exclusively.
                if let Some(core) = parse_rate_limit_body(body) {
                    self.usage.lock().unwrap().fold_probe(core, now_epoch());
                }
            }
            Kind::SessionInventory => {
                match serde_json::from_str::<gql::Envelope<gql::SnapshotData>>(body) {
                    Ok(env) => {
                        for e in &env.errors {
                            log::warn!("inventory: {}", e.message);
                        }
                        let Some(data) = env.data else {
                            log::warn!("inventory: data null — keeping last-known");
                            return; // stale data stays (docs/0007 §6)
                        };
                        if let Some(rl) = &data.rate_limit {
                            if rl.remaining <= 0 {
                                self.rate_limited_until = Some(rl.reset_epoch());
                                log::warn!(
                                    "inventory: GraphQL point budget exhausted — paused until {}",
                                    rl.reset_at
                                );
                            }
                        }
                        // One run, one atomic apply (docs/0007 D4).
                        self.user = Some(User {
                            login: data.viewer.login.clone(),
                            avatar_url: data.viewer.avatar_url.clone(),
                        });
                        self.apply_orgs(gql::orgs_from_values(&data.viewer.organizations.nodes));
                        if let Some(page) = data.viewer.repositories {
                            // Metadata only (docs/0051 D3): `prs`/`issues`
                            // summaries stay empty — the lazy repo-scope
                            // query fills them on view open. No rows land
                            // here, so `rows_landed` is untouched.
                            let incoming: Vec<Repo> =
                                page.nodes.iter().filter_map(gql::repo_from_value).collect();
                            let completed = result.cursor.is_none();
                            self.merge_repos(incoming, completed);
                        }
                        self.persist_snapshot();
                        log::info!("inventory applied: {} repos", self.repos.len());
                    }
                    Err(e) => log::warn!("inventory payload: {e}"),
                }
            }
            Kind::CardTotals => {
                match serde_json::from_str::<gql::Envelope<gql::CardTotalsData>>(body) {
                    Ok(env) => {
                        for e in &env.errors {
                            log::warn!("card-totals: {}", e.message);
                        }
                        let Some(data) = env.data else {
                            log::warn!("card-totals: data null — keeping last-known");
                            return;
                        };
                        if let Some(rl) = &data.rate_limit {
                            if rl.remaining <= 0 {
                                self.rate_limited_until = Some(rl.reset_epoch());
                                log::warn!(
                                    "card-totals: GraphQL point budget exhausted — paused until {}",
                                    rl.reset_at
                                );
                            }
                        }
                        if let Some(page) = data.viewer.repositories {
                            let mut prs = 0i64;
                            let mut issues = 0i64;
                            for node in &page.nodes {
                                prs += node["pullRequests"]["totalCount"].as_i64().unwrap_or(0);
                                issues += node["issues"]["totalCount"].as_i64().unwrap_or(0);
                            }
                            self.card_totals = Some(CardTotals {
                                open_prs: prs,
                                open_issues: issues,
                                landed: Instant::now(),
                            });
                            // A landed sync owns freshness now (docs/0034 D2).
                            self.snapshot_age = None;
                            log::info!("card-totals applied: {prs} open PRs, {issues} open issues");
                        }
                    }
                    Err(e) => log::warn!("card-totals payload: {e}"),
                }
            }
            Kind::RepoPulls | Kind::RepoIssues => {
                let lane = match result.kind {
                    Kind::RepoPulls => RepoLane::Pulls,
                    _ => RepoLane::Issues,
                };
                self.apply_repo_lane(result.ident.as_deref(), body, lane);
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
            Kind::AccountPullRequests => {
                // docs/0047 D2: parse the envelope, land the page. The
                // ident carries `scope:login`; a `cursor` on the result
                // (set by the run arm from the spec's `$after`) marks a
                // load-more continuation — append, don't replace. The
                // docs/0031 dedup keeps a page-1 and a load-more of the
                // same account from ever flying at once, so an append
                // always lands on the entry its chain built.
                let Some((scope, login)) = result.ident.as_deref().and_then(HomeScope::from_ident)
                else {
                    log::warn!("home-prs: malformed ident {:?}", result.ident);
                    return;
                };
                match serde_json::from_str::<gql::Envelope<gql::SearchData>>(body) {
                    Ok(env) => {
                        for e in &env.errors {
                            log::warn!("home-prs: {}", e.message);
                        }
                        let Some(search) = env.data.and_then(|d| d.search) else {
                            log::warn!("home-prs: search null — keeping last-known");
                            return;
                        };
                        let page = gql::home_prs_page(&search);
                        let key = (login.to_string(), scope);

                        // docs/0072 D2: home rows are the summary layer —
                        // every landed page lands its rows into the
                        // document store (documents keep their
                        // detail/pending books across refreshes).
                        for row in &page.nodes {
                            let repo = row.key.split('#').next().unwrap_or_default();
                            self.land_summary(repo, row);
                        }
                        // docs/0073 D2: the lane records only the server
                        // key order — never the rows themselves.
                        let order = page
                            .nodes
                            .iter()
                            .map(|row| {
                                let repo = row.key.split('#').next().unwrap_or_default();
                                PrKey::new(repo, row.number)
                            })
                            .collect();
                        let appending = result.cursor.is_some();
                        let entry = self
                            .home_lanes
                            .entry(key)
                            .or_insert_with(|| HomeLane {
                                order: Vec::new(),
                                total_count: 0,
                                complete: true,
                                cursor: None,
                            });
                        if appending {
                            entry.order.extend(order);
                        } else {
                            entry.order = order;
                        }
                        entry.total_count = page.total_count;
                        entry.complete = !page.has_next_page;
                        entry.cursor = page.end_cursor;
                        self.home_landed = Some(Instant::now());
                        let suffix = if entry.complete { ", complete" } else { "" };
                        log::info!(
                            "home-prs applied: {}:{}, {} of {} open PRs{}",
                            scope.qualifier(),
                            login,
                            entry.order.len(),
                            entry.total_count,
                            suffix
                        );
                    }
                    Err(e) => log::warn!("home-prs payload: {e}"),
                }
            }
            // Page-owned kind (docs/0016 D2): the PR page parses its own
            // 200 bodies. Only non-200 results are ever forwarded here,
            // and those return in the status handling above — this arm
            // exists purely for match exhaustiveness.
            Kind::PrDetail
            | Kind::PrMutation(_)
            | Kind::PrFiles
            | Kind::JobLog
            | Kind::PrThreads
            | Kind::IssueDetail
            | Kind::RepoLabels
            | Kind::RepoAssignableUsers
            | Kind::RepoMilestones
            | Kind::RepoBranches
            | Kind::RepoIssueTemplates
            | Kind::RepoPrTemplates
            | Kind::RepoTemplatesConfig
            | Kind::ThreadDetail
            | Kind::ThreadMutation(_)
            // docs/0066: gated-lane kinds are page-owned too — the CI
            // pane parses its own 200 bodies; only their failures ever
            // reach here, handled in the status arms above.
            | Kind::GatedRuns
            | Kind::GateProbe
            | Kind::RunGateReview => {}
        }

        // ETag keyed by logical resource (kind + ident), not raw URL, so
        // pagination page-1 etags stay stable. GraphQL kinds have no
        // conditional path — never stored.
        if matches!(
            result.kind,
            Kind::Notifications
                | Kind::RepoNotifications
                // docs/0038 D4: the page-owned REST kinds join the
                // conditional path — their 200s stamp the etag, their
                // 304s land unchanged (page-side).
                | Kind::ThreadDetail
                | Kind::PrFiles
        ) {
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
    /// One lane's landing (docs/0077 D2/D4): parse, land PR summaries
    /// (docs/0073 D4 — only the pulls lane carries PR facts), swap only
    /// the landed collection into the held repo entry, persist, stamp
    /// the lane clock. The sibling collection is untouched — serde
    /// defaults an absent key, and [`Self::merge_repo_lane`] ignores it.
    fn apply_repo_lane(&mut self, ident: Option<&str>, body: &str, lane: RepoLane) {
        let slug = lane.slug();
        match serde_json::from_str::<gql::Envelope<gql::RepoLaneData>>(body) {
            Ok(env) => {
                for e in &env.errors {
                    log::warn!("repo-{slug}: {}", e.message);
                }
                match env.data.and_then(|d| d.repository) {
                    Some(value) => match gql::repo_from_value(&value) {
                        Some(repo) => {
                            let full_name = repo.full_name.clone();
                            let total = match lane {
                                RepoLane::Pulls => {
                                    // docs/0073 D4 (stage 2): snapshot rows
                                    // are PR facts — land them into the
                                    // document store so every surface (the
                                    // PR page, the overlays) reads one copy.
                                    for row in &repo.prs.nodes {
                                        self.land_summary(&full_name, row);
                                    }
                                    repo.prs.total_count
                                }
                                _ => {
                                    // docs/0072 stage 4 (amended): the
                                    // Issues lane refreshes its documents
                                    // exactly like the PR lane's stage-2
                                    // landing — a document summary that
                                    // outlives its lane goes stale (the
                                    // row renders the document, so a
                                    // label added upstream never showed).
                                    // `land_issue_summary` keeps the
                                    // patched/detail state.
                                    for row in &repo.issues.nodes {
                                        self.land_issue_summary(&full_name, row);
                                    }
                                    repo.issues.total_count
                                }
                            };
                            self.merge_repo_lane(repo, lane);
                            self.persist_snapshot();
                            self.snapshot_age = None;
                            if let Some(id) = ident {
                                self.rows_landed
                                    .insert((id.to_string(), lane), Instant::now());
                            }
                            log::info!("repo-{slug} applied: {ident:?}, {total} open");
                        }
                        None => log::warn!("repo-{slug}: malformed repository node"),
                    },
                    None => log::warn!("repo-{slug}: repository null — keeping last-known"),
                }
            }
            Err(e) => log::warn!("repo-{slug} payload: {e}"),
        }
    }

    /// Lane-aware single-repo merge (docs/0077 D2): the held entry keeps
    /// its other collection; fresh scalars (description, pushes,
    /// creation gates) ride in with the payload. A repo the snapshot
    /// hasn't seen yet is inserted whole.
    fn merge_repo_lane(&mut self, incoming: Repo, lane: RepoLane) {
        if let Some(held) = self.repos.iter_mut().find(|r| r.key == incoming.key) {
            match lane {
                RepoLane::Pulls => held.prs = incoming.prs,
                RepoLane::Issues => held.issues = incoming.issues,
                RepoLane::Notifications => {}
            }
            held.database_id = incoming.database_id;
            held.description = incoming.description;
            held.private = incoming.private;
            held.is_archived = incoming.is_archived;
            held.node_id = incoming.node_id;
            held.has_issues_enabled = incoming.has_issues_enabled;
            held.pushed_at = incoming.pushed_at;
        } else {
            self.repos.push(incoming);
        }
        self.repos.sort_by(|a, b| {
            b.last_activity()
                .cmp(&a.last_activity())
                .then_with(|| a.key.cmp(&b.key))
        });
    }

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

    /// Persist the snapshot to `snapshot-v3` (docs/0007 D7, #275 bumped
    /// the schema for `is_archived`) — atomic
    /// writes + quarantine via the 0004 store primitives.
    fn persist_snapshot(&self) {
        let snapshot = gql::SnapshotCache {
            schema: 3,
            user: self.user.clone(),
            orgs: self.orgs.clone(),
            repos: self.repos.clone(),
        };
        self.cache.put(gql::SNAPSHOT_CACHE_KEY, &snapshot);
    }

    /// Card totals (docs/0051 D2): unread notifications from the REST
    /// lane + the `CardTotals` lane's sums. `None` totals render the
    /// card's skeleton; notifications count regardless.
    pub fn card_counts(&self) -> (Option<(usize, usize)>, usize) {
        let unread = self.notifications.iter().filter(|n| n.unread).count();
        let totals = self
            .card_totals
            .as_ref()
            .map(|t| (t.open_prs.max(0) as usize, t.open_issues.max(0) as usize));
        (totals, unread)
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
        // A fresh credential re-arms the auth alert (docs/0058 §13):
        // the next terminal failure is a new incident.
        self.auth_alert_open = false;
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
                    self.note_auth_failure();
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
                self.note_auth_failure();
                GateOutcome::Surface
            }
        }
    }

    /// Sign-out (Q5): keychain + token slot + caches + data.
    pub fn sign_out(&mut self) {
        let _ = self.store.delete();
        // Alert books reset with the session (docs/0058 §13).
        self.pending_alerts.clear();
        self.net_down = false;
        self.auth_alert_open = false;
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
        // The document store is session state (docs/0072 D1) — it goes
        // with the account, like every other session book above. The
        // persisted rows too (D8): sign-out wipes them the way it wipes
        // the JSON cache lanes.
        if let Some(db) = &self.db {
            db.delete_documents();
        }
        self.pulls.clear();
        // #555 follow-up, caught by the #556 sign-out test: the issue
        // map cleared with its twin or an account switch leaked the
        // previous account's issue documents.
        self.issues.clear();
        self.next_pending_token = 0;
        self.etags.clear();
        self.selected = None;
        self.rate_limited_until = None;
        // Per-token budget (docs/0050 D3): the usage book goes with
        // the account — a reconnect starts a fresh one.
        self.usage.lock().unwrap().reset();
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

pub(crate) fn etag_key(kind: Kind, ident: Option<&str>) -> String {
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
    let (client, gate, recorder) = cx.update(|cx| {
        state.update(cx, |s, _| {
            (s.client.clone(), s.gate.clone(), s.recorder.clone())
        })
    });
    let mut used_gen = gate.generation();
    let mut rounds: u32 = 0;
    let result = loop {
        // docs/0054 D14: dispatch instant, re-stamped per round — a
        // 401 retry is a fresh dispatch with its own queue wait.
        let dispatched_at = Instant::now();
        let result = cx
            .background_executor()
            .spawn({
                let client = client.clone();
                let recorder = recorder.clone();
                let spec = spec.clone();
                async move { run_spec(&*client, &recorder, &spec, dispatched_at) }
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

/// The home lane (docs/0047 D2): one GraphQL search page. A "Load more"
/// spec carries `$after`; the run arm copies it into [`SpecResult::cursor`]
/// so `apply` appends the page instead of replacing the snapshot — the
/// same continuation semantics the capped snapshot run reports.
fn run_home_prs(http: &dyn Http, spec: &FetchSpec) -> SpecResult {
    let mut result = run_graphql_fetch(http, spec, gql::ACCOUNT_PULL_REQUESTS_QUERY, "home-prs");
    result.cursor = spec
        .variables
        .as_ref()
        .and_then(|v| v.get("after"))
        .and_then(Value::as_str)
        .map(str::to_string);
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

/// Read dispatch behind the per-key skip guard (docs/0031 D1/D2): when an
/// identical `(Kind, ident)` read is already in flight the new dispatch is
/// skipped — the caller gets `None` and applies nothing, converging on its
/// next trigger (skip, never queue; a dropped tick/focus/manual dispatch
/// always has a successor). Mutations and the connect flow stay on
/// [`run_spec_tracked`]: they are user-initiated one-shots gated by
/// `BusyOp`/`ConnectPhase`, and a retry after a failed write's refetch
/// must never be blocked.
pub(crate) async fn run_read_deduped(
    state: Entity<GithubState>,
    spec: FetchSpec,
    cx: &mut AsyncApp,
) -> Option<SpecResult> {
    let kind_name = spec.kind.kind_name();
    let key: InFlightKey = (spec.kind, spec.ident.clone());
    // A failed `cx.update` means the app is tearing down — skipping is
    // the harmless answer.
    let armed = cx.update(|cx| state.update(cx, |s, _| s.in_flight_arm(key.clone())));
    // docs/0031 D2 amended: a Manual read (docs/0052) is the user's
    // explicit re-fetch — it rides out an in-flight identical read
    // instead of being absorbed. The natural "switch lane, immediately
    // hit refresh" flow must not be a silent no-op; the in-flight
    // holder keeps its own arm, and the manual result lands after it
    // and restamps the lane clock.
    if !armed && spec.origin.trigger != Trigger::Manual {
        log::debug!("{kind_name}: skipped — an identical read is in flight");
        return None;
    }
    let result = run_spec_tracked(state.clone(), spec, cx).await;
    // Only the holder clears the arm — a Manual rider never touches it.
    if armed {
        cx.update(|cx| state.update(cx, |s, _| s.in_flight_clear(&key)));
    }
    Some(result)
}

/// One resolved Actions job for the expanded check (docs/0076 D2):
/// the resolution chain's outcome — the run id the live watch polls and
/// the name-matched job's full node (live status + steps, additive
/// parse). `job.status` decides completed-path vs live watch.
pub(crate) struct ResolvedCheckJob {
    pub run_id: u64,
    pub job: api::JobNode,
}

/// The docs/0039 D1 resolution chain, steps 1–3 (docs/0076 D7 split):
/// from the expanded check's (head SHA, name) to its matched job. Each
/// hop's non-200 forwards through [`GithubState::apply`] for the shared
/// auth/rate-limit handling before the chain fails; network errors
/// surface without the forward (the files-walk convention, docs/0016
/// §3 D8).
pub(crate) async fn resolve_check_job(
    state: Entity<GithubState>,
    repo: &str,
    number: i64,
    head_oid: &str,
    check_name: &str,
    cx: &mut AsyncApp,
) -> Result<ResolvedCheckJob, String> {
    let Some((owner, name)) = repo.split_once('/') else {
        return Err("Malformed repository id".into());
    };
    let ident = format!("{repo}#{number}#{check_name}");
    let spec_for = |url: String| FetchSpec {
        // docs/0039 D4: check-run logs fetch on expand.
        origin: Origin::new(Surface::PrPage, Trigger::Expand),
        kind: Kind::JobLog,
        url,
        etag: None,
        ident: Some(ident.clone()),
        max_pages: 1,
        variables: None,
    };
    async fn fetch(
        state: Entity<GithubState>,
        spec_for: &dyn Fn(String) -> FetchSpec,
        url: String,
        cx: &mut AsyncApp,
    ) -> SpecResult {
        run_spec_tracked(state, spec_for(url), cx).await
    }
    async fn forward_non_200(result: SpecResult, state: &Entity<GithubState>, cx: &mut AsyncApp) {
        cx.update(|cx| {
            state.update(cx, |s, cx| {
                s.apply(result);
                cx.notify();
            })
        });
    }
    let http_err = |status: u16| format!("GitHub answered HTTP {status}");

    // Step 1 — the check run, matched server-side by name on the head
    // commit; Actions-origin enforced here too (a third-party check with
    // the same name must not win the match).
    let url = api::check_runs_for_sha_url(owner, name, head_oid, check_name);
    let result = fetch(state.clone(), &spec_for, url, cx).await;
    if let Some(m) = result.network {
        return Err(format!("Network error: {m}"));
    }
    if result.status != 200 {
        let status = result.status;
        forward_non_200(result, &state, cx).await;
        return Err(http_err(status));
    }
    let page: api::CheckRunsPage = serde_json::from_str(result.body.as_deref().unwrap_or_default())
        .map_err(|e| format!("Malformed check-runs payload ({e})"))?;
    let node = page
        .check_runs
        .into_iter()
        .find(|c| c.app.as_ref().and_then(|a| a.slug.as_deref()) == Some("github-actions"))
        .ok_or("No GitHub Actions check with this name on the head commit")?;
    let suite_id = node
        .check_suite
        .map(|s| s.id)
        .ok_or("The matching check carries no suite id")?;

    // Step 2 — the one workflow run behind the suite.
    let url = api::workflow_runs_by_suite_url(owner, name, suite_id);
    let result = fetch(state.clone(), &spec_for, url, cx).await;
    if let Some(m) = result.network {
        return Err(format!("Network error: {m}"));
    }
    if result.status != 200 {
        let status = result.status;
        forward_non_200(result, &state, cx).await;
        return Err(http_err(status));
    }
    let page: api::WorkflowRunsPage =
        serde_json::from_str(result.body.as_deref().unwrap_or_default())
            .map_err(|e| format!("Malformed workflow-runs payload ({e})"))?;
    let run_id = page
        .workflow_runs
        .first()
        .map(|r| r.id)
        .ok_or("No workflow run found for this check")?;

    // Step 3 — the run's jobs, name-matched (docs/0039 D1: the sanctioned
    // iterate-and-match, exact names on both sides, matrices included).
    let url = api::run_jobs_url(owner, name, run_id);
    let result = fetch(state.clone(), &spec_for, url, cx).await;
    if let Some(m) = result.network {
        return Err(format!("Network error: {m}"));
    }
    if result.status != 200 {
        let status = result.status;
        forward_non_200(result, &state, cx).await;
        return Err(http_err(status));
    }
    let page: api::JobsPage = serde_json::from_str(result.body.as_deref().unwrap_or_default())
        .map_err(|e| format!("Malformed jobs payload ({e})"))?;
    let job = page
        .jobs
        .into_iter()
        .find(|j| j.name == check_name)
        .ok_or("No job with this name in the workflow run — it may have been renamed")?;
    Ok(ResolvedCheckJob { run_id, job })
}

/// Why a live-watch log fetch failed (docs/0076 D3): the 404 is
/// *materialization pending* — the blob can lag job completion by
/// seconds — and must not surface as an error while retries remain.
#[derive(Debug)]
pub(crate) enum JobLogsError {
    NotFound,
    Http(u16),
    Network(String),
    Malformed(String),
}

/// [`fetch_job_logs`] core with the 404 policy as a flag (docs/0076
/// D3/D7): the live watch treats the 404 as *materialization pending*
/// (no `apply` forward — the tick retries); the completed path
/// forwards every non-200 like the old monolithic chain did. Other
/// non-200s always forward for the shared auth/rate-limit handling;
/// network errors surface without the forward (the files-walk
/// convention).
pub(crate) async fn fetch_job_logs_typed(
    state: Entity<GithubState>,
    repo: &str,
    job_id: u64,
    ident: &str,
    forward_not_found: bool,
    cx: &mut AsyncApp,
) -> Result<(String, bool), JobLogsError> {
    let Some((owner, name)) = repo.split_once('/') else {
        return Err(JobLogsError::Malformed("repository id".into()));
    };
    let spec_for = |url: String| FetchSpec {
        // docs/0039 D4: check-run logs fetch on expand (docs/0076 D3:
        // the watch's landing fetch).
        origin: Origin::new(Surface::PrPage, Trigger::Expand),
        kind: Kind::JobLog,
        url,
        etag: None,
        ident: Some(ident.to_string()),
        max_pages: 1,
        variables: None,
    };
    let url = api::job_logs_url(owner, name, job_id);
    let result = run_spec_tracked(state.clone(), spec_for(url), cx).await;
    if let Some(m) = result.network {
        return Err(JobLogsError::Network(m));
    }
    if result.status == 404 && !forward_not_found {
        return Err(JobLogsError::NotFound);
    }
    if result.status != 200 {
        let status = result.status;
        cx.update(|cx| {
            state.update(cx, |s, cx| {
                s.apply(result);
                cx.notify();
            })
        });
        return Err(JobLogsError::Http(status));
    }
    Ok(crate::pr_page::prepare_log_body(
        result.body.as_deref().unwrap_or_default(),
        crate::pr_page::LOG_TAIL_CAP,
    ))
}

/// Step 4 for the completed path (docs/0076 D7): the resolved job was
/// already `completed` at resolve time, so the old chain semantics hold
/// byte-for-byte — every non-200 (the 404 included) forwards through
/// `apply()` and fails the pane into its Retry path.
pub(crate) async fn fetch_job_logs(
    state: Entity<GithubState>,
    repo: &str,
    job_id: u64,
    ident: &str,
    cx: &mut AsyncApp,
) -> Result<(String, bool), String> {
    match fetch_job_logs_typed(state, repo, job_id, ident, true, cx).await {
        Ok(landed) => Ok(landed),
        Err(JobLogsError::NotFound) => Err("GitHub answered HTTP 404".into()),
        Err(JobLogsError::Http(status)) => Err(format!("GitHub answered HTTP {status}")),
        Err(JobLogsError::Network(m)) => Err(format!("Network error: {m}")),
        Err(JobLogsError::Malformed(e)) => Err(format!("Malformed log payload ({e})")),
    }
}

/// Why a live-watch jobs tick failed (docs/0076 D2): non-200s have
/// already forwarded through `apply()`; the tick decides fatal vs
/// retryable.
#[derive(Debug)]
pub(crate) enum JobsTickError {
    Network(String),
    Http(u16),
    Malformed(String),
}

/// One live-watch tick (docs/0076 D2): the run's jobs page — one GET
/// carrying every job's embedded, live `steps[]`. Same forwarding
/// discipline as the resolution chain.
pub(crate) async fn fetch_run_jobs(
    state: Entity<GithubState>,
    repo: &str,
    run_id: u64,
    ident: &str,
    cx: &mut AsyncApp,
) -> Result<api::JobsPage, JobsTickError> {
    let Some((owner, name)) = repo.split_once('/') else {
        return Err(JobsTickError::Malformed("repository id".into()));
    };
    let spec_for = |url: String| FetchSpec {
        origin: Origin::new(Surface::PrPage, Trigger::Expand),
        kind: Kind::JobLog,
        url,
        etag: None,
        ident: Some(ident.to_string()),
        max_pages: 1,
        variables: None,
    };
    let url = api::run_jobs_url(owner, name, run_id);
    let result = run_spec_tracked(state.clone(), spec_for(url), cx).await;
    if let Some(m) = result.network {
        return Err(JobsTickError::Network(m));
    }
    if result.status != 200 {
        let status = result.status;
        cx.update(|cx| {
            state.update(cx, |s, cx| {
                s.apply(result);
                cx.notify();
            })
        });
        return Err(JobsTickError::Http(status));
    }
    serde_json::from_str(result.body.as_deref().unwrap_or_default())
        .map_err(|e| JobsTickError::Malformed(format!("{e}")))
}

// ---------------------------------------------------------------------------
// Gated workflow runs (docs/0066): sweep → gate probes → approve/reject.
// ---------------------------------------------------------------------------

/// One waiting-for-approval run, gate-resolved (docs/0066 D3/D4) — the
/// sweep's domain row; what the CI pane's waiting section renders.
#[derive(Clone, Debug)]
pub(crate) struct GatedRunRow {
    pub run_id: u64,
    pub name: String,
    pub run_number: Option<i64>,
    pub created_at: Option<String>,
    pub gate: GatedRunGate,
}

/// Which gate holds the run (docs/0066 §2.2), decided by the probe: empty
/// `pending_deployments` ⇒ fork/external-contributor (approve only —
/// upstream has no reject); non-empty ⇒ environment protection rule
/// (approve or reject; `comment` is required on the wire).
#[derive(Clone, Debug)]
pub(crate) enum GatedRunGate {
    ForkGate,
    Environments {
        /// The probe's `current_user_can_approve` across the pending
        /// deployments (absent ⇒ assumed yes; the 403 copy is the
        /// fallback). Drives button enablement (docs/0066 D6).
        can_approve: bool,
        environments: Vec<GateEnvironment>,
        /// Prior reviews when readable — `None` on read failure (the
        /// popover hides the section rather than pretending it's empty).
        approvals: Option<Vec<api::RunApprovalNode>>,
    },
    /// The probe failed — the gate (and therefore the right actions) are
    /// unknowable; the row renders a retry hint instead of buttons.
    ProbeFailed,
}

#[derive(Clone, Debug)]
pub(crate) struct GateEnvironment {
    pub id: u64,
    pub name: String,
    pub html_url: Option<String>,
}

/// Why a gate write failed — the status picks the copy (docs/0066 D6).
#[derive(Clone, Debug)]
pub(crate) struct GateActionFailure {
    pub status: u16,
    pub network: Option<String>,
}

/// One plain GET on the gated lane (docs/0066 D2/D4): dispatches through
/// the tracked lane (usage record + 401 gate), forwards non-200s through
/// [`GithubState::apply`] for the shared auth/rate-limit handling before
/// failing — the [`resolve_check_job`] convention.
async fn gated_get(
    state: &Entity<GithubState>,
    kind: Kind,
    ident: String,
    url: String,
    trigger: Trigger,
    cx: &mut AsyncApp,
) -> Result<SpecResult, String> {
    let spec = FetchSpec {
        origin: Origin::new(Surface::PrPage, trigger),
        kind,
        url,
        etag: None,
        ident: Some(ident),
        max_pages: 1,
        variables: None,
    };
    let result = run_spec_tracked(state.clone(), spec, cx).await;
    if let Some(m) = &result.network {
        return Err(format!("Network error: {m}"));
    }
    if result.status != 200 {
        let status = result.status;
        cx.update(|cx| {
            state.update(cx, |s, cx| {
                s.apply(result);
                cx.notify();
            })
        });
        return Err(format!("GitHub answered HTTP {status}"));
    }
    Ok(result)
}

/// One gated-run POST (docs/0066 D4): forwards failures for the shared
/// handling, then reports them with the status so the page can pick the
/// D6 copy (403 vs 404/422).
async fn gate_review_post(
    state: &Entity<GithubState>,
    ident: String,
    url: String,
    body: Option<Value>,
    cx: &mut AsyncApp,
) -> Result<(), GateActionFailure> {
    let spec = FetchSpec {
        origin: Origin::new(Surface::PrPage, Trigger::MutationRefetch),
        kind: Kind::RunGateReview,
        url,
        etag: None,
        ident: Some(ident),
        max_pages: 1,
        variables: body,
    };
    let result = run_spec_tracked(state.clone(), spec, cx).await;
    if let Some(m) = &result.network {
        return Err(GateActionFailure {
            status: 0,
            network: Some(m.clone()),
        });
    }
    if !(200..300).contains(&result.status) {
        let status = result.status;
        cx.update(|cx| {
            state.update(cx, |s, cx| {
                s.apply(result);
                cx.notify();
            })
        });
        return Err(GateActionFailure {
            status,
            network: None,
        });
    }
    Ok(())
}

/// The gate probe (docs/0066 D4): `pending_deployments` empty ⇒ fork
/// gate; non-empty ⇒ environment gate, carrying writability, the
/// environments, and any prior reviews. Probe/read failures ⇒
/// [`GatedRunGate::ProbeFailed`] — the row stays visible with a retry
/// hint instead of guessing at actions.
async fn probe_gate(
    state: &Entity<GithubState>,
    repo: &str,
    run_id: u64,
    cx: &mut AsyncApp,
) -> GatedRunGate {
    let Some((owner, name)) = repo.split_once('/') else {
        return GatedRunGate::ProbeFailed;
    };
    let ident = format!("{repo}#run-{run_id}");
    let url = api::run_pending_deployments_url(owner, name, run_id);
    let result = match gated_get(state, Kind::GateProbe, ident, url, Trigger::Probe, cx).await {
        Ok(result) => result,
        Err(_) => return GatedRunGate::ProbeFailed,
    };
    let pending: Vec<api::PendingDeploymentNode> =
        serde_json::from_str(result.body.as_deref().unwrap_or_default()).unwrap_or_default();
    if pending.is_empty() {
        return GatedRunGate::ForkGate;
    }
    let mut environments = Vec::new();
    let mut can_approve = true;
    for deployment in pending {
        can_approve &= deployment.current_user_can_approve != Some(false);
        if let Some(env) = deployment.environment {
            environments.push(GateEnvironment {
                id: env.id,
                name: env.name.unwrap_or_else(|| env.id.to_string()),
                html_url: env.html_url,
            });
        }
    }
    if environments.is_empty() {
        return GatedRunGate::ProbeFailed;
    }
    let approvals = read_run_approvals(state, repo, run_id, cx).await;
    GatedRunGate::Environments {
        can_approve,
        environments,
        approvals,
    }
}

/// Prior deployment reviews for an env-gated run (docs/0066 §2.2 read
/// side) — `None` on any failure; the popover hides the history rather
/// than showing a lie.
async fn read_run_approvals(
    state: &Entity<GithubState>,
    repo: &str,
    run_id: u64,
    cx: &mut AsyncApp,
) -> Option<Vec<api::RunApprovalNode>> {
    let (owner, name) = repo.split_once('/')?;
    let ident = format!("{repo}#run-{run_id}-approvals");
    let url = api::run_approvals_url(owner, name, run_id);
    let result = gated_get(state, Kind::GateProbe, ident, url, Trigger::Probe, cx)
        .await
        .ok()?;
    serde_json::from_str(result.body.as_deref().unwrap_or_default()).ok()
}

/// The docs/0066 D2 sweep: runs waiting on approval for the PR's head
/// commit — server-filtered by branch + event + `action_required`, then
/// client-filtered by exact head SHA, then probe-resolved per run.
pub(crate) async fn load_gated_runs(
    state: Entity<GithubState>,
    repo: &str,
    head_branch: &str,
    head_oid: &str,
    cx: &mut AsyncApp,
) -> Result<Vec<GatedRunRow>, String> {
    let Some((owner, name)) = repo.split_once('/') else {
        return Err("Malformed repository id".into());
    };
    let short = head_oid.get(..7).unwrap_or(head_oid);
    let ident = format!("{repo}#gated-{short}");
    let url = api::gated_runs_url(owner, name, head_branch);
    let result = gated_get(&state, Kind::GatedRuns, ident, url, Trigger::ViewShown, cx).await?;
    let page: api::GatedRunsPage = serde_json::from_str(result.body.as_deref().unwrap_or_default())
        .map_err(|e| format!("Malformed gated-runs payload ({e})"))?;
    let mut rows = Vec::new();
    for run in page.workflow_runs {
        if run.head_sha.as_deref() != Some(head_oid) {
            continue;
        }
        let gate = probe_gate(&state, repo, run.id, cx).await;
        rows.push(GatedRunRow {
            run_id: run.id,
            name: run.name.unwrap_or_else(|| format!("run {}", run.id)),
            run_number: run.run_number,
            created_at: run.created_at,
            gate,
        });
    }
    Ok(rows)
}

/// The docs/0066 D2 fallback: when the sweep finds nothing while the PR
/// shows zero check runs, walk the head SHA's check suites (which exist
/// even pre-approval) and resolve each `action_required` suite's run
/// through the docs/0039 step-2 URL. Fallback rows know the run id but
/// not the workflow name (the suite payload doesn't carry it).
pub(crate) async fn load_gated_runs_via_suites(
    state: Entity<GithubState>,
    repo: &str,
    head_oid: &str,
    cx: &mut AsyncApp,
) -> Result<Vec<GatedRunRow>, String> {
    let Some((owner, name)) = repo.split_once('/') else {
        return Err("Malformed repository id".into());
    };
    let short = head_oid.get(..7).unwrap_or(head_oid);
    let ident = format!("{repo}#suites-{short}");
    let url = api::check_suites_for_sha_url(owner, name, head_oid);
    let result = gated_get(&state, Kind::GatedRuns, ident, url, Trigger::Expand, cx).await?;
    let page: api::CheckSuitesPage =
        serde_json::from_str(result.body.as_deref().unwrap_or_default())
            .map_err(|e| format!("Malformed check-suites payload ({e})"))?;
    let mut rows = Vec::new();
    for suite in page.check_suites {
        if suite.conclusion.as_deref() != Some("action_required") {
            continue;
        }
        let suite_ident = format!("{repo}#suite-{}", suite.id);
        let suite_url = api::workflow_runs_by_suite_url(owner, name, suite.id);
        let runs_result = gated_get(
            &state,
            Kind::GatedRuns,
            suite_ident,
            suite_url,
            Trigger::Expand,
            cx,
        )
        .await?;
        let runs: api::WorkflowRunsPage =
            serde_json::from_str(runs_result.body.as_deref().unwrap_or_default())
                .map_err(|e| format!("Malformed suite-runs payload ({e})"))?;
        for run in runs.workflow_runs {
            let gate = probe_gate(&state, repo, run.id, cx).await;
            rows.push(GatedRunRow {
                run_id: run.id,
                name: format!("workflow run {}", run.id),
                run_number: None,
                created_at: None,
                gate,
            });
        }
    }
    Ok(rows)
}

/// Approve a fork-gated run (docs/0066 D4): `POST …/runs/{run_id}/approve`,
/// empty body, 201. The fork gate's only disposition — there is no
/// upstream reject.
pub(crate) async fn approve_gated_run(
    state: Entity<GithubState>,
    repo: &str,
    run_id: u64,
    cx: &mut AsyncApp,
) -> Result<(), GateActionFailure> {
    let Some((owner, name)) = repo.split_once('/') else {
        return Err(GateActionFailure {
            status: 0,
            network: Some("Malformed repository id".into()),
        });
    };
    let ident = format!("{repo}#approve-{run_id}");
    let url = api::approve_run_url(owner, name, run_id);
    gate_review_post(&state, ident, url, None, cx).await
}

/// Approve or reject an environment-gated run (docs/0066 D4): `POST
/// …/runs/{run_id}/pending_deployments` with the selected environments,
/// the decision, and the (non-empty) wire comment.
pub(crate) async fn review_pending_deployments(
    state: Entity<GithubState>,
    repo: &str,
    run_id: u64,
    environment_ids: Vec<u64>,
    decision: &str,
    comment: &str,
    cx: &mut AsyncApp,
) -> Result<(), GateActionFailure> {
    let Some((owner, name)) = repo.split_once('/') else {
        return Err(GateActionFailure {
            status: 0,
            network: Some("Malformed repository id".into()),
        });
    };
    let ident = format!("{repo}#review-{run_id}");
    let url = api::run_pending_deployments_url(owner, name, run_id);
    let body = serde_json::json!({
        "environment_ids": environment_ids,
        "state": decision,
        "comment": comment,
    });
    gate_review_post(&state, ident, url, Some(body), cx).await
}

/// The single refresh procedure (docs/0028 §3.2): the gate lead only.
/// Reads the grant inputs on the main thread, runs the grant on the
/// background executor, applies the outcome on the main thread (install
/// is persist-before-publish, docs/0011 §4.4), then completes the queue.
/// A sign-out or another install that landed mid-refresh drops the
/// result — the slots it would overwrite no longer belong to this
/// session.
async fn run_refresh(state: Entity<GithubState>, cx: &mut AsyncApp) {
    let (gate, client, recorder, refresh_token, used_gen) = cx.update(|cx| {
        state.update(cx, |s, _| {
            (
                s.gate.clone(),
                s.client.clone(),
                s.recorder.clone(),
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
        .spawn(async move {
            // docs/0053 D4: the auth POST records too — free (the login
            // endpoints never bill the API quota, docs/0050 D1).
            // docs/0054 D14: auth flights stamp queue wait from spawn.
            let dispatched_at = Instant::now();
            refresh(
                &recording_for_auth(&*client, &recorder, dispatched_at),
                &refresh_token,
            )
        })
        .await;
    if guard.gate.generation() != used_gen {
        log::debug!("token refresh landed after a session change — dropped");
        return; // the guard drains any waiter that raced in
    }
    // Entity observers fire only on cx.notify — a window.refresh repaint
    // never triggers them. The page sync + crumb (docs/0009 D6, docs/0014
    // D1/D6) and the card badge rely on these notifies.
    let gate_outcome = cx.update(|cx| {
        let (outcome, alerts) = state.update(cx, |s, cx| {
            let o = s.apply_refresh(outcome);
            cx.notify();
            (o, s.take_pending_alerts())
        });
        alerts
            .into_iter()
            .for_each(|toast| dashboard_core::toasts::raise(cx, toast));
        outcome
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
    origin: Origin,
    cx: &mut AsyncApp,
) -> SpecResult {
    let spec = FetchSpec {
        // docs/0053 D2: the write's refetch origin — callers pass their
        // surface; a mutation is always a MutationRefetch trigger.
        origin,
        kind: Kind::PrMutation(mutation),
        url: gql::GRAPHQL_ENDPOINT.to_string(),
        etag: None,
        ident: Some(ident),
        max_pages: 1,
        variables: Some(variables),
    };
    run_spec_tracked(state, spec, cx).await
}

/// The 10-second service loop (docs/0005 Q7/Q8, docs/0052 D1): plan →
/// (drain) → background-run → apply. The plan carries only non-content
/// duties — pause clearing, token arming, the free usage probe, the
/// bootstrap arm; a proactive refresh armed by planning drains in the
/// same tick (docs/0028 §3.4).
pub fn spawn_tick_loop(state: Entity<GithubState>, window: AnyWindowHandle, cx: &mut App) {
    cx.spawn(async move |cx| {
        let mut tick: u64 = 0;
        loop {
            // First beat runs immediately on open — the card must move off
            // its boot-seeded cache within seconds, not after one full
            // TICK sleep. Subsequent beats land on the 60s cadence.
            tick += 1;

            let (mut specs, pending, expired) = cx.update(|cx| {
                state.update(cx, |s, _| {
                    // docs/0072 D4: unblock documents whose confirmation
                    // never landed before planning the beat.
                    let expired = s.expire_stale_pending();
                    let specs = s.plan_tick(tick);
                    (specs, std::mem::take(&mut s.pending_refresh), expired)
                })
            });
            // docs/0072 D4: an expiry mutated documents — observers
            // repaint (the pill leaves, the header unblocks).
            if expired > 0 {
                log::info!("tick {tick}: {expired} stale pending op(s) expired");
                cx.update(|cx| state.update(cx, |_, cx| cx.notify()));
            }
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
                // Tick traffic is reads — the skip guard applies (docs/0031
                // D1/D2): a spec whose twin is still running (a page lane
                // refetch) waits for the next beat.
                let Some(result) = run_read_deduped(state.clone(), spec, cx).await else {
                    continue;
                };
                cx.update(|cx| {
                    let alerts = state.update(cx, |s, cx| {
                        s.apply(result);
                        cx.notify();
                        s.take_pending_alerts()
                    });
                    alerts
                        .into_iter()
                        .for_each(|toast| dashboard_core::toasts::raise(cx, toast));
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
        let (client, recorder) =
            cx.update(|cx| state.update(cx, |s, _| (s.client.clone(), s.recorder.clone())));

        // Step 1 — device + user codes.
        let auth_client = client.clone();
        let code_recorder = recorder.clone();
        let start = cx
            .background_executor()
            .spawn(async move {
                // docs/0053 D4: connect flights record through the same
                // decorator — free (login endpoints, docs/0050 D1).
                // docs/0054 D14: queue wait stamps from spawn.
                let dispatched_at = Instant::now();
                start_device_flow(&RecordingHttp::with_transport(
                    &*auth_client,
                    &code_recorder,
                    Origin::new(Surface::Connect, Trigger::Auth),
                    "connect-code",
                    Transport::Auth,
                    true,
                    Some(dispatched_at),
                ))
            })
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
                let recorder = recorder.clone();
                let device_code = start.device_code.clone();
                cx.background_executor()
                    .spawn(async move {
                        // docs/0054 D14: queue wait stamps from spawn.
                        let dispatched_at = Instant::now();
                        poll_token(
                            &RecordingHttp::with_transport(
                                &*client,
                                &recorder,
                                Origin::new(Surface::Connect, Trigger::Auth),
                                "connect-poll",
                                Transport::Auth,
                                true,
                                Some(dispatched_at),
                            ),
                            &device_code,
                            interval,
                        )
                    })
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

    /// docs/0053 §5: a fresh recorder over a small private sink —
    /// transport tests don't assert records (the dispatch table does).
    fn test_recorder() -> dashboard_core::usage_records::Recorder {
        dashboard_core::usage_records::Recorder::new(
            "github",
            std::sync::Arc::new(dashboard_core::usage_records::MemorySink::new(64)),
        )
    }
    use super::*;
    use crate::auth::MockTokenStore;
    use crate::drafts::DraftStore;
    use crate::http::HttpResult;
    use crate::http::mock::{MockHttp, Recorded};
    use crate::usage::Surface;
    use dashboard_core::usage_records::{Transport, UsageSource as _};
    use gpui::AppContext as _;
    use std::collections::HashMap;

    /// docs/0050 D3: a fresh-looking snapshot keeps plan_tick's free
    /// probe lane out of the cadence tests below.
    fn seed_fresh_usage(state: &GithubState) {
        let now = now_epoch();
        state.usage.lock().unwrap().seed_snapshots(
            crate::usage::BudgetSnapshot {
                limit: 5000,
                remaining: 5000,
                reset_at: now + 3600,
            },
            crate::usage::BudgetSnapshot {
                limit: 5000,
                remaining: 5000,
                reset_at: now + 3600,
            },
            now,
        );
    }

    fn make_state(http: Arc<MockHttp>) -> GithubState {
        let shared: SharedToken = Arc::new(std::sync::RwLock::new(Some("t0k".into())));
        let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
        let client: Arc<dyn Http> = http;
        let state = GithubState::new(
            client,
            Arc::new(MockTokenStore::with("t0k".into())),
            shared,
            refresh,
            GithubCache::at_tmp("state"),
            DraftStore::at_tmp("state"),
        );
        seed_fresh_usage(&state);
        state
    }

    fn make_state_at(http: Arc<MockHttp>, cache: GithubCache, drafts: DraftStore) -> GithubState {
        let shared: SharedToken = Arc::new(std::sync::RwLock::new(Some("t0k".into())));
        let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
        let client: Arc<dyn Http> = http;
        let state = GithubState::new(
            client,
            Arc::new(MockTokenStore::with("t0k".into())),
            shared,
            refresh,
            cache,
            drafts,
        );
        seed_fresh_usage(&state);
        state
    }

    fn service_lane_result(status: u16, network: Option<&str>) -> SpecResult {
        SpecResult {
            kind: Kind::Notifications,
            ident: None,
            status,
            etag: None,
            rate: None,
            body: None,
            network: network.map(str::to_string),
            cursor: None,
        }
    }

    fn staged_keys(state: &GithubState) -> Vec<(dashboard_core::toasts::Level, String)> {
        state
            .pending_alerts
            .iter()
            .map(|t| {
                (
                    t.level,
                    t.dedupe_key.as_deref().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn offline_toast_stages_once_per_incident_and_recovery_re_arms() {
        // docs/0058 §13: transition-only. The first transport failure on
        // a service lane stages exactly one offline toast; repeats stage
        // none; any landed response heals silently; the next failure is
        // a new incident.
        let mut state = make_state(Arc::new(MockHttp::ok(200, "{}")));
        state.apply(service_lane_result(0, Some("timeout")));
        assert!(state.net_down);
        assert_eq!(
            staged_keys(&state),
            vec![(
                dashboard_core::toasts::Level::Warning,
                "github-poll-offline".to_string()
            )]
        );
        state.apply(service_lane_result(0, Some("timeout")));
        assert_eq!(state.pending_alerts.len(), 1, "repeats stay log-only");
        // A landed response — even a bare 304 — heals the incident.
        state.apply(service_lane_result(304, None));
        assert!(!state.net_down);
        assert_eq!(state.pending_alerts.len(), 1, "recovery is log-only");
        // The next failure is a new incident: it raises again.
        state.apply(service_lane_result(0, Some("timeout")));
        assert_eq!(state.pending_alerts.len(), 2);
    }

    #[test]
    fn page_lane_network_failures_stay_log_only() {
        // docs/0058 §13: page-owned lanes surface failures inline where
        // the user is looking — no offline toast, no FSM flip.
        let mut state = make_state(Arc::new(MockHttp::ok(200, "{}")));
        state.apply(SpecResult {
            kind: Kind::PrDetail,
            ident: None,
            status: 0,
            etag: None,
            rate: None,
            body: None,
            network: Some("timeout".to_string()),
            cursor: None,
        });
        assert!(state.pending_alerts.is_empty());
        assert!(!state.net_down);
    }

    #[test]
    fn auth_incident_raises_once_until_reconnect() {
        // docs/0058 §13: one "reconnect required" toast per auth
        // incident; repeats (drained waiters' 401s) stay log-only; a
        // fresh credential re-arms.
        let mut state = make_state(Arc::new(MockHttp::ok(200, "{}")));
        state.apply(service_lane_result(401, None));
        assert_eq!(
            staged_keys(&state),
            vec![(
                dashboard_core::toasts::Level::Error,
                "github-auth-expired".to_string()
            )]
        );
        state.apply(service_lane_result(401, None));
        assert_eq!(state.pending_alerts.len(), 1, "transition-only");
        state
            .install_credential(crate::auth::StoredCredential {
                access_token: "t1".to_string(),
                refresh_token: Some("r1".to_string()),
                access_expires_at: None,
            })
            .unwrap();
        assert!(!state.auth_alert_open, "reconnect re-arms the alert");
        state.apply(service_lane_result(401, None));
        assert_eq!(state.pending_alerts.len(), 2);
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
                origin: Origin::BACKGROUND,
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
    fn files_review_mutations_post_their_documents() {
        // docs/0030 D7: the Files-tab review writes — one document per
        // variant except ReviewBatch, which rides the addPullRequestReview
        // document with the draft batch in its `threads` array (D4).
        let cases: Vec<(Mutation, &str, serde_json::Value)> = vec![
            (
                Mutation::ReviewThread,
                "addPullRequestReviewThread",
                serde_json::json!({ "input": {
                    "pullRequestId": "PR_1",
                    "path": "src/main.rs",
                    "line": 14,
                    "side": "RIGHT",
                    "body": "nit"
                } }),
            ),
            (
                // #211: the same document carries the optional range —
                // `startLine`/`startSide` (verified live 2026-09-16).
                Mutation::ReviewThread,
                "addPullRequestReviewThread",
                serde_json::json!({ "input": {
                    "pullRequestId": "PR_1",
                    "path": "src/main.rs",
                    "line": 16,
                    "side": "RIGHT",
                    "startLine": 14,
                    "startSide": "RIGHT",
                    "body": "nit on both lines"
                } }),
            ),
            (
                Mutation::ReviewThreadReply,
                "addPullRequestReviewComment",
                serde_json::json!({ "input": { "body": "agreed", "inReplyTo": "RC_9" } }),
            ),
            (
                // D4: the whole draft review is ONE request — the batch
                // rides the threads array; the summary body is optional and
                // present here.
                Mutation::ReviewBatch,
                "addPullRequestReview",
                serde_json::json!({ "input": {
                    "pullRequestId": "PR_1",
                    "event": "COMMENT",
                    "threads": [
                        { "body": "first", "path": "src/main.rs", "line": 14, "side": "RIGHT" },
                        { "body": "second", "path": "src/lib.rs", "line": 2, "side": "LEFT" },
                        { "body": "third", "path": "src/main.rs", "line": 16, "side": "RIGHT", "startLine": 14, "startSide": "RIGHT" },
                    ],
                    "body": "three nits"
                } }),
            ),
            (
                Mutation::ThreadResolve,
                "resolveReviewThread",
                serde_json::json!({ "input": { "threadId": "RT_1" } }),
            ),
            (
                // #213: the same document carries the optional reason —
                // `PullRequestReviewThreadResolutionReason` enum token.
                Mutation::ThreadResolve,
                "resolveReviewThread",
                serde_json::json!({ "input": { "threadId": "RT_1", "resolutionReason": "WONT_FIX" } }),
            ),
            (
                Mutation::ThreadUnresolve,
                "unresolveReviewThread",
                serde_json::json!({ "input": { "threadId": "RT_1" } }),
            ),
            (
                // #212: edit — note the `pullRequestReviewCommentId` id
                // field (verified live 2026-09-16), unlike `id`/
                // `pullRequestReviewId` on the timeline edits.
                Mutation::UpdateReviewComment,
                "updatePullRequestReviewComment",
                serde_json::json!({ "input": { "pullRequestReviewCommentId": "RC_9", "body": "edited" } }),
            ),
            (
                Mutation::DeleteReviewComment,
                "deletePullRequestReviewComment",
                serde_json::json!({ "input": { "id": "RC_9" } }),
            ),
        ];
        for (mutation, op, variables) in cases {
            let http = Arc::new(MockHttp::ok(200, r#"{"data":{}}"#));
            let spec = FetchSpec {
                origin: Origin::BACKGROUND,
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
                origin: Origin::BACKGROUND,
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
            is_archived: false,
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

    // --- docs/0080: the unfocused-poll arm + the announce diff ---------

    /// One REST notification item as JSON, for the announce tests.
    fn poll_notif(id: &str, repo: &str, title: &str) -> String {
        format!(
            r#"{{"id":"{id}","unread":true,"subject":{{"title":"{title}","type":"Issue","url":"u"}},"repository":{{"full_name":"{repo}"}}}}"#
        )
    }

    fn poll_notifs(items: &[String]) -> Vec<Notification> {
        serde_json::from_str(&format!("[{}]", items.join(","))).unwrap()
    }

    #[test]
    fn plan_tick_polls_notifications_only_while_unfocused() {
        let http = Arc::new(MockHttp::new(vec![]));
        let mut state = make_state(http.clone());
        state.auth = AuthStatus::Connected;
        state.needs_bootstrap = false;

        // Focused: docs/0052 D1 holds — no content lane from the tick.
        let specs = state.plan_tick(1);
        assert!(
            specs.iter().all(|s| s.kind != Kind::Notifications),
            "a focused tick plans no notifications"
        );

        // Unfocused: the poll arm offers the lane, stamped
        // `service-tick × poll` for the Usage receipts (docs/0080 D3).
        state.window_active = false;
        let specs = state.plan_tick(2);
        let poll = specs
            .iter()
            .find(|s| s.kind == Kind::Notifications)
            .expect("the unfocused tick polls notifications");
        assert_eq!(poll.origin.surface.as_str(), "service-tick");
        assert_eq!(poll.origin.trigger.as_str(), "poll");

        // The 60 s floor holds after a fresh landing (docs/0080 D3).
        state.notifications_landed = Some(Instant::now());
        assert!(
            state
                .plan_tick(3)
                .iter()
                .all(|s| s.kind != Kind::Notifications)
        );

        // Stale past the floor: the arm re-offers.
        state.notifications_landed = Some(Instant::now() - UNFOCUSED_POLL_MIN_AGE);
        assert!(
            state
                .plan_tick(4)
                .iter()
                .any(|s| s.kind == Kind::Notifications)
        );

        // Backoff gates the arm like every other trigger (docs/0036,
        // reused by docs/0080 D3) — no request storm on failures.
        state.notifications_fail_streak = 1;
        state.notifications_attempted_at = Some(Instant::now());
        assert!(
            state
                .plan_tick(5)
                .iter()
                .all(|s| s.kind != Kind::Notifications)
        );
    }

    #[test]
    fn apply_notifications_seeds_silently_then_announces_the_diff() {
        let http = Arc::new(MockHttp::new(vec![]));
        let mut state = make_state(http.clone());
        state.auth = AuthStatus::Connected;

        // First landing after launch: seeds silently (docs/0080 D5) —
        // no launch-time banner storm for pre-existing unread threads.
        state.apply_notifications(poll_notifs(&[poll_notif("1", "o/r", "t1")]));
        assert!(state.pending_alerts.is_empty(), "the seed stays silent");

        // A later landing announces only the unseen threads, keyed
        // `github-notif-{id}`, Info, source `github` (docs/0080 D4).
        state.apply_notifications(poll_notifs(&[
            poll_notif("1", "o/r", "t1"),
            poll_notif("2", "o/r", "t2"),
            poll_notif("3", "acme/x", "t3"),
        ]));
        let alerts = state.take_pending_alerts();
        assert_eq!(alerts.len(), 2, "seen thread 1 stays silent");
        assert_eq!(alerts[0].dedupe_key.as_deref(), Some("github-notif-2"));
        assert_eq!(alerts[1].dedupe_key.as_deref(), Some("github-notif-3"));
        assert_eq!(alerts[0].source.as_deref(), Some("github"));
        assert_eq!(alerts[0].level, dashboard_core::toasts::Level::Info);
        assert_eq!(&alerts[0].message[..], "t2");
        assert_eq!(alerts[0].title.as_deref(), Some("o/r"));

        // Repeats and already-seen threads stay silent (docs/0058 D4).
        state.apply_notifications(poll_notifs(&[poll_notif("2", "o/r", "t2")]));
        assert!(state.pending_alerts.is_empty());

        // A thread that first landed read announces when it turns
        // unread — read landings don't enter the seen-set.
        let read_landed: Vec<Notification> = serde_json::from_str(
            r#"[{"id":"9","unread":false,"subject":{"title":"t9","type":"Issue","url":"u"},"repository":{"full_name":"o/r"}}]"#,
        )
        .unwrap();
        state.apply_notifications(read_landed);
        assert!(state.pending_alerts.is_empty(), "read lands silent");
        let unread_later: Vec<Notification> =
            serde_json::from_str(&format!("[{}]", poll_notif("9", "o/r", "t9"))).unwrap();
        state.apply_notifications(unread_later);
        let alerts = state.take_pending_alerts();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].dedupe_key.as_deref(), Some("github-notif-9"));
    }

    #[test]
    fn apply_notifications_caps_announce_with_summary() {
        let http = Arc::new(MockHttp::new(vec![]));
        let mut state = make_state(http.clone());
        state.auth = AuthStatus::Connected;

        state.apply_notifications(poll_notifs(&[poll_notif("0", "o/r", "seed")]));
        assert!(state.pending_alerts.is_empty());

        let five: Vec<String> = (1..=5)
            .map(|i| poll_notif(&i.to_string(), "o/r", &format!("t{i}")))
            .collect();
        state.apply_notifications(poll_notifs(&five));
        let alerts = state.take_pending_alerts();
        assert_eq!(alerts.len(), 4, "3 per-thread + 1 summary (docs/0080 D4)");
        for (i, alert) in alerts.iter().take(ANNOUNCE_CAP).enumerate() {
            assert_eq!(
                alert.dedupe_key.as_deref(),
                Some(format!("github-notif-{}", i + 1).as_str())
            );
        }
        let summary = &alerts[ANNOUNCE_CAP];
        assert_eq!(summary.dedupe_key.as_deref(), Some("github-notif-summary"));
        assert_eq!(&summary.message[..], "+2 more GitHub notifications");
        assert_eq!(summary.source.as_deref(), Some("github"));
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

    fn inventory_result(body: &str, cursor: Option<&str>) -> SpecResult {
        SpecResult {
            kind: Kind::SessionInventory,
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
    fn anchor_walk_preserves_parse_patch_classification() {
        // The old parse_patch table, re-run through the anchored walk:
        // classification is identical, the walk only adds line numbers.
        let rows = anchor_patch(
            "@@ -1,3 +1,4 @@\n fn one() {}\n+fn two() {}\n-fn zero() {}\n@@ -10,2 +11,2 @@\n context with no leading space",
        );
        assert_eq!(
            rows.iter().map(|a| a.line.clone()).collect::<Vec<_>>(),
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
        assert!(anchor_patch("").is_empty());
    }

    // --- anchor_patch (docs/0030 D2/§5) ----------------------------------

    use crate::service::{AnchoredLine, anchor_patch};

    fn nums(a: &AnchoredLine) -> (Option<u32>, Option<u32>) {
        (a.old, a.new)
    }

    #[test]
    fn anchor_walk_seeds_and_advances_both_sides() {
        let rows = anchor_patch("@@ -10,3 +11,4 @@\n ctx\n-del\n+add\n ctx2\n+add2");
        assert_eq!(rows.len(), 6);
        assert_eq!(nums(&rows[0]), (None, None), "hunk header anchors at None");
        assert_eq!(nums(&rows[1]), (Some(10), Some(11)), "context: both");
        assert_eq!(nums(&rows[2]), (Some(11), None), "del: old only");
        assert_eq!(nums(&rows[3]), (None, Some(12)), "add: new only");
        assert_eq!(nums(&rows[4]), (Some(12), Some(13)), "counters keep up");
        assert_eq!(nums(&rows[5]), (None, Some(14)));
    }

    #[test]
    fn anchor_walk_reseeds_per_hunk() {
        let rows = anchor_patch("@@ -1,1 +2,1 @@\n a\n@@ -50,1 +60,1 @@\n b");
        assert_eq!(nums(&rows[1]), (Some(1), Some(2)));
        assert_eq!(nums(&rows[3]), (Some(50), Some(60)));
    }

    #[test]
    fn anchor_walk_tolerates_bad_headers_and_newline_markers() {
        // Rows before/between parseable headers anchor at None and
        // advance nothing; `\\ No newline` markers are not file lines.
        let rows = anchor_patch(
            "junk line\n@@ -bogus @@\n a\n\\ No newline at end of file\n@@ -1,1 +1,1 @@\n b",
        );
        assert_eq!(rows.len(), 6);
        assert_eq!(nums(&rows[0]), (None, None));
        assert_eq!(nums(&rows[2]), (None, None), "after a bad header");
        assert_eq!(nums(&rows[3]), (None, None), "newline marker");
        assert_eq!(nums(&rows[5]), (Some(1), Some(1)), "valid header re-arms");
    }

    #[test]
    fn anchor_walk_empty_patch_is_empty() {
        assert!(anchor_patch("").is_empty());
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
            DraftStore::at_tmp("disconnected"),
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
    fn plan_tick_bootstrap_arms_inventory_and_notifications() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.auth = AuthStatus::Connected;

        // Bootstrap (docs/0051 D6): session inventory + notifications,
        // then the flag is consumed. The notifications spec paginates to
        // the full MAX_PAGES walk — the first 100 unread must not
        // silently truncate (#170).
        state.needs_bootstrap = true;
        let specs = state.plan_tick(1);
        assert_eq!(
            specs.iter().map(|s| s.kind).collect::<Vec<_>>(),
            vec![Kind::SessionInventory, Kind::Notifications]
        );
        let notif = specs
            .iter()
            .find(|s| s.kind == Kind::Notifications)
            .unwrap();
        assert_eq!(notif.max_pages, MAX_PAGES);
        assert!(!state.needs_bootstrap);

        // docs/0052 D1: the tick plans no content lanes — the snapshot's
        // old 30-tick cycle and the notifications cadence are both gone;
        // notifications fire on the view-shown/refocus triggers (D5).
        for tick in 2..=30 {
            assert!(
                state.plan_tick(tick).is_empty(),
                "tick {tick}: no content specs"
            );
        }
    }

    // docs/0052 D5: the trigger gate's floor and backoff — the
    // view-shown/refocus call sites ask the state; a failure delays the
    // lane no matter how stale (docs/0036's shared delay), the pause
    // silences it entirely.
    #[test]
    fn notifications_trigger_gate_table() {
        use std::time::{Duration, Instant};

        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.auth = AuthStatus::Connected;

        let wants = |s: &GithubState| {
            s.notifications_spec_if_stale(FOCUS_MIN_AGE, Origin::BACKGROUND)
                .is_some_and(|spec| spec.kind == Kind::Notifications)
        };

        // Never landed: the first view-shown fires.
        assert!(wants(&state));

        // Landed just now: inside the 15 s floor — skip.
        state.notifications_landed = Some(Instant::now());
        assert!(!wants(&state), "fresh lane skips");

        // Aged past the floor: the refocus fires.
        state.notifications_landed = Some(Instant::now() - Duration::from_secs(20));
        assert!(wants(&state), "past the 15 s floor");

        // Backoff beats staleness: a failed lane waits out the shared
        // delay even aged and stale.
        state.notifications_fail_streak = 1;
        state.notifications_attempted_at = Some(Instant::now());
        assert!(!wants(&state), "a failed lane waits out the backoff");
        state.notifications_attempted_at = Some(Instant::now() - Duration::from_secs(61));
        assert!(wants(&state), "past the streak-1 delay the lane re-fires");

        // The rate-limit pause silences the trigger (docs/0036 D4).
        state.rate_limited_until = Some(u64::MAX);
        assert!(!wants(&state), "the pause gates the trigger");
    }

    // docs/0038 D7: a 304 counts as success in the lane's books — the
    // streak zeroes and (via the last-known restore) the landed clock
    // advances.
    #[test]
    fn notifications_304_resets_backoff_and_lands() {
        use std::time::Instant;

        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.auth = AuthStatus::Connected;
        // The 304 restore re-applies the last-known blob.
        state.cache.put("notifications", &serde_json::json!([]));
        state.notifications_fail_streak = 2;
        state.notifications_landed = Some(Instant::now() - std::time::Duration::from_secs(600));

        state.apply(SpecResult {
            kind: Kind::Notifications,
            ident: None,
            status: 304,
            etag: Some("e9".into()),
            rate: None,
            body: None,
            network: None,
            cursor: None,
        });
        assert_eq!(state.notifications_fail_streak, 0, "304 is success");
        assert!(
            state
                .notifications_landed
                .is_some_and(|t| t.elapsed().as_secs() < 5),
            "the restore path lands the clock"
        );
    }

    // docs/0038 D4: the page-owned REST kinds store their etags — the
    // page builders attach them on the next spec (when the lane holds
    // data), so the engaged tier's polls revalidate for free.
    #[test]
    fn page_owned_rest_kinds_store_etags() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.auth = AuthStatus::Connected;

        for (kind, ident) in [
            (Kind::ThreadDetail, Some("th1".to_string())),
            (Kind::PrFiles, Some("o/r#9".to_string())),
        ] {
            state.apply(SpecResult {
                kind,
                ident,
                status: 200,
                etag: Some("W/\"e1\"".into()),
                rate: None,
                // Page-owned: the 200 body is parsed page-side, never
                // here — but a body must be present (apply returns
                // before the etag bookkeeping otherwise).
                body: Some("{}".into()),
                network: None,
                cursor: None,
            });
        }
        assert_eq!(
            state.etag_for(Kind::ThreadDetail, Some("th1")).as_deref(),
            Some("W/\"e1\"")
        );
        assert_eq!(
            state.etag_for(Kind::PrFiles, Some("o/r#9")).as_deref(),
            Some("W/\"e1\"")
        );
    }

    #[test]
    fn keychain_launch_fires_inventory_on_first_tick() {
        // Regression (owner report): a launch with an existing keychain
        // token must not wait — the bootstrap arm fetches the session
        // inventory + notifications on the first tick (docs/0051 D6).
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        assert_eq!(state.auth, AuthStatus::Connected);
        assert!(state.needs_bootstrap);
        let specs = state.plan_tick(1);
        assert!(specs.iter().any(|s| s.kind == Kind::SessionInventory));
    }

    #[test]
    fn view_specs_start_from_top_and_identify_their_lane() {
        // docs/0051 D2/D3/D9: the view queries page to exhaustion per
        // run — no continuation cursor survives between runs; idents
        // feed the docs/0031 inflight dedup.
        let state = make_state(Arc::new(MockHttp::new(vec![])));
        let inv = state.session_inventory_spec(Origin::BACKGROUND);
        assert_eq!(inv.kind, Kind::SessionInventory);
        assert_eq!(inv.ident.as_deref(), Some("session"));
        assert!(inv.variables.unwrap()["cursor"].is_null());

        let card = state.card_totals_spec(Origin::BACKGROUND);
        assert_eq!(card.kind, Kind::CardTotals);
        assert_eq!(card.ident.as_deref(), Some("card-totals"));
        assert!(card.variables.unwrap()["cursor"].is_null());
    }

    // --- Home lane (docs/0047) ------------------------------------------

    #[test]
    fn home_open_spec_encodes_scope_query_and_ident() {
        let state = make_state(Arc::new(MockHttp::new(vec![])));

        let org = state.home_open_spec("acme", HomeScope::Org, Origin::BACKGROUND);
        assert_eq!(org.kind, Kind::AccountPullRequests);
        assert_eq!(org.ident.as_deref(), Some("org:acme"));
        assert!(org.etag.is_none(), "GraphQL never etags");
        let vars = org.variables.clone().unwrap();
        assert_eq!(
            vars["query"], "is:pr is:open sort:updated-desc org:acme",
            "the account qualifier + verified sort ride the query"
        );
        assert!(vars["after"].is_null(), "page 1 carries no cursor");

        let user = state.home_open_spec("me", HomeScope::User, Origin::BACKGROUND);
        assert_eq!(user.ident.as_deref(), Some("user:me"));
        assert_eq!(
            user.variables.unwrap()["query"],
            "is:pr is:open sort:updated-desc user:me"
        );
    }

    #[test]
    fn home_more_spec_carries_cursor_for_the_run_arm() {
        let state = make_state(Arc::new(MockHttp::new(vec![])));
        let spec = state.home_more_spec("acme", HomeScope::Org, "c9");
        let vars = spec.variables.clone().unwrap();
        assert_eq!(vars["after"], "c9");
        assert_eq!(vars["query"], "is:pr is:open sort:updated-desc org:acme");

        // The run arm lifts `$after` into SpecResult::cursor — apply's
        // append-vs-replace signal (docs/0047 D2).
        let body = r#"{"data":{"search":{"issueCount":0,"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}}"#;
        let http = Arc::new(MockHttp::new(vec![gql_ok(body)]));
        let result = run_home_prs(&*http, &spec);
        assert_eq!(result.cursor.as_deref(), Some("c9"));
        assert_eq!(result.status, 200);

        // Page 1 reports no continuation.
        let spec = state.home_open_spec("acme", HomeScope::Org, Origin::BACKGROUND);
        let http = Arc::new(MockHttp::new(vec![gql_ok(body)]));
        let result = run_home_prs(&*http, &spec);
        assert!(result.cursor.is_none());
    }

    #[test]
    fn home_refresh_specs_obey_clock_and_backoff() {
        let state = make_state(Arc::new(MockHttp::new(vec![])));

        // Never landed → the beat fires the first page.
        assert!(
            state
                .home_refresh_specs("acme", HomeScope::Org, Duration::ZERO, Origin::BACKGROUND)
                .is_some()
        );

        // Just landed → fresh, no-op at a real min_age.
        let mut fresh = make_state(Arc::new(MockHttp::new(vec![])));
        fresh.home_landed = Some(Instant::now());
        assert!(
            fresh
                .home_refresh_specs(
                    "acme",
                    HomeScope::Org,
                    Duration::from_secs(60),
                    Origin::BACKGROUND
                )
                .is_none()
        );

        // Failed → the docs/0036 backoff gates the retry.
        let mut failed = make_state(Arc::new(MockHttp::new(vec![])));
        failed.bump_home_backoff(false);
        assert!(
            failed
                .home_refresh_specs("acme", HomeScope::Org, Duration::ZERO, Origin::BACKGROUND)
                .is_none()
        );
        failed.bump_home_backoff(true);
        assert!(
            failed
                .home_refresh_specs("acme", HomeScope::Org, Duration::ZERO, Origin::BACKGROUND)
                .is_some()
        );
    }

    #[test]
    fn apply_home_prs_lands_and_appends() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        let page1 = r#"{"data":{"search":{"issueCount":5,"pageInfo":{"hasNextPage":true,"endCursor":"c1"},"nodes":[{"state":"OPEN","number":1,"title":"a","repository":{"nameWithOwner":"acme/one"}}]}}}"#;
        let page2 = r#"{"data":{"search":{"issueCount":5,"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{"state":"OPEN","number":2,"title":"b","repository":{"nameWithOwner":"acme/two"}}]}}}"#;

        // Page 1 replaces.
        state.apply(SpecResult {
            kind: Kind::AccountPullRequests,
            ident: Some("org:acme".into()),
            status: 200,
            etag: None,
            rate: None,
            body: Some(page1.into()),
            network: None,
            cursor: None,
        });
        let entry = state
            .home_lanes
            .get(&("acme".into(), HomeScope::Org))
            .unwrap();
        assert_eq!(entry.order.len(), 1);
        assert_eq!(entry.total_count, 5);
        assert!(!entry.complete && entry.cursor.as_deref() == Some("c1"));
        assert!(state.home_error.is_none() && state.home_landed.is_some());

        // Load more appends — page-1 rows survive.
        state.apply(SpecResult {
            kind: Kind::AccountPullRequests,
            ident: Some("org:acme".into()),
            status: 200,
            etag: None,
            rate: None,
            body: Some(page2.into()),
            network: None,
            cursor: Some("c1".into()),
        });
        let entry = state
            .home_lanes
            .get(&("acme".into(), HomeScope::Org))
            .unwrap();
        assert_eq!(entry.order.len(), 2, "appended");
        assert!(entry.complete);
        // docs/0073 D3 — the derived list serves the rows in server
        // order from the documents.
        let view = state.lane_view("acme", HomeScope::Org).unwrap();
        assert_eq!(view.rows.len(), 2);
        assert_eq!(view.rows[0].key, "acme/one#1");
        assert_eq!(view.rows[1].key, "acme/two#2");

        // A failure books the lane: stale snapshot stays, error recorded,
        // backoff armed — the docs/0047 D3 retry state.
        state.apply(SpecResult {
            kind: Kind::AccountPullRequests,
            ident: Some("org:acme".into()),
            status: 0,
            etag: None,
            rate: None,
            body: None,
            network: Some("down".into()),
            cursor: None,
        });
        assert_eq!(
            state
                .home_lanes
                .get(&("acme".into(), HomeScope::Org))
                .unwrap()
                .order
                .len(),
            2
        );
        assert_eq!(state.home_error.as_deref(), Some("down"));
        assert!(state.home_fail_streak > 0);
    }

    // --- Snapshot run + apply ------------------------------------------

    #[test]
    fn run_snapshot_follows_cursor_and_stitches() {
        let http = Arc::new(MockHttp::new(vec![
            gql_ok(&gql_page(true, "c2", &repo_node("o/a", 1, &pr_node(1)))),
            gql_ok(&gql_page(false, "unused", &repo_node("o/b", 0, ""))),
        ]));
        let state = make_state(http.clone());
        let spec = state.session_inventory_spec(Origin::BACKGROUND);

        let result = run_spec(&*http, &test_recorder(), &spec, std::time::Instant::now());
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
    fn run_paged_view_caps_at_ten_pages_and_returns_cursor() {
        // docs/0051 D9: exhaustion with a hard 10-page cap; the surviving
        // cursor marks the run partial.
        let page = || gql_ok(&gql_page(true, "cx", &repo_node("o/a", 0, "")));
        let pages: Vec<_> = (0..11).map(|_| page()).collect();
        let http = Arc::new(MockHttp::new(pages));
        let state = make_state(http.clone());

        let result = run_spec(
            &*http,
            &test_recorder(),
            &state.session_inventory_spec(Origin::BACKGROUND),
            std::time::Instant::now(),
        );
        assert_eq!(http.recorded().len(), gql::VIEW_MAX_PAGES as usize);
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
        state.apply(inventory_result(&body, None));

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

        // Persisted: the cache round-trips through snapshot-v3.
        let loaded: gql::SnapshotCache = state
            .cache
            .load(gql::SNAPSHOT_CACHE_KEY)
            .expect("snapshot persisted");
        assert_eq!(loaded.schema, 3);
        assert_eq!(loaded.repos.len(), 2);
        assert_eq!(loaded.user.as_ref().unwrap().login, "noosxe");
    }

    #[test]
    fn snapshot_v3_bump_ignores_old_caches_and_roundtrips_archived() {
        // #275: the key bump (snapshot-v2 → v3) means pre-`is_archived`
        // caches are ignored wholesale, and archived repos survive the
        // round-trip with the flag set.
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.auth = AuthStatus::Connected;

        let archived = repo_node("o/arch", 0, "").replace(
            "\"isPrivate\":false",
            "\"isPrivate\":false,\"isArchived\":true",
        );
        let body = gql_page(
            false,
            "unused",
            &format!("{},{}", archived, repo_node("o/live", 0, "")),
        );
        state.apply(inventory_result(&body, None));

        let loaded: gql::SnapshotCache = state
            .cache
            .load(gql::SNAPSHOT_CACHE_KEY)
            .expect("snapshot persisted under the v3 key");
        assert_eq!(loaded.schema, 3);
        let by_name = |k: &str| {
            loaded
                .repos
                .iter()
                .find(|r| r.full_name == k)
                .expect("repo in snapshot")
        };
        assert!(by_name("o/arch").is_archived);
        assert!(!by_name("o/live").is_archived);
    }

    #[test]
    fn apply_snapshot_capped_run_keeps_earlier_repos() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.repos = vec![repo("o/a"), repo("o/b")];

        let body = gql_page(true, "c2", &repo_node("o/c", 0, ""));
        state.apply(inventory_result(&body, Some("c2")));

        // Continuation batch overlays; earlier pages survive.
        let mut names: Vec<&str> = state.repos.iter().map(|r| r.full_name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["o/a", "o/b", "o/c"]);
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
        state.apply(inventory_result(&body, None));
        assert_eq!(state.repos.len(), 1);
        assert_eq!(state.repos[0].full_name, "o/a");
    }

    #[test]
    fn apply_snapshot_data_null_keeps_stale() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.repos = vec![repo("o/a")];
        state.apply(inventory_result(
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
        state.apply(inventory_result(body, None));
        assert_eq!(state.rate_limited_until, Some(1_893_456_000));
        assert!(state.plan_tick(5).is_empty());
    }

    // --- Repo-scoped lanes (docs/0077) ---------------------------------

    #[test]
    fn repo_lane_spec_shape() {
        let state = make_state(Arc::new(MockHttp::new(vec![])));
        // The pulls lane: one repo-scoped GraphQL spec.
        let spec = state.repo_lane_spec("o/r", RepoLane::Pulls, Origin::BACKGROUND);
        assert_eq!(spec.kind, Kind::RepoPulls);
        assert_eq!(spec.variables.as_ref().unwrap()["owner"], "o");
        assert_eq!(spec.variables.as_ref().unwrap()["name"], "r");
        // The issues lane: same shape, its own kind.
        let spec = state.repo_lane_spec("o/r", RepoLane::Issues, Origin::BACKGROUND);
        assert_eq!(spec.kind, Kind::RepoIssues);
        // The notifications lane: the ident-keyed REST call.
        let spec = state.repo_lane_spec("o/r", RepoLane::Notifications, Origin::BACKGROUND);
        assert_eq!(spec.kind, Kind::RepoNotifications);
        assert_eq!(spec.ident.as_deref(), Some("o/r"));
        assert!(spec.url.contains("/repos/o/r/notifications"));
    }

    #[test]
    fn repo_pulls_landing_replaces_only_prs() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.repos = vec![repo("o/a"), repo("o/r")];

        let result = SpecResult {
            kind: Kind::RepoPulls,
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
        // docs/0077 D4: the lane clock stamps.
        assert!(
            state
                .rows_landed
                .contains_key(&("o/r".to_string(), RepoLane::Pulls))
        );
    }

    #[test]
    fn repo_issues_landing_replaces_only_issues() {
        // docs/0077 D2: the issues lane lands its collection and leaves
        // the held PR rows alone — serde defaults the absent key away.
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.repos = vec![repo("o/r")];
        state.repos[0].prs.total_count = 5;

        let body = r#"{"data":{"repository":{"databaseId":7,"name":"r","nameWithOwner":"o/r",
            "owner":{"login":"o"},
            "issues":{"totalCount":2,"nodes":[
                {"databaseId":21,"number":9,"title":"bug: x","author":null,
                 "updatedAt":"u","url":"u1"}]}}}}"#;
        state.apply(SpecResult {
            kind: Kind::RepoIssues,
            ident: Some("o/r".into()),
            status: 200,
            etag: None,
            rate: None,
            body: Some(body.into()),
            network: None,
            cursor: None,
        });

        let r = state.repos.iter().find(|r| r.full_name == "o/r").unwrap();
        assert_eq!(r.issues.total_count, 2);
        assert_eq!(r.issues.nodes[0].key, "o/r#9");
        assert_eq!(r.prs.total_count, 5, "held PR rows untouched");
        assert!(
            state
                .rows_landed
                .contains_key(&("o/r".to_string(), RepoLane::Issues))
        );
    }

    #[test]
    fn repo_notifications_landing_stamps_its_lane() {
        // docs/0077 D4: the Notifications tab's own clock.
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.apply_repo_notifications("o/r", vec![]);
        assert!(
            state
                .rows_landed
                .contains_key(&("o/r".to_string(), RepoLane::Notifications))
        );
    }

    #[test]
    fn fetch_backoff_policy_table() {
        // docs/0036 D2: streak -> delay, stale-at->=.
        use dashboard_core::freshness::{BACKOFF_CAP, backoff_delay};
        assert_eq!(backoff_delay(1), Duration::from_secs(60));
        assert_eq!(backoff_delay(2), Duration::from_secs(120));
        assert_eq!(backoff_delay(3), Duration::from_secs(240));
        assert_eq!(backoff_delay(4), Duration::from_secs(480));
        assert_eq!(backoff_delay(5), BACKOFF_CAP);
        assert_eq!(backoff_delay(50), BACKOFF_CAP);

        let mut f: Fetch<u8> = Fetch::new();
        assert!(f.backoff_elapsed(), "never-dispatched: no clock to wait on");

        f.begin();
        assert!(f.backoff_elapsed(), "a healthy lane (streak 0) never waits");
        f.fail("probe".into());
        assert!(
            !f.backoff_elapsed(),
            "a fresh attempt after a failure waits"
        );
        f.attempted_at = Some(Instant::now() - backoff_delay(1));
        assert!(f.backoff_elapsed(), "stale-at->=: elapsed delay re-fires");

        // The second failure doubles the wait — and the clock only
        // moves at `begin()`, so the still-backdated attempt is 60s
        // old against a 120s delay.
        f.fail("probe".into());
        assert!(!f.backoff_elapsed());
        f.land(1);
        assert!(f.backoff_elapsed(), "any land resets the streak");
        assert_eq!(f.fail_streak, 0);
    }

    #[test]
    fn landed_clock_gate_and_age() {
        use std::time::{Duration, Instant};

        use dashboard_core::freshness::Freshness;

        // Never landed: refreshable (the 0035 gate) but no label age
        // (0033 D2's honest blank).
        let clock = LandedClock(None);
        assert!(clock.wants_refresh(Duration::ZERO));
        assert_eq!(clock.age(), None);

        // Landed: stale-at->= on both the gate and the label clock.
        let clock = LandedClock(Some(Instant::now() - Duration::from_secs(60)));
        assert!(!clock.wants_refresh(Duration::from_secs(61)));
        assert!(clock.wants_refresh(Duration::from_secs(60)));
        assert_eq!(clock.age().unwrap().as_secs(), 60);
        assert!(!clock.is_loading());
    }

    #[test]
    fn repo_rows_backoff_gates_and_resets() {
        use std::time::Instant;

        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.auth = AuthStatus::Connected;

        // A failed lane bumps the streak; a fresh attempt gates the
        // lane even though the rows never landed (never-landed would
        // otherwise always be refreshable).
        state.bump_rows_backoff(false);
        assert_eq!(state.rows_fail_streak, 1);
        assert!(
            state
                .repo_lane_refresh_specs("o/r", RepoLane::Pulls, Duration::ZERO, Origin::BACKGROUND)
                .is_none()
        );

        // Success resets.
        state.bump_rows_backoff(true);
        assert_eq!(state.rows_fail_streak, 0);
        assert!(
            state
                .repo_lane_refresh_specs("o/r", RepoLane::Pulls, Duration::ZERO, Origin::BACKGROUND)
                .is_some()
        );

        // Backoff elapsed: stale-at->= (docs/0032's boundary). Streak 2
        // waits 120s.
        state.bump_rows_backoff(false);
        state.bump_rows_backoff(false);
        state.rows_attempted_at = Some(Instant::now() - Duration::from_secs(119));
        assert!(
            state
                .repo_lane_refresh_specs("o/r", RepoLane::Pulls, Duration::ZERO, Origin::BACKGROUND)
                .is_none()
        );
        state.rows_attempted_at = Some(Instant::now() - Duration::from_secs(120));
        assert!(
            state
                .repo_lane_refresh_specs("o/r", RepoLane::Pulls, Duration::ZERO, Origin::BACKGROUND)
                .is_some()
        );
    }

    /// docs/0077 D3/D4 (amending docs/0035 D1/D2): the lane's own clock
    /// gates its refresh — `repo_lane_refresh_specs` returns the lane's
    /// single spec on the stale-at-`>=` boundary (docs/0032), and a
    /// landing on one lane never freshens a sibling.
    #[test]
    fn repo_lane_refresh_specs_gate_on_landed_age() {
        use std::time::Instant;

        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.auth = AuthStatus::Connected;

        // Never landed this session → refreshable, one spec.
        let spec = state
            .repo_lane_refresh_specs(
                "o/r",
                RepoLane::Pulls,
                Duration::from_secs(60),
                Origin::BACKGROUND,
            )
            .expect("never-landed is refreshable");
        assert_eq!(spec.kind, Kind::RepoPulls);

        // A landed pulls lane freshens itself only — the beat on the
        // pulls lane is a no-op; the issues lane stays refreshable.
        state.apply(SpecResult {
            kind: Kind::RepoPulls,
            ident: Some("o/r".into()),
            status: 200,
            etag: None,
            rate: None,
            body: Some(format!(
                r#"{{"data":{{"repository":{}}}}}"#,
                repo_node("o/r", 3, "")
            )),
            network: None,
            cursor: None,
        });
        assert!(
            state
                .repo_lane_refresh_specs(
                    "o/r",
                    RepoLane::Pulls,
                    Duration::from_secs(60),
                    Origin::BACKGROUND
                )
                .is_none(),
            "just-landed pulls lane is fresh"
        );
        assert!(
            state
                .repo_lane_refresh_specs(
                    "o/r",
                    RepoLane::Issues,
                    Duration::from_secs(60),
                    Origin::BACKGROUND
                )
                .is_some(),
            "the sibling lane never inherits the clock"
        );

        // A repo the snapshot didn't carry stays refreshable.
        assert!(
            state
                .repo_lane_refresh_specs(
                    "o/other",
                    RepoLane::Pulls,
                    Duration::from_secs(60),
                    Origin::BACKGROUND
                )
                .is_some(),
            "a repo the page didn't carry stays refreshable"
        );

        // Backdated to exactly min_age → stale at the `>=` boundary
        // (docs/0032 semantics).
        state.rows_landed.insert(
            ("o/r".to_string(), RepoLane::Pulls),
            Instant::now() - Duration::from_secs(60),
        );
        assert!(
            state
                .repo_lane_refresh_specs(
                    "o/r",
                    RepoLane::Pulls,
                    Duration::from_secs(60),
                    Origin::BACKGROUND
                )
                .is_some(),
            "stale at >="
        );
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
        state.apply(inventory_result(&body, None));

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
            kind: Kind::RepoPulls,
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
            kind: Kind::RepoPulls,
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
            kind: Kind::RepoPulls,
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
                origin: Origin::BACKGROUND,
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

        let spec = state.spec(
            Kind::Notifications,
            api::notifications_url(1),
            Some(2),
            Origin::BACKGROUND,
        );
        state.apply(run_spec(
            &*http,
            &test_recorder(),
            &spec,
            std::time::Instant::now(),
        ));

        assert_eq!(state.notifications.len(), 1);
        assert_eq!(state.etags.get("notifications").unwrap(), "\"etag-1\"");
        // The next conditional fetch carries If-None-Match.
        let spec2 = state.spec(
            Kind::Notifications,
            api::notifications_url(1),
            Some(2),
            Origin::BACKGROUND,
        );
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

        let spec = state.spec(
            Kind::Notifications,
            api::notifications_url(1),
            Some(2),
            Origin::BACKGROUND,
        );
        state.apply(run_spec(
            &*http,
            &test_recorder(),
            &spec,
            std::time::Instant::now(),
        ));
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
        let mut first = make_state_at(http.clone(), cache.clone(), DraftStore::at_tmp("svc"));
        first.auth = AuthStatus::Connected;
        first.selected = Some("o/r".into());
        let spec = first.spec(
            Kind::Notifications,
            api::notifications_url(1),
            Some(2),
            Origin::BACKGROUND,
        );
        first.apply(run_spec(
            &*http,
            &test_recorder(),
            &spec,
            std::time::Instant::now(),
        ));
        assert_eq!(first.notifications.len(), 1);

        let mut second = make_state_at(
            Arc::new(MockHttp::new(vec![])),
            cache,
            DraftStore::at_tmp("svc"),
        );
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
        let mut first = make_state_at(http.clone(), cache.clone(), DraftStore::at_tmp("svc"));
        first.auth = AuthStatus::Connected;
        let spec = first.spec_ident(
            Kind::RepoNotifications,
            Some("o/r"),
            api::repo_notifications_url("o", "r", 1),
            Some(2),
            Origin::BACKGROUND,
        );
        first.apply(run_spec(
            &*http,
            &test_recorder(),
            &spec,
            std::time::Instant::now(),
        ));
        assert_eq!(first.repo_notifications.get("o/r").unwrap().len(), 1);

        let mut second = make_state_at(
            Arc::new(MockHttp::new(vec![])),
            cache,
            DraftStore::at_tmp("svc"),
        );
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

        let spec = state.spec(
            Kind::Notifications,
            api::notifications_url(1),
            Some(2),
            Origin::BACKGROUND,
        );
        state.apply(run_spec(
            &*http,
            &test_recorder(),
            &spec,
            std::time::Instant::now(),
        ));
        assert_eq!(state.auth, AuthStatus::TokenRejected);
        assert!(state.plan_tick(1).is_empty());
    }

    #[test]
    fn apply_drops_in_flight_results_after_sign_out() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.auth = AuthStatus::Connected;
        state.sign_out();

        // A late 200 snapshot must not resurrect user/repos — and via
        // persist_snapshot must not recreate the just-deleted cache dir.
        let body = gql_page(false, "unused", &repo_node("o/a", 1, &pr_node(1)));
        state.apply(inventory_result(&body, None));
        assert!(state.user.is_none());
        assert!(state.repos.is_empty());
        assert!(!state.cache.root.exists(), "cache dir must stay deleted");

        // A late 401 must not flip a signed-out session into the
        // reconnect state either.
        state.apply(SpecResult {
            kind: Kind::Notifications,
            ident: None,
            status: 401,
            etag: None,
            rate: None,
            body: None,
            network: None,
            cursor: None,
        });
        assert_eq!(state.auth, AuthStatus::Disconnected);
    }

    #[test]
    fn apply_still_takes_results_while_token_rejected() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.auth = AuthStatus::TokenRejected;

        // Still signed in (rejected credential, docs/0005 Q9): in-flight
        // 200s keep painting — the guard must not over-drop.
        let body = gql_page(false, "unused", &repo_node("o/a", 1, &pr_node(1)));
        state.apply(inventory_result(&body, None));
        assert_eq!(state.user.as_ref().unwrap().login, "noosxe");
        assert_eq!(state.repos.len(), 1);
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

        let spec = state.spec(
            Kind::Notifications,
            api::notifications_url(1),
            None,
            Origin::BACKGROUND,
        );
        state.apply(run_spec(
            &*http,
            &test_recorder(),
            &spec,
            std::time::Instant::now(),
        ));
        assert_eq!(state.rate_limited_until, Some(9_999_999_999));
        assert!(state.plan_tick(1).is_empty());
    }

    // --- Card + persistence ----------------------------------------------

    #[test]
    fn card_counts_read_the_totals_lane_and_clamp() {
        // docs/0051 D2: PR/issue numbers come from the CardTotals lane,
        // not from repo summaries; `None` renders the card skeleton.
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.notifications =
            serde_json::from_str::<Vec<Notification>>(one_notification()).unwrap();

        // No lane yet — skeleton, but notifications still count.
        assert_eq!(state.card_counts(), (None, 1));

        state.card_totals = Some(CardTotals {
            open_prs: 254,
            open_issues: 307,
            landed: Instant::now(),
        });
        assert_eq!(state.card_counts(), (Some((254, 307)), 1));

        // Clamp parity: a negative sum can't surface.
        state.card_totals = Some(CardTotals {
            open_prs: 0,
            open_issues: -400,
            landed: Instant::now(),
        });
        assert_eq!(state.card_counts().0, Some((0, 0)));
    }

    #[test]
    fn snapshot_v2_seeds_a_new_state() {
        let cache = GithubCache::at_tmp("seed");
        let mut state = make_state_at(
            Arc::new(MockHttp::new(vec![])),
            cache.clone(),
            DraftStore::at_tmp("svc"),
        );
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
        let seeded = make_state_at(
            Arc::new(MockHttp::new(vec![])),
            cache,
            DraftStore::at_tmp("svc"),
        );
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
    fn needs_refresh_policy_table() {
        // docs/0032 D2: loading never; Idle never (no surprise fetch of
        // an unopened lane); Ready by age; failed retries at any age.
        let fresh = Duration::from_secs(3600);
        let stale = Duration::ZERO;

        let mut loading = Fetch::<u8>::new();
        loading.begin();
        assert!(!loading.needs_refresh(stale));

        let idle = Fetch::<u8>::new();
        assert!(
            !idle.needs_refresh(stale),
            "Idle lanes are never beat-eligible"
        );

        let ready = Fetch::ready(1u8);
        assert!(
            !ready.needs_refresh(fresh),
            "fresh data is never re-fetched"
        );
        assert!(ready.needs_refresh(stale), "age >= min_age dispatches");

        let mut failed = Fetch::<u8>::new();
        failed.fail("boom".into());
        assert!(failed.needs_refresh(fresh), "Failed lanes self-heal");
    }

    #[test]
    fn stale_chip_buckets() {
        use crate::page::stale_chip_label;
        use std::time::Duration;
        // Nothing seeded, or everything under the threshold — no chip.
        assert_eq!(stale_chip_label(None, None), None);
        assert_eq!(
            stale_chip_label(Some(Duration::from_secs(29 * 60)), None),
            None,
            "just under STALE_AGE stays unmarked"
        );
        // The older age wins; the boundary itself marks (>=).
        assert_eq!(
            stale_chip_label(
                Some(Duration::from_secs(10)),
                Some(Duration::from_secs(30 * 60))
            )
            .as_deref(),
            Some("last synced 30m ago — reconnect to update")
        );
    }

    #[test]
    fn cache_age_reads_blob_mtime() {
        use std::time::SystemTime;
        let cache = GithubCache::at_tmp("age");
        assert_eq!(cache.age("notifications"), None, "no blob, no age");

        cache.put("notifications", &serde_json::json!([]));
        let fresh = cache.age("notifications").expect("blob exists");
        assert!(fresh.as_secs() < 60, "a just-written blob is fresh");

        // Backdate the mtime (std File::set_times — no new dependency,
        // docs/0034 D1).
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(cache.last_known_path("notifications"))
            .unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(SystemTime::now() - Duration::from_secs(2 * 3600)),
        )
        .unwrap();
        drop(file);
        let age = cache.age("notifications").unwrap();
        assert!(
            age.as_secs() >= 2 * 3600 - 5 && age.as_secs() <= 2 * 3600 + 60,
            "backdated mtime reads as a ~2h age, got {age:?}"
        );
    }

    #[test]
    fn disk_seeded_ages_land_and_a_landed_apply_clears() {
        use std::time::SystemTime;

        fn backdate(cache: &GithubCache, rel: &str) {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(cache.last_known_path(rel))
                .unwrap();
            file.set_times(
                std::fs::FileTimes::new()
                    .set_modified(SystemTime::now() - Duration::from_secs(2 * 3600)),
            )
            .unwrap();
        }

        // Connected state (the mock store holds a token): both ages seed
        // from the backdated blobs (docs/0034 D2).
        let cache = GithubCache::at_tmp("seed");
        cache.put("notifications", &serde_json::json!([]));
        // A blob that fails to parse would be quarantined (renamed away)
        // by new()'s cache.load — the seed must survive it (0004 store).
        let snapshot = gql::SnapshotCache {
            schema: 3,
            user: None,
            orgs: vec![],
            repos: vec![],
        };
        cache.put(gql::SNAPSHOT_CACHE_KEY, &snapshot);
        backdate(&cache, "notifications");
        backdate(&cache, gql::SNAPSHOT_CACHE_KEY);
        let mut state = make_state_at(
            Arc::new(MockHttp::new(vec![])),
            cache,
            DraftStore::at_tmp("svc"),
        );
        assert!(state.snapshot_age.unwrap().as_secs() >= 2 * 3600 - 5);
        assert!(state.notifications_age.unwrap().as_secs() >= 2 * 3600 - 5);

        // docs/0051 D2: the card's freshness is the CardTotals lane — an
        // inventory landing alone does not clear the seeded card age.
        state.apply(inventory_result(
            &gql_page(false, "unused", &repo_node("o/a", 0, "")),
            None,
        ));
        assert!(
            state.snapshot_age.is_some(),
            "inventory alone is not the card's lane"
        );
        state.apply(SpecResult {
            kind: Kind::CardTotals,
            ident: None,
            status: 200,
            etag: None,
            rate: None,
            body: Some(
                r#"{"data":{"viewer":{"repositories":{"nodes":[
                    {"pullRequests":{"totalCount":1},"issues":{"totalCount":0}}
                ]}}}}"#
                    .to_string(),
            ),
            network: None,
            cursor: None,
        });
        assert_eq!(
            state.snapshot_age, None,
            "a landed card lane owns freshness"
        );
        assert!(state.notifications_age.is_some());

        // A landed notifications apply clears the other.
        state.apply(SpecResult {
            kind: Kind::Notifications,
            ident: None,
            status: 200,
            etag: None,
            rate: None,
            body: Some(one_notification().to_string()),
            network: None,
            cursor: None,
        });
        assert_eq!(state.notifications_age, None);
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
            DraftStore::at_tmp("signout"),
        );
        state.auth = AuthStatus::Connected;
        state.repos = vec![repo("o/r")];
        state.access_expires_at = Some(1_799_999_999);
        state.pending_refresh = true;
        // docs/0072 D1: the document store is session state too.
        state.land_summary("o/r", &store_row(482));

        state.sign_out();
        assert_eq!(state.auth, AuthStatus::Disconnected);
        assert!(state.repos.is_empty());
        assert!(shared.read().unwrap().is_none());
        // docs/0011: the refresh slot and scheduling fields go too.
        assert!(refresh.read().unwrap().is_none());
        assert_eq!(state.access_expires_at, None);
        assert!(!state.pending_refresh);
        // docs/0028: the gate spends the credential until the next connect.
        assert!(state.gate.exhausted());
        assert!(state.pulls.is_empty());
        assert_eq!(state.next_pending_token, 0);
    }

    // --- PR document store (docs/0072) --------------------------------

    fn store_row(number: i64) -> gql::PullRequest {
        serde_json::from_value(serde_json::json!({
            "key": format!("acme/one#{number}"),
            "database_id": null,
            "number": number,
            "title": format!("PR {number}"),
            "draft": false,
            "state": "OPEN",
            "author": null,
            "updated_at": null,
            "url": format!("https://github.com/acme/one/pull/{number}"),
            "status": null,
        }))
        .expect("row parses")
    }

    fn store_issue(number: i64) -> gql::Issue {
        serde_json::from_value(serde_json::json!({
            "key": format!("acme/one#{number}"),
            "database_id": null,
            "number": number,
            "title": format!("Issue {number}"),
            "state": null,
            "author": null,
            "updated_at": null,
            "url": format!("https://github.com/acme/one/issues/{number}"),
            "labels": null,
        }))
        .expect("issue row parses")
    }

    fn store_issue_detail() -> gql::IssueDetail {
        let value = serde_json::json!({
            "issue": {
                "id": "I_12",
                "number": 12,
                "title": "Issue 12",
                "url": "u",
                "state": "OPEN",
                "createdAt": "2026-09-01T00:00:00Z",
                "comments": { "totalCount": 0, "nodes": [] },
                "timelineItems": { "totalCount": 0, "filteredCount": 0, "nodes": [] },
            }
        });
        gql::issue_detail_from_value(&value).expect("issue detail parses")
    }

    fn store_detail(number: i64) -> gql::PrDetail {
        let value = serde_json::json!({
            "pullRequest": {
                "id": format!("PR_{number}"),
                "number": number,
                "title": format!("PR {number}"),
                "url": "u",
                "state": "OPEN",
                "isDraft": false,
                "createdAt": "2026-09-01T00:00:00Z",
            }
        });
        gql::pr_detail_from_value(&value).expect("detail parses")
    }

    #[test]
    fn pr_store_summary_refresh_keeps_detail_books() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.land_summary("acme/one", &store_row(482));
        state.land_detail("acme/one", 482, store_detail(482));
        let before = state.pr_entry("acme/one", 482).unwrap().detail_at;

        // A later home refresh lands the same document again: the summary
        // layer updates, the detail layer and its books survive (D2).
        state.land_summary("acme/one", &store_row(482));
        let entry = state.pr_entry("acme/one", 482).unwrap();
        assert!(entry.detail.is_some());
        assert_eq!(entry.detail_at, before);
        assert!(entry.books.last_error.is_none());
    }

    #[test]
    fn pr_store_pending_round_trip_patches_both_layers() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.land_summary("acme/one", &store_row(482));
        state.land_detail("acme/one", 482, store_detail(482));

        // D5: one optimistic write per document.
        let token = state.begin_pending("acme/one", 482, PendingKind::Merge);
        assert!(token.is_some());
        assert!(
            state
                .begin_pending("acme/one", 482, PendingKind::Close)
                .is_none()
        );

        // D6: a stale token never applies.
        assert!(!state.resolve_pending("acme/one", 482, token.unwrap() + 1));
        assert!(state.pr_entry("acme/one", 482).unwrap().pending.is_some());

        assert!(state.resolve_pending("acme/one", 482, token.unwrap()));
        let entry = state.pr_entry("acme/one", 482).unwrap();
        assert!(entry.pending.is_none());
        assert_eq!(entry.summary.state.as_deref(), Some("MERGED"));
        assert_eq!(
            entry.detail.as_ref().unwrap().state.as_deref(),
            Some("MERGED")
        );
        // D7: the completion toast stages with its dedupe key + source.
        let alerts = state.take_pending_alerts();
        assert_eq!(alerts.len(), 1);
        assert_eq!(
            alerts[0].dedupe_key.as_deref(),
            Some("pr-merged:acme/one#482")
        );
    }

    #[test]
    fn pr_store_fail_pending_keeps_state_and_records_inline() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.land_summary("acme/one", &store_row(482));
        let token = state
            .begin_pending("acme/one", 482, PendingKind::Merge)
            .unwrap();

        assert!(state.fail_pending("acme/one", 482, token, "not mergeable".into()));
        let entry = state.pr_entry("acme/one", 482).unwrap();
        // D4: rollback is the prior document; the failure records in its
        // inline scope (docs/0016 D5 placement).
        assert_eq!(entry.summary.state.as_deref(), Some("OPEN"));
        assert!(entry.pending.is_none());
        let error = entry.error.as_ref().unwrap();
        assert_eq!(error.scope, ErrorScope::Panel);
        assert_eq!(error.message, "not mergeable");
        // D7: the warning toast stages.
        assert_eq!(state.take_pending_alerts().len(), 1);
    }

    // Stage 3 (#554): an append-style confirmation patches no document
    // field — the toast stages, pending clears, and the cached detail's
    // freshness VOIDS so a dead page's document revalidates instead of
    // serving a pre-write doc (the SWR skip keys on a young detail_at).
    #[test]
    fn pr_store_append_confirm_patches_nothing_and_voids_detail() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.land_summary("acme/one", &store_row(482));
        state.land_detail("acme/one", 482, store_detail(482));
        assert!(state.pr_entry("acme/one", 482).unwrap().detail_at.is_some());

        let token = state
            .begin_pending("acme/one", 482, PendingKind::Comment)
            .unwrap();
        assert!(state.resolve_pending("acme/one", 482, token));

        let entry = state.pr_entry("acme/one", 482).unwrap();
        assert!(entry.pending.is_none());
        assert_eq!(entry.summary.state.as_deref(), Some("OPEN"), "no patch");
        assert!(entry.detail_at.is_none(), "the cached detail voided");
        let alerts = state.take_pending_alerts();
        assert_eq!(alerts.len(), 1, "the completion toast stages (D7)");
        assert_eq!(
            alerts[0].dedupe_key.as_deref(),
            Some("pr-commented:acme/one#482")
        );
    }

    // Stage 3 (#554): append-style failures record on the entry in the
    // scope the op had as a page-modal op — Files for the 0030-D7
    // writes (docs/0016 D5 placement, kept).
    #[test]
    fn pr_store_append_failure_records_its_documented_scope() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.land_summary("acme/one", &store_row(482));
        state.land_detail("acme/one", 482, store_detail(482));

        let token = state
            .begin_pending("acme/one", 482, PendingKind::LineComment)
            .unwrap();
        assert!(state.fail_pending("acme/one", 482, token, "nope".into()));
        let entry = state.pr_entry("acme/one", 482).unwrap();
        assert!(entry.pending.is_none());
        let error = entry.error.as_ref().unwrap();
        assert_eq!(error.scope, ErrorScope::Files);
        assert_eq!(error.message, "nope");
        assert_eq!(state.take_pending_alerts().len(), 1);
    }

    // Stage 4 (#555): the issue twins — close patches its post-state
    // into both layers (the one field the payload speaks verbatim),
    // an append-style confirmation voids the cached detail, and a
    // snapshot row landing never moves a patched state (rows carry
    // no state).
    #[test]
    fn issue_store_close_patches_both_layers_and_comment_voids() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.land_issue_summary("acme/one", &store_issue(12));
        state.land_issue_detail("acme/one", 12, store_issue_detail());
        assert!(
            state
                .issue_entry("acme/one", 12)
                .unwrap()
                .detail_at
                .is_some()
        );

        // Close patches both layers and stages the completion toast.
        let token = state
            .begin_issue_pending("acme/one", 12, IssuePendingKind::Close)
            .unwrap();
        assert!(state.resolve_issue_pending("acme/one", 12, token));
        let entry = state.issue_entry("acme/one", 12).unwrap();
        assert_eq!(entry.summary.state.as_deref(), Some("CLOSED"));
        assert_eq!(
            entry.detail.as_ref().unwrap().state.as_deref(),
            Some("CLOSED")
        );
        let alerts = state.take_pending_alerts();
        assert_eq!(
            alerts[0].dedupe_key.as_deref(),
            Some("issue-closed:acme/one#12")
        );

        // A comment confirmation patches nothing — the freshness voids
        // so the next open revalidates through the new row.
        let token = state
            .begin_issue_pending("acme/one", 12, IssuePendingKind::Comment)
            .unwrap();
        assert!(state.resolve_issue_pending("acme/one", 12, token));
        let entry = state.issue_entry("acme/one", 12).unwrap();
        assert!(entry.pending.is_none());
        assert!(entry.detail_at.is_none(), "the cached detail voided");
        assert_eq!(
            state.take_pending_alerts()[0].dedupe_key.as_deref(),
            Some("issue-commented:acme/one#12")
        );
    }

    /// make_state with a real `app_db` sink (docs/0072 D8 tests): a
    /// temp-path store, explicit so each test owns its file.
    fn make_state_db(db: Arc<SQLiteStore>) -> GithubState {
        let shared: SharedToken = Arc::new(std::sync::RwLock::new(Some("t0k".into())));
        let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
        let client: Arc<dyn Http> = Arc::new(MockHttp::new(vec![]));
        GithubState::with_recorder(
            client,
            Arc::new(MockTokenStore::with("t0k".into())),
            shared,
            refresh,
            GithubCache::at_tmp("doc-store"),
            DraftStore::at_tmp("doc-store"),
            Recorder::new(MODULE_ID, Arc::new(MemorySink::new(RECORD_CAP))),
            Some(db),
        )
    }

    fn doc_db(tag: &str) -> Arc<SQLiteStore> {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        SQLiteStore::open_at(
            std::env::temp_dir().join(format!("dash-doc-store-{tag}-{}-{n}", std::process::id())),
        )
    }

    /// docs/0072 D8 (#556): landings and confirmations write through;
    /// a state rebuilt over the same DB restores the documents with
    /// honest stamps (summary always, detail while young).
    #[test]
    fn doc_store_persists_and_a_new_state_restores() {
        let db = doc_db("roundtrip");
        let mut state = make_state_db(db.clone());
        state.land_summary("acme/one", &store_row(7));
        state.land_issue_summary("acme/one", &store_issue(12));
        state.land_issue_detail("acme/one", 12, store_issue_detail());
        // One flush per store lifetime: the quit-flush is the only
        // deterministic drain (the writer parks 1 s between wakes).
        db.flush_blocking();
        assert_eq!(db.load_documents().len(), 2, "pr + issue rows");

        let state2 = make_state_db(db.clone());
        let pr = state2.pr_entry("acme/one", 7).expect("pr restored");
        assert_eq!(pr.summary.title, "PR 7");
        assert!(pr.detail.is_none(), "no detail was ever landed");
        assert!(pr.summary_at.elapsed() < std::time::Duration::from_secs(60));
        let issue = state2.issue_entry("acme/one", 12).expect("issue restored");
        assert!(issue.detail.is_some(), "young detail restores");
        assert!(issue.detail_at.is_some());
        assert!(issue.books.last_error.is_none(), "books start clean");
        assert!(issue.pending.is_none(), "pending is memory-only");
    }

    #[test]
    fn doc_store_confirmed_close_persists_and_void_persists_summary_only() {
        let db = doc_db("mutations");
        let mut state = make_state_db(db.clone());
        state.land_issue_summary("acme/one", &store_issue(12));
        state.land_issue_detail("acme/one", 12, store_issue_detail());
        // Close patches both layers — and the persisted row.
        let token = state
            .begin_issue_pending("acme/one", 12, IssuePendingKind::Close)
            .unwrap();
        assert!(state.resolve_issue_pending("acme/one", 12, token));
        // A comment confirmation voids the detail stamp — the
        // persisted row drops the layer so a restore cannot claim
        // freshness the write invalidated.
        let token = state
            .begin_issue_pending("acme/one", 12, IssuePendingKind::Comment)
            .unwrap();
        assert!(state.resolve_issue_pending("acme/one", 12, token));
        db.flush_blocking();

        let rows = db.load_documents();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].detail_json.is_none(), "voided layer not persisted");
        assert!(rows[0].detail_at_ms.is_none());

        let state2 = make_state_db(db);
        let issue = state2.issue_entry("acme/one", 12).unwrap();
        assert_eq!(
            issue.summary.state.as_deref(),
            Some("CLOSED"),
            "the confirmed close survived the restart"
        );
        assert!(
            issue.detail.is_none() && issue.detail_at.is_none(),
            "stamp-less detail restores summary-only"
        );
    }

    #[test]
    fn doc_store_restore_drops_details_past_the_ttl() {
        let db = doc_db("ttl");
        let now = now_epoch_ms();
        let hour = RESTORE_DETAIL_TTL.as_millis() as u64;
        // A detail landed 2h ago: not worth showing on the next boot.
        db.put_document(StoredDocument {
            kind: "issue".into(),
            repo: "acme/one".into(),
            number: 12,
            summary_at_ms: now - hour - 3_600_000,
            detail_at_ms: Some(now - 2 * hour),
            summary_json: serde_json::to_string(&store_issue(12)).unwrap(),
            detail_json: Some(serde_json::to_string(&store_issue_detail()).unwrap()),
        });
        db.flush_blocking();
        let state = make_state_db(db);
        let issue = state.issue_entry("acme/one", 12).expect("summary restores");
        assert!(issue.detail.is_none(), "over-TTL detail dropped");
        assert!(issue.detail_at.is_none());
    }

    /// docs/0072 stage 4 (amended): a repo-lane Issues landing refreshes
    /// the documents' summary layer — the rows render the document, so a
    /// label added upstream must reach the row on the next landing (the
    /// Pulls lane has had this since stage 2).
    #[test]
    fn repo_lane_landing_refreshes_issue_document_labels() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        // The document was seeded when the issue page last opened: two
        // labels, detail state recorded.
        let mut seeded = store_issue(92);
        seeded.labels = Some(gql::LabelSummary {
            total_count: 2,
            nodes: vec![
                gql::LabelRef {
                    name: "data-model".into(),
                    color: Some("FBCA04".into()),
                },
                gql::LabelRef {
                    name: "deferred".into(),
                    color: Some("DDDDDD".into()),
                },
            ],
        });
        state.land_issue_summary("acme/one", &seeded);
        state.land_issue_detail("acme/one", 92, store_issue_detail());
        assert_eq!(
            state.issue_entry("acme/one", 92).unwrap().summary.state,
            Some("OPEN".into())
        );

        // Upstream grows a third label; the lane lands the fresh row.
        let body = serde_json::json!({
            "data": {
                "repository": {
                    "name": "one",
                    "nameWithOwner": "acme/one",
                    "owner": {"login": "acme"},
                    "databaseId": null,
                    "id": null,
                    "pullRequests": {"totalCount": 0, "nodes": []},
                    "issues": {
                        "totalCount": 1,
                        "nodes": [{
                            "number": 92,
                            "title": "How high a PAX door sits above the floor",
                            "url": "u",
                            "labels": {"totalCount": 3, "nodes": [
                                {"name": "data-model", "color": "FBCA04"},
                                {"name": "deferred", "color": "DDDDDD"},
                                {"name": "needs-store-visit", "color": "C2185B"}
                            ]}
                        }]
                    }
                }
            }
        });
        state.apply_repo_lane(
            Some("acme/one"),
            &body.to_string(),
            crate::service::RepoLane::Issues,
        );

        // The row derives from the document — which now carries the
        // fresh label set, while the detail-landed state survives.
        // The row derives from the document — which now carries the
        // fresh label set, while the detail-landed state survives.
        let held = state.repos.iter().find(|r| r.key == "acme/one").unwrap();
        let rows = state.repo_issue_rows(held);
        assert_eq!(rows.len(), 1);
        let labels = rows[0].labels.as_ref().expect("labels land");
        assert_eq!(labels.total_count, 3);
        let names: Vec<_> = labels.nodes.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["data-model", "deferred", "needs-store-visit"]);
        assert_eq!(
            state.issue_entry("acme/one", 92).unwrap().summary.state,
            Some("OPEN".into()),
            "the lane never moves a detail-landed state"
        );
    }

    /// docs/0072 stage 4 (amended): a detail landing propagates the
    /// fresh label set into the row summary — the detail is the
    /// freshest read the store holds, and the user has already seen
    /// its labels on the page.
    #[test]
    fn land_issue_detail_propagates_labels_into_the_row_summary() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        let mut seeded = store_issue(92);
        seeded.labels = Some(gql::LabelSummary {
            total_count: 2,
            nodes: vec![
                gql::LabelRef {
                    name: "data-model".into(),
                    color: Some("FBCA04".into()),
                },
                gql::LabelRef {
                    name: "deferred".into(),
                    color: Some("DDDDDD".into()),
                },
            ],
        });
        state.land_issue_summary("acme/one", &seeded);

        // The detail the page just fetched carries the upstream truth:
        // three labels (the query's first page, 20 wide).
        let value = serde_json::json!({
            "issue": {
                "id": "I_92",
                "number": 92,
                "title": "How high a PAX door sits above the floor",
                "url": "u",
                "state": "OPEN",
                "createdAt": "2026-09-01T00:00:00Z",
                "labels": {
                    "totalCount": 3,
                    "nodes": [
                        {"id": "L1", "name": "data-model", "color": "FBCA04"},
                        {"id": "L2", "name": "deferred", "color": "DDDDDD"},
                        {"id": "L3", "name": "needs-store-visit", "color": "C2185B"}
                    ]
                },
                "comments": {"totalCount": 0, "nodes": []},
                "timelineItems": {"totalCount": 0, "filteredCount": 0, "nodes": []}
            }
        });
        state.land_issue_detail(
            "acme/one",
            92,
            gql::issue_detail_from_value(&value).expect("detail parses"),
        );

        let summary = &state.issue_entry("acme/one", 92).unwrap().summary;
        let labels = summary.labels.as_ref().expect("labels propagate");
        assert_eq!(labels.total_count, 3);
        let names: Vec<_> = labels.nodes.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["data-model", "deferred", "needs-store-visit"]);
        assert_eq!(summary.state, Some("OPEN".into()));
    }
    #[test]
    fn doc_store_unparseable_rows_are_skipped_not_fatal() {
        let db = doc_db("garbage");
        db.put_document(StoredDocument {
            kind: "pr".into(),
            repo: "acme/one".into(),
            number: 7,
            summary_at_ms: now_epoch_ms(),
            detail_at_ms: None,
            summary_json: "{not json".into(),
            detail_json: None,
        });
        db.flush_blocking();
        let state = make_state_db(db);
        assert!(state.pr_entry("acme/one", 7).is_none(), "skipped, no panic");
    }

    #[test]
    fn doc_store_sign_out_wipes_the_persisted_documents() {
        let db = doc_db("signout");
        let mut state = make_state_db(db.clone());
        state.land_issue_summary("acme/one", &store_issue(12));
        state.sign_out();
        // Drain order: the put lands, then the sign-out's all-delete
        // clears it — one flush covers the whole arc.
        db.flush_blocking();
        assert!(db.load_documents().is_empty());
        assert!(state.issue_entry("acme/one", 12).is_none());
    }

    #[test]
    fn issue_store_summary_landing_preserves_state_and_rows_drop_closed() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.land_issue_summary("acme/one", &store_issue(12));
        state.land_issue_detail("acme/one", 12, store_issue_detail());

        // Confirm a close, then land a fresh snapshot row (stateless —
        // the query asks for open-only): the patched state survives.
        let token = state
            .begin_issue_pending("acme/one", 12, IssuePendingKind::Close)
            .unwrap();
        assert!(state.resolve_issue_pending("acme/one", 12, token));
        state.land_issue_summary("acme/one", &store_issue(12));
        let entry = state.issue_entry("acme/one", 12).unwrap();
        assert_eq!(
            entry.summary.state.as_deref(),
            Some("CLOSED"),
            "a stateless row never moves the patched state"
        );

        // The repo view's derived rows drop the closed document (the
        // #561 semantics, issue edition).
        let mut repo = repo("acme/one");
        repo.issues = gql::IssueSummary {
            total_count: 1,
            nodes: vec![store_issue(12)],
        };
        assert!(state.repo_issue_rows(&repo).is_empty());
        // Reopen brings it back through the same overlay.
        let token = state
            .begin_issue_pending("acme/one", 12, IssuePendingKind::Reopen)
            .unwrap();
        assert!(state.resolve_issue_pending("acme/one", 12, token));
        assert_eq!(state.repo_issue_rows(&repo).len(), 1);
    }

    #[test]
    fn pr_store_eviction_caps_details_and_pins() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        let old = Instant::now() - Duration::from_secs(3_600);
        for n in 0..PR_STORE_DETAIL_CAP as i64 {
            state.land_summary("acme/one", &store_row(n));
            state.land_detail("acme/one", n, store_detail(n));
            state
                .pulls
                .get_mut(&PrKey::new("acme/one", n))
                .unwrap()
                .detail_at = Some(old);
        }
        // Arm the pending op before the overflow lands: a pending entry
        // pins its detail against eviction (D6).
        assert!(
            state
                .begin_pending("acme/one", 3, PendingKind::Close)
                .is_some()
        );
        state.land_summary("acme/one", &store_row(9_001));
        state.land_detail("acme/one", 9_001, store_detail(9_001));
        state.land_summary("acme/one", &store_row(9_002));
        state.land_detail("acme/one", 9_002, store_detail(9_002));

        // Two documents overflowed the cap; both evictions take the
        // oldest-landed unpinned candidates (#0 and #1 — equal stamps
        // sort stably in key order), never the pinned #3, never the
        // just-landed fresh documents.
        assert!(state.pr_entry("acme/one", 0).unwrap().detail.is_none());
        assert!(state.pr_entry("acme/one", 1).unwrap().detail.is_none());
        assert!(state.pr_entry("acme/one", 3).unwrap().detail.is_some());
        assert!(state.pr_entry("acme/one", 9_002).unwrap().detail.is_some());
        let with_detail = state.pulls.values().filter(|e| e.detail.is_some()).count();
        assert!(with_detail <= PR_STORE_DETAIL_CAP, "cap holds");
    }

    /// The session-scoped resolver (docs/0072 D4): clean 200s confirm,
    /// GraphQL `errors` fail with the upstream message, and non-200s
    /// fail **after** forwarding to `apply` — the shared 401 gate runs
    /// exactly as for every other fetch.
    #[test]
    fn store_resolver_classifies_and_resolves() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.land_summary("acme/one", &store_row(482));
        let token = state
            .begin_pending("acme/one", 482, PendingKind::Merge)
            .unwrap();

        // Clean 200: confirmed, completion toast staged.
        let ok = SpecResult {
            kind: Kind::PrMutation(Mutation::Merge),
            ident: None,
            status: 200,
            etag: None,
            rate: None,
            body: Some("{}".into()),
            network: None,
            cursor: None,
        };
        assert!(state.apply_store_mutation_result("acme/one", 482, PendingKind::Merge, token, ok));
        let entry = state.pr_entry("acme/one", 482).unwrap();
        assert_eq!(entry.summary.state.as_deref(), Some("MERGED"));
        assert!(entry.pending.is_none());
        assert_eq!(state.take_pending_alerts().len(), 1);

        // 200 carrying GraphQL errors: refused, upstream message kept.
        let token = state
            .begin_pending("acme/one", 482, PendingKind::Merge)
            .unwrap();
        let gql_err = SpecResult {
            kind: Kind::PrMutation(Mutation::Merge),
            ident: None,
            status: 200,
            etag: None,
            rate: None,
            body: Some(
                r#"{"data":null,"errors":[{"message":"Pull request is not mergeable"}]}"#.into(),
            ),
            network: None,
            cursor: None,
        };
        assert!(!state.apply_store_mutation_result(
            "acme/one",
            482,
            PendingKind::Merge,
            token,
            gql_err
        ));
        let entry = state.pr_entry("acme/one", 482).unwrap();
        assert_eq!(
            entry.error.as_ref().unwrap().message,
            "Pull request is not mergeable"
        );

        // 401: forwarded to `apply` first (the session flips to
        // TokenRejected), then the op fails on the mapped status copy.
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.land_summary("acme/one", &store_row(482));
        state.auth = AuthStatus::Connected;
        let token = state
            .begin_pending("acme/one", 482, PendingKind::Close)
            .unwrap();
        let rejected = SpecResult {
            kind: Kind::PrMutation(Mutation::Close),
            ident: None,
            status: 401,
            etag: None,
            rate: None,
            body: None,
            network: None,
            cursor: None,
        };
        assert!(!state.apply_store_mutation_result(
            "acme/one",
            482,
            PendingKind::Close,
            token,
            rejected
        ));
        assert_eq!(state.auth, AuthStatus::TokenRejected);
        let entry = state.pr_entry("acme/one", 482).unwrap();
        assert_eq!(entry.summary.state.as_deref(), Some("OPEN"));
        assert_eq!(
            entry.error.as_ref().unwrap().message,
            "GitHub answered HTTP 401"
        );
    }

    /// docs/0072 D4: a pending op whose confirmation never landed
    /// unblocks after the max age — prior state kept, the inline surface
    /// explains, no "failed" toast (the expiry does not know the op
    /// failed), and a late confirmation is refused by its gone token.
    #[test]
    fn stale_pending_expires_without_a_lying_toast() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.land_summary("acme/one", &store_row(482));
        let token = state
            .begin_pending("acme/one", 482, PendingKind::Merge)
            .unwrap();
        state
            .pulls
            .get_mut(&PrKey::new("acme/one", 482))
            .unwrap()
            .pending
            .as_mut()
            .unwrap()
            .started_at = Instant::now() - PR_PENDING_MAX_AGE - Duration::from_secs(1);

        assert_eq!(state.expire_stale_pending(), 1);
        // A second beat finds nothing left to expire.
        assert_eq!(state.expire_stale_pending(), 0);

        let entry = state.pr_entry("acme/one", 482).unwrap();
        assert!(entry.pending.is_none());
        assert_eq!(entry.summary.state.as_deref(), Some("OPEN"));
        assert_eq!(entry.error.as_ref().unwrap().scope, ErrorScope::Panel);
        assert!(entry.error.as_ref().unwrap().message.contains("github.com"));
        assert!(state.take_pending_alerts().is_empty(), "no lying toast");

        // The late confirmation arrives into a document that moved on:
        // refused, not applied (D6).
        assert!(!state.resolve_pending("acme/one", 482, token));
    }

    /// docs/0072 D3: the checks rollup is a shared fact between the two
    /// layers — detail→summary on landing (the row's glyph is fresh
    /// immediately), summary→drift mark→stale detail (the page
    /// revalidates so both surfaces report the same status).
    #[test]
    fn rollup_is_a_shared_fact_both_ways() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        // Home lane lands the row; the page opens and a detail lands
        // with the checks finished.
        state.land_summary("acme/one", &store_row(482));
        let mut d = store_detail(482);
        d.rollup_state = Some("SUCCESS".into());
        state.land_detail("acme/one", 482, d);

        // detail → summary: the row's glyph reads the fresh layer
        // immediately, not at the next home tick.
        let entry = state.pr_entry("acme/one", 482).unwrap();
        assert_eq!(entry.summary.status.as_deref(), Some("SUCCESS"));
        assert!(!entry.rollup_dirty);
        assert!(entry.detail_fresh());

        // The home lane lands a moved rollup: the row shows it and the
        // cached detail is behind — drift.
        let mut row = store_row(482);
        row.status = Some("PENDING".into());
        state.land_summary("acme/one", &row);
        let entry = state.pr_entry("acme/one", 482).unwrap();
        assert_eq!(entry.summary.status.as_deref(), Some("PENDING"));
        assert!(entry.rollup_dirty);
        assert!(!entry.detail_fresh(), "drift voids the SWR skip");

        // An unchanged landing never clears the mark...
        state.land_summary("acme/one", &row);
        assert!(state.pr_entry("acme/one", 482).unwrap().rollup_dirty);

        // ...only the catch-up landing does — and it follows the
        // summary down as well.
        let mut d2 = store_detail(482);
        d2.rollup_state = Some("PENDING".into());
        state.land_detail("acme/one", 482, d2);
        let entry = state.pr_entry("acme/one", 482).unwrap();
        assert!(!entry.rollup_dirty);
        assert!(entry.detail_fresh());
        assert_eq!(entry.summary.status.as_deref(), Some("PENDING"));
    }

    /// docs/0073 D8 — the journey test: land a lane, derive the open
    /// list, confirm a merge — the derived list drops the row and the
    /// document shows MERGED, with no lane landing in between. This is
    /// the seam behind the #561 field report, pinned permanently.
    #[test]
    fn confirmed_merge_drops_the_row_in_the_derived_open_list() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        state.land_summary("acme/one", &store_row(7));
        state.land_summary("acme/one", &store_row(8));
        state.home_lanes.insert(
            ("acme".to_string(), HomeScope::User),
            HomeLane {
                order: vec![PrKey::new("acme/one", 7), PrKey::new("acme/one", 8)],
                total_count: 2,
                complete: true,
                cursor: None,
            },
        );
        let view = state.lane_view("acme", HomeScope::User).unwrap();
        assert_eq!(view.rows.len(), 2, "both open before the merge");

        let token = state
            .begin_pending("acme/one", 7, PendingKind::Merge)
            .expect("arms a pending op");
        let ok = SpecResult {
            kind: Kind::PrMutation(Mutation::Merge),
            ident: None,
            status: 200,
            etag: None,
            rate: None,
            body: Some("{}".into()),
            network: None,
            cursor: None,
        };
        assert!(state.apply_store_mutation_result("acme/one", 7, PendingKind::Merge, token, ok));

        // The document is MERGED and the derived open list has dropped
        // the row — no landing involved.
        let entry = state.pr_entry("acme/one", 7).unwrap();
        assert_eq!(entry.summary.state.as_deref(), Some("MERGED"));
        let view = state.lane_view("acme", HomeScope::User).unwrap();
        assert_eq!(view.rows.len(), 1);
        assert_eq!(view.rows[0].number, 8, "server order kept for the rest");
    }

    /// docs/0073 D3 — the selector: server key order ⊗ documents,
    /// filtered to OPEN; missing entries skipped; `None` when the lane
    /// never landed.
    #[test]
    fn lane_view_derives_from_index_and_documents() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        assert!(state.lane_view("acme", HomeScope::User).is_none());

        state.land_summary("acme/one", &store_row(7));
        let mut closed = store_row(8);
        closed.state = Some("CLOSED".into());
        state.land_summary("acme/one", &closed);
        state.land_summary("acme/two", &store_row(9));
        state.home_lanes.insert(
            ("acme".to_string(), HomeScope::User),
            HomeLane {
                order: vec![
                    PrKey::new("acme/two", 9),
                    PrKey::new("acme/one", 8),
                    PrKey::new("acme/one", 7),
                    PrKey::new("acme/one", 99),
                ],
                total_count: 4,
                complete: true,
                cursor: None,
            },
        );

        let view = state.lane_view("acme", HomeScope::User).unwrap();
        // Server order; the CLOSED document is filtered; the entry-less
        // key (never landed / evicted) is skipped.
        let numbers: Vec<i64> = view.rows.iter().map(|r| r.number).collect();
        assert_eq!(numbers, vec![9, 7]);
        assert_eq!(view.total_count, 4);
        assert!(view.complete && !view.has_cursor);
    }

    /// docs/0073 D4 (stage 2) — the repo snapshot lands its PR rows
    /// into the document store, and the repo view's derived list
    /// overlays entries onto the snapshot: the entry wins, OPEN-
    /// filtered — a confirmed merge leaves the repo list on confirm.
    #[test]
    fn repo_snapshot_lands_rows_and_the_view_overlays_entries() {
        let mut state = make_state(Arc::new(MockHttp::new(vec![])));
        let body = r#"{"data":{"repository":{"databaseId":7,"id":"R_1","hasIssuesEnabled":true,
            "name":"r","nameWithOwner":"o/r","description":null,"isPrivate":false,
            "pushedAt":"2026-09-01T00:00:00Z","owner":{"login":"o"},
            "pullRequests":{"totalCount":2,"nodes":[
                {"databaseId":11,"number":123,"title":"feat: x","isDraft":false,
                 "state":"OPEN","author":null,"updatedAt":"u","url":"u1",
                 "statusCheckRollup":{"state":"SUCCESS"}},
                {"databaseId":12,"number":124,"title":"feat: y","isDraft":false,
                 "state":"OPEN","author":null,"updatedAt":"u","url":"u2"}]}},
            "issues":{"totalCount":0,"nodes":[]}}}"#;
        state.apply(SpecResult {
            kind: Kind::RepoPulls,
            ident: Some("repo:o/r".into()),
            status: 200,
            etag: None,
            rate: None,
            body: Some(body.into()),
            network: None,
            cursor: None,
        });

        // The landing enriched the document store: both rows are
        // documents now.
        assert!(state.pr_entry("o/r", 123).is_some());
        assert!(state.pr_entry("o/r", 124).is_some());

        let view = {
            let repo = state.repos.iter().find(|r| r.key == "o/r").unwrap();
            state.repo_pr_rows(repo)
        };
        assert_eq!(view.len(), 2, "both open before the merge");

        // Confirm a merge on #123: the entry wins, the OPEN filter
        // drops it — no snapshot landing involved.
        let token = state
            .begin_pending("o/r", 123, PendingKind::Merge)
            .expect("arms a pending op");
        let ok = SpecResult {
            kind: Kind::PrMutation(Mutation::Merge),
            ident: None,
            status: 200,
            etag: None,
            rate: None,
            body: Some("{}".into()),
            network: None,
            cursor: None,
        };
        assert!(state.apply_store_mutation_result("o/r", 123, PendingKind::Merge, token, ok));
        let view = {
            let repo = state.repos.iter().find(|r| r.key == "o/r").unwrap();
            state.repo_pr_rows(repo)
        };
        assert_eq!(view.len(), 1);
        assert_eq!(view[0].number, 124, "the survivor keeps snapshot order");

        // The entry also wins on fresher facts: a landed rollup shows
        // through the overlay even though the snapshot row predates it.
        let mut d = store_detail(124);
        d.rollup_state = Some("FAILURE".into());
        state.land_detail("o/r", 124, d);
        let view = {
            let repo = state.repos.iter().find(|r| r.key == "o/r").unwrap();
            state.repo_pr_rows(repo)
        };
        assert_eq!(view[0].status.as_deref(), Some("FAILURE"));
    }

    /// docs/0073 D1 — the fact-copy audit: no struct field outside the
    /// store may hold a PR fact. Handoff params and event payloads
    /// (`open_pr`'s row, the creator's `Created` event) are messages,
    /// not holders; the repo snapshot (service.rs) is the repo
    /// domain's persisted cache (docs/0007 D6) and is reviewed by hand.
    #[test]
    fn no_pr_fact_fields_outside_the_store() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        // An owned PR-fact field: `name: gql::X`, optionally behind
        // `Option<` / `Vec<` / `Box<` (nested allowed). Borrowed shapes
        // (`&gql::…` params, `-> …` returns) are views, not holders.
        fn is_pr_fact_field(line: &str) -> bool {
            let t = line.trim_start();
            let Some((_, rest)) = t.split_once(": ") else {
                return false;
            };
            let mut rest = rest;
            while let Some(inner) = rest
                .strip_prefix("Option<")
                .or_else(|| rest.strip_prefix("Vec<"))
                .or_else(|| rest.strip_prefix("Box<"))
            {
                rest = inner;
            }
            rest.starts_with("gql::PullRequest") || rest.starts_with("gql::PrDetail")
        }
        let mut offenders = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            if path.file_name().is_some_and(|n| n == "service.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let code = text.split("#[cfg(test)]").next().unwrap_or("");
            for (ix, line) in code.lines().enumerate() {
                if is_pr_fact_field(line) {
                    offenders.push(format!("{}:{}: {}", path.display(), ix + 1, line.trim()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "PR-fact fields outside the store:\n{}",
            offenders.join("\n")
        );
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
        GithubState::new(
            client,
            store,
            slots.0,
            slots.1,
            GithubCache::at_tmp(tag),
            DraftStore::at_tmp(tag),
        )
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
        let spec = state.spec(
            Kind::ThreadDetail,
            crate::api::thread_url("19"),
            None,
            Origin::BACKGROUND,
        );
        let result = run_spec(&*http, &test_recorder(), &spec, std::time::Instant::now());
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
            let spec = state.spec(
                Kind::ThreadMutation(op),
                crate::api::thread_url("19"),
                None,
                Origin::BACKGROUND,
            );
            let result = run_spec(&*http, &test_recorder(), &spec, std::time::Instant::now());
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
        let specs = state.mark_refetch_specs(Surface::Background);
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].kind, Kind::Notifications);
        assert_eq!(specs[1].kind, Kind::RepoNotifications);
        assert_eq!(specs[1].ident.as_deref(), Some("o/r"));
        // No selection: the global list only.
        let http = Arc::new(MockHttp::new(vec![]));
        let mut state = make_state(http);
        state.selected = None;
        assert_eq!(state.mark_refetch_specs(Surface::Background).len(), 1);
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
            origin: Origin::BACKGROUND,
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
                DraftStore::at_tmp(tag),
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

    // --- Fetch<T> record + run_read_deduped guard (docs/0031 §5) ---------

    fn dedup_spec(kind: Kind, ident: Option<&str>) -> FetchSpec {
        FetchSpec {
            origin: Origin::BACKGROUND,
            kind,
            url: api::notifications_url(1),
            etag: None,
            ident: ident.map(Into::into),
            max_pages: 1,
            variables: None,
        }
    }

    fn manual_spec(kind: Kind, ident: Option<&str>) -> FetchSpec {
        FetchSpec {
            origin: Origin::new(Surface::RepoView, Trigger::Manual),
            kind,
            url: api::notifications_url(1),
            etag: None,
            ident: ident.map(Into::into),
            max_pages: 1,
            variables: None,
        }
    }

    #[test]
    fn fetch_record_transitions() {
        // docs/0031 D3/D4: `land` stamps fetched_at and clears errors;
        // `fail` with nothing landed is terminal; `fail` over landed data
        // keeps the payload and surfaces `last_error` (stale-on-error).
        let mut f: Fetch<&'static str> = Fetch::new();
        assert!(f.is_idle());
        assert!(!f.is_loading() && f.data().is_none());

        f.begin();
        assert!(f.is_loading(), "first-load spinner: in flight, no data");

        f.fail("boom".into());
        assert!(!f.in_flight);
        assert_eq!(f.load_error(), Some("boom"));

        f.begin();
        f.land("data");
        assert!(f.is_ready() && !f.in_flight);
        assert_eq!(f.data(), Some(&"data"));
        assert!(f.fetched_at().is_some());
        assert!(f.age().is_some());

        f.fail("late error".into());
        assert_eq!(f.data(), Some(&"data"), "stale-on-error keeps the payload");
        assert_eq!(f.load_error(), Some("late error"));

        f.land("fresh");
        assert_eq!(f.data(), Some(&"fresh"));
        assert_eq!(f.load_error(), None, "landing clears last_error");
    }

    #[gpui::test]
    async fn read_deduped_skips_duplicate_dispatch(cx: &mut gpui::TestAppContext) {
        cx.update(dashboard_core::status::init);
        cx.update(dashboard_core::toasts::init); // refresh raises auth alerts
        let http = Arc::new(MockHttp::new(vec![api_result(200, "[]")]));
        let state = gate_state_entity(cx, http.clone(), "dedup-skip");

        // Lane A mid-flight, simulated deterministically: the key is
        // armed without a transport round (docs/0031 §5).
        let key: InFlightKey = (Kind::Notifications, None);
        cx.update(|cx| {
            state.update(cx, |s, _| assert!(s.in_flight_arm(key.clone())));
        });

        // Lane B: same key → skipped, no transport round, no result.
        let skipped = cx
            .spawn({
                let state = state.clone();
                move |mut cx: gpui::AsyncApp| async move {
                    run_read_deduped(state, dedup_spec(Kind::Notifications, None), &mut cx).await
                }
            })
            .await;
        assert!(skipped.is_none());
        assert!(
            http.recorded().is_empty(),
            "a skipped dispatch never hits the transport"
        );

        // The holder clears; the next dispatch runs and clears behind it.
        cx.update(|cx| {
            state.update(cx, |s, _| s.in_flight_clear(&key));
        });
        let result = cx
            .spawn({
                let state = state.clone();
                move |mut cx: gpui::AsyncApp| async move {
                    run_read_deduped(state, dedup_spec(Kind::Notifications, None), &mut cx).await
                }
            })
            .await;
        assert!(result.is_some());
        assert_eq!(http.recorded().len(), 1);
        assert!(
            cx.update(|cx| state.read(cx).in_flight.is_empty()),
            "the guard clears when the run lands"
        );
    }

    /// docs/0031 D2 amended: a Manual read (docs/0052 — the refresh
    /// buttons) is the user's explicit re-fetch. With the key armed it
    /// still dispatches, lands behind the holder, and — critically —
    /// never clears the holder's arm for it.
    #[gpui::test]
    async fn read_deduped_manual_trigger_rides_out_an_in_flight_read(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(dashboard_core::status::init);
        cx.update(dashboard_core::toasts::init); // refresh raises auth alerts
        let http = Arc::new(MockHttp::new(vec![
            api_result(200, "[]"),
            api_result(200, "[]"),
        ]));
        let state = gate_state_entity(cx, http.clone(), "dedup-manual");

        // Lane A mid-flight, simulated deterministically: the key is
        // armed without a transport round (docs/0031 §5).
        let key: InFlightKey = (Kind::Notifications, None);
        cx.update(|cx| {
            state.update(cx, |s, _| assert!(s.in_flight_arm(key.clone())));
        });

        // The Manual rider: same key → still dispatches.
        let result = cx
            .spawn({
                let state = state.clone();
                move |mut cx: gpui::AsyncApp| async move {
                    run_read_deduped(state, manual_spec(Kind::Notifications, None), &mut cx).await
                }
            })
            .await;
        assert!(result.is_some(), "a Manual read is never absorbed");
        assert_eq!(http.recorded().len(), 1, "the rider hits the transport");
        assert!(
            cx.update(|cx| state.read(cx).in_flight.contains(&key)),
            "the rider leaves the holder's arm in place"
        );

        // The holder still owns the guard: its own clear reopens the
        // lane, and the next automatic dispatch runs and clears behind
        // itself.
        cx.update(|cx| {
            state.update(cx, |s, _| s.in_flight_clear(&key));
        });
        let http2 = http.clone();
        let next = cx
            .spawn({
                let state = state.clone();
                move |mut cx: gpui::AsyncApp| async move {
                    run_read_deduped(state, dedup_spec(Kind::Notifications, None), &mut cx).await
                }
            })
            .await;
        assert!(next.is_some());
        assert_eq!(http2.recorded().len(), 2);
        assert!(
            cx.update(|cx| state.read(cx).in_flight.is_empty()),
            "the guard clears when the run lands"
        );
    }

    #[gpui::test]
    async fn read_deduped_never_blocks_the_raw_lane(cx: &mut gpui::TestAppContext) {
        // Mutations and the connect flow stay on `run_spec_tracked`, which
        // never consults the guard (docs/0031 D1) — a write retry must go
        // out even while an identical-keyed read is in flight.
        cx.update(dashboard_core::status::init);
        cx.update(dashboard_core::toasts::init); // refresh raises auth alerts
        let http = Arc::new(MockHttp::new(vec![api_result(200, "[]")]));
        let state = gate_state_entity(cx, http.clone(), "dedup-mutation");

        let key: InFlightKey = (Kind::Notifications, None);
        cx.update(|cx| {
            state.update(cx, |s, _| assert!(s.in_flight_arm(key.clone())));
        });

        let result = cx
            .spawn({
                let state = state.clone();
                move |mut cx: gpui::AsyncApp| async move {
                    run_spec_tracked(state, dedup_spec(Kind::Notifications, None), &mut cx).await
                }
            })
            .await;
        assert_eq!(result.status, 200);
        assert_eq!(http.recorded().len(), 1, "the raw lane is unguarded");
    }

    #[gpui::test]
    async fn wrapper_retries_the_401d_request_after_refresh(cx: &mut gpui::TestAppContext) {
        // docs/0028 §5: the reactive flow — the 401'd request itself is
        // retried after the refresh + install, no tick, no bootstrap.
        cx.update(dashboard_core::status::init);
        cx.update(dashboard_core::toasts::init); // refresh raises auth alerts
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
        cx.update(dashboard_core::toasts::init); // refresh raises auth alerts
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
                origin: Origin::BACKGROUND,
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
        cx.update(dashboard_core::toasts::init); // refresh raises auth alerts
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
        cx.update(dashboard_core::toasts::init); // refresh raises auth alerts
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

    // --- usage records (docs/0053 §5) --------------------------------------

    fn records_sink(cap: usize) -> std::sync::Arc<dashboard_core::usage_records::MemorySink> {
        std::sync::Arc::new(dashboard_core::usage_records::MemorySink::new(cap))
    }

    fn notifications_body(n: usize) -> String {
        let items: Vec<String> = (0..n)
            .map(|i| format!(r#"{{"id":"t{i}","unread":true,"reason":"subscribed","updated_at":"2024-01-01T00:00:00Z","subject":{{"title":"s","url":"https://api.github.com/repos/o/r/issues/1","type":"Issue"}},"repository":{{"full_name":"o/r"}}}}"#))
            .collect();
        format!("[{}]", items.join(","))
    }

    #[test]
    fn records_one_per_flight_on_a_paged_rest_walk() {
        // docs/0053 D1: one record per HTTP flight — a 3-page walk is 3
        // records of cost 1, sharing origin and ident.
        let http = Arc::new(MockHttp::new(vec![
            ok(&notifications_body(100), "\"e1\""),
            ok(&notifications_body(100), "\"e2\""),
            ok(&notifications_body(3), "\"e3\""),
        ]));
        let sink = records_sink(16);
        let recorder = dashboard_core::usage_records::Recorder::new("github", sink.clone());
        let state = make_state_at(
            http.clone(),
            GithubCache::at_tmp("rec-walk"),
            DraftStore::at_tmp("rec-walk"),
        );
        let spec = state.spec(
            Kind::Notifications,
            api::notifications_url(1),
            Some(3),
            Origin::new(Surface::NotificationPage, Trigger::ViewShown),
        );
        run_spec(&*http, &recorder, &spec, std::time::Instant::now());
        let rows = sink.snapshot();
        assert_eq!(rows.len(), 3, "one record per flight, not per spec");
        for r in &rows {
            assert_eq!(r.module.as_ref(), "github");
            assert_eq!(r.transport, Transport::Rest);
            assert_eq!(r.op.as_ref(), "notifications");
            assert_eq!(r.cost, Some(1));
            assert_eq!(r.status, 200);
            assert_eq!(r.remaining_after, Some(4900));
            assert_eq!(r.cycle_reset_epoch, Some(9_999_999));
            assert!(!r.network_error);
            // docs/0054 D14: the ceiling rides the header snapshot,
            // the queue wait is stamped at dispatch, bytes = body len.
            assert_eq!(r.limit_after, Some(5000));
            assert!(r.queue_ms.is_some(), "queue wait is stamped");
        }
        // Bytes match each page's own body (pages shrink on the tail).
        assert_eq!(
            rows[0].resp_bytes,
            Some(notifications_body(100).len() as u32)
        );
        assert_eq!(rows[2].resp_bytes, Some(notifications_body(3).len() as u32));
        assert_eq!(rows[0].surface.as_ref(), "notification-page");
        assert_eq!(rows[0].trigger.as_ref(), "view-shown");
        assert_eq!(rows[0].seq + 1, rows[1].seq);
    }

    #[test]
    fn records_graphql_cost_from_the_body() {
        // docs/0053 D1: GraphQL cost is the response's own rateLimit.cost
        // — never a guessed number; remaining/reset ride the same object.
        let http = Arc::new(MockHttp::new(vec![gql_ok(
            r#"{"data":{"rateLimit":{"cost":7,"remaining":4310,"resetAt":"2030-01-01T00:00:00Z","limit":5000},"viewer":{"repositories":{"nodes":[],"pageInfo":{"hasNextPage":false}}}}}"#,
        )]));
        let sink = records_sink(16);
        let recorder = dashboard_core::usage_records::Recorder::new("github", sink.clone());
        let state = make_state_at(
            http.clone(),
            GithubCache::at_tmp("rec-gql"),
            DraftStore::at_tmp("rec-gql"),
        );
        let spec =
            state.session_inventory_spec(Origin::new(Surface::ServiceTick, Trigger::Bootstrap));
        run_spec(&*http, &recorder, &spec, std::time::Instant::now());
        let rows = sink.snapshot();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].transport, Transport::Graphql);
        assert_eq!(rows[0].cost, Some(7));
        assert_eq!(rows[0].remaining_after, Some(4310));
        // docs/0054 D14: the body's own ceiling, same object as cost.
        assert_eq!(rows[0].limit_after, Some(5000));
        assert_eq!(rows[0].cycle_reset_epoch, Some(1_893_456_000));
        assert!(rows[0].queue_ms.is_some());
        assert!(rows[0].resp_bytes.is_some());
        assert_eq!(rows[0].surface.as_ref(), "service-tick");
        assert_eq!(rows[0].trigger.as_ref(), "bootstrap");
    }

    #[test]
    fn records_304_and_probe_are_free() {
        // docs/0050 D1 booking rules: ETag revalidations and the
        // GET /rate_limit probe cost nothing.
        let http304 = Arc::new(MockHttp::new(vec![Ok(HttpResult {
            status: 304,
            etag: Some("e".into()),
            rate: Some(RateLimit {
                limit: 5000,
                remaining: 4900,
                reset_epoch: 42,
            }),
            body: None,
        })]));
        let sink = records_sink(16);
        let recorder = dashboard_core::usage_records::Recorder::new("github", sink.clone());
        let state = make_state_at(
            http304.clone(),
            GithubCache::at_tmp("rec-304"),
            DraftStore::at_tmp("rec-304"),
        );
        let spec = state.spec(
            Kind::Notifications,
            api::notifications_url(1),
            Some(1),
            Origin::BACKGROUND,
        );
        run_spec(&*http304, &recorder, &spec, std::time::Instant::now());
        assert_eq!(sink.snapshot()[0].cost, Some(0));
        assert!(sink.snapshot()[0].etag_304);
        assert_eq!(sink.snapshot()[0].limit_after, Some(5000));
        assert_eq!(sink.snapshot()[0].resp_bytes, None, "304 sends no body");

        let http_probe = Arc::new(MockHttp::new(vec![ok(r#"{"resources":{}}"#, "p")]));
        let sink2 = records_sink(16);
        let recorder2 = dashboard_core::usage_records::Recorder::new("github", sink2.clone());
        let state2 = make_state_at(
            http_probe.clone(),
            GithubCache::at_tmp("rec-probe"),
            DraftStore::at_tmp("rec-probe"),
        );
        let spec2 = state2.spec(
            Kind::RateLimit,
            api::rate_limit_url(),
            None,
            Origin::new(Surface::ServiceTick, Trigger::Probe),
        );
        run_spec(&*http_probe, &recorder2, &spec2, std::time::Instant::now());
        let row = &sink2.snapshot()[0];
        assert_eq!(row.cost, Some(0), "the free probe records zero");
        assert_eq!(row.trigger.as_ref(), "probe");
    }

    #[test]
    fn records_network_errors_are_unattributable() {
        // docs/0053 D1: a network error's server-side cost is unknowable
        // — cost stays None, never a guessed number.
        let http = Arc::new(MockHttp::new(vec![Err(HttpError("dns gone".into()))]));
        let sink = records_sink(16);
        let recorder = dashboard_core::usage_records::Recorder::new("github", sink.clone());
        let state = make_state_at(
            http.clone(),
            GithubCache::at_tmp("rec-net"),
            DraftStore::at_tmp("rec-net"),
        );
        let spec = state.spec(
            Kind::Notifications,
            api::notifications_url(1),
            Some(1),
            Origin::BACKGROUND,
        );
        run_spec(&*http, &recorder, &spec, std::time::Instant::now());
        let row = &sink.snapshot()[0];
        assert!(row.network_error);
        assert_eq!(row.status, 0);
        assert_eq!(row.cost, None);
        assert_eq!(row.remaining_after, None);
        assert_eq!(row.limit_after, None, "no response → no ceiling");
        assert_eq!(row.resp_bytes, None);
        assert!(
            row.queue_ms.is_some(),
            "the queue wait still happened (docs/0054 D14)"
        );
    }

    #[test]
    fn records_token_refresh_is_free_auth_transport() {
        // docs/0053 D4: the auth POST records through the same decorator
        // — Auth transport, cost 0 (login endpoints never bill).
        let http = Arc::new(MockHttp::new(vec![Ok(HttpResult {
            status: 200,
            etag: None,
            rate: None,
            body: Some(r#"{"access_token":"t1k","refresh_token":"r2","expires_in":28800}"#.into()),
        })]));
        let sink = records_sink(16);
        let recorder = dashboard_core::usage_records::Recorder::new("github", sink.clone());
        let rec = crate::usage::recording_for_auth(&*http, &recorder, std::time::Instant::now());
        let outcome = crate::auth::refresh(&rec, "ghr_old");
        match outcome {
            crate::auth::RefreshOutcome::Refreshed(_) => {}
            _ => panic!("expected a refreshed credential"),
        }
        let rows = sink.snapshot();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].transport, Transport::Auth);
        assert_eq!(rows[0].cost, Some(0));
        assert_eq!(rows[0].surface.as_ref(), "auth");
        assert_eq!(rows[0].trigger.as_ref(), "auth");
        assert_eq!(rows[0].op.as_ref(), "token-refresh");
        assert_eq!(rows[0].limit_after, None, "auth endpoints send no ceiling");
        assert!(rows[0].queue_ms.is_some());
        assert!(rows[0].resp_bytes.is_some());
    }

    #[test]
    fn records_queue_wait_spans_dispatch_to_flight_start() {
        // docs/0054 D14: `queue_ms` is the dispatch→flight-start wait —
        // a dispatch stamped 60 ms in the past must show a gap >= 60,
        // and it must not include the flight itself (measured before
        // the request goes out).
        let http = Arc::new(MockHttp::new(vec![Ok(HttpResult {
            status: 200,
            etag: None,
            rate: None,
            body: Some(r#"{"access_token":"t1k","refresh_token":"r2","expires_in":28800}"#.into()),
        })]));
        let sink = records_sink(16);
        let recorder = dashboard_core::usage_records::Recorder::new("github", sink.clone());
        let dispatched = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_millis(60))
            .expect("60 ms ago is representable");
        let rec = crate::usage::recording_for_auth(&*http, &recorder, dispatched);
        let outcome = crate::auth::refresh(&rec, "ghr_old");
        assert!(matches!(outcome, crate::auth::RefreshOutcome::Refreshed(_)));
        let row = &sink.snapshot()[0];
        assert!(
            row.queue_ms.unwrap() >= 60,
            "queue wait covers dispatch→flight start"
        );
    }

    #[test]
    fn background_origins_are_loud() {
        // docs/0053 D2 runtime layer: a Background record warns and
        // counts — visible in the first session's logs, not just data.
        let before = crate::usage::background_records();
        let http = Arc::new(MockHttp::new(vec![ok("[]", "b")]));
        let sink = records_sink(16);
        let recorder = dashboard_core::usage_records::Recorder::new("github", sink.clone());
        let state = make_state_at(
            http.clone(),
            GithubCache::at_tmp("rec-bg"),
            DraftStore::at_tmp("rec-bg"),
        );
        let spec = state.spec(
            Kind::Notifications,
            api::notifications_url(1),
            Some(1),
            Origin::BACKGROUND,
        );
        run_spec(&*http, &recorder, &spec, std::time::Instant::now());
        assert!(crate::usage::background_records() > before);
        assert_eq!(sink.snapshot()[0].surface.as_ref(), "background");
    }
}
