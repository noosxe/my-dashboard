//! GitHub REST v3 leftovers (docs/0005 §3.3, docs/0007 D6): the
//! notifications endpoints — the only data that stays on REST, since
//! notifications have no GraphQL API upstream (community discussion
//! #13056). Everything else moved to the GraphQL snapshot (docs/0007).
//!
//! Payload structs are deliberately minimal and tolerant: unknown fields
//! are ignored (serde default). GitHub's JSON is snake_case — no rename
//! needed.

use serde::{Deserialize, Serialize};

pub const API_BASE: &str = "https://api.github.com";

/// `GET /notifications` / repo-scoped item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub subject: Subject,
    pub repository: RepoRef,
    pub updated_at: Option<String>,
    #[serde(default = "default_true")]
    pub unread: bool,
    /// Why the notification arrived ("comment", "review_requested", …) —
    /// present in both the list payload and the thread GET (docs/0026 §2.1);
    /// the page renders it raw.
    #[serde(default)]
    pub reason: Option<String>,
}

/// The thread schema (docs/0026 §2.1): list items and the single-thread
/// GET are the same shape, so one struct serves both — the notification
/// page parses the `GET /notifications/threads/{id}` body straight into
/// [`Notification`].
pub type Thread = Notification;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subject {
    pub title: String,
    /// GitHub calls this `type` ("PullRequest", "Issue", …) — renamed.
    #[serde(rename = "type")]
    pub kind: String,
    /// The subject's API URL (`…/repos/{o}/{r}/issues/{n}` etc.) —
    /// present in both payloads (docs/0026 §2.2); the web-URL derivation
    /// (D5) and the Issue/PR hand-off (D6) parse it.
    #[serde(default)]
    pub url: Option<String>,
    /// The subject's latest-comment API URL (docs/0026 D9, #154):
    /// `…/repos/{o}/{r}/issues/comments/{id}` — the page parses the REST
    /// comment id out of it for the native-page jump (D9);
    /// [`latest_comment_id`] is the pure parser.
    #[serde(default)]
    pub latest_comment_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoRef {
    /// Grouping labels (UI layer) and the per-repo derivation key
    /// (docs/0007 D6: the repo GroupBox derives from the global list).
    pub full_name: String,
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Endpoint builders — per_page=100 everywhere, page-driven pagination.
// (The global endpoint is server-clamped to 50/page; pagination absorbs
// it — docs/0007 §9.)
// ---------------------------------------------------------------------------

fn page_query(page: u32) -> String {
    format!("per_page=100&page={page}")
}

pub fn notifications_url(page: u32) -> String {
    format!("{API_BASE}/notifications?{}", page_query(page))
}

/// The usage-calculator's free reconcile probe (docs/0050 D3): per
/// GitHub docs, `GET /rate_limit` does not count against the primary
/// limit — its body carries the full per-bucket breakdown the ledger
/// folds (`resources.core`).
pub fn rate_limit_url() -> String {
    format!("{API_BASE}/rate_limit")
}
pub fn repo_notifications_url(owner: &str, repo: &str, page: u32) -> String {
    format!(
        "{API_BASE}/repos/{owner}/{repo}/notifications?{}",
        page_query(page)
    )
}

/// The single notification thread (docs/0026 D2/D3): one URL serves the
/// page-owned GET (`Kind::ThreadDetail`) and the REST mutation lane
/// (`Kind::ThreadMutation`) — the verb picks the operation (PATCH =
/// mark as read → 205, DELETE = mark as done → 204, docs/0026 §2.3).
pub fn thread_url(id: &str) -> String {
    format!("{API_BASE}/notifications/threads/{id}")
}

/// Kind-aware API→web URL derivation (docs/0026 D5) — pure, table-tested.
/// `None` = no faithful web equivalent (unknown kind, malformed or foreign
/// URL, non-pageable subject like `releases/{id}`): the page falls back to
/// the repo's notifications page (docs/0015 target).
pub fn web_url(api_url: &str) -> Option<String> {
    let segs: Vec<&str> = api_url
        .strip_prefix(API_BASE)?
        .trim_start_matches('/')
        .split('/')
        .collect();
    match segs.as_slice() {
        ["repos", o, r, "issues", n]
        | ["repos", o, r, "pulls", n]
        | ["repos", o, r, "discussions", n]
            if !n.is_empty() =>
        {
            // API segment `pulls` is plural; the canonical web path is
            // `/pull/N` singular (issue #173) — `commits`→`commit` below
            // follows the same singularization.
            let kind = if segs[3] == "pulls" { "pull" } else { segs[3] };
            Some(format!("https://github.com/{o}/{r}/{kind}/{n}"))
        }
        ["repos", o, r, "commits", sha] => Some(format!("https://github.com/{o}/{r}/commit/{sha}")),
        _ => None,
    }
}

/// The Issue/PR hand-off target (docs/0026 D6) — pure, table-tested:
/// `owner/repo` + number parsed from the subject's API URL. Only those
/// two kinds hand off to the native pages (docs/0026 §2.5).
pub fn subject_handoff(kind: &str, api_url: Option<&str>) -> Option<(String, i64)> {
    let segs: Vec<&str> = api_url?
        .strip_prefix(API_BASE)?
        .trim_start_matches('/')
        .split('/')
        .collect();
    let (o, r, n) = match (kind, segs.as_slice()) {
        ("Issue", ["repos", o, r, "issues", n]) => (*o, *r, *n),
        ("PullRequest", ["repos", o, r, "pulls", n]) => (*o, *r, *n),
        _ => return None,
    };
    Some((format!("{o}/{r}"), n.parse().ok()?))
}
/// The latest-comment anchor (docs/0026 D9, #154): the REST comment id
/// parsed from the thread's `latest_comment_url`. Upstream shape
/// (github/rest-api-description, `thread.subject`):
/// `…/repos/{o}/{r}/issues/comments/{id}` — the issue-comment collection
/// item, which serves both Issue and PR conversation comments. Anything
/// else yields `None`: PR review comments (`…/pulls/comments/{id}`) have
/// no in-app surface to anchor to (no review-thread rendering), and
/// subject/foreign/malformed URLs were never comments. Pure, table-tested.
pub fn latest_comment_id(url: Option<&str>) -> Option<i64> {
    let segs: Vec<&str> = url?
        .strip_prefix(API_BASE)?
        .trim_start_matches('/')
        .split('/')
        .collect();
    match segs.as_slice() {
        ["repos", _, _, "issues", "comments", id] if !id.is_empty() => id.parse().ok(),
        _ => None,
    }
}

/// `GET /repos/{o}/{r}/pulls/{n}/files` item (docs/0016 §3 D8):
/// `patch` is absent for binary and oversized files — the renderer
/// shows the placeholder row then. Unknown extra fields ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrFile {
    pub filename: String,
    /// Change type: `added`/`modified`/`removed`/`renamed` — raw string
    /// through (future kinds degrade to a muted badge, 0007 D3).
    pub status: String,
    pub additions: i64,
    pub deletions: i64,
    pub changes: i64,
    #[serde(default)]
    pub patch: Option<String>,
}

pub fn pr_files_url(owner: &str, repo: &str, number: i64, page: u32) -> String {
    format!(
        "{API_BASE}/repos/{owner}/{repo}/pulls/{number}/files?{}",
        page_query(page)
    )
}

/// docs/0046 D5 — `GET /repos/{o}/{r}/contents/.github/ISSUE_TEMPLATE/config.yml`:
/// the issue creator's `blank_issues_enabled` source. No query
/// parameters — a one-shot plain GET (the [`crate::service::Kind::RepoTemplatesConfig`] lane).
pub fn issue_template_config_url(owner: &str, repo: &str) -> String {
    format!("{API_BASE}/repos/{owner}/{repo}/contents/.github/ISSUE_TEMPLATE/config.yml")
}
/// Percent-encode one query-parameter value: the RFC 3986 unreserved set
/// passes through, everything else hex-escapes. Check and job names may
/// carry spaces, parens, `&`, `#`, `%` — each builder below funnels its
/// free-text pieces through here (docs/0039 D1).
fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `GET /repos/{o}/{r}/commits/{sha}/check-runs?check_name=` — step 1 of
/// the log chain (docs/0039 D1): the check run for the expanded row,
/// matched server-side, with its suite id and app slug.
pub fn check_runs_for_sha_url(owner: &str, repo: &str, sha: &str, check_name: &str) -> String {
    format!(
        "{API_BASE}/repos/{owner}/{repo}/commits/{sha}/check-runs?check_name={}",
        encode_query_value(check_name)
    )
}

/// `GET /repos/{o}/{r}/actions/runs?check_suite_id=` — step 2: the one
/// workflow run behind the check suite (docs/0039 D1).
pub fn workflow_runs_by_suite_url(owner: &str, repo: &str, suite_id: u64) -> String {
    format!("{API_BASE}/repos/{owner}/{repo}/actions/runs?check_suite_id={suite_id}")
}

/// `GET /repos/{o}/{r}/actions/runs/{run_id}/jobs?per_page=100` — step 3:
/// the run's jobs, name-matched client-side for the job id (docs/0039 D1).
/// 100 is the endpoint's documented max (default 30, #757); further pages
/// append `&page=N` (see `run_jobs`). `filter` stays at its `latest` default.
pub fn run_jobs_url(owner: &str, repo: &str, run_id: u64) -> String {
    format!("{API_BASE}/repos/{owner}/{repo}/actions/runs/{run_id}/jobs?per_page=100")
}

/// docs/0066 D2 — the gated-runs sweep: server-filtered to runs waiting on
/// approval for one branch. Rows are filtered client-side by `head_sha`
/// (branch names are not unique across forks; the SHA is the guarantee —
/// live-verified 2026-10-05 that the endpoint honors the filters and
/// carries `head_sha` on every row).
pub fn gated_runs_url(owner: &str, repo: &str, head_branch: &str) -> String {
    format!(
        "{API_BASE}/repos/{owner}/{repo}/actions/runs?branch={}&event=pull_request&status=action_required&per_page=50",
        encode_query_value(head_branch)
    )
}

/// docs/0066 D4 — the gate probe: non-empty ⇒ environment gate (required
/// reviewers), empty ⇒ fork/external-contributor gate. Live-verified
/// 2026-10-05: `[]` on worker-mcp's fork-gated runs.
pub fn run_pending_deployments_url(owner: &str, repo: &str, run_id: u64) -> String {
    format!("{API_BASE}/repos/{owner}/{repo}/actions/runs/{run_id}/pending_deployments")
}

/// docs/0066 D4 — the fork gate's only disposition: approve (201, empty
/// body). Upstream has no reject for this gate (§2.2).
pub fn approve_run_url(owner: &str, repo: &str, run_id: u64) -> String {
    format!("{API_BASE}/repos/{owner}/{repo}/actions/runs/{run_id}/approve")
}

/// docs/0066 D2 fallback — suites for a head SHA: a gated run has a check
/// suite (with `conclusion: "action_required"`) even though it has no
/// check runs; live-verified 2026-10-05 on worker-mcp PR #69's head.
pub fn check_suites_for_sha_url(owner: &str, repo: &str, sha: &str) -> String {
    format!("{API_BASE}/repos/{owner}/{repo}/commits/{sha}/check-suites")
}

/// docs/0066 §2.2 read side — prior deployment reviews for a gated run
/// (environment gate only; the fork gate has no approvals read side).
pub fn run_approvals_url(owner: &str, repo: &str, run_id: u64) -> String {
    format!("{API_BASE}/repos/{owner}/{repo}/actions/runs/{run_id}/approvals")
}

/// `GET /repos/{o}/{r}/actions/jobs/{job_id}/logs` — step 4: a 302 to a
/// 1-minute signed URL; the transport follows it and hands back the
/// plain-text log (docs/0039 §2.1, D2).
pub fn job_logs_url(owner: &str, repo: &str, job_id: u64) -> String {
    format!("{API_BASE}/repos/{owner}/{repo}/actions/jobs/{job_id}/logs")
}

/// Step-1 payload (docs/0039 D1): the server-filtered check runs for the
/// expanded row's name.
#[derive(Debug, Clone, Deserialize)]
pub struct CheckRunsPage {
    #[serde(default)]
    pub check_runs: Vec<CheckRunNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckRunNode {
    pub name: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub check_suite: Option<CheckSuiteRef>,
    #[serde(default)]
    pub app: Option<AppRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckSuiteRef {
    pub id: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppRef {
    #[serde(default)]
    pub slug: Option<String>,
}

/// Step-2 payload: the suite's workflow runs (exactly one per suite).
#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowRunsPage {
    #[serde(default)]
    pub workflow_runs: Vec<WorkflowRunNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowRunNode {
    pub id: u64,
}

/// docs/0066 D2: the sweep's payload (`workflow_runs` array, same envelope
/// as [`WorkflowRunsPage`]).
#[derive(Debug, Clone, Deserialize)]
pub struct GatedRunsPage {
    #[serde(default)]
    pub workflow_runs: Vec<GatedRunNode>,
}

/// One gated workflow run. Live-verified 2026-10-05 (worker-mcp runs
/// 37300500213/37300500222): `status` reads `"completed"` while the run is
/// held — `action_required` is a *conclusion*, not a status. Fields beyond
/// these are tolerated (`serde(default)`), the upstream object is wide.
#[derive(Debug, Clone, Deserialize)]
pub struct GatedRunNode {
    pub id: u64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub head_sha: Option<String>,
    #[serde(default)]
    pub run_number: Option<i64>,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

/// docs/0066 D4: one environment awaiting review (reference response shape;
/// a bare array — no envelope struct). `current_user_can_approve` is the
/// cheap writability signal D6 hoped existed: it gates the buttons.
#[derive(Debug, Clone, Deserialize)]
pub struct PendingDeploymentNode {
    #[serde(default)]
    pub environment: Option<PendingEnvironment>,
    #[serde(default)]
    pub current_user_can_approve: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PendingEnvironment {
    pub id: u64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub html_url: Option<String>,
}

/// docs/0066 D4: one recorded deployment review (`GET .../approvals`,
/// reference response shape — a bare array). Env-gate only; the fork gate
/// has no approvals read side.
#[derive(Debug, Clone, Deserialize)]
pub struct RunApprovalNode {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub user: Option<RunApprovalUser>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunApprovalUser {
    #[serde(default)]
    pub login: Option<String>,
}

/// docs/0066 D2 fallback payload: check suites for a head SHA (live
/// verified: suites carry `conclusion: "action_required"` pre-approval).
#[derive(Debug, Clone, Deserialize)]
pub struct CheckSuitesPage {
    #[serde(default)]
    pub check_suites: Vec<CheckSuiteNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckSuiteNode {
    pub id: u64,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub head_sha: Option<String>,
}

/// Step-3 payload: the run's jobs, name-matched for the job id.
#[derive(Debug, Clone, Deserialize)]
pub struct JobsPage {
    #[serde(default)]
    pub total_count: u64,
    #[serde(default)]
    pub jobs: Vec<JobNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JobNode {
    pub id: u64,
    pub name: String,
    /// Live-watch fields (docs/0076 D7): additive parse — minimal
    /// payloads (older fixtures, third-party summaries) still parse.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub steps: Vec<StepNode>,
}

/// One step of a workflow job (docs/0076 D2): the live tick payload's
/// per-step row. Same additive-parse convention as [`JobNode`].
#[derive(Debug, Clone, Deserialize)]
pub struct StepNode {
    pub name: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub number: i64,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_notification_with_type_rename() {
        let n: Notification = serde_json::from_str(
            r#"{"id":"19","unread":true,
                "subject":{"title":"PR #1","type":"PullRequest","url":"https://…"},
                "repository":{"full_name":"o/r","id":1},
                "updated_at":"2026-08-29T08:00:00Z",
                "reason":"author"}"#,
        )
        .unwrap();
        assert_eq!(n.subject.kind, "PullRequest");
        assert!(n.unread);
    }

    #[test]
    fn parses_job_steps_and_survives_minimal_payloads() {
        let full: JobNode = serde_json::from_str(
            r#"{"id":9,"name":"build","status":"in_progress",
            "conclusion":null,
            "started_at":"2020-01-20T17:42:40Z",
            "completed_at":null,
            "steps":[{"name":"Set up job","status":"completed","conclusion":"success",
                      "number":1,"started_at":"2020-01-20T09:42:40.000-08:00",
                      "completed_at":"2020-01-20T09:42:41.000-08:00"}]}"#,
        )
        .unwrap();
        assert_eq!(full.status.as_deref(), Some("in_progress"));
        assert_eq!(full.steps.len(), 1);
        assert_eq!(full.steps[0].conclusion.as_deref(), Some("success"));

        // Additive parse (docs/0076 D7): the chain's older fixtures carry
        // only id+name and must keep parsing.
        let minimal: JobNode =
            serde_json::from_str(r#"{"id":1,"name":"use-existing-src"}"#).unwrap();
        assert!(minimal.status.is_none());
        assert!(minimal.steps.is_empty());
    }
    #[test]
    fn log_chain_urls_are_well_formed_and_encoded() {
        // docs/0039 D1: the four documented endpoints. The check name is
        // free text — matrix names carry spaces and parens.
        assert_eq!(
            check_runs_for_sha_url("o", "r", "abc123", "build (linux)"),
            format!(
                "{API_BASE}/repos/o/r/commits/abc123/check-runs?check_name=build%20%28linux%29"
            )
        );
        assert_eq!(
            check_runs_for_sha_url("o", "r", "abc123", "a&b#c?d%e"),
            format!("{API_BASE}/repos/o/r/commits/abc123/check-runs?check_name=a%26b%23c%3Fd%25e")
        );
        assert_eq!(
            workflow_runs_by_suite_url("o", "r", 42),
            format!("{API_BASE}/repos/o/r/actions/runs?check_suite_id=42")
        );
        assert_eq!(
            run_jobs_url("o", "r", 7),
            format!("{API_BASE}/repos/o/r/actions/runs/7/jobs?per_page=100")
        );
        assert_eq!(
            job_logs_url("o", "r", 9),
            format!("{API_BASE}/repos/o/r/actions/jobs/9/logs")
        );
    }

    #[test]
    fn log_chain_dto_parse_minimal_payloads() {
        let page: CheckRunsPage = serde_json::from_str(
            r#"{"total_count": 1, "check_runs": [
                {"name": "build (linux)", "status": "COMPLETED", "conclusion": "FAILURE",
                 "check_suite": {"id": 5}, "app": {"slug": "github-actions"}}
            ]}"#,
        )
        .unwrap();
        assert_eq!(page.check_runs.len(), 1);
        assert_eq!(page.check_runs[0].check_suite.as_ref().unwrap().id, 5);
        assert_eq!(
            page.check_runs[0].app.as_ref().unwrap().slug.as_deref(),
            Some("github-actions")
        );
        let runs: WorkflowRunsPage =
            serde_json::from_str(r#"{"total_count": 1, "workflow_runs": [{"id": 77}]}"#).unwrap();
        assert_eq!(runs.workflow_runs[0].id, 77);
        let jobs: JobsPage = serde_json::from_str(
            r#"{"total_count": 2, "jobs": [
                {"id": 1, "name": "use-existing-src"}, {"id": 2, "name": "build (linux)"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(jobs.jobs.len(), 2);
        assert_eq!(jobs.jobs[1].name, "build (linux)");
    }

    #[test]
    fn parses_pr_file_with_and_without_patch() {
        let with: PrFile = serde_json::from_str(
            r#"{"filename":"src/main.rs","status":"modified",
                "additions":12,"deletions":3,"changes":15,
                "patch":"@@ -1,2 +1,3 @@\n context\n+new line\n-old line"}"#,
        )
        .unwrap();
        assert_eq!(with.filename, "src/main.rs");
        assert_eq!(with.status, "modified");
        assert_eq!(with.additions, 12);
        assert!(with.patch.as_deref().unwrap().starts_with("@@"));

        // Binary/oversized files: `patch` absent, not null.
        let binary: PrFile = serde_json::from_str(
            r#"{"filename":"logo.png","status":"added",
                "additions":0,"deletions":0,"changes":0}"#,
        )
        .unwrap();
        assert_eq!(binary.patch, None);
    }

    #[test]
    fn notification_unread_defaults_true_when_absent() {
        let n: Notification = serde_json::from_str(
            r#"{"id":"2","subject":{"title":"t","type":"Issue","url":"u"},
                "repository":{"full_name":"o/r"}}"#,
        )
        .unwrap();
        assert!(n.unread);
    }

    #[test]
    fn endpoint_urls_shape() {
        assert_eq!(
            notifications_url(2),
            "https://api.github.com/notifications?per_page=100&page=2"
        );
        assert_eq!(
            repo_notifications_url("o", "r", 1),
            "https://api.github.com/repos/o/r/notifications?per_page=100&page=1"
        );
        assert_eq!(
            thread_url("19"),
            "https://api.github.com/notifications/threads/19"
        );
    }

    #[test]
    fn notification_carries_reason_and_subject_urls_when_present() {
        let n: Notification = serde_json::from_str(
            r#"{"id":"19","unread":true,
                "subject":{"title":"PR #1","type":"PullRequest",
                    "url":"https://api.github.com/repos/o/r/pulls/1",
                    "latest_comment_url":"https://api.github.com/repos/o/r/issues/1/comments"},
                "repository":{"full_name":"o/r"},
                "reason":"review_requested"}"#,
        )
        .unwrap();
        assert_eq!(n.reason.as_deref(), Some("review_requested"));
        assert_eq!(
            n.subject.url.as_deref(),
            Some("https://api.github.com/repos/o/r/pulls/1")
        );
        assert!(n.subject.latest_comment_url.is_some());
    }

    #[test]
    fn notification_tolerates_missing_reason_and_subject_urls() {
        let n: Notification = serde_json::from_str(
            r#"{"id":"2","subject":{"title":"t","type":"Issue"},
                "repository":{"full_name":"o/r"}}"#,
        )
        .unwrap();
        assert_eq!(n.reason, None);
        assert_eq!(n.subject.url, None);
        assert_eq!(n.subject.latest_comment_url, None);
    }

    #[test]
    fn web_url_derives_per_subject_kind() {
        let cases: &[(&str, Option<&str>)] = &[
            (
                "https://api.github.com/repos/o/r/issues/42",
                Some("https://github.com/o/r/issues/42"),
            ),
            (
                "https://api.github.com/repos/o/r/pulls/7",
                Some("https://github.com/o/r/pull/7"),
            ),
            (
                "https://api.github.com/repos/o/r/discussions/9",
                Some("https://github.com/o/r/discussions/9"),
            ),
            (
                "https://api.github.com/repos/o/r/commits/abc123",
                Some("https://github.com/o/r/commit/abc123"),
            ),
            // No faithful web page: Release by id, comment sub-paths,
            // foreign hosts, malformed shapes — all fall back.
            ("https://api.github.com/repos/o/r/releases/5", None),
            ("https://api.github.com/repos/o/r/issues/1/comments", None),
            ("https://example.com/repos/o/r/issues/1", None),
            ("https://api.github.com/repos/o/r/issues/", None),
            ("not a url", None),
        ];
        for (input, want) in cases {
            assert_eq!(web_url(input).as_deref(), *want, "web_url({input})");
        }
    }

    #[test]
    fn gated_runs_url_encodes_the_branch() {
        let q = |b: &str| {
            gated_runs_url("o", "r", b)
                .split_once("branch=")
                .unwrap()
                .1
                .split_once("&event=")
                .unwrap()
                .0
                .to_string()
        };
        for (branch, want) in [
            ("main", "main"),
            ("fix/#12", "fix%2F%2312"),
            ("a&b", "a%26b"),
            ("feat+x", "feat%2Bx"),
            ("my branch", "my%20branch"),
            ("100%", "100%25"),
            ("fix/ünï", "fix%2F%C3%BCn%C3%AF"),
        ] {
            assert_eq!(q(branch), want, "branch {branch:?}");
        }
        assert!(gated_runs_url("o", "r", "x#y").ends_with("&per_page=50"));
    }

    #[test]
    fn gated_run_urls_are_well_formed() {
        // docs/0066 D2/D4: the sweep, the gate probe, the fork-gate
        // approve, the D2 fallback suites, and the approvals read side.
        assert_eq!(
            gated_runs_url("o", "r", "update_flake_lock_action"),
            format!(
                "{API_BASE}/repos/o/r/actions/runs?branch=update_flake_lock_action&event=pull_request&status=action_required&per_page=50"
            )
        );
        assert_eq!(
            run_pending_deployments_url("o", "r", 37300500213),
            format!("{API_BASE}/repos/o/r/actions/runs/37300500213/pending_deployments")
        );
        assert_eq!(
            approve_run_url("o", "r", 37300500213),
            format!("{API_BASE}/repos/o/r/actions/runs/37300500213/approve")
        );
        assert_eq!(
            run_approvals_url("o", "r", 7),
            format!("{API_BASE}/repos/o/r/actions/runs/7/approvals")
        );
        assert_eq!(
            check_suites_for_sha_url("o", "r", "58df0f2"),
            format!("{API_BASE}/repos/o/r/commits/58df0f2/check-suites")
        );
    }

    #[test]
    fn gated_run_parses_live_fork_gated_payload() {
        // Live-captured 2026-10-05, noosxe/worker-mcp run 37300500213
        // (PR #69's CI, fork-gated): `status` reads completed while the
        // run is held — `action_required` is a conclusion, not a status.
        // Fields beyond the struct are tolerated (upstream is wide).
        let page: GatedRunsPage = serde_json::from_str(
            r#"{"total_count": 2, "workflow_runs": [
                {"id": 37300500213, "name": "CI",
                 "head_branch": "update_flake_lock_action",
                 "head_sha": "58df0f2221d69d16ac68bc8d6a85c3305121cb9b",
                 "status": "completed", "conclusion": "action_required",
                 "run_number": 138, "event": "pull_request", "run_attempt": 1,
                 "check_suite_id": 101028134107,
                 "created_at": "2026-10-05T11:03:38Z"},
                {"id": 999, "head_sha": "0000000000000000000000000000000000000000"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(page.workflow_runs.len(), 2);
        let run = &page.workflow_runs[0];
        assert_eq!(run.id, 37300500213);
        assert_eq!(run.name.as_deref(), Some("CI"));
        assert_eq!(run.conclusion.as_deref(), Some("action_required"));
        assert_eq!(run.run_number, Some(138));
        assert_eq!(
            run.head_sha.as_deref(),
            Some("58df0f2221d69d16ac68bc8d6a85c3305121cb9b")
        );
        // The second row proves the SHA filter's input: a run for a
        // different head must not survive `load_gated_runs`' filter.
        assert_eq!(
            page.workflow_runs[1].head_sha.as_deref(),
            Some("0000000000000000000000000000000000000000")
        );
    }

    #[test]
    fn pending_deployments_parse_reference_shape() {
        // Reference response shape (REST reference, fetched 2026-10-05):
        // a bare array; `current_user_can_approve` gates the buttons (D6).
        let pending: Vec<PendingDeploymentNode> = serde_json::from_str(
            r#"[{"environment": {"id": 161088068, "name": "staging",
                  "html_url": "https://github.com/o/r/deployments"},
                 "wait_timer": 30,
                 "current_user_can_approve": true,
                 "reviewers": []},
                {"environment": {"id": 2, "name": "prod"}}]"#,
        )
        .unwrap();
        assert_eq!(pending.len(), 2);
        let env = pending[0].environment.as_ref().unwrap();
        assert_eq!(env.id, 161088068);
        assert_eq!(env.name.as_deref(), Some("staging"));
        assert_eq!(pending[0].current_user_can_approve, Some(true));
        // Absent writability ⇒ None — assumed yes, the 403 copy is the
        // fallback (D6).
        assert_eq!(pending[1].current_user_can_approve, None);
        // Empty array = the fork gate (live-verified on worker-mcp).
        let empty: Vec<PendingDeploymentNode> = serde_json::from_str("[]").unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn run_approvals_parse_reference_shape() {
        // Reference response shape: state + comment + user login.
        let approvals: Vec<RunApprovalNode> = serde_json::from_str(
            r#"[{"state": "approved", "comment": "Ship it!",
                 "user": {"login": "octocat", "id": 1}}]"#,
        )
        .unwrap();
        assert_eq!(approvals[0].state.as_deref(), Some("approved"));
        assert_eq!(
            approvals[0].user.as_ref().unwrap().login.as_deref(),
            Some("octocat")
        );
    }

    #[test]
    fn check_suites_parse_live_gated_payload() {
        // Live-captured 2026-10-05: a gated run has a check suite with
        // `conclusion: "action_required"` even though it has no check
        // runs — the D2 fallback's discovery key.
        let page: CheckSuitesPage = serde_json::from_str(
            r#"{"total_count": 2, "check_suites": [
                {"id": 101028134107, "status": "completed",
                 "conclusion": "action_required",
                 "head_sha": "58df0f2221d69d16ac68bc8d6a85c3305121cb9b",
                 "app": {"slug": "github-actions"}}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            page.check_suites[0].conclusion.as_deref(),
            Some("action_required")
        );
        assert_eq!(page.check_suites[0].id, 101028134107);
    }

    #[test]
    fn subject_handoff_parses_issue_and_pr_urls_only() {
        assert_eq!(
            subject_handoff("Issue", Some("https://api.github.com/repos/o/r/issues/42"),),
            Some(("o/r".into(), 42)),
        );
        assert_eq!(
            subject_handoff(
                "PullRequest",
                Some("https://api.github.com/repos/o/r/pulls/7"),
            ),
            Some(("o/r".into(), 7)),
        );
        // Other kinds never hand off, even with a parseable URL.
        assert_eq!(
            subject_handoff(
                "Discussion",
                Some("https://api.github.com/repos/o/r/discussions/9"),
            ),
            None
        );
        // Missing/malformed URLs never hand off.
        assert_eq!(subject_handoff("Issue", None), None);
        assert_eq!(
            subject_handoff(
                "Issue",
                Some("https://api.github.com/repos/o/r/issues/notanumber"),
            ),
            None
        );
    }

    #[test]
    fn latest_comment_id_parses_issue_comment_urls_only() {
        // The upstream shape (docs/0026 D9): the issue-comment collection
        // item — serves Issue and PR conversation comments alike.
        assert_eq!(
            latest_comment_id(Some(
                "https://api.github.com/repos/o/r/issues/comments/1234567",
            )),
            Some(1_234_567),
        );
        // PR review comments have no in-app surface — no anchor.
        assert_eq!(
            latest_comment_id(Some("https://api.github.com/repos/o/r/pulls/comments/9",)),
            None,
        );
        // Subject URLs, sub-paths, foreign hosts, malformed shapes: None.
        assert_eq!(
            latest_comment_id(Some("https://api.github.com/repos/o/r/issues/42")),
            None,
        );
        assert_eq!(
            latest_comment_id(Some("https://api.github.com/repos/o/r/issues/comments/",)),
            None,
        );
        assert_eq!(
            latest_comment_id(Some(
                "https://api.github.com/repos/o/r/issues/comments/notanumber",
            )),
            None,
        );
        assert_eq!(
            latest_comment_id(Some("https://example.com/repos/o/r/issues/comments/1")),
            None,
        );
        assert_eq!(latest_comment_id(Some("not a url")), None);
        assert_eq!(latest_comment_id(None), None);
    }
}
