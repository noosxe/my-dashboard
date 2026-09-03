//! GitHub GraphQL v4 queries, response shapes, and node→domain mapping
//! (docs/0007 §3–§5).
//!
//! One **global snapshot** query gathers everything the module shows
//! (viewer + orgs + repos with PR/issue data) in a single round trip; one
//! **repo-scoped snapshot** query refreshes a single repo on view open.
//! Rate-limit points scale with connections, not fields (§5): node fields
//! ride along free.
//!
//! Tolerance follows [DG] per-repo degradation: node lists parse per item —
//! a null/malformed repo (permission error, deleted mid-flight) is skipped
//! with a warning, never failing the whole snapshot.

use serde::{Deserialize, Serialize};

pub const GRAPHQL_ENDPOINT: &str = "https://api.github.com/graphql";

/// Repos pages fetched per snapshot run (doc §5): 3 × 100 = 300 repos;
/// beyond that the cursor persists in memory and the next snapshot tick
/// continues where this run stopped.
pub const SNAPSHOT_MAX_PAGES: u32 = 3;

/// The global snapshot (doc 0007 D1/D2). `ownerAffiliations` MUST be
/// explicit — the schema default `[OWNER, COLLABORATOR]` silently drops
/// org-member repos that REST `/user/repos` includes.
pub const GLOBAL_SNAPSHOT_QUERY: &str = r#"
query Snapshot($cursor: String) {
  rateLimit { cost remaining resetAt }
  viewer {
    login
    avatarUrl
    organizations(first: 100) {
      nodes { login avatarUrl description }
    }
    repositories(first: 100, after: $cursor,
                 ownerAffiliations: [OWNER, COLLABORATOR, ORGANIZATION_MEMBER],
                 orderBy: {field: PUSHED_AT, direction: DESC}) {
      totalCount
      pageInfo { hasNextPage endCursor }
      nodes {
        databaseId
        name
        nameWithOwner
        description
        isPrivate
        pushedAt
        owner { login }
        pullRequests(states: OPEN, first: 100,
                     orderBy: {field: UPDATED_AT, direction: DESC}) {
          totalCount
          nodes {
            databaseId
            number
            title
            isDraft
            state
            author { login avatarUrl }
            updatedAt
            url
            statusCheckRollup { state }
          }
        }
        issues(states: OPEN, first: 100,
               orderBy: {field: UPDATED_AT, direction: DESC}) {
          totalCount
          nodes {
            databaseId
            number
            title
            author { login avatarUrl }
            updatedAt
            url
          }
        }
      }
    }
  }
}
"#;

/// Repo-scoped refresh on view open (doc 0007 D6) — same node shape as the
/// global snapshot, one repo. `repository` is `null` on 404/permission
/// (handled as per-repo degradation, §6).
pub const REPO_SNAPSHOT_QUERY: &str = r#"
query RepoSnapshot($owner: String!, $name: String!) {
  rateLimit { cost remaining resetAt }
  repository(owner: $owner, name: $name) {
    databaseId
    name
    nameWithOwner
    description
    isPrivate
    pushedAt
    owner { login }
    pullRequests(states: OPEN, first: 100,
                 orderBy: {field: UPDATED_AT, direction: DESC}) {
      totalCount
      nodes {
        databaseId
        number
        title
        isDraft
        state
        author { login avatarUrl }
        updatedAt
        url
        statusCheckRollup { state }
      }
    }
    issues(states: OPEN, first: 100,
           orderBy: {field: UPDATED_AT, direction: DESC}) {
      totalCount
      nodes {
        databaseId
        number
        title
        author { login avatarUrl }
        updatedAt
        url
      }
    }
  }
}
"#;

// ---------------------------------------------------------------------------
// Wire shapes (serde) — camelCase GraphQL, nodes kept as raw values and
// parsed per item so one bad node degrades alone.
// ---------------------------------------------------------------------------

/// GraphQL response envelope: HTTP 200 with `errors[]` and possibly partial
/// `data` (doc 0007 D5).
#[derive(Debug, Deserialize)]
pub struct Envelope<T> {
    pub data: Option<T>,
    #[serde(default)]
    pub errors: Vec<GqlError>,
}

#[derive(Debug, Deserialize)]
pub struct GqlError {
    pub message: String,
}

/// `rateLimit` root field — the authoritative per-request cost for GraphQL
/// (doc 0007 D5); `resetAt` is RFC-3339.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitField {
    pub remaining: i64,
    pub reset_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotData {
    #[serde(default)]
    pub rate_limit: Option<RateLimitField>,
    pub viewer: ViewerData,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewerData {
    pub login: String,
    #[serde(default)]
    pub avatar_url: String,
    #[serde(default)]
    pub organizations: OrgConnection,
    /// `null` when the subselection errored — keep last-known repos then.
    pub repositories: Option<RepoPage>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgConnection {
    #[serde(default)]
    pub nodes: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoPage {
    #[serde(default)]
    pub total_count: i64,
    #[serde(default)]
    pub page_info: PageInfo,
    #[serde(default)]
    pub nodes: Vec<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    #[serde(default)]
    pub has_next_page: bool,
    #[serde(default)]
    pub end_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoSnapshotData {
    #[serde(default)]
    pub rate_limit: Option<RateLimitField>,
    pub repository: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Domain types — what the state, UI, and `snapshot-v2` cache hold.
// ---------------------------------------------------------------------------

/// The authenticated account (was REST `GET /user`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub login: String,
    #[serde(default)]
    pub avatar_url: String,
}

/// Org dropdown entry (was REST `GET /user/orgs`); the service prepends
/// the personal account (docs/0005 Q10).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Org {
    pub login: String,
    #[serde(default)]
    pub avatar_url: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Owner {
    pub login: String,
}

/// PR/issue author — nullable as a whole (deleted users, bot Apps).
/// `rename_all` is load-bearing: the wire key is `avatarUrl` — without it
/// avatars silently deserialize empty (latent since docs/0007; surfaced by
/// the doc 0012 issue-avatar test).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Author {
    pub login: String,
    #[serde(default)]
    pub avatar_url: String,
}

/// A repository with its open-PR/issue summaries (doc 0007 D2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    /// `nameWithOwner` — the stable per-repo key.
    pub key: String,
    #[allow(dead_code)] // REST-parity field, kept for cache continuity
    pub database_id: Option<i64>,
    pub name: String,
    pub full_name: String,
    pub owner: Owner,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub private: bool,
    /// Repo pushes — one input of the composite list ordering (issue #35).
    #[serde(default)]
    pub pushed_at: Option<String>,
    pub prs: PrSummary,
    pub issues: IssueSummary,
}

/// RFC-3339 → epoch seconds; 0 when absent or unparseable.
fn stamp_epoch(stamp: &Option<String>) -> u64 {
    stamp
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.timestamp().max(0) as u64)
        .unwrap_or(0)
}

impl Repo {
    /// Composite last-activity (issue #35): the newest of the repo's own
    /// pushes, its open PRs, and its open issues. Nodes arrive
    /// `UPDATED_AT DESC` (D2) but the max over nodes stays correct even
    /// if that ordering ever shifts; missing stamps count as 0.
    pub fn last_activity(&self) -> u64 {
        let mut latest = stamp_epoch(&self.pushed_at);
        for pr in &self.prs.nodes {
            latest = latest.max(stamp_epoch(&pr.updated_at));
        }
        for issue in &self.issues.nodes {
            latest = latest.max(stamp_epoch(&issue.updated_at));
        }
        latest
    }
}

/// Open PRs: exact count (fixes the REST page-1 cap, doc 0007 §1) plus the
/// first 100 rows for repo views.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrSummary {
    pub total_count: i64,
    pub nodes: Vec<PullRequest>,
}

/// Open issues: exact count plus the first 100 rows for the home card and
/// repo views (doc 0012).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueSummary {
    pub total_count: i64,
    pub nodes: Vec<Issue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    /// `owner/name#number` — stable identity; the fallback when
    /// `databaseId` is null (doc 0007 D3) and the cache/row key.
    pub key: String,
    #[allow(dead_code)] // REST-parity field, kept for cache continuity
    pub database_id: Option<i64>,
    pub number: i64,
    pub title: String,
    #[serde(default)]
    pub draft: bool,
    #[allow(dead_code)] // kept for the CI-signal follow-up (doc 0007 §9)
    pub state: Option<String>,
    pub author: Option<Author>,
    pub updated_at: Option<String>,
    /// Row click → browser (UI layer).
    pub url: String,
    /// `statusCheckRollup.state` — the CI signal 0005 deferred; gathered
    /// now, surfaced in the UI as a follow-up (doc 0007 D2).
    #[allow(dead_code)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub key: String,
    #[allow(dead_code)] // REST-parity field, kept for cache continuity
    pub database_id: Option<i64>,
    pub number: i64,
    pub title: String,
    pub author: Option<Author>,
    pub updated_at: Option<String>,
    /// Row click → browser (UI layer).
    pub url: String,
}

// ---------------------------------------------------------------------------
// PR detail page (docs/0016) — one query on open, no cache (D2).
// ---------------------------------------------------------------------------

/// The stage-1+2 field set (docs/0016 §6.1–§6.2): everything the header,
/// tab counts, the conversation read path, and the write path render —
/// plus the merge-method repository booleans and the mergeability pair
/// the rail panel needs. `contexts` (checks rows) land with stage 3
/// (#93) after the union shape is re-verified.
pub const PR_DETAIL_QUERY: &str = r#"
query PrDetail($owner: String!, $name: String!, $number: Int!) {
  rateLimit { cost remaining resetAt }
  repository(owner: $owner, name: $name) {
    mergeCommitAllowed
    squashMergeAllowed
    rebaseMergeAllowed
    pullRequest(number: $number) {
      id
      number
      title
      body
      state
      isDraft
      changedFiles
      baseRefName
      headRefName
      url
      createdAt
      author { login avatarUrl }
      viewerCanUpdate
      mergeable
      mergeStateStatus
      statusCheckRollup {
        state
        contexts(first: 100) {
          totalCount
          nodes {
            __typename
            ... on CheckRun { name status conclusion detailsUrl }
            ... on StatusContext { context state targetUrl }
          }
        }
      }
      commits(first: 100) {
        totalCount
        nodes {
          commit {
            oid
            messageHeadline
            committedDate
            author { name avatarUrl user { login } }
          }
        }
      }
      reviews(first: 100) {
        totalCount
        nodes { author { login avatarUrl } state body submittedAt }
      }
      comments(first: 100) {
        totalCount
        nodes { author { login avatarUrl } body createdAt }
      }
      files(first: 1) { totalCount }
    }
  }
}
"#;

/// Envelope data for the detail query — the repository node stays raw so
/// the PR parses through [`pr_detail_from_value`] with the same per-node
/// degradation rules as the snapshot (doc 0007 D3).
#[derive(Debug, Deserialize)]
pub struct PrDetailData {
    pub repository: Option<serde_json::Value>,
}

/// Everything the PR page renders (docs/0016 D2). Session-only — this
/// never enters a cache.
#[derive(Debug, Clone)]
pub struct PrDetail {
    /// Node ID — the mutation target for the stage-2 write path.
    pub id: String,
    pub number: i64,
    pub title: String,
    pub body: Option<String>,
    /// `PullRequestState` — OPEN / MERGED / CLOSED.
    pub state: Option<String>,
    pub draft: bool,
    /// `viewerCanUpdate` — gates the description Edit affordance (D4).
    pub viewer_can_update: bool,
    /// `MergeableState` — MERGEABLE / CONFLICTING / UNKNOWN; the Merge
    /// button stays disabled on anything but MERGEABLE (D5).
    pub mergeable: Option<String>,
    /// `MergeStateStatus` — the finer-grained rail state line (D5).
    pub merge_state_status: Option<String>,
    /// `statusCheckRollup.state` — the checks mini-summary signal (D5).
    pub rollup_state: Option<String>,
    /// Repository merge settings — which methods the rail offers (D5).
    pub merge_methods: MergeMethods,
    /// `changedFiles` (top-level `PullRequest` field — the `…IfAvailable` variant the docs recommend is NOT in the public schema; bridge-verified 2026-09-03), with `files.totalCount` as the fallback
    /// (the schema recommends the *IfAvailable variant precisely because
    /// the eager field can error for huge diffs).
    pub changed_files: Option<i64>,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
    pub url: String,
    pub created_at: Option<String>,
    pub author: Option<Author>,
    pub commits: PrCommits,
    pub checks: PrChecks,
    pub reviews: PrReviews,
    pub comments: PrComments,
}

#[derive(Debug, Clone, Default)]
pub struct PrCommits {
    pub total_count: i64,
    pub nodes: Vec<PrCommit>,
}

#[derive(Debug, Clone)]
pub struct PrCommit {
    pub oid: String,
    pub headline: String,
    pub committed_at: Option<String>,
    /// `author.name` — present even when the author has no GitHub account.
    pub author_name: Option<String>,
    /// `author.user.login` — `None` for non-user commits.
    pub author_login: Option<String>,
    pub author_avatar: Option<String>,
}

/// Checks connection from `statusCheckRollup.contexts` (docs/0016 D7).
/// The union's two members unify into [`PrCheck`]; `total_count` is the
/// server-side count (`first: 100` caps the rows).
#[derive(Debug, Clone, Default)]
pub struct PrChecks {
    pub total_count: i64,
    pub nodes: Vec<PrCheck>,
}

/// One check row, normalized across the union members
/// (live-verified 2026-09-03: `CheckRun { name status conclusion detailsUrl }`,
/// `StatusContext { context state targetUrl }`).
#[derive(Debug, Clone, PartialEq)]
pub struct PrCheck {
    pub name: String,
    pub state: CheckState,
    pub details_url: Option<String>,
}

/// Unified check outcome for tone/icon mapping (D7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Success,
    Failure,
    Pending,
    Neutral,
}

/// `CheckRun.status` + `conclusion` → unified state. While a run is
/// queued/in progress/waiting the conclusion is `null`; conclusions are
/// otherwise terminal (SUCCESS, FAILURE, TIMED_OUT, ACTION_REQUIRED, ...).
pub fn check_run_state(status: Option<&str>, conclusion: Option<&str>) -> CheckState {
    match conclusion {
        Some("SUCCESS") => CheckState::Success,
        Some("FAILURE") | Some("TIMED_OUT") | Some("ACTION_REQUIRED") => CheckState::Failure,
        Some("NEUTRAL") | Some("SKIPPED") | Some("STALE") | Some("CANCELLED") => {
            CheckState::Neutral
        }
        // No conclusion yet — the status field carries the phase. Only
        // treat completed-with-unknown-conclusion as terminal-failure;
        // everything else (QUEUED / IN_PROGRESS / WAITING / null) is
        // still running.
        Some(_) => CheckState::Failure,
        None => match status {
            Some("COMPLETED") => CheckState::Failure,
            _ => CheckState::Pending,
        },
    }
}

/// `StatusContext.state` (SUCCESS / ERROR / FAILURE / PENDING) → unified.
pub fn status_context_state(state: Option<&str>) -> CheckState {
    match state {
        Some("SUCCESS") => CheckState::Success,
        Some("ERROR") | Some("FAILURE") => CheckState::Failure,
        Some("PENDING") => CheckState::Pending,
        _ => CheckState::Neutral,
    }
}
/// The repository's enabled merge methods (docs/0016 D5) — ride along
/// on the detail query's `repository` wrapper.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MergeMethods {
    pub merge: bool,
    pub squash: bool,
    pub rebase: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PrReviews {
    pub total_count: i64,
    pub nodes: Vec<PrReview>,
}

#[derive(Debug, Clone)]
pub struct PrReview {
    pub author: Option<Author>,
    /// `PullRequestReviewState` — APPROVED / CHANGES_REQUESTED / COMMENTED …
    pub state: Option<String>,
    pub body: Option<String>,
    pub submitted_at: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PrComments {
    pub total_count: i64,
    pub nodes: Vec<PrComment>,
}

#[derive(Debug, Clone)]
pub struct PrComment {
    pub author: Option<Author>,
    pub body: String,
    pub created_at: Option<String>,
}

/// Parse one `repository.pullRequest` node; `None` only when the node is
/// not an object (null / malformed) — the caller keeps its skeleton/error
/// state then. List nodes degrade individually (0007 D3): one bad commit
/// or comment never takes the page down.
pub fn pr_detail_from_value(value: &serde_json::Value) -> Option<PrDetail> {
    let pr = value.get("pullRequest")?;
    if !pr.is_object() {
        return None;
    }
    let str_field = |name: &str| pr.get(name).and_then(|v| v.as_str()).map(str::to_string);
    // Repository-level merge settings (docs/0016 D5) — read off the
    // repository node itself (`value`), missing booleans = disabled.
    let merge_methods = gql_merge_methods(value);
    let files_total = pr.pointer("/files/totalCount").and_then(|v| v.as_i64());
    let commits = pr
        .pointer("/commits/nodes")
        .and_then(|nodes| nodes.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|c| {
                    let commit = c.get("commit")?;
                    let author = commit.get("author");
                    Some(PrCommit {
                        oid: commit.get("oid")?.as_str()?.to_string(),
                        headline: commit
                            .get("messageHeadline")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        committed_at: commit
                            .get("committedDate")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        author_name: author
                            .and_then(|a| a.get("name"))
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        author_login: author
                            .and_then(|a| a.pointer("/user/login"))
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        author_avatar: author
                            .and_then(|a| a.get("avatarUrl"))
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                    })
                })
                .collect::<Vec<PrCommit>>()
        })
        .unwrap_or_default();
    let commits_total = pr
        .pointer("/commits/totalCount")
        .and_then(|v| v.as_i64())
        .unwrap_or(commits.len() as i64);
    let reviews = pr
        .pointer("/reviews/nodes")
        .and_then(|nodes| nodes.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .map(|r| PrReview {
                    author: author_from_value(r.get("author")),
                    state: r.get("state").and_then(|v| v.as_str()).map(str::to_string),
                    body: r.get("body").and_then(|v| v.as_str()).map(str::to_string),
                    submitted_at: r
                        .get("submittedAt")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                })
                .collect::<Vec<PrReview>>()
        })
        .unwrap_or_default();
    let reviews_total = pr
        .pointer("/reviews/totalCount")
        .and_then(|v| v.as_i64())
        .unwrap_or(reviews.len() as i64);
    let comments = pr
        .pointer("/comments/nodes")
        .and_then(|nodes| nodes.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|c| {
                    Some(PrComment {
                        author: author_from_value(c.get("author")),
                        body: c.get("body")?.as_str()?.to_string(),
                        created_at: c
                            .get("createdAt")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                    })
                })
                .collect::<Vec<PrComment>>()
        })
        .unwrap_or_default();
    let comments_total = pr
        .pointer("/comments/totalCount")
        .and_then(|v| v.as_i64())
        .unwrap_or(comments.len() as i64);
    Some(PrDetail {
        id: str_field("id").unwrap_or_default(),
        number: pr
            .get("number")
            .and_then(|v| v.as_i64())
            .unwrap_or_default(),
        title: str_field("title").unwrap_or_default(),
        body: str_field("body"),
        state: str_field("state"),
        draft: pr.get("isDraft").and_then(|v| v.as_bool()).unwrap_or(false),
        viewer_can_update: pr
            .get("viewerCanUpdate")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        mergeable: str_field("mergeable"),
        merge_state_status: str_field("mergeStateStatus"),
        rollup_state: pr
            .pointer("/statusCheckRollup/state")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        merge_methods,
        changed_files: pr
            .get("changedFiles")
            .and_then(|v| v.as_i64())
            .or(files_total),
        base_ref: str_field("baseRefName"),
        head_ref: str_field("headRefName"),
        url: str_field("url").unwrap_or_default(),
        created_at: str_field("createdAt"),
        author: author_from_value(pr.get("author")),
        commits: PrCommits {
            total_count: commits_total,
            nodes: commits,
        },
        checks: gql_pr_checks(pr),
        reviews: PrReviews {
            total_count: reviews_total,
            nodes: reviews,
        },
        comments: PrComments {
            total_count: comments_total,
            nodes: comments,
        },
    })
}

/// `statusCheckRollup.contexts` → [`PrChecks`] (docs/0016 D7), with
/// per-node degradation (0007 D3): a node with an unknown
/// `__typename` or a missing name drops out while the rest parse; a
/// `null` rollup (PR without checks) yields the empty struct.
fn gql_pr_checks(pr: &serde_json::Value) -> PrChecks {
    let contexts = pr
        .pointer("/statusCheckRollup/contexts")
        .filter(|v| v.is_object())
        .unwrap_or(&serde_json::Value::Null);
    let total_count = contexts
        .pointer("/totalCount")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let nodes = contexts
        .pointer("/nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|node| {
                    let str_field =
                        |key: &str| node.get(key).and_then(|v| v.as_str()).map(str::to_string);
                    let url = || {
                        ["detailsUrl", "targetUrl"]
                            .iter()
                            .find_map(|k| str_field(k))
                    };
                    match node.get("__typename").and_then(|v| v.as_str()) {
                        Some("CheckRun") => Some(PrCheck {
                            name: str_field("name")?,
                            state: check_run_state(
                                node.get("status").and_then(|v| v.as_str()),
                                node.get("conclusion").and_then(|v| v.as_str()),
                            ),
                            details_url: url(),
                        }),
                        Some("StatusContext") => Some(PrCheck {
                            name: str_field("context")?,
                            state: status_context_state(node.get("state").and_then(|v| v.as_str())),
                            details_url: url(),
                        }),
                        _ => None,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    PrChecks { total_count, nodes }
}
/// The three `repository` merge-method booleans — absent fields count
/// as disabled (repos really do turn methods off; `false` is a value the
/// API returns, `null`/absent only on degraded payloads).
fn gql_merge_methods(repo: &serde_json::Value) -> MergeMethods {
    let flag = |name: &str| repo.get(name).and_then(|v| v.as_bool()).unwrap_or(false);
    MergeMethods {
        merge: flag("mergeCommitAllowed"),
        squash: flag("squashMergeAllowed"),
        rebase: flag("rebaseMergeAllowed"),
    }
}

// ---------------------------------------------------------------------------
// PR page mutations (docs/0016 §6.2) — inputs verified against the GraphQL
// reference; every payload selection is a field confirmed on that payload.
// ---------------------------------------------------------------------------

/// Post a plain comment (`AddCommentInput { subjectId body }`).
pub const PR_ADD_COMMENT_MUTATION: &str = r#"
mutation PrAddComment($input: AddCommentInput!) {
  addComment(input: $input) { clientMutationId }
}
"#;

/// Submit a review (`AddPullRequestReviewInput { pullRequestId event body }`)
/// — `event` APPROVE / REQUEST_CHANGES; `body` omitted when empty (a
/// body-less approve is legal).
pub const PR_ADD_REVIEW_MUTATION: &str = r#"
mutation PrAddReview($input: AddPullRequestReviewInput!) {
  addPullRequestReview(input: $input) { pullRequestReview { id state } }
}
"#;

/// Edit the description (`UpdateIssueInput { id body }` — a PR is an
/// issue, so `updateIssue` takes the PR node id).
pub const PR_UPDATE_DESCRIPTION_MUTATION: &str = r#"
mutation PrUpdateDescription($input: UpdateIssueInput!) {
  updateIssue(input: $input) { issue { id } }
}
"#;

/// Merge (`MergePullRequestInput { pullRequestId mergeMethod }` — method
/// MERGE / SQUASH / REBASE, server default MERGE).
pub const PR_MERGE_MUTATION: &str = r#"
mutation PrMerge($input: MergePullRequestInput!) {
  mergePullRequest(input: $input) { pullRequest { state } }
}
"#;

/// Close (`ClosePullRequestInput { pullRequestId }`).
pub const PR_CLOSE_MUTATION: &str = r#"
mutation PrClose($input: ClosePullRequestInput!) {
  closePullRequest(input: $input) { pullRequest { state } }
}
"#;
/// `author { login avatarUrl }` — shared by reviews and comments (the PR
/// node's own author uses the same shape).
fn author_from_value(value: Option<&serde_json::Value>) -> Option<Author> {
    let a = value?;
    Some(Author {
        login: a.get("login")?.as_str()?.to_string(),
        avatar_url: a
            .get("avatarUrl")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}
/// The persisted snapshot (doc 0007 D7): one versioned cache key replacing
/// the granular `user`/`orgs`/`repos`/`pr-counts`/`prs-*` keys.
pub const SNAPSHOT_CACHE_KEY: &str = "snapshot-v2";

#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotCache {
    pub schema: u16,
    pub user: Option<User>,
    #[serde(default)]
    pub orgs: Vec<Org>,
    #[serde(default)]
    pub repos: Vec<Repo>,
}

// ---------------------------------------------------------------------------
// Node → domain mapping
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoNode {
    database_id: Option<i64>,
    name: String,
    name_with_owner: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    is_private: bool,
    pushed_at: Option<String>,
    owner: Owner,
    #[serde(default)]
    pull_requests: PrPageNode,
    #[serde(default)]
    issues: IssuePageNode,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrPageNode {
    #[serde(default)]
    total_count: i64,
    #[serde(default)]
    nodes: Vec<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssuePageNode {
    #[serde(default)]
    total_count: i64,
    #[serde(default)]
    nodes: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrNode {
    database_id: Option<i64>,
    number: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    is_draft: bool,
    state: Option<String>,
    author: Option<Author>,
    updated_at: Option<String>,
    #[serde(default)]
    url: String,
    status_check_rollup: Option<StatusRollupNode>,
}

#[derive(Debug, Deserialize)]
struct StatusRollupNode {
    state: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssueNode {
    database_id: Option<i64>,
    number: i64,
    #[serde(default)]
    title: String,
    author: Option<Author>,
    updated_at: Option<String>,
    #[serde(default)]
    url: String,
}

/// Parse a list of raw nodes tolerantly: per-item failures log a warning
/// and degrade that item alone ([DG] — never the whole snapshot).
fn parse_nodes<T: serde::de::DeserializeOwned>(values: &[serde_json::Value]) -> Vec<T> {
    values
        .iter()
        .filter_map(|v| match serde_json::from_value::<T>(v.clone()) {
            Ok(node) => Some(node),
            Err(e) => {
                log::warn!("snapshot: skipping malformed node: {e}");
                None
            }
        })
        .collect()
}

/// Map one raw `repositories.nodes` item to the domain `Repo`.
pub fn repo_from_value(value: &serde_json::Value) -> Option<Repo> {
    let node: RepoNode = serde_json::from_value(value.clone()).ok()?;
    Some(Repo {
        key: node.name_with_owner.clone(),
        database_id: node.database_id,
        name: node.name,
        full_name: node.name_with_owner.clone(),
        owner: node.owner,
        description: node.description,
        private: node.is_private,
        pushed_at: node.pushed_at,
        prs: PrSummary {
            total_count: node.pull_requests.total_count,
            nodes: parse_nodes::<PrNode>(&node.pull_requests.nodes)
                .into_iter()
                .map(|pr| PullRequest {
                    key: format!("{}#{}", node.name_with_owner, pr.number),
                    database_id: pr.database_id,
                    number: pr.number,
                    title: pr.title,
                    draft: pr.is_draft,
                    state: pr.state,
                    author: pr.author,
                    updated_at: pr.updated_at,
                    url: pr.url,
                    status: pr.status_check_rollup.and_then(|r| r.state),
                })
                .collect(),
        },
        issues: IssueSummary {
            total_count: node.issues.total_count,
            nodes: parse_nodes::<IssueNode>(&node.issues.nodes)
                .into_iter()
                .map(|issue| Issue {
                    key: format!("{}#{}", node.name_with_owner, issue.number),
                    database_id: issue.database_id,
                    number: issue.number,
                    title: issue.title,
                    author: issue.author,
                    updated_at: issue.updated_at,
                    url: issue.url,
                })
                .collect(),
        },
    })
}

/// Viewer orgs: raw values → `Org` (personal account is prepended by the
/// service, docs/0005 Q10 — not part of this connection).
pub fn orgs_from_values(values: &[serde_json::Value]) -> Vec<Org> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct OrgNode {
        login: String,
        #[serde(default)]
        avatar_url: String,
        #[serde(default)]
        description: Option<String>,
    }
    parse_nodes::<OrgNode>(values)
        .into_iter()
        .map(|o| Org {
            login: o.login,
            avatar_url: o.avatar_url,
            description: o.description,
        })
        .collect()
}

impl RateLimitField {
    /// Window-reset epoch from the RFC-3339 `resetAt`; an unparseable
    /// timestamp falls back to now+60s (conservative pause, Q9).
    pub fn reset_epoch(&self) -> u64 {
        chrono::DateTime::parse_from_rfc3339(&self.reset_at)
            .ok()
            .map(|t| t.timestamp().max(0) as u64)
            .unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() + 60)
                    .unwrap_or(60)
            })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn last_activity_picks_max_across_repo_prs_and_issues() {
        let mut value = repo_value("o/r", "", 0);
        // pushedAt January, PRs Feb+Apr, issue June — June wins.
        value["pushedAt"] = serde_json::json!("2026-01-01T00:00:00Z");
        value["pullRequests"] = serde_json::json!({"totalCount": 2, "nodes": [
            {"databaseId":1,"number":1,"title":"t","isDraft":false,
             "state":"OPEN","author":{"login":"x","avatarUrl":"a"},
             "updatedAt":"2026-04-01T00:00:00Z","url":"u"},
            {"databaseId":2,"number":2,"title":"t2","isDraft":false,
             "state":"OPEN","author":{"login":"x","avatarUrl":"a"},
             "updatedAt":"2026-02-01T00:00:00Z","url":"u"}]});
        value["issues"]["nodes"] = serde_json::json!([
            {"databaseId":31,"number":9,"title":"i",
             "author":{"login":"octo"},"updatedAt":"2026-06-01T00:00:00Z",
             "url":"iu"}]);
        let repo = repo_from_value(&value).unwrap();
        assert_eq!(repo.last_activity(), 1_780_272_000); // 2026-06-01T00:00:00Z
    }

    fn repo_value(full_name: &str, prs: &str, pr_total: i64) -> serde_json::Value {
        serde_json::from_str(&format!(
            r#"{{"databaseId":7,"name":"{}","nameWithOwner":"{full_name}",
                "description":null,"isPrivate":false,
                "pushedAt":"2026-09-01T00:00:00Z","owner":{{"login":"o"}},
                "pullRequests":{{"totalCount":{pr_total},"nodes":[{prs}]}},
                "issues":{{"totalCount":2,"nodes":[
                    {{"databaseId":31,"number":9,"title":"i",
                      "author":{{"login":"octo","avatarUrl":"https://a/i.png"}},"updatedAt":"u","url":"iu"}}]}}}}"#,
            full_name.split('/').nth(1).unwrap_or(full_name),
        ))
        .unwrap()
    }

    #[test]
    fn maps_repo_nodes_with_prs_and_issues() {
        let repo = repo_from_value(&repo_value(
            "o/r",
            r#"{"databaseId":11,"number":123,"title":"feat: x","isDraft":true,
                "state":"OPEN","author":{"login":"octo","avatarUrl":"https://a/c.png"},
                "updatedAt":"2026-09-01T01:00:00Z","url":"https://github.com/o/r/pull/123",
                "statusCheckRollup":{"state":"SUCCESS"}}"#,
            1,
        ))
        .unwrap();

        assert_eq!(repo.key, "o/r");
        assert_eq!(repo.full_name, "o/r");
        assert_eq!(repo.prs.total_count, 1);
        let pr = &repo.prs.nodes[0];
        assert_eq!(pr.key, "o/r#123");
        assert!(pr.draft);
        assert_eq!(pr.author.as_ref().unwrap().login, "octo");
        assert_eq!(pr.status.as_deref(), Some("SUCCESS"));
        assert_eq!(repo.issues.total_count, 2);
        assert_eq!(repo.issues.nodes[0].key, "o/r#9");
        // Doc 0012 D1: issue authors carry avatars (PR-row parity).
        assert_eq!(repo.issues.nodes[0].author.as_ref().unwrap().login, "octo");
        assert_eq!(
            repo.issues.nodes[0].author.as_ref().unwrap().avatar_url,
            "https://a/i.png"
        );
    }

    #[test]
    fn database_id_null_falls_back_to_composite_key() {
        let repo = repo_from_value(&repo_value(
            "o/r",
            r#"{"databaseId":null,"number":7,"title":"t","isDraft":false,
                "author":null,"updatedAt":null,"url":"u"}"#,
            1,
        ))
        .unwrap();
        let pr = &repo.prs.nodes[0];
        assert_eq!(pr.key, "o/r#7");
        assert!(pr.database_id.is_none());
        assert!(pr.author.is_none());
    }

    #[test]
    fn count_exceeds_node_list() {
        // >100 open PRs: totalCount is exact, nodes are capped at 100.
        let repo = repo_from_value(&repo_value("o/big", "", 250)).unwrap();
        assert_eq!(repo.prs.total_count, 250);
        assert!(repo.prs.nodes.is_empty());
    }

    #[test]
    fn envelope_parses_partial_errors_with_data() {
        let env: Envelope<SnapshotData> = serde_json::from_str(
            r#"{"data":{"viewer":{"login":"noosxe","avatarUrl":"a",
                  "organizations":{"nodes":[]},
                  "repositories":{"totalCount":0,"nodes":[]}}},
                "errors":[{"message":"some field failed"}]}"#,
        )
        .unwrap();
        assert_eq!(env.errors.len(), 1);
        assert_eq!(env.errors[0].message, "some field failed");
        assert_eq!(env.data.as_ref().unwrap().viewer.login, "noosxe");
    }

    #[test]
    fn envelope_tolerates_null_data() {
        let env: Envelope<SnapshotData> =
            serde_json::from_str(r#"{"data":null,"errors":[{"message":"boom"}]}"#).unwrap();
        assert!(env.data.is_none());
    }

    #[test]
    fn rate_limit_reset_epoch_parses_rfc3339() {
        let rl: RateLimitField =
            serde_json::from_str(r#"{"remaining":0,"resetAt":"2030-01-01T00:00:00Z"}"#).unwrap();
        assert_eq!(rl.reset_epoch(), 1_893_456_000);
    }

    // --- PR detail (docs/0016 D2) --------------------------------------

    fn pr_detail_body(pr_json: &str) -> String {
        // Concat, not format!: the JSON braces make a format string a
        // counting hazard.
        let mut body = String::from(r#"{"data":{"repository":{"pullRequest":"#);
        body.push_str(pr_json);
        body.push_str("}}}");
        body
    }

    #[test]
    fn pr_detail_parses_full_node() {
        let body = pr_detail_body(
            r#"{
                "id": "PR_1", "number": 482, "title": "Add retry budget",
                "body": "Fixes the fetch layer.", "state": "OPEN", "isDraft": false,
                "changedFiles": 23, "baseRefName": "main",
                "headRefName": "add-retry", "url": "https://github.com/o/r/pull/482",
                "createdAt": "2026-08-01T00:00:00Z",
                "author": {"login": "alice", "avatarUrl": "https://av"},
                "commits": {"totalCount": 12, "nodes": [
                    {"commit": {"oid": "abc1234def", "messageHeadline": "first",
                                "committedDate": "2026-08-01T01:00:00Z",
                                "author": {"name": "Alice", "avatarUrl": "https://av",
                                           "user": {"login": "alice"}}}}
                ]},
                "reviews": {"totalCount": 1, "nodes": [
                    {"author": {"login": "bob"}, "state": "APPROVED",
                     "body": "lgtm", "submittedAt": "2026-08-02T00:00:00Z"}
                ]},
                "comments": {"totalCount": 2, "nodes": [
                    {"author": {"login": "carol"}, "body": "nice",
                     "createdAt": "2026-08-03T00:00:00Z"}
                ]},
                "files": {"totalCount": 23}
            }"#,
        );
        let env: Envelope<PrDetailData> = serde_json::from_str(&body).unwrap();
        let value = env.data.unwrap().repository.unwrap();
        let d = pr_detail_from_value(&value).unwrap();
        assert_eq!(d.id, "PR_1");
        assert_eq!(d.number, 482);
        assert_eq!(d.title, "Add retry budget");
        assert_eq!(d.body.as_deref(), Some("Fixes the fetch layer."));
        assert_eq!(d.state.as_deref(), Some("OPEN"));
        assert!(!d.draft);
        assert_eq!(d.changed_files, Some(23));
        assert_eq!(d.base_ref.as_deref(), Some("main"));
        assert_eq!(d.head_ref.as_deref(), Some("add-retry"));
        assert_eq!(d.author.as_ref().map(|a| a.login.as_str()), Some("alice"));
        assert_eq!(d.commits.total_count, 12);
        assert_eq!(d.commits.nodes.len(), 1);
        assert_eq!(d.commits.nodes[0].oid, "abc1234def");
        assert_eq!(d.commits.nodes[0].headline, "first");
        assert_eq!(d.commits.nodes[0].author_login.as_deref(), Some("alice"));
        assert_eq!(d.commits.nodes[0].author_name.as_deref(), Some("Alice"));
        assert_eq!(d.reviews.total_count, 1);
        assert_eq!(d.reviews.nodes[0].state.as_deref(), Some("APPROVED"));
        assert_eq!(d.comments.total_count, 2);
        assert_eq!(d.comments.nodes[0].body, "nice");
    }

    #[test]
    fn check_run_state_maps_status_conclusion_pairs() {
        use super::{CheckState, check_run_state};
        // Terminal conclusions.
        assert_eq!(
            check_run_state(Some("COMPLETED"), Some("SUCCESS")),
            CheckState::Success
        );
        for conclusion in ["FAILURE", "TIMED_OUT", "ACTION_REQUIRED"] {
            assert_eq!(
                check_run_state(Some("COMPLETED"), Some(conclusion)),
                CheckState::Failure,
                "conclusion {conclusion}"
            );
        }
        for conclusion in ["NEUTRAL", "SKIPPED", "STALE", "CANCELLED"] {
            assert_eq!(
                check_run_state(Some("COMPLETED"), Some(conclusion)),
                CheckState::Neutral,
                "conclusion {conclusion}"
            );
        }
        // Unknown future conclusion on a completed run: failure, not
        // silently-neutral (a red run that reads as muted would mislead).
        assert_eq!(
            check_run_state(Some("COMPLETED"), Some("STARTUP_FAILURE")),
            CheckState::Failure
        );
        // No conclusion — phase fields say pending.
        for status in [
            Some("QUEUED"),
            Some("IN_PROGRESS"),
            Some("WAITING"),
            Some("PENDING"),
            None,
        ] {
            assert_eq!(
                check_run_state(status, None),
                CheckState::Pending,
                "status {status:?}"
            );
        }
        // Completed but conclusion missing/null: treat as failed.
        assert_eq!(
            check_run_state(Some("COMPLETED"), None),
            CheckState::Failure
        );
    }

    #[test]
    fn status_context_state_maps_legacy_states() {
        use super::{CheckState, status_context_state};
        assert_eq!(status_context_state(Some("SUCCESS")), CheckState::Success);
        assert_eq!(status_context_state(Some("ERROR")), CheckState::Failure);
        assert_eq!(status_context_state(Some("FAILURE")), CheckState::Failure);
        assert_eq!(status_context_state(Some("PENDING")), CheckState::Pending);
        assert_eq!(status_context_state(None), CheckState::Neutral);
    }

    #[test]
    fn pr_detail_parses_checks_union_with_degradation() {
        let body = pr_detail_body(
            r#"{
                "id": "PR_7", "number": 7,
                "statusCheckRollup": {
                  "state": "FAILURE",
                  "contexts": {"totalCount": 5, "nodes": [
                    {"__typename": "CheckRun", "name": "build", "status": "COMPLETED",
                     "conclusion": "SUCCESS", "detailsUrl": "https://ci/build"},
                    {"__typename": "CheckRun", "name": "test", "status": "IN_PROGRESS",
                     "conclusion": null, "detailsUrl": null},
                    {"__typename": "StatusContext", "context": "license/cla",
                     "state": "SUCCESS", "targetUrl": "https://cla"},
                    {"__typename": "StatusContext", "context": "codecov",
                     "state": "ERROR", "targetUrl": null},
                    {"__typename": "SomethingNew", "name": "future"},
                    {"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "FAILURE"}
                  ]}
                }
            }"#,
        );
        let env: Envelope<PrDetailData> = serde_json::from_str(&body).unwrap();
        let value = env.data.unwrap().repository.unwrap();
        let d = pr_detail_from_value(&value).unwrap();
        assert_eq!(d.rollup_state.as_deref(), Some("FAILURE"));
        // Server-side count survives even though two nodes degrade out
        // (unknown typename; CheckRun without a name).
        assert_eq!(d.checks.total_count, 5);
        let nodes = &d.checks.nodes;
        assert_eq!(nodes.len(), 4);
        assert_eq!(nodes[0].name, "build");
        assert_eq!(nodes[0].state, CheckState::Success);
        assert_eq!(nodes[0].details_url.as_deref(), Some("https://ci/build"));
        assert_eq!(nodes[1].state, CheckState::Pending);
        assert_eq!(nodes[1].details_url, None);
        assert_eq!(nodes[2].name, "license/cla");
        assert_eq!(nodes[2].state, CheckState::Success);
        assert_eq!(nodes[3].name, "codecov");
        assert_eq!(nodes[3].state, CheckState::Failure);
    }

    #[test]
    fn pr_detail_null_rollup_is_empty_checks() {
        let body = pr_detail_body(r#"{"id": "PR_8", "number": 8, "statusCheckRollup": null}"#);
        let env: Envelope<PrDetailData> = serde_json::from_str(&body).unwrap();
        let value = env.data.unwrap().repository.unwrap();
        let d = pr_detail_from_value(&value).unwrap();
        assert_eq!(d.rollup_state, None);
        assert_eq!(d.checks.total_count, 0);
        assert!(d.checks.nodes.is_empty());
    }

    #[test]
    fn pr_detail_parses_stage2_write_path_fields() {
        // The repository wrapper carries the merge-method booleans (D5);
        // the PR node carries viewerCanUpdate / mergeability / rollup.
        let mut body = String::from(
            r#"{"data":{"repository":{"mergeCommitAllowed":true,"squashMergeAllowed":false,"rebaseMergeAllowed":true,"pullRequest":"#,
        );
        body.push_str(
            r#"{"id":"PR_9","viewerCanUpdate":true,"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":{"state":"SUCCESS"}}"#,
        );
        body.push_str("}}}");
        let env: Envelope<PrDetailData> = serde_json::from_str(&body).unwrap();
        let value = env.data.unwrap().repository.unwrap();
        let d = pr_detail_from_value(&value).unwrap();
        assert!(d.viewer_can_update);
        assert_eq!(d.mergeable.as_deref(), Some("MERGEABLE"));
        assert_eq!(d.merge_state_status.as_deref(), Some("CLEAN"));
        assert_eq!(d.rollup_state.as_deref(), Some("SUCCESS"));
        assert_eq!(
            d.merge_methods,
            MergeMethods {
                merge: true,
                squash: false,
                rebase: true
            }
        );
    }

    #[test]
    fn pr_detail_absent_stage2_fields_default_off() {
        // A degraded payload (no write-path fields at all) must not take
        // the page down: editing stays hidden, merging stays disabled.
        let body = pr_detail_body(r#"{"id":"PR_1","number":1,"title":"t","url":"u"}"#);
        let env: Envelope<PrDetailData> = serde_json::from_str(&body).unwrap();
        let value = env.data.unwrap().repository.unwrap();
        let d = pr_detail_from_value(&value).unwrap();
        assert!(!d.viewer_can_update);
        assert_eq!(d.mergeable, None);
        assert_eq!(d.merge_state_status, None);
        assert_eq!(d.rollup_state, None);
        assert_eq!(d.merge_methods, MergeMethods::default());
    }

    #[test]
    fn pr_detail_null_pull_request_is_none() {
        let body = r#"{"data":{"repository":{"pullRequest":null}}}"#;
        let env: Envelope<PrDetailData> = serde_json::from_str(body).unwrap();
        let value = env.data.unwrap().repository.unwrap();
        assert!(pr_detail_from_value(&value).is_none());
    }

    #[test]
    fn pr_detail_bad_list_nodes_degrade_alone() {
        let body = pr_detail_body(
            r#"{
                "id": "PR_2", "number": 7, "title": "t", "state": "OPEN", "isDraft": true,
                "url": "u",
                "commits": {"totalCount": 3, "nodes": [
                    {"commit": {"oid": "aaa", "messageHeadline": "ok"}},
                    {"commit": {"messageHeadline": "no oid — dropped"}},
                    "not-even-an-object"
                ]},
                "reviews": {"totalCount": 0, "nodes": []},
                "comments": {"totalCount": 2, "nodes": [
                    {"author": {"login": "d"}, "body": "kept", "createdAt": null},
                    {"author": {"login": "e"}}
                ]}
            }"#,
        );
        let env: Envelope<PrDetailData> = serde_json::from_str(&body).unwrap();
        let d = pr_detail_from_value(&env.data.unwrap().repository.unwrap()).unwrap();
        // totalCount stays the server's exact number; nodes keep the good ones.
        assert_eq!(d.commits.total_count, 3);
        assert_eq!(d.commits.nodes.len(), 1);
        assert_eq!(d.comments.total_count, 2);
        assert_eq!(d.comments.nodes.len(), 1);
        assert_eq!(d.comments.nodes[0].body, "kept");
        assert!(d.draft);
        // changedFiles absent and files.totalCount absent → None.
        assert_eq!(d.changed_files, None);
    }

    #[test]
    fn pr_detail_changed_files_falls_back_to_files_total() {
        let body = pr_detail_body(
            r#"{"id":"P","number":1,"title":"t","url":"u",
                "files": {"totalCount": 41}}"#,
        );
        let env: Envelope<PrDetailData> = serde_json::from_str(&body).unwrap();
        let d = pr_detail_from_value(&env.data.unwrap().repository.unwrap()).unwrap();
        assert_eq!(d.changed_files, Some(41));
    }
}
