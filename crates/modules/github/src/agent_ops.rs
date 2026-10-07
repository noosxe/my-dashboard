//! Agent-ops provider (docs/0068 D3/D5, §6): the github module's phase-1
//! read ops over `GithubState` — zero network flights, mandatory
//! freshness/truncation envelopes, structured `not_held` instead of
//! misleading empty arrays. Ops live here, in the owning module; the
//! bridge core (`dashboard-agent`) only routes.

use gpui::Entity;
use serde::Deserialize;
use serde_json::{Value, json};

use dashboard_agent::AgentOps;
use dashboard_agent::protocol::{Freshness, FreshnessSource, RpcError, error_code, parse};

use crate::gql;
use crate::service::GithubState;

pub const OP_APP_STATUS: &str = "app_status";
pub const OP_LIST_REPOS: &str = "list_repos";
pub const OP_NOTIFICATIONS_UNREAD: &str = "notifications_unread";
pub const OP_REPO_PRS: &str = "repo_prs";
pub const OP_REPO_ISSUES: &str = "repo_issues";
pub const OP_API_USAGE: &str = "api_usage";

/// Snapshot-run inventory ceiling (gql.rs §5: 3 × 100 = 300 repos).
/// At the cap the true repo total is unknowable — `total_known` goes
/// null rather than lying (docs/0029 §2, docs/0068 D5).
const REPO_INVENTORY_SNAPSHOT_CAP: usize = 300;

/// The github module's contribution to the agent bridge: read-only ops
/// over the single state entity. Registration happens at module init;
/// resolution is per request (D3).
pub struct GithubAgentOps {
    state: Entity<GithubState>,
}

impl GithubAgentOps {
    pub fn new(state: Entity<GithubState>) -> Self {
        Self { state }
    }
}

#[derive(Deserialize)]
struct ListReposParams {
    #[serde(default)]
    org: Option<String>,
}

#[derive(Deserialize)]
struct NotificationsParams {
    #[serde(default)]
    repo: Option<String>,
}

#[derive(Deserialize)]
struct RepoRowsParams {
    repo: String,
    /// Phase 1 holds open rows only (the inventory summaries are
    /// open-only); anything else is an explicit `BAD_PARAMS`.
    #[serde(default)]
    state: Option<String>,
}

#[derive(Deserialize)]
struct ApiUsageParams {
    #[serde(default)]
    from_ms: Option<u64>,
    #[serde(default)]
    to_ms: Option<u64>,
    #[serde(default)]
    group_by: Option<String>,
    /// analytics `Range` label (`1h|1d|1w|1m`) as an alternative to
    /// explicit `from_ms`/`to_ms`.
    #[serde(default)]
    window: Option<String>,
}

impl AgentOps for GithubAgentOps {
    fn ops(&self) -> &'static [&'static str] {
        &[
            OP_APP_STATUS,
            OP_LIST_REPOS,
            OP_NOTIFICATIONS_UNREAD,
            OP_REPO_PRS,
            OP_REPO_ISSUES,
            OP_API_USAGE,
        ]
    }

    fn handle(&self, cx: &mut gpui::App, op: &str, params: &Value) -> Result<Value, RpcError> {
        match op {
            OP_APP_STATUS => self.app_status(cx),
            OP_LIST_REPOS => {
                let p: ListReposParams =
                    parse(params).ok_or_else(|| bad_params("params: {org?: string}"))?;
                self.list_repos(cx, p.org.as_deref())
            }
            OP_NOTIFICATIONS_UNREAD => {
                let p: NotificationsParams =
                    parse(params).ok_or_else(|| bad_params("params: {repo?: string}"))?;
                self.notifications_unread(cx, p.repo.as_deref())
            }
            OP_REPO_PRS => self.repo_rows(cx, params, Rows::Prs),
            OP_REPO_ISSUES => self.repo_rows(cx, params, Rows::Issues),
            OP_API_USAGE => {
                let p: ApiUsageParams = parse(params).ok_or_else(|| {
                    bad_params(
                        "params: {from_ms?, to_ms?, group_by?: op|surface|trigger|status, \
                         window?: 1h|1d|1w|1m}",
                    )
                })?;
                self.api_usage(cx, p)
            }
            // The registry only routes listed ops (first-wins, resolved
            // per request) — an unknown op never reaches the provider.
            _ => Err(RpcError::new(
                error_code::UNKNOWN_OP,
                format!("no op named {op:?}"),
            )),
        }
    }
}

enum Rows {
    Prs,
    Issues,
}

impl GithubAgentOps {
    /// The "am I looking at live data?" op (docs/0068 §6): auth state,
    /// freshness stamps, budget snapshot — no network, ever.
    fn app_status(&self, cx: &mut gpui::App) -> Result<Value, RpcError> {
        let s = self.state.read(cx);
        let freshness = match (s.snapshot_age, s.notifications_age) {
            (Some(age), _) => Freshness {
                age_s: Some(age.as_secs()),
                source: FreshnessSource::Disk,
            },
            (None, Some(age)) => Freshness {
                age_s: Some(age.as_secs()),
                source: FreshnessSource::Disk,
            },
            // Session-held with no stamp surfaced yet (docs/0034: a landed
            // sync clears the disk-seeded ages; the session records own
            // freshness per lane, not for the aggregate).
            (None, None) => Freshness {
                age_s: None,
                source: FreshnessSource::Cache,
            },
        };
        // The app-wide ledger is installed at boot (main.rs) before any
        // module init, so this global is present whenever ops can run.
        let high_water = dashboard_core::usage_records::source(cx).high_water();
        Ok(json!({
            "freshness": freshness,
            "truncation": {"returned": 1, "total_known": 1},
            "auth": auth_name(&s.auth),
            "connected": s.auth.connected(),
            "user": s.user.as_ref().map(|u| u.login.clone()),
            "orgs": s.orgs.iter().map(|o| o.login.clone()).collect::<Vec<_>>(),
            "repo_count": s.repos.len(),
            "unread_notifications": s.notifications.len(),
            "selected": s.selected,
            "rate_limited_until": s.rate_limited_until,
            "access_expires_at": s.access_expires_at,
            "snapshot_age_s": s.snapshot_age.map(|d| d.as_secs()),
            "notifications_age_s": s.notifications_age.map(|d| d.as_secs()),
            "usage_high_water": high_water,
        }))
    }

    /// The held repo inventory (metadata; PR/issue summaries arrive with
    /// the lazy repo scope, docs/0007 D6).
    fn list_repos(&self, cx: &mut gpui::App, org: Option<&str>) -> Result<Value, RpcError> {
        let s = self.state.read(cx);
        let filtered: Vec<&gql::Repo> = s
            .repos
            .iter()
            .filter(|repo| org.is_none_or(|org| repo.owner.login == org))
            .collect();
        let rows: Vec<Value> = filtered
            .iter()
            .map(|repo| {
                json!({
                    "key": repo.key,
                    "full_name": repo.full_name,
                    "name": repo.name,
                    "owner": repo.owner.login,
                    "private": repo.private,
                    "archived": repo.is_archived,
                    "description": repo.description,
                    "pushed_at": repo.pushed_at,
                    "open_prs": repo.prs.total_count,
                    "open_issues": repo.issues.total_count,
                    // Rows held ≠ summaries present: the lazy scope may
                    // not have run for this repo yet (docs/0007 D6).
                    "rows_held": s.rows_landed.contains_key(&repo.key),
                })
            })
            .collect();
        Ok(json!({
            "freshness": inventory_freshness(s.snapshot_age),
            "truncation": {
                "returned": rows.len(),
                "total_known": inventory_total_known(s.repos.len()),
            },
            "org": org,
            "repos": rows,
        }))
    }

    /// The global unread list — the state holds **unread** only (§6:
    /// honest name), optionally narrowed to one repo.
    fn notifications_unread(
        &self,
        cx: &mut gpui::App,
        repo: Option<&str>,
    ) -> Result<Value, RpcError> {
        let s = self.state.read(cx);
        let rows: Vec<Value> = s
            .notifications
            .iter()
            .filter(|n| repo.is_none_or(|repo| n.repository.full_name == repo))
            .map(|n| {
                json!({
                    "id": n.id,
                    "repo": n.repository.full_name,
                    "subject": {"title": n.subject.title, "kind": n.subject.kind},
                    "reason": n.reason,
                    "updated_at": n.updated_at,
                })
            })
            .collect();
        let freshness = match s.notifications_age {
            Some(age) => Freshness {
                age_s: Some(age.as_secs()),
                source: FreshnessSource::Disk,
            },
            None => Freshness {
                age_s: None,
                source: FreshnessSource::Cache,
            },
        };
        Ok(json!({
            "freshness": freshness,
            "truncation": {
                "returned": rows.len(),
                "total_known": Some(s.notifications.len()),
            },
            "repo": repo,
            "notifications": rows,
        }))
    }

    /// Held open-PR / open-issue rows for one repo (§6): the lazy
    /// repo-scope summaries when they landed this session, structured
    /// `not_held` otherwise — never an empty array.
    fn repo_rows(
        &self,
        cx: &mut gpui::App,
        params: &Value,
        which: Rows,
    ) -> Result<Value, RpcError> {
        let p: RepoRowsParams =
            parse(params).ok_or_else(|| bad_params("params: {repo: string, state?: \"open\"}"))?;
        if let Some(state) = p.state.as_deref() {
            if state != "open" {
                return Err(bad_params(
                    "phase 1 holds open rows only (the inventory summaries are open-only)",
                ));
            }
        }
        let s = self.state.read(cx);
        let Some(repo) = s
            .repos
            .iter()
            .find(|repo| repo.key == p.repo || repo.full_name == p.repo)
        else {
            return Ok(not_held(
                &p.repo,
                "repo not in the session inventory (inventory is the metadata-only cap set, \
                 docs/0007 D6)",
            ));
        };
        let Some(landed) = s.rows_landed.get(&repo.key) else {
            return Ok(not_held(
                repo.full_name.as_str(),
                "rows never landed this session — open the repo view, or use the phase-2 \
                 refresh op once it exists (docs/0068 §7)",
            ));
        };
        let (nodes, total_count, kind) = match which {
            Rows::Prs => (
                repo.prs
                    .nodes
                    .iter()
                    .map(|n| serde_json::to_value(n).unwrap_or_default())
                    .collect::<Vec<_>>(),
                repo.prs.total_count,
                "prs",
            ),
            Rows::Issues => (
                repo.issues
                    .nodes
                    .iter()
                    .map(|n| serde_json::to_value(n).unwrap_or_default())
                    .collect::<Vec<_>>(),
                repo.issues.total_count,
                "issues",
            ),
        };
        // `total_count` is exact (docs/0007 D2) — the 100-row cap is the
        // only truncation, and it is always knowable.
        Ok(json!({
            "freshness": {
                "age_s": Some(landed.elapsed().as_secs()),
                "source": FreshnessSource::Cache,
            },
            "truncation": {
                "returned": nodes.len(),
                "total_known": Some(total_count.max(0) as u64),
            },
            "repo": repo.full_name,
            "held": true,
            "kind": kind,
            "rows": nodes,
        }))
    }

    /// The agent's own spend report (§6): rows from the app-wide ledger
    /// over `UsageSource` — a live read, zero attribution side effects.
    fn api_usage(&self, cx: &mut gpui::App, p: ApiUsageParams) -> Result<Value, RpcError> {
        let (from, to) = match p.window.as_deref() {
            Some(label) => {
                let range = dashboard_core::usage_analytics::Range::all()
                    .into_iter()
                    .find(|range| range.label() == label)
                    .ok_or_else(|| bad_params("unknown window label; use 1h|1d|1w|1m"))?;
                // `[now − range, now)` (docs/0054 D10: windows anchor at
                // the read instant, half-open).
                let now = now_epoch_ms();
                (now.saturating_sub(range.ms()), now)
            }
            None => (
                p.from_ms.unwrap_or(0),
                // Half-open `[from, to)` — the *default* "now" must still
                // include rows stamped in this very millisecond, hence +1.
                p.to_ms.unwrap_or_else(|| now_epoch_ms().saturating_add(1)),
            ),
        };
        let group_by = p.group_by.as_deref().unwrap_or("op");
        if !matches!(group_by, "op" | "surface" | "trigger" | "status") {
            return Err(bad_params(
                "group_by must be one of op|surface|trigger|status",
            ));
        }
        let source = dashboard_core::usage_records::source(cx);
        let rows = source.records_between(from, to);
        let mut groups: std::collections::BTreeMap<String, UsageGroup> = Default::default();
        for row in &rows {
            let key = match group_by {
                "op" => row.op.to_string(),
                "surface" => row.surface.to_string(),
                "trigger" => row.trigger.to_string(),
                _ => row.status.to_string(),
            };
            let group = groups.entry(key).or_default();
            group.count += 1;
            group.cost_sum = match (group.cost_sum, row.cost) {
                (sum, Some(cost)) => Some(sum.unwrap_or(0) + cost as u64),
                (sum, None) => sum,
            };
            group.remaining_after_min = match (group.remaining_after_min, row.remaining_after) {
                (min, Some(remaining)) => Some(min.unwrap_or(u32::MAX).min(remaining)),
                (min, None) => min,
            };
            if row.network_error || row.status == 0 {
                group.errors += 1;
            }
            if row.etag_304 {
                group.etag_304 += 1;
            }
        }
        // The in-window count is exact for the store; older rows may have
        // fallen off the retention cap, so the true total is unknowable
        // (D5: null, never a guess).
        Ok(json!({
            "freshness": {"age_s": Some(0), "source": FreshnessSource::Cache},
            "truncation": {"returned": rows.len(), "total_known": None::<u64>},
            "from_ms": from,
            "to_ms": to,
            "group_by": group_by,
            "high_water": source.high_water(),
            "groups": groups.into_iter().map(|(key, g)| json!({
                "key": key,
                "count": g.count,
                "cost_sum": g.cost_sum,
                "remaining_after_min": g.remaining_after_min,
                "errors": g.errors,
                "etag_304": g.etag_304,
            })).collect::<Vec<_>>(),
        }))
    }
}

#[derive(Default)]
struct UsageGroup {
    count: u64,
    cost_sum: Option<u64>,
    remaining_after_min: Option<u32>,
    errors: u64,
    etag_304: u64,
}

/// Inventory freshness (docs/0034 D2): a disk-seeded age is honest and
/// `Disk`; a session-held inventory has no aggregate stamp — `null`, not
/// a guess.
fn inventory_freshness(snapshot_age: Option<std::time::Duration>) -> Freshness {
    match snapshot_age {
        Some(age) => Freshness {
            age_s: Some(age.as_secs()),
            source: FreshnessSource::Disk,
        },
        None => Freshness {
            age_s: None,
            source: FreshnessSource::Cache,
        },
    }
}

/// At the snapshot-run cap the true total is unknowable (§6's
/// cursor-catch-up caveat); below it, the inventory is the whole set.
fn inventory_total_known(repos_len: usize) -> Option<u64> {
    if repos_len >= REPO_INVENTORY_SNAPSHOT_CAP {
        None
    } else {
        Some(repos_len as u64)
    }
}

fn auth_name(auth: &crate::service::AuthStatus) -> &'static str {
    match auth {
        crate::service::AuthStatus::Connected => "connected",
        crate::service::AuthStatus::Disconnected => "disconnected",
        crate::service::AuthStatus::ConnectFlow(_) => "connect_flow",
        crate::service::AuthStatus::TokenRejected => "token_rejected",
        crate::service::AuthStatus::KeychainError(_) => "keychain_error",
    }
}

/// Structured not-held (D5/§6): still a success response, still carries
/// the envelope — an agent must be able to distinguish "nothing open"
/// from "never fetched".
fn not_held(repo: &str, reason: &str) -> Value {
    json!({
        "freshness": {"age_s": None::<u64>, "source": FreshnessSource::Cache},
        "truncation": {"returned": 0, "total_known": None::<u64>},
        "repo": repo,
        "held": false,
        "not_held": {
            "reason": reason,
            "hint": "rows land when the repo view opens; the phase-2 refresh op would \
                     fetch on demand (docs/0068 §7)",
        },
    })
}

fn bad_params(message: &str) -> RpcError {
    RpcError::new(error_code::BAD_PARAMS, message)
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Notification, Subject};
    use crate::auth::{StoredCredential, TokenStore};
    use crate::drafts::DraftStore;
    use crate::gql::{Author, Issue, IssueSummary, Owner, PrSummary, PullRequest, Repo};
    use crate::http::SharedToken;
    use crate::service::GithubState;
    use dashboard_core::usage_records::{
        MemorySink, Recorder, UsageRecord, UsageSink as _, UsageSource as _,
    };
    use gpui::{AppContext as _, TestAppContext};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    // -- silent transport: the zero-flight proof ----------------------------

    /// Any call fails the test — phase-1 ops read held state only.
    #[derive(Default)]
    struct SilentHttp(AtomicUsize);

    impl crate::http::Http for SilentHttp {
        fn request(
            &self,
            _verb: crate::http::Verb,
            _url: &str,
            _body: Option<&str>,
            _etag: Option<&str>,
        ) -> Result<crate::http::HttpResult, crate::http::HttpError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(crate::http::HttpError("agent ops must not fly".into()))
        }

        fn post_graphql(
            &self,
            _query: &str,
            _variables: &serde_json::Value,
        ) -> Result<crate::http::HttpResult, crate::http::HttpError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(crate::http::HttpError("agent ops must not fly".into()))
        }
    }

    struct StubTokenStore(Option<StoredCredential>);

    impl StubTokenStore {
        fn connected() -> Self {
            Self(Some(StoredCredential {
                access_token: "token".into(),
                refresh_token: Some("refresh".into()),
                access_expires_at: Some(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        + 3600,
                ),
            }))
        }
    }

    impl TokenStore for StubTokenStore {
        fn save(&self, _cred: &StoredCredential) -> Result<(), String> {
            Err("not used".into())
        }
        fn load(&self) -> Result<Option<StoredCredential>, String> {
            Ok(self.0.clone())
        }
        fn delete(&self) -> Result<(), String> {
            Err("not used".into())
        }
    }

    fn slots() -> (SharedToken, SharedToken) {
        (
            Arc::new(std::sync::RwLock::new(None)),
            Arc::new(std::sync::RwLock::new(None)),
        )
    }

    fn ops_entity(cx: &mut TestAppContext) -> (GithubAgentOps, Arc<SilentHttp>, Arc<MemorySink>) {
        let http = Arc::new(SilentHttp(AtomicUsize::new(0)));
        let sink = Arc::new(MemorySink::new(256));
        let (token, refresh) = slots();
        let entity = cx.new(|_| {
            GithubState::with_recorder(
                http.clone(),
                Arc::new(StubTokenStore::connected()),
                token,
                refresh,
                crate::service::GithubCache::at_tmp("agent-ops"),
                DraftStore::at_tmp("agent-ops"),
                Recorder::new("github", sink.clone()),
            )
        });
        (GithubAgentOps::new(entity), http, sink)
    }

    fn repo(key: &str, owner: &str) -> Repo {
        Repo {
            key: key.into(),
            database_id: None,
            name: key.split('/').next_back().unwrap_or(key).into(),
            full_name: key.into(),
            owner: Owner {
                login: owner.into(),
            },
            description: None,
            private: false,
            is_archived: false,
            node_id: None,
            has_issues_enabled: true,
            pushed_at: None,
            prs: PrSummary {
                total_count: 0,
                nodes: vec![],
            },
            issues: IssueSummary {
                total_count: 0,
                nodes: vec![],
            },
        }
    }

    fn pr(n: i64) -> PullRequest {
        PullRequest {
            key: format!("o/r#{n}"),
            database_id: None,
            number: n,
            title: format!("PR {n}"),
            draft: false,
            state: None,
            author: Some(Author {
                login: "someone".into(),
                avatar_url: String::new(),
            }),
            updated_at: None,
            url: String::new(),
            status: None,
            labels: None,
            review_requests: None,
        }
    }

    fn issue(n: i64) -> Issue {
        Issue {
            key: format!("o/r#{n}"),
            database_id: None,
            number: n,
            title: format!("Issue {n}"),
            author: None,
            updated_at: None,
            url: String::new(),
            labels: None,
        }
    }

    fn notification(id: &str, repo: &str) -> Notification {
        Notification {
            id: id.into(),
            subject: Subject {
                title: format!("subject {id}"),
                kind: "PullRequest".into(),
                url: None,
                latest_comment_url: None,
            },
            repository: crate::api::RepoRef {
                full_name: repo.into(),
            },
            updated_at: None,
            unread: true,
            reason: Some("review_requested".into()),
        }
    }

    fn rec(op: &str, surface: &str, trigger: &str, status: u16, cost: Option<u32>) -> UsageRecord {
        UsageRecord {
            module: "github".into(),
            seq: 0,
            ts_epoch_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            transport: dashboard_core::usage_records::Transport::Rest,
            op: op.into(),
            surface: surface.into(),
            trigger: trigger.into(),
            ident: None,
            status,
            etag_304: status == 304,
            cost,
            remaining_after: Some(4000),
            limit_after: Some(5000),
            cycle_reset_epoch: None,
            duration_ms: 10,
            queue_ms: None,
            resp_bytes: None,
            network_error: false,
        }
    }

    fn install_ledger(cx: &mut TestAppContext, sink: &Arc<MemorySink>) {
        cx.update(|cx| {
            dashboard_core::usage_records::init_with(cx, sink.clone());
        });
    }

    // -- the acceptance matrix ----------------------------------------------

    #[gpui::test]
    fn ops_list_covers_phase_1(cx: &mut TestAppContext) {
        let (provider, _, _) = ops_entity(cx);
        assert_eq!(
            provider.ops(),
            &[
                OP_APP_STATUS,
                OP_LIST_REPOS,
                OP_NOTIFICATIONS_UNREAD,
                OP_REPO_PRS,
                OP_REPO_ISSUES,
                OP_API_USAGE,
            ][..]
        );
    }

    #[gpui::test]
    fn every_op_is_flight_free_and_carries_the_envelope(cx: &mut TestAppContext) {
        let (provider, http, sink) = ops_entity(cx);
        install_ledger(cx, &sink);
        cx.update(|cx| {
            provider.state.update(cx, |s, _| {
                s.repos = vec![repo("o/r", "o")];
                s.notifications = vec![notification("n1", "o/r")];
                s.rows_landed.insert("o/r".into(), Instant::now());
            });
        });

        let requests: Vec<(&'static str, Value)> = vec![
            (OP_APP_STATUS, json!({})),
            (OP_LIST_REPOS, json!({})),
            (OP_NOTIFICATIONS_UNREAD, json!({})),
            (OP_REPO_PRS, json!({"repo": "o/r"})),
            (OP_REPO_ISSUES, json!({"repo": "o/r"})),
            (OP_API_USAGE, json!({"group_by": "op"})),
        ];
        for (op, params) in requests {
            let result = cx
                .update(|cx| provider.handle(cx, op, &params))
                .unwrap_or_else(|err| panic!("{op} failed: {}", err.message));
            assert!(
                result["freshness"]["source"].is_string(),
                "{op} must carry freshness"
            );
            assert!(
                result["truncation"]["returned"].is_u64(),
                "{op} must carry truncation"
            );
        }
        assert_eq!(http.0.load(Ordering::SeqCst), 0, "agent ops never fly");
        assert!(
            sink.snapshot().is_empty(),
            "agent ops never book usage records"
        );
    }

    #[gpui::test]
    fn app_status_reports_auth_and_budget_snapshot(cx: &mut TestAppContext) {
        let (provider, _, sink) = ops_entity(cx);
        install_ledger(cx, &sink);
        let result = cx
            .update(|cx| provider.handle(cx, OP_APP_STATUS, &json!({})))
            .unwrap();
        assert_eq!(result["connected"], json!(true));
        assert_eq!(result["auth"], json!("connected"));
        assert!(result["usage_high_water"].is_u64());
        assert_eq!(result["truncation"]["total_known"], json!(1));
    }

    #[gpui::test]
    fn list_repos_filters_by_org_and_reports_inventory_total(cx: &mut TestAppContext) {
        let (provider, _, sink) = ops_entity(cx);
        install_ledger(cx, &sink);
        cx.update(|cx| {
            provider.state.update(cx, |s, _| {
                s.repos = vec![repo("acme/x", "acme"), repo("o/r", "o")];
                s.snapshot_age = Some(Duration::from_secs(42));
            });
        });

        let result = cx
            .update(|cx| provider.handle(cx, OP_LIST_REPOS, &json!({})))
            .unwrap();
        assert_eq!(result["truncation"]["returned"], json!(2));
        assert_eq!(result["truncation"]["total_known"], json!(2));
        assert_eq!(result["freshness"], json!({"age_s": 42, "source": "disk"}));

        let result = cx
            .update(|cx| provider.handle(cx, OP_LIST_REPOS, &json!({"org": "acme"})))
            .unwrap();
        assert_eq!(result["truncation"]["returned"], json!(1));
        assert_eq!(result["repos"][0]["key"], json!("acme/x"));
        assert_eq!(result["repos"][0]["rows_held"], json!(false));
    }

    #[test]
    fn inventory_at_the_snapshot_cap_reports_unknown_total() {
        assert_eq!(inventory_total_known(299), Some(299));
        assert_eq!(inventory_total_known(300), None);
        assert_eq!(inventory_total_known(500), None);
    }

    #[gpui::test]
    fn notifications_narrow_by_repo(cx: &mut TestAppContext) {
        let (provider, _, sink) = ops_entity(cx);
        install_ledger(cx, &sink);
        cx.update(|cx| {
            provider.state.update(cx, |s, _| {
                s.notifications = vec![notification("n1", "o/r"), notification("n2", "acme/x")];
                s.notifications_age = Some(Duration::from_secs(7));
            });
        });

        let result = cx
            .update(|cx| provider.handle(cx, OP_NOTIFICATIONS_UNREAD, &json!({"repo": "acme/x"})))
            .unwrap();
        assert_eq!(result["truncation"]["returned"], json!(1));
        assert_eq!(result["truncation"]["total_known"], json!(2));
        assert_eq!(result["notifications"][0]["id"], json!("n2"));
        assert_eq!(result["freshness"], json!({"age_s": 7, "source": "disk"}));
    }

    #[gpui::test]
    fn repo_prs_distinguish_not_held_from_empty(cx: &mut TestAppContext) {
        let (provider, _, sink) = ops_entity(cx);
        install_ledger(cx, &sink);
        cx.update(|cx| {
            provider.state.update(cx, |s, _| {
                let mut held = repo("o/r", "o");
                held.prs = PrSummary {
                    total_count: 2,
                    nodes: vec![pr(1), pr(2)],
                };
                held.issues = IssueSummary {
                    total_count: 1,
                    nodes: vec![issue(1)],
                };
                // Landed, zero open: "nothing open", not "never fetched".
                let mut quiet = repo("acme/quiet", "acme");
                quiet.issues = IssueSummary {
                    total_count: 0,
                    nodes: vec![],
                };
                s.repos = vec![held, quiet];
                s.rows_landed.insert("o/r".into(), Instant::now());
                s.rows_landed.insert("acme/quiet".into(), Instant::now());
            });
        });

        // Held: exact total known, rows returned, session freshness.
        let result = cx
            .update(|cx| provider.handle(cx, OP_REPO_PRS, &json!({"repo": "o/r"})))
            .unwrap();
        assert_eq!(result["held"], json!(true));
        assert_eq!(result["truncation"]["returned"], json!(2));
        assert_eq!(result["truncation"]["total_known"], json!(2));
        assert!(result["freshness"]["age_s"].is_u64());
        assert_eq!(result["rows"][0]["number"], json!(1));

        // Issues held for the same repo — the rows kind is a parameter.
        let result = cx
            .update(|cx| provider.handle(cx, OP_REPO_ISSUES, &json!({"repo": "o/r"})))
            .unwrap();
        assert_eq!(result["held"], json!(true));
        assert_eq!(result["truncation"]["returned"], json!(1));
        assert_eq!(result["truncation"]["total_known"], json!(1));
        assert_eq!(result["rows"][0]["number"], json!(1));

        // Landed with zero rows must NOT read as not_held.
        let result = cx
            .update(|cx| provider.handle(cx, OP_REPO_ISSUES, &json!({"repo": "acme/quiet"})))
            .unwrap();
        assert_eq!(result["held"], json!(true));
        assert_eq!(result["truncation"]["returned"], json!(0));
        assert_eq!(result["truncation"]["total_known"], json!(0));

        // Never landed: structured not_held, still a success.
        let result = cx
            .update(|cx| provider.handle(cx, OP_REPO_PRS, &json!({"repo": "acme/late"})))
            .unwrap();
        assert_eq!(result["held"], json!(false));
        assert!(result["not_held"]["hint"].is_string());
    }

    #[gpui::test]
    fn repo_rows_reject_non_open_state(cx: &mut TestAppContext) {
        let (provider, _, sink) = ops_entity(cx);
        install_ledger(cx, &sink);
        let error = cx
            .update(|cx| {
                provider.handle(cx, OP_REPO_PRS, &json!({"repo": "o/r", "state": "closed"}))
            })
            .unwrap_err();
        assert_eq!(error.code, error_code::BAD_PARAMS);
    }

    #[gpui::test]
    fn api_usage_groups_rows_and_sums_cost(cx: &mut TestAppContext) {
        let (provider, _, sink) = ops_entity(cx);
        sink.record(rec("pr-detail", "pr_page", "view_open", 200, Some(1)));
        sink.record(rec("pr-detail", "pr_page", "view_open", 200, Some(1)));
        sink.record(rec("notifications", "card", "background", 304, None));
        install_ledger(cx, &sink);

        let result = cx
            .update(|cx| provider.handle(cx, OP_API_USAGE, &json!({"group_by": "op"})))
            .unwrap();
        let groups = result["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        let pr_detail = groups
            .iter()
            .find(|g| g["key"] == json!("pr-detail"))
            .unwrap();
        assert_eq!(pr_detail["count"], json!(2));
        assert_eq!(pr_detail["cost_sum"], json!(2));
        assert_eq!(pr_detail["remaining_after_min"], json!(4000));
        let notifications = groups
            .iter()
            .find(|g| g["key"] == json!("notifications"))
            .unwrap();
        assert_eq!(notifications["etag_304"], json!(1));

        // Agent-attribution readiness: group by surface works the same.
        let result = cx
            .update(|cx| provider.handle(cx, OP_API_USAGE, &json!({"group_by": "surface"})))
            .unwrap();
        assert_eq!(result["groups"].as_array().unwrap().len(), 2);
    }

    #[gpui::test]
    fn api_usage_rejects_unknown_grouping_and_window(cx: &mut TestAppContext) {
        let (provider, _, sink) = ops_entity(cx);
        install_ledger(cx, &sink);
        for params in [json!({"group_by": "author"}), json!({"window": "5m"})] {
            let error = cx
                .update(|cx| provider.handle(cx, OP_API_USAGE, &params))
                .unwrap_err();
            assert_eq!(error.code, error_code::BAD_PARAMS);
        }
    }

    #[gpui::test]
    fn registry_resolution_reaches_the_provider(cx: &mut TestAppContext) {
        // The registration path (lib.rs init) resolves ops per request.
        let (provider, _, sink) = ops_entity(cx);
        install_ledger(cx, &sink);
        cx.update(|cx| {
            dashboard_agent::register_ops(
                cx,
                Arc::new(GithubAgentOps::new(provider.state.clone())),
            );
        });
        let result = cx.update(|cx| {
            use dashboard_agent::OpsRegistry;
            let registry = cx.global::<OpsRegistry>();
            let resolved = registry.resolve(OP_APP_STATUS).unwrap();
            resolved.handle(cx, OP_APP_STATUS, &json!({}))
        });
        assert!(result.unwrap()["connected"].is_boolean());
    }
}
