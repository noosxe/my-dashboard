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
        id
        hasIssuesEnabled
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
    id
    hasIssuesEnabled
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
    /// docs/0024 D1: the repository's GraphQL node id — the `createIssue`
    /// target. `None` gates creation off (docs/0024 D2) until a fetch
    /// lands the field.
    #[serde(default)]
    pub node_id: Option<String>,
    /// docs/0024 D1: issues-enabled flag; a cache missing the field
    /// degrades to enabled (assumption errors stay visible, D2).
    pub has_issues_enabled: bool,
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
    /// `statusCheckRollup.state` — the CI signal 0005 deferred,
    /// gathered in 0007 D2 and surfaced in the PR rows (#67).
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
        # 0019 D2: node id + viewer gates + updatedAt per node — the
        # edit/delete affordances and the (edited) marker live off these.
        nodes { id viewerCanUpdate viewerCanDelete updatedAt author { login avatarUrl } state body submittedAt }
      }
      comments(first: 100) {
        totalCount
        # docs/0026 D9: databaseId rides too — the REST id the notification
        # hand-off anchors its jump on (same field the issue query selects).
        nodes {
          id
          databaseId
          viewerCanUpdate
          viewerCanDelete
          updatedAt
          author { login avatarUrl }
          body
          createdAt
        }
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

/// `statusCheckRollup.state` → unified tone for the PR rows (#67).
/// GitHub's rollup states are ERROR / EXPECTED / FAILURE / PENDING /
/// SUCCESS; `null` (no checks) and anything unrecognized yield
/// `None` — the row renders no glyph at all.
pub fn rollup_state(status: Option<&str>) -> Option<CheckState> {
    match status {
        Some("SUCCESS") => Some(CheckState::Success),
        Some("FAILURE") | Some("ERROR") => Some(CheckState::Failure),
        Some("PENDING") | Some("EXPECTED") => Some(CheckState::Pending),
        _ => None,
    }
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

/// `StatusContext.state` (GraphQL `StatusState`: SUCCESS / ERROR /
/// EXPECTED / FAILURE / PENDING) → unified. `EXPECTED` is a required
/// status that hasn't reported yet — Pending, matching `rollup_state`
/// (#174); `rollup_state`'s doc lists the same enum.
pub fn status_context_state(state: Option<&str>) -> CheckState {
    match state {
        Some("SUCCESS") => CheckState::Success,
        Some("ERROR") | Some("FAILURE") => CheckState::Failure,
        Some("PENDING") | Some("EXPECTED") => CheckState::Pending,
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

// ---------------------------------------------------------------------------
// Issue detail page (docs/0020) — the PR page's sibling: one query on
// open, no cache, the same per-node degradation rules. The conversation
// comments are the same `IssueComment` nodes the PR page renders; what
// is new here is the close/reopen write path and the metadata rail.
// ---------------------------------------------------------------------------

/// The issue detail field set (docs/0020 §3 — live-verified via `gh api
/// graphql` introspection, 2026-09-05; the static docql mirror predates
/// the viewer-close fields and is stale for `Issue`): everything the
/// header, the conversation, and the rail's state panel render.
pub const ISSUE_DETAIL_QUERY: &str = r#"
query IssueDetail($owner: String!, $name: String!, $number: Int!) {
  rateLimit { cost remaining resetAt }
  repository(owner: $owner, name: $name) {
    # docs/0023 D5: the lock gate — no viewerCanLock flag exists
    # upstream; triage-and-above locks, read off the repo permission.
    viewerPermission
    issue(number: $number) {
      id
      number
      title
      body
      state
      stateReason
      closedAt
      url
      createdAt
      author { login avatarUrl }
      viewerCanUpdate
      # docs/0022 D1: per-surface gates — title rides viewerCanUpdate,
      # the three pickers each gate independently.
      viewerCanLabel
      viewerCanAssign
      viewerCanSetMilestone
      viewerCanClose
      viewerCanReopen
      # docs/0023 D1: reaction chips + lock state. viewerCanReact gates
      # both reaction surfaces (issue body + comments).
      viewerCanReact
      locked
      activeLockReason
      reactionGroups { content viewerHasReacted reactors { totalCount } }
      # docs/0022 D3: nodes carry node ids — the replace-semantics
      # save computes its target list from them, so a missing id
      # disarms the picker (D3 guard), never a silent drop.
      labels(first: 20) {
        totalCount
        nodes { id name color }
      }
      assignees(first: 20) {
        totalCount
        nodes { id login avatarUrl }
      }
      # docs/0022 D8: the applied milestone — display cell + picker
      # merge row.
      milestone { id title state }
      # docs/0020 D2: the same IssueComment node shape the PR page
      # parses (0019 D2 gate fields included). The reaction fields are
      # issue-query-only (0023 D1) — the PR page's parse degrades them
      # to false/empty until 0023's PR-page follow-up.
      comments(first: 100) {
        totalCount
        nodes {
          id
          # docs/0026 D9: the REST id — matches the thread payload's
          # latest_comment_url tail so a notification can anchor a jump.
          databaseId
          viewerCanUpdate
          viewerCanDelete
          updatedAt
          author { login avatarUrl }
          body
          createdAt
          viewerCanReact
          reactionGroups { content viewerHasReacted reactors { totalCount } }
        }
      }
      # docs/0021 D1: events only — ISSUE_COMMENT is deliberately not in the
      # filter; comments come from the gated connection above (0019 D2/D3).
      timelineItems(first: 100, itemTypes: [ASSIGNED_EVENT, UNASSIGNED_EVENT,
          LABELED_EVENT, UNLABELED_EVENT, CLOSED_EVENT, REOPENED_EVENT,
          RENAMED_TITLE_EVENT, MILESTONED_EVENT, DEMILESTONED_EVENT]) {
        totalCount
        filteredCount
        nodes {
          __typename
          # 2026-09-07 live-API fix: id/createdAt/actor are NOT selectable
          # on the IssueTimelineItems union itself — they ride each inline
          # fragment (the response merges them into the node object, so the
          # parser is unchanged). Bare selections here fail the whole
          # query server-side with "Selections can't be made directly on
          # unions".
          ... on AssignedEvent {
            id
            createdAt
            actor { login }
            assignee { __typename ... on User { login } ... on Bot { login }
              ... on Organization { login } ... on Mannequin { login } }
          }
          ... on UnassignedEvent {
            id
            createdAt
            actor { login }
            assignee { __typename ... on User { login } ... on Bot { login }
              ... on Organization { login } ... on Mannequin { login } }
          }
          ... on LabeledEvent {
            id
            createdAt
            actor { login }
            label { name }
          }
          ... on UnlabeledEvent {
            id
            createdAt
            actor { login }
            label { name }
          }
          ... on ClosedEvent {
            id
            createdAt
            actor { login }
            stateReason
          }
          ... on ReopenedEvent {
            id
            createdAt
            actor { login }
          }
          ... on RenamedTitleEvent {
            id
            createdAt
            actor { login }
            previousTitle currentTitle
          }
          ... on MilestonedEvent {
            id
            createdAt
            actor { login }
            milestoneTitle
          }
          ... on DemilestonedEvent {
            id
            createdAt
            actor { login }
            milestoneTitle
          }
        }
      }
    }
  }
}
"#;

/// Envelope data for the issue detail query — the repository node stays
/// raw so the issue parses through [`issue_detail_from_value`] with the
/// same per-node degradation rules as the PR detail (0007 D3).
#[derive(Debug, Deserialize)]
pub struct IssueDetailData {
    pub repository: Option<serde_json::Value>,
}

/// Everything the issue page renders (docs/0020 D2). Session-only —
/// this never enters a cache.
#[derive(Debug, Clone)]
pub struct IssueDetail {
    /// Node ID — the mutation target (comment subject, body edit, close).
    pub id: String,
    pub number: i64,
    pub title: String,
    pub body: Option<String>,
    /// `IssueState` — OPEN / CLOSED.
    pub state: Option<String>,
    /// `IssueStateReason` — COMPLETED / NOT_PLANNED / REOPENED /
    /// DUPLICATE; null while open.
    pub state_reason: Option<String>,
    pub closed_at: Option<String>,
    /// Gates the description Edit affordance (0020 D4) and the title
    /// editor (0022 D4).
    pub viewer_can_update: bool,
    /// Gates the labels picker (0022 D3).
    pub viewer_can_label: bool,
    /// Gates the assignees picker (0022 D3).
    pub viewer_can_assign: bool,
    /// Gates the milestone picker (0022 D3).
    pub viewer_can_set_milestone: bool,
    /// Gates the rail's Close button (0020 D5).
    pub viewer_can_close: bool,
    /// Gates the rail's Reopen button (0020 D5).
    pub viewer_can_reopen: bool,
    pub url: String,
    pub created_at: Option<String>,
    pub author: Option<Author>,
    /// Applied labels (0022 D3): node ids feed the replace-semantics
    /// save; `id: None` flags a degraded node and disarms the picker.
    pub labels: Vec<IssueLabel>,
    pub labels_total: i64,
    /// Applied assignees — same id story as labels (0022 D3).
    pub assignees: Vec<IssueAssignee>,
    pub assignees_total: i64,
    /// The applied milestone (0022 D8) — `None` when the issue has
    /// none or the node degraded.
    pub milestone: Option<IssueMilestone>,
    pub comments: PrComments,
    /// docs/0021 D2 — timeline events (itemTypes-filtered, comments excluded;
    /// they arrive via [`Self::comments`]). Chronologically unsorted here;
    /// the page's merge sorts (0021 D3).
    /// docs/0021 D2 — timeline events (itemTypes-filtered, comments excluded;
    pub events: Vec<IssueEvent>,
    /// docs/0021 Q2 — `filteredCount` of the events connection: the row
    /// count after the itemTypes filter, fetched page included or not.
    pub events_total: i64,
    /// docs/0021 Q2 — the events connection was capped: `events_total`
    /// exceeded the fetched page.
    pub events_capped: bool,
    /// docs/0023 D1 — gates both reaction surfaces (body + comments).
    pub viewer_can_react: bool,
    /// docs/0023 D3 — the body's reaction groups (comments carry their
    /// own on [`PrComment`]).
    pub reaction_groups: Vec<ReactionGroup>,
    /// docs/0023 D5 — conversation lock state; `active_lock_reason` is
    /// the raw `LockReason` wire string (null while unlocked).
    pub locked: bool,
    pub active_lock_reason: Option<String>,
    /// docs/0023 D5 — the lock gate: no `viewerCanLock` flag exists
    /// upstream, so triage-and-above is read off the repo permission.
    pub viewer_permission: Option<String>,
}

/// `Label { id name color }`. `id` feeds the labels picker's
/// replace-semantics save (0022 D3); `color` is user data — the
/// data-colored chip tint (0022 Q2, #119), degraded to the muted pill
/// when absent or garbled.
#[derive(Debug, Clone)]
pub struct IssueLabel {
    /// `None` = degraded node (id absent) — display works, the D3
    /// guard disarms the picker.
    pub id: Option<String>,
    pub name: String,
    pub color: Option<String>,
}

/// `Assignee { id login avatarUrl }` — the applied-assignee row (0022
/// D3). Replaces the bare [`Author`] rows the rail used to carry: the
/// picker's replace-semantics save needs node ids.
#[derive(Debug, Clone)]
pub struct IssueAssignee {
    /// Same degrade story as [`IssueLabel::id`].
    pub id: Option<String>,
    pub login: String,
    pub avatar_url: String,
}

/// `Milestone { id title state }` — the applied milestone (0022 D8)
/// and, with the same shape, the picker's source rows.
#[derive(Debug, Clone)]
pub struct IssueMilestone {
    pub id: String,
    pub title: String,
    /// `MilestoneState` — OPEN / CLOSED; only the display tag reads it.
    pub state: Option<String>,
}

/// docs/0023 D2 — GitHub's eight reaction contents, in the canonical
/// order the API returns groups in. The glyph mapping lives here (one
/// place, D8); `label` is the picker's text fallback should the platform
/// font lack emoji (D4 contingency).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReactionContent {
    ThumbsUp,
    ThumbsDown,
    Laugh,
    Hooray,
    Confused,
    Heart,
    Rocket,
    Eyes,
}

impl ReactionContent {
    /// Canonical order — chips and picker rows render in this sequence,
    /// never count-sorted (D3: rows don't reshuffle on refetch).
    pub const ALL: [ReactionContent; 8] = [
        ReactionContent::ThumbsUp,
        ReactionContent::ThumbsDown,
        ReactionContent::Laugh,
        ReactionContent::Hooray,
        ReactionContent::Confused,
        ReactionContent::Heart,
        ReactionContent::Rocket,
        ReactionContent::Eyes,
    ];

    /// The wire name (`ReactionContent` enum value).
    pub fn wire(self) -> &'static str {
        match self {
            ReactionContent::ThumbsUp => "THUMBS_UP",
            ReactionContent::ThumbsDown => "THUMBS_DOWN",
            ReactionContent::Laugh => "LAUGH",
            ReactionContent::Hooray => "HOORAY",
            ReactionContent::Confused => "CONFUSED",
            ReactionContent::Heart => "HEART",
            ReactionContent::Rocket => "ROCKET",
            ReactionContent::Eyes => "EYES",
        }
    }

    /// The chip glyph (D4; shortcode fallback if the font lacks emoji).
    pub fn glyph(self) -> &'static str {
        match self {
            ReactionContent::ThumbsUp => "👍",
            ReactionContent::ThumbsDown => "👎",
            ReactionContent::Laugh => "😄",
            ReactionContent::Hooray => "🎉",
            ReactionContent::Confused => "😕",
            ReactionContent::Heart => "❤️",
            ReactionContent::Rocket => "🚀",
            ReactionContent::Eyes => "👀",
        }
    }

    /// The picker's text label (also the D4 shortcode fallback's data).
    pub fn label(self) -> &'static str {
        match self {
            ReactionContent::ThumbsUp => "+1",
            ReactionContent::ThumbsDown => "-1",
            ReactionContent::Laugh => "laugh",
            ReactionContent::Hooray => "hooray",
            ReactionContent::Confused => "confused",
            ReactionContent::Heart => "heart",
            ReactionContent::Rocket => "rocket",
            ReactionContent::Eyes => "eyes",
        }
    }

    /// Unknown wire values skip their group alone (D2, 0007 D3).
    pub fn parse(wire: &str) -> Option<Self> {
        Some(match wire {
            "THUMBS_UP" => ReactionContent::ThumbsUp,
            "THUMBS_DOWN" => ReactionContent::ThumbsDown,
            "LAUGH" => ReactionContent::Laugh,
            "HOORAY" => ReactionContent::Hooray,
            "CONFUSED" => ReactionContent::Confused,
            "HEART" => ReactionContent::Heart,
            "ROCKET" => ReactionContent::Rocket,
            "EYES" => ReactionContent::Eyes,
            _ => return None,
        })
    }
}

/// docs/0023 D2 — one parsed reaction group, shared by the issue body
/// and every comment (same `Reactable` shape upstream).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionGroup {
    pub content: ReactionContent,
    pub viewer_has_reacted: bool,
    pub total: i64,
}

/// Parse a `reactionGroups` array (docs/0023 D2). Unknown or missing
/// `content` skips that group alone; absent array → empty (nothing pins
/// GitHub's all-eight-even-at-zero envelope).
pub fn reaction_groups_from_value(value: Option<&serde_json::Value>) -> Vec<ReactionGroup> {
    value
        .and_then(|v| v.as_array())
        .map(|groups| {
            groups
                .iter()
                .filter_map(|g| {
                    Some(ReactionGroup {
                        content: ReactionContent::parse(g.get("content")?.as_str()?)?,
                        viewer_has_reacted: g
                            .get("viewerHasReacted")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        total: g
                            .pointer("/reactors/totalCount")
                            .and_then(|v| v.as_i64())
                            .unwrap_or_default(),
                    })
                })
                .collect::<Vec<ReactionGroup>>()
        })
        .unwrap_or_default()
}

/// docs/0021 D2 — one timeline event row. Hand-parsed from the
/// `IssueTimelineItems` union per 0007 D3; unknown `__typename` values skip
/// (defensive — the itemTypes filter should make them impossible).
#[derive(Debug, Clone)]
pub struct IssueEvent {
    /// Wire completeness: events are read-only decoration (0021 D4) —
    /// nothing keys on the node id today; interactive event rows (the
    /// deferred cross-reference follow-up) will.
    #[allow(dead_code)]
    pub id: String,
    /// Ghost / deleted actors degrade the copy to "Someone" (0021 D3).
    pub actor_login: Option<String>,
    pub created_at: Option<String>,
    pub kind: IssueEventKind,
}

/// docs/0021 D2 — the event kinds the page renders. Closed carries the
/// state reason so the row copy matches the header badge vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueEventKind {
    Assigned { login: String },
    Unassigned { login: String },
    Labeled { label: String },
    Unlabeled { label: String },
    Closed { reason: Option<String> },
    Reopened,
    Renamed { from: String, to: String },
    Milestoned { title: String },
    Demilestoned { title: String },
}

/// Parse one `repository.issue` node; `None` only when the node is not
/// an object (null / malformed) — the caller keeps its skeleton/error
/// state then. Comment nodes degrade individually (0007 D3).
pub fn issue_detail_from_value(value: &serde_json::Value) -> Option<IssueDetail> {
    let issue = value.get("issue")?;
    if !issue.is_object() {
        return None;
    }
    let str_field = |name: &str| issue.get(name).and_then(|v| v.as_str()).map(str::to_string);
    let comments = issue
        .pointer("/comments/nodes")
        .and_then(|nodes| nodes.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(comment_from_node)
                .collect::<Vec<PrComment>>()
        })
        .unwrap_or_default();
    let comments_total = issue
        .pointer("/comments/totalCount")
        .and_then(|v| v.as_i64())
        .unwrap_or(comments.len() as i64);
    let events = issue
        .pointer("/timelineItems/nodes")
        .and_then(|nodes| nodes.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(issue_event_from_node)
                .collect::<Vec<IssueEvent>>()
        })
        .unwrap_or_default();
    let events_total = issue
        .pointer("/timelineItems/filteredCount")
        .and_then(|v| v.as_i64())
        .unwrap_or(events.len() as i64);
    let events_capped = events_total > events.len() as i64;
    let labels = issue
        .pointer("/labels/nodes")
        .and_then(|nodes| nodes.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|l| {
                    Some(IssueLabel {
                        // 0022 D3: a degraded node keeps rendering —
                        // `id: None` only disarms the picker.
                        id: l.get("id").and_then(|v| v.as_str()).map(str::to_string),
                        name: l.get("name")?.as_str()?.to_string(),
                        color: l.get("color").and_then(|v| v.as_str()).map(str::to_string),
                    })
                })
                .collect::<Vec<IssueLabel>>()
        })
        .unwrap_or_default();
    let assignees = issue
        .pointer("/assignees/nodes")
        .and_then(|nodes| nodes.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|a| {
                    Some(IssueAssignee {
                        id: a.get("id").and_then(|v| v.as_str()).map(str::to_string),
                        login: a.get("login")?.as_str()?.to_string(),
                        avatar_url: a
                            .get("avatarUrl")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    })
                })
                .collect::<Vec<IssueAssignee>>()
        })
        .unwrap_or_default();
    let milestone = issue
        .get("milestone")
        .filter(|m| m.is_object())
        .and_then(|m| {
            Some(IssueMilestone {
                id: m.get("id")?.as_str()?.to_string(),
                title: m
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                state: m.get("state").and_then(|v| v.as_str()).map(str::to_string),
            })
        });
    Some(IssueDetail {
        id: str_field("id").unwrap_or_default(),
        number: issue
            .get("number")
            .and_then(|v| v.as_i64())
            .unwrap_or_default(),
        title: str_field("title").unwrap_or_default(),
        body: str_field("body"),
        state: str_field("state"),
        state_reason: str_field("stateReason"),
        closed_at: str_field("closedAt"),
        viewer_can_update: issue
            .get("viewerCanUpdate")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        viewer_can_label: issue
            .get("viewerCanLabel")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        viewer_can_assign: issue
            .get("viewerCanAssign")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        viewer_can_set_milestone: issue
            .get("viewerCanSetMilestone")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        viewer_can_close: issue
            .get("viewerCanClose")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        viewer_can_reopen: issue
            .get("viewerCanReopen")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        url: str_field("url").unwrap_or_default(),
        created_at: str_field("createdAt"),
        author: author_from_value(issue.get("author")),
        labels,
        labels_total: issue
            .pointer("/labels/totalCount")
            .and_then(|v| v.as_i64())
            .unwrap_or_default(),
        assignees,
        assignees_total: issue
            .pointer("/assignees/totalCount")
            .and_then(|v| v.as_i64())
            .unwrap_or_default(),
        milestone,
        comments: PrComments {
            total_count: comments_total,
            nodes: comments,
        },
        events,
        events_total,
        events_capped,
        // docs/0023 D1 — the lock gate reads off the repository node (the
        // parse input IS the repository object, 0022 D5's envelope shape).
        viewer_can_react: issue
            .get("viewerCanReact")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        reaction_groups: reaction_groups_from_value(issue.get("reactionGroups")),
        locked: issue
            .get("locked")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        active_lock_reason: issue
            .get("activeLockReason")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        viewer_permission: value
            .get("viewerPermission")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

// --- repository-level picker sources (docs/0022 D5) ------------------------

/// Shared envelope for the three picker queries — same shape as
/// [`IssueDetailData`]: the repository node stays raw and parses through
/// the per-source fn with the same degradation rules (0007 D3).
#[derive(Debug, Deserialize)]
pub struct RepoPickerData {
    pub repository: Option<serde_json::Value>,
}

/// docs/0022 §5 — the labels picker's source page, name-ordered.
pub const REPO_LABELS_QUERY: &str = r#"
query RepoLabels($owner: String!, $name: String!) {
  rateLimit { cost remaining resetAt }
  repository(owner: $owner, name: $name) {
    labels(first: 50, orderBy: {field: NAME, direction: ASC}) {
      nodes { id name color }
    }
  }
}
"#;

/// docs/0022 §5 — the assignees picker's source page (logins only here;
/// the page sorts client-side).
pub const REPO_ASSIGNABLE_USERS_QUERY: &str = r#"
query RepoAssignableUsers($owner: String!, $name: String!) {
  rateLimit { cost remaining resetAt }
  repository(owner: $owner, name: $name) {
    assignableUsers(first: 50) {
      nodes { id login avatarUrl }
    }
  }
}
"#;

/// docs/0022 D5 — open milestones only: closed milestones are rarely
/// assignment targets. A closed milestone already applied to the issue
/// still renders (from the issue payload) and stays choosable via the
/// picker's merge row.
pub const REPO_MILESTONES_QUERY: &str = r#"
query RepoMilestones($owner: String!, $name: String!) {
  rateLimit { cost remaining resetAt }
  repository(owner: $owner, name: $name) {
    milestones(first: 50, states: [OPEN]) {
      nodes { id title state }
    }
  }
}
"#;

/// Parse the repository node of [`REPO_LABELS_QUERY`]. Nodes without an
/// `id` skip — the picker exists to produce ids (a name-only row could
/// never be written back); display coverage is the issue payload's job.
pub fn repo_labels_from_value(value: &serde_json::Value) -> Vec<IssueLabel> {
    value
        .pointer("/labels/nodes")
        .and_then(|nodes| nodes.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|l| {
                    Some(IssueLabel {
                        id: Some(l.get("id")?.as_str()?.to_string()),
                        name: l.get("name")?.as_str()?.to_string(),
                        color: l.get("color").and_then(|v| v.as_str()).map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse the repository node of [`REPO_ASSIGNABLE_USERS_QUERY`]. Same
/// skip rule as [`repo_labels_from_value`].
pub fn repo_assignable_users_from_value(value: &serde_json::Value) -> Vec<IssueAssignee> {
    value
        .pointer("/assignableUsers/nodes")
        .and_then(|nodes| nodes.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|a| {
                    Some(IssueAssignee {
                        id: Some(a.get("id")?.as_str()?.to_string()),
                        login: a.get("login")?.as_str()?.to_string(),
                        avatar_url: a
                            .get("avatarUrl")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse the repository node of [`REPO_MILESTONES_QUERY`]. `id` is
/// required (`IssueMilestone::id` is a plain `String` — the picker row
/// is the write target); partial nodes skip.
pub fn repo_milestones_from_value(value: &serde_json::Value) -> Vec<IssueMilestone> {
    value
        .pointer("/milestones/nodes")
        .and_then(|nodes| nodes.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|m| {
                    Some(IssueMilestone {
                        id: m.get("id")?.as_str()?.to_string(),
                        title: m.get("title")?.as_str()?.to_string(),
                        state: m.get("state").and_then(|v| v.as_str()).map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// docs/0021 D2 — parse one union node of the filtered `timelineItems`
/// connection. `None` skips: unknown `__typename`, or a kind-specific field
/// missing that the row copy cannot render without (0007 D3 degrade).
fn issue_event_from_node(node: &serde_json::Value) -> Option<IssueEvent> {
    let typename = node.get("__typename")?.as_str()?;
    let kind = match typename {
        "AssignedEvent" => IssueEventKind::Assigned {
            login: assignee_login(node)?,
        },
        "UnassignedEvent" => IssueEventKind::Unassigned {
            login: assignee_login(node)?,
        },
        "LabeledEvent" => IssueEventKind::Labeled {
            label: node.pointer("/label/name")?.as_str()?.to_string(),
        },
        "UnlabeledEvent" => IssueEventKind::Unlabeled {
            label: node.pointer("/label/name")?.as_str()?.to_string(),
        },
        "ClosedEvent" => IssueEventKind::Closed {
            reason: node
                .get("stateReason")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        },
        "ReopenedEvent" => IssueEventKind::Reopened,
        "RenamedTitleEvent" => IssueEventKind::Renamed {
            from: node.get("previousTitle")?.as_str()?.to_string(),
            to: node.get("currentTitle")?.as_str()?.to_string(),
        },
        "MilestonedEvent" => IssueEventKind::Milestoned {
            title: node.get("milestoneTitle")?.as_str()?.to_string(),
        },
        "DemilestonedEvent" => IssueEventKind::Demilestoned {
            title: node.get("milestoneTitle")?.as_str()?.to_string(),
        },
        _ => return None,
    };
    Some(IssueEvent {
        id: node
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        actor_login: node
            .pointer("/actor/login")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        created_at: node
            .get("createdAt")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        kind,
    })
}

/// docs/0021 §3 — `assignee` is a singular `Assignee` union; the response
/// carries the resolved object, so a plain pointer read suffices.
fn assignee_login(node: &serde_json::Value) -> Option<String> {
    node.pointer("/assignee/login")?
        .as_str()
        .map(str::to_string)
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
    /// 0019 D2: node id — the edit/delete mutation target. Empty when the
    /// payload omits it: affordances hide (per-node degrade).
    pub id: String,
    /// 0019 D3: gating is the schema's flags, nothing else.
    pub viewer_can_update: bool,
    pub viewer_can_delete: bool,
    /// The (edited) marker (0019 D4).
    pub updated_at: Option<String>,
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
    /// 0019 D2: node id — the edit/delete mutation target. Empty when the
    /// payload omits it: affordances hide (per-node degrade).
    pub id: String,
    /// docs/0026 D9: the REST database id — the notification hand-off
    /// anchors its jump on it. `None` degrades to "not anchorable" (the
    /// field absent from the payload); never a drop condition.
    pub database_id: Option<i64>,
    /// 0019 D3: gating is the schema's flags, nothing else.
    pub viewer_can_update: bool,
    pub viewer_can_delete: bool,
    /// The (edited) marker (0019 D4).
    pub updated_at: Option<String>,
    /// docs/0023 D1 — comment-level reaction chips. The PR page's query
    /// doesn't select these yet, so its parse degrades to false/empty.
    pub viewer_can_react: bool,
    pub reaction_groups: Vec<ReactionGroup>,
}

/// The shared per-node edit/delete fields (0019 D2), read identically
/// wherever a node is parsed: (id, viewerCanUpdate, viewerCanDelete,
/// updatedAt). Missing = degraded: empty id, false flags.
fn node_gate(n: &serde_json::Value) -> (String, bool, bool, Option<String>) {
    (
        n.get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        n.get("viewerCanUpdate")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        n.get("viewerCanDelete")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        n.get("updatedAt")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    )
}

/// One `IssueComment` node → [`PrComment`] — the same struct serves the
/// PR and issue conversations (docs/0020 D2; both timelines render
/// `IssueComment` nodes). Bad nodes drop alone (0007 D3).
fn comment_from_node(c: &serde_json::Value) -> Option<PrComment> {
    let (id, viewer_can_update, viewer_can_delete, updated_at) = node_gate(c);
    Some(PrComment {
        author: author_from_value(c.get("author")),
        body: c.get("body")?.as_str()?.to_string(),
        created_at: c
            .get("createdAt")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        id,
        database_id: c.get("databaseId").and_then(|v| v.as_i64()),
        viewer_can_update,
        viewer_can_delete,
        updated_at,
        // docs/0023 D1 — degrade to false/empty when the query doesn't
        // select the reaction fields (the PR page's doesn't, yet).
        viewer_can_react: c
            .get("viewerCanReact")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        reaction_groups: reaction_groups_from_value(c.get("reactionGroups")),
    })
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
                .map(|r| {
                    let (id, viewer_can_update, viewer_can_delete, updated_at) = node_gate(r);
                    PrReview {
                        author: author_from_value(r.get("author")),
                        state: r.get("state").and_then(|v| v.as_str()).map(str::to_string),
                        body: r.get("body").and_then(|v| v.as_str()).map(str::to_string),
                        submitted_at: r
                            .get("submittedAt")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        id,
                        viewer_can_update,
                        viewer_can_delete,
                        updated_at,
                    }
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
                .filter_map(comment_from_node)
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

/// Edit a conversation comment (docs/0019 D5,
/// `UpdateIssueCommentInput { id body }`).
pub const PR_UPDATE_COMMENT_MUTATION: &str = r#"
mutation PrUpdateComment($input: UpdateIssueCommentInput!) {
  updateIssueComment(input: $input) { clientMutationId }
}
"#;

/// Delete a conversation comment (`DeleteIssueCommentInput { id }`).
pub const PR_DELETE_COMMENT_MUTATION: &str = r#"
mutation PrDeleteComment($input: DeleteIssueCommentInput!) {
  deleteIssueComment(input: $input) { clientMutationId }
}
"#;

/// Edit a review summary body (docs/0019 D5,
/// `UpdatePullRequestReviewInput { pullRequestReviewId body }`).
pub const PR_UPDATE_REVIEW_MUTATION: &str = r#"
mutation PrUpdateReview($input: UpdatePullRequestReviewInput!) {
  updatePullRequestReview(input: $input) { clientMutationId }
}
"#;

/// Delete a review — the whole review goes, its APPROVED /
/// REQUEST_CHANGES state with it (docs/0019 D1).
pub const PR_DELETE_REVIEW_MUTATION: &str = r#"
mutation PrDeleteReview($input: DeletePullRequestReviewInput!) {
  deletePullRequestReview(input: $input) { clientMutationId }
}
"#;

/// Close an issue (docs/0020 D2, `CloseIssueInput { issueId stateReason
/// … }` — live-verified 2026-09-05): the stateReason is optional
/// upstream, but the panel always sends the user's choice (default
/// COMPLETED), so the wire state is never implicit.
pub const ISSUE_CLOSE_MUTATION: &str = r#"
mutation IssueClose($input: CloseIssueInput!) {
  closeIssue(input: $input) { issue { id state stateReason } }
}
"#;

/// Reopen an issue (docs/0020 D2, `ReopenIssueInput { issueId }`).
pub const ISSUE_REOPEN_MUTATION: &str = r#"
mutation IssueReopen($input: ReopenIssueInput!) {
  reopenIssue(input: $input) { issue { id state } }
}
"#;

/// docs/0024 D5 — the creator's single document, the minimum input:
/// `repositoryId` (the GraphQL node id — NOT the numeric `databaseId`,
/// live-verified 2026-09-07) plus `title`; `body` optional. The triage
/// fields (`labelIds`, `assigneeIds`, `milestoneId`, …) are deliberately
/// absent — post-create metadata editing is the 0022 pickers' job.
pub const CREATE_ISSUE_MUTATION: &str = r#"
mutation CreateIssue($input: CreateIssueInput!) {
  createIssue(input: $input) { issue { number title url updatedAt } }
}
"#;
/// docs/0024 D6 — parse a successful `createIssue` payload and
/// synthesize the minimal snapshot row the host needs to open the
/// in-app issue page (initial paint only; the page re-fetches its own
/// truth). GraphQL `errors` or a malformed payload → `None` (the
/// caller turns that into a visible scoped error, never a silent drop).
pub fn created_issue_from_body(body: &str, repo_full: &str) -> Option<Issue> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Payload {
        create_issue: CreatedNode,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct CreatedNode {
        issue: IssueCreated,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct IssueCreated {
        number: i64,
        #[serde(default)]
        title: String,
        #[serde(default)]
        url: String,
        #[serde(default)]
        updated_at: Option<String>,
    }
    let env: Envelope<Payload> = serde_json::from_str(body).ok()?;
    let issue = env.data?.create_issue.issue;
    Some(Issue {
        key: format!("{}#{}", repo_full, issue.number),
        database_id: None,
        number: issue.number,
        title: issue.title,
        author: None,
        updated_at: issue.updated_at,
        url: issue.url,
    })
}
/// docs/0022 D1 — every metadata write (title, labels, assignees,
/// milestone) rides `updateIssue`; the input carries exactly one
/// field-group per post, the op identity living in the `Mutation`
/// variant. Same document text the body editors post.
pub const ISSUE_UPDATE_MUTATION: &str = r#"
mutation IssueUpdateMetadata($input: UpdateIssueInput!) {
  updateIssue(input: $input) { issue { id } }
}
"#;

/// docs/0023 D3/D7 — the reaction toggles. `subjectId` is the issue or
/// comment node id, `content` the `ReactionContent` wire name; add and
/// remove share the input shape, so two documents over the same pattern.
pub const ADD_REACTION_MUTATION: &str = r#"
mutation AddReaction($input: AddReactionInput!) {
  addReaction(input: $input) { clientMutationId }
}
"#;

pub const REMOVE_REACTION_MUTATION: &str = r#"
mutation RemoveReaction($input: RemoveReactionInput!) {
  removeReaction(input: $input) { clientMutationId }
}
"#;

/// docs/0023 D5 — conversation locking. `lockReason` is omitted in v1
/// (input optional; GitHub stores null → generic "Locked" tag); unlock
/// carries only the `lockableId`. Payload records are ignored — the page
/// refetches (D7).
pub const LOCK_LOCKABLE_MUTATION: &str = r#"
mutation LockLockable($input: LockLockableInput!) {
  lockLockable(input: $input) { clientMutationId }
}
"#;

pub const UNLOCK_LOCKABLE_MUTATION: &str = r#"
mutation UnlockLockable($input: UnlockLockableInput!) {
  unlockLockable(input: $input) { clientMutationId }
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
    // docs/0024 D1: the GraphQL node id (NOT the numeric databaseId) —
    // the `createIssue` mutation target; absent in pre-0024 cached
    // snapshots (creation stays gated off until the next fetch).
    id: Option<String>,
    // docs/0024 D1/D2: `None` = field missing (old cache) → assumed
    // enabled; a wrong assumption dies as a visible scoped mutation
    // error, never a silently hidden button.
    #[serde(default)]
    has_issues_enabled: Option<bool>,
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
        node_id: node.id,
        has_issues_enabled: node.has_issues_enabled.unwrap_or(true),
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
// PR review threads (docs/0030 D1/D3) — stage 1 read side
// ---------------------------------------------------------------------------

pub const PR_THREADS_QUERY: &str = r#"
query PrThreads($owner: String!, $name: String!, $number: Int!) {
  rateLimit { cost remaining resetAt }
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      id
      reviewThreads(first: 100) {
        totalCount
        nodes {
          id
          path
          line
          startLine
          diffSide
          isOutdated
          isResolved
          resolvedBy { login }
          subjectType
          viewerCanReply
          viewerCanResolve
          viewerCanUnresolve
          comments(first: 100) {
            totalCount
            nodes {
              id
              author { login avatarUrl }
              body
              createdAt
              updatedAt
              lastEditedAt
              url
              viewerCanUpdate
              viewerCanDelete
              viewerDidAuthor
            }
          }
        }
      }
    }
  }
}
"#;

/// Envelope data for the threads query — the repository node stays raw so
/// the parse runs through [`pr_threads_from_value`] with the same per-node
/// degradation rules as the detail (doc 0007 D3).
#[derive(Debug, Deserialize)]
pub struct PrThreadsData {
    pub repository: Option<serde_json::Value>,
}

/// Which side of the diff a thread anchors to (`DiffSide`). Read-side
/// name is `diffSide`; the write inputs say `side` (0030 §3 asymmetry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffSide {
    Left,
    Right,
}

/// `PullRequestReviewThreadSubjectType` — a diff line or the whole file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadSubject {
    Line,
    File,
}

/// The PR's review threads (docs/0030 D3). Session-only, page-owned.
#[derive(Debug, Clone, Default)]
pub struct PrThreads {
    pub total_count: i64,
    pub nodes: Vec<PrThread>,
}

/// One review thread. A node without `id` or `path` drops alone —
/// nothing else on it is anchorable (0007 D3 degrade).
#[derive(Debug, Clone)]
pub struct PrThread {
    pub id: String,
    pub path: String,
    /// End line of the anchored range; `None` for file-subject threads.
    pub line: Option<i64>,
    /// Start line (multi-line threads only).
    pub start_line: Option<i64>,
    pub side: DiffSide,
    pub subject: ThreadSubject,
    pub is_outdated: bool,
    pub is_resolved: bool,
    pub resolved_by: Option<String>,
    pub viewer_can_reply: bool,
    pub viewer_can_resolve: bool,
    pub viewer_can_unresolve: bool,
    /// Authored order preserved.
    pub comments: Vec<PrThreadComment>,
}

/// One comment inside a thread (docs/0030 D3). The `url` permalink backs
/// the thread header's Open-on-GitHub button (D6 — applying suggested
/// changes has no public API, so that is where the flow finishes).
#[derive(Debug, Clone)]
pub struct PrThreadComment {
    pub id: String,
    pub author: Option<Author>,
    pub body: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub last_edited_at: Option<String>,
    pub url: Option<String>,
    /// Stage-2 affordance gates, selected now so the payload doesn't
    /// change later (0030 D3).
    pub viewer_can_update: bool,
    pub viewer_can_delete: bool,
    pub viewer_did_author: bool,
}

/// Parse one `repository` wrapper node for its PR's review threads.
/// `None` only when the pullRequest node is missing/malformed — the caller
/// degrades to an empty thread set then (the diff pane must not break).
pub fn pr_threads_from_value(value: &serde_json::Value) -> Option<PrThreads> {
    let pr = value.get("pullRequest")?;
    if !pr.is_object() {
        return None;
    }
    let nodes = pr
        .pointer("/reviewThreads/nodes")
        .and_then(|n| n.as_array());
    let threads: Vec<PrThread> = nodes
        .map(|nodes| nodes.iter().filter_map(thread_from_node).collect())
        .unwrap_or_default();
    let total_count = pr
        .pointer("/reviewThreads/totalCount")
        .and_then(|v| v.as_i64())
        .unwrap_or(threads.len() as i64);
    Some(PrThreads {
        total_count,
        nodes: threads,
    })
}

fn diff_side(v: Option<&serde_json::Value>) -> DiffSide {
    match v.and_then(|v| v.as_str()) {
        Some("LEFT") => DiffSide::Left,
        _ => DiffSide::Right,
    }
}

fn thread_from_node(n: &serde_json::Value) -> Option<PrThread> {
    let id = n.get("id")?.as_str()?.to_string();
    let path = n.get("path")?.as_str()?.to_string();
    let comments = n
        .pointer("/comments/nodes")
        .and_then(|c| c.as_array())
        .map(|nodes| nodes.iter().filter_map(thread_comment_from_node).collect())
        .unwrap_or_default();
    Some(PrThread {
        id,
        path,
        line: n.get("line").and_then(|v| v.as_i64()),
        start_line: n.get("startLine").and_then(|v| v.as_i64()),
        side: diff_side(n.get("diffSide")),
        subject: match n.get("subjectType").and_then(|v| v.as_str()) {
            Some("FILE") => ThreadSubject::File,
            _ => ThreadSubject::Line,
        },
        is_outdated: n
            .get("isOutdated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        is_resolved: n
            .get("isResolved")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        resolved_by: n
            .pointer("/resolvedBy/login")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        viewer_can_reply: n
            .get("viewerCanReply")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        viewer_can_resolve: n
            .get("viewerCanResolve")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        viewer_can_unresolve: n
            .get("viewerCanUnresolve")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        comments,
    })
}

fn thread_comment_from_node(n: &serde_json::Value) -> Option<PrThreadComment> {
    Some(PrThreadComment {
        id: n.get("id")?.as_str()?.to_string(),
        author: author_from_value(n.get("author")),
        body: n.get("body")?.as_str()?.to_string(),
        created_at: n
            .get("createdAt")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        updated_at: n
            .get("updatedAt")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        last_edited_at: n
            .get("lastEditedAt")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        url: n.get("url").and_then(|v| v.as_str()).map(str::to_string),
        viewer_can_update: n
            .get("viewerCanUpdate")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        viewer_can_delete: n
            .get("viewerCanDelete")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        viewer_did_author: n
            .get("viewerDidAuthor")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    /// GraphQL comments are `#` — a JS-style `//` inside a document makes
    /// GitHub reject the whole request. Caught live on 2026-09-04: the
    /// detail query shipped with a `//` note and every PR tab errored.
    #[test]
    fn gql_documents_use_hash_comments_only() {
        for (name, doc) in [
            ("PR_DETAIL_QUERY", PR_DETAIL_QUERY),
            ("PR_ADD_COMMENT_MUTATION", PR_ADD_COMMENT_MUTATION),
            ("PR_ADD_REVIEW_MUTATION", PR_ADD_REVIEW_MUTATION),
            (
                "PR_UPDATE_DESCRIPTION_MUTATION",
                PR_UPDATE_DESCRIPTION_MUTATION,
            ),
            ("PR_MERGE_MUTATION", PR_MERGE_MUTATION),
            ("PR_CLOSE_MUTATION", PR_CLOSE_MUTATION),
            ("PR_UPDATE_COMMENT_MUTATION", PR_UPDATE_COMMENT_MUTATION),
            ("PR_DELETE_COMMENT_MUTATION", PR_DELETE_COMMENT_MUTATION),
            ("PR_UPDATE_REVIEW_MUTATION", PR_UPDATE_REVIEW_MUTATION),
            ("PR_DELETE_REVIEW_MUTATION", PR_DELETE_REVIEW_MUTATION),
            ("ISSUE_DETAIL_QUERY", ISSUE_DETAIL_QUERY),
            ("ISSUE_CLOSE_MUTATION", ISSUE_CLOSE_MUTATION),
            ("ISSUE_REOPEN_MUTATION", ISSUE_REOPEN_MUTATION),
            ("ISSUE_UPDATE_MUTATION", ISSUE_UPDATE_MUTATION),
            ("REPO_LABELS_QUERY", REPO_LABELS_QUERY),
            ("REPO_ASSIGNABLE_USERS_QUERY", REPO_ASSIGNABLE_USERS_QUERY),
            ("REPO_MILESTONES_QUERY", REPO_MILESTONES_QUERY),
        ] {
            assert!(!doc.contains("//"), "{name} contains a JS-style comment");
        }
    }

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
            r#"{{"databaseId":7,"id":"R_kgDOABC","hasIssuesEnabled":true,"name":"{}","nameWithOwner":"{full_name}",
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
    fn repo_node_carries_creation_fields_and_degrades_tolerantly() {
        // docs/0024 D1: id + hasIssuesEnabled ride the snapshot; the
        // node id is the GraphQL id, NOT the numeric databaseId.
        let repo = repo_from_value(&repo_value("o/r", "", 0)).unwrap();
        assert_eq!(repo.node_id.as_deref(), Some("R_kgDOABC"));
        assert!(repo.has_issues_enabled);

        // Old cached snapshots lack the fields: id → None (creation
        // gated off), hasIssuesEnabled → assumed enabled (D1).
        let mut value = repo_value("o/r", "", 0);
        value.as_object_mut().unwrap().remove("id").unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("hasIssuesEnabled")
            .unwrap();
        let repo = repo_from_value(&value).unwrap();
        assert_eq!(repo.node_id, None);
        assert!(repo.has_issues_enabled);

        // Issues disabled survives the parse verbatim.
        let mut value = repo_value("o/r", "", 0);
        value["hasIssuesEnabled"] = serde_json::json!(false);
        let repo = repo_from_value(&value).unwrap();
        assert!(!repo.has_issues_enabled);
    }

    #[test]
    fn rollup_state_maps_and_rejects() {
        // #67: the five real rollup states tone-map; null (no checks)
        // and garbage render nothing.
        assert_eq!(rollup_state(Some("SUCCESS")), Some(CheckState::Success));
        assert_eq!(rollup_state(Some("FAILURE")), Some(CheckState::Failure));
        assert_eq!(rollup_state(Some("ERROR")), Some(CheckState::Failure));
        assert_eq!(rollup_state(Some("PENDING")), Some(CheckState::Pending));
        assert_eq!(rollup_state(Some("EXPECTED")), Some(CheckState::Pending));
        assert_eq!(rollup_state(None), None);
        assert_eq!(rollup_state(Some("NEUTRAL")), None);
        assert_eq!(rollup_state(Some("")), None);
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
                     "databaseId": 987654,
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
        // docs/0026 D9: the REST id rides through for the anchor jump.
        assert_eq!(d.comments.nodes[0].database_id, Some(987_654));
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
        assert_eq!(status_context_state(Some("EXPECTED")), CheckState::Pending);
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

    // --- timeline-row gates (docs/0019 D2/D3) --------------------------

    #[test]
    fn pr_detail_parses_row_gate_fields() {
        let body = pr_detail_body(
            r#"{
                "id": "PR_2", "number": 7, "title": "t", "url": "u",
                "reviews": {"totalCount": 1, "nodes": [
                    {"id": "PRR_1", "viewerCanUpdate": true,
                     "viewerCanDelete": true,
                     "updatedAt": "2026-09-01T10:00:00Z",
                     "author": {"login": "bob"}, "state": "APPROVED",
                     "body": "lgtm", "submittedAt": "2026-09-01T09:00:00Z"}
                ]},
                "comments": {"totalCount": 2, "nodes": [
                    {"id": "IC_1", "viewerCanUpdate": true,
                     "viewerCanDelete": false,
                     "updatedAt": "2026-09-02T10:00:00Z",
                     "author": {"login": "carol"}, "body": "nice",
                     "createdAt": "2026-09-02T09:00:00Z"},
                    {"author": null, "body": "plain",
                     "createdAt": "2026-09-03T09:00:00Z"}
                ]}
            }"#,
        );
        let env: Envelope<PrDetailData> = serde_json::from_str(&body).unwrap();
        let value = env.data.unwrap().repository.unwrap();
        let d = pr_detail_from_value(&value).unwrap();
        let r = &d.reviews.nodes[0];
        assert_eq!(r.id, "PRR_1");
        assert!(r.viewer_can_update);
        assert!(r.viewer_can_delete);
        assert_eq!(r.updated_at.as_deref(), Some("2026-09-01T10:00:00Z"));
        let c = &d.comments.nodes[0];
        assert_eq!(c.id, "IC_1");
        assert!(c.viewer_can_update);
        assert!(!c.viewer_can_delete);
        assert_eq!(c.updated_at.as_deref(), Some("2026-09-02T10:00:00Z"));
    }

    #[test]
    fn pr_detail_absent_row_gate_fields_degrade_off() {
        // A node without the gate fields (old payload shape, or the
        // fields failing to come back) parses fine with affordances off
        // — one bad node never takes the page down (0007 D3).
        let body = pr_detail_body(
            r#"{
                "id": "PR_2", "number": 7, "title": "t", "url": "u",
                "reviews": {"totalCount": 1, "nodes": [
                    {"author": {"login": "bob"}, "state": "APPROVED",
                     "body": "lgtm", "submittedAt": "2026-09-01T09:00:00Z"}
                ]},
                "comments": {"totalCount": 1, "nodes": [
                    {"author": {"login": "carol"}, "body": "nice",
                     "createdAt": "2026-09-02T09:00:00Z"}
                ]}
            }"#,
        );
        let env: Envelope<PrDetailData> = serde_json::from_str(&body).unwrap();
        let value = env.data.unwrap().repository.unwrap();
        let d = pr_detail_from_value(&value).unwrap();
        for r in &d.reviews.nodes {
            assert_eq!(r.id, "");
            assert!(!r.viewer_can_update);
            assert!(!r.viewer_can_delete);
            assert_eq!(r.updated_at, None);
        }
        for c in &d.comments.nodes {
            assert_eq!(c.id, "");
            assert!(!c.viewer_can_update);
            assert!(!c.viewer_can_delete);
            assert_eq!(c.updated_at, None);
        }
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

    // --- issue detail (docs/0020 D2) ------------------------------------

    fn issue_detail_body(issue_json: &str) -> String {
        format!(r#"{{"data":{{"repository":{{"issue":{issue_json}}}}}}}"#)
    }

    #[test]
    fn issue_detail_parses_full_node() {
        let body = issue_detail_body(
            r#"{
                "id": "I_1", "number": 487, "title": "Race on retry",
                "body": "steps to reproduce", "state": "CLOSED",
                "stateReason": "NOT_PLANNED", "closedAt": "2026-09-02T00:00:00Z",
                "url": "u", "createdAt": "2026-09-01T00:00:00Z",
                "author": {"login": "alice", "avatarUrl": "a"},
                "viewerCanUpdate": true, "viewerCanClose": false, "viewerCanReopen": true,
                "labels": {"totalCount": 3, "nodes": [
                    {"name": "bug", "color": "ff0000"},
                    {"name": "p1"}
                ]},
                "assignees": {"totalCount": 1, "nodes": [
                    {"login": "bob", "avatarUrl": "b"}
                ]},
                "comments": {"totalCount": 2, "nodes": [
                    {"id": "IC_1", "viewerCanUpdate": true, "viewerCanDelete": false,
                     "updatedAt": "2026-09-03T02:00:00Z",
                     "author": {"login": "dave"}, "body": "nice",
                     "createdAt": "2026-09-03T01:00:00Z"},
                    {"author": {"login": "erin"}, "body": "plain",
                     "createdAt": "2026-09-04T01:00:00Z"}
                ]}
            }"#,
        );
        let env: Envelope<IssueDetailData> = serde_json::from_str(&body).unwrap();
        let d = issue_detail_from_value(&env.data.unwrap().repository.unwrap()).unwrap();
        assert_eq!(d.id, "I_1");
        assert_eq!(d.number, 487);
        assert_eq!(d.state.as_deref(), Some("CLOSED"));
        assert_eq!(d.state_reason.as_deref(), Some("NOT_PLANNED"));
        assert_eq!(d.closed_at.as_deref(), Some("2026-09-02T00:00:00Z"));
        assert!(d.viewer_can_update);
        assert!(!d.viewer_can_close);
        assert!(d.viewer_can_reopen);
        assert_eq!(d.author.as_ref().unwrap().login, "alice");
        // Labels parse; a missing color degrades to None, not the node.
        assert_eq!(d.labels_total, 3);
        assert_eq!(d.labels.len(), 2);
        assert_eq!(d.labels[0].name, "bug");
        assert_eq!(d.labels[0].color.as_deref(), Some("ff0000"));
        assert_eq!(d.labels[1].color, None);
        assert_eq!(d.assignees.len(), 1);
        assert_eq!(d.assignees[0].login, "bob");
        // Same comment rows as the PR timeline — gates per node (0019).
        assert_eq!(d.comments.total_count, 2);
        assert_eq!(d.comments.nodes.len(), 2);
        assert_eq!(d.comments.nodes[0].id, "IC_1");
        assert!(d.comments.nodes[0].viewer_can_update);
        assert!(!d.comments.nodes[0].viewer_can_delete);
        assert_eq!(d.comments.nodes[1].id, "");
    }

    // 2026-09-07 live-API regression (#118 follow-up): bare id/createdAt/
    // actor selections on the IssueTimelineItems union fail the whole
    // query server-side ("Selections can't be made directly on unions")
    // — fixture tests never hit the wire, so the tripwire lives on the
    // document: every member fragment must carry the shared fields.
    #[test]
    fn timeline_fragments_carry_the_shared_node_fields() {
        const MEMBERS: [&str; 9] = [
            "AssignedEvent",
            "UnassignedEvent",
            "LabeledEvent",
            "UnlabeledEvent",
            "ClosedEvent",
            "ReopenedEvent",
            "RenamedTitleEvent",
            "MilestonedEvent",
            "DemilestonedEvent",
        ];
        let mut spans = Vec::new();
        for member in MEMBERS {
            let marker = format!("... on {member} {{");
            let at = ISSUE_DETAIL_QUERY
                .find(&marker)
                .unwrap_or_else(|| panic!("{member} fragment missing from query"));
            spans.push((at, member));
        }
        spans.sort();
        for (ix, (start, member)) in spans.iter().enumerate() {
            let end = spans
                .get(ix + 1)
                .map(|(next, _)| *next)
                .unwrap_or(ISSUE_DETAIL_QUERY.len());
            let fragment = &ISSUE_DETAIL_QUERY[*start..end];
            assert!(fragment.contains("id"), "{member}: id missing");
            assert!(
                fragment.contains("createdAt"),
                "{member}: createdAt missing"
            );
            assert!(
                fragment.contains("actor { login }"),
                "{member}: actor missing"
            );
        }
        // And the union itself stays bare: __typename only.
        let nodes_at = ISSUE_DETAIL_QUERY
            .find("timelineItems")
            .map(|at| ISSUE_DETAIL_QUERY[at..].find("nodes {").unwrap() + at)
            .unwrap();
        let head = &ISSUE_DETAIL_QUERY[nodes_at..nodes_at + 40];
        assert!(
            !head.contains("\nid") && !head.contains("\n          id"),
            "bare union selection crept back in"
        );
    }

    #[test]
    fn issue_detail_degrades_absent_groups_and_null_node() {
        // No labels/assignees/comments at all → empty groups, page intact.
        let body =
            issue_detail_body(r#"{"id":"I_2","number":1,"title":"t","url":"u","state":"OPEN"}"#);
        let env: Envelope<IssueDetailData> = serde_json::from_str(&body).unwrap();
        let d = issue_detail_from_value(&env.data.unwrap().repository.unwrap()).unwrap();
        assert!(d.labels.is_empty());
        assert_eq!(d.labels_total, 0);
        assert!(d.assignees.is_empty());
        assert!(d.comments.nodes.is_empty());
        assert_eq!(d.state_reason, None);
        assert_eq!(d.closed_at, None);
        // Absent viewer flags degrade off — affordances hidden (0007 D3).
        assert!(!d.viewer_can_update);
        assert!(!d.viewer_can_close);
        assert!(!d.viewer_can_reopen);

        // Null / malformed issue → None (caller keeps its error state).
        let env: Envelope<IssueDetailData> =
            serde_json::from_str(r#"{"data":{"repository":{"issue":null}}}"#).unwrap();
        let value = env.data.unwrap().repository.unwrap();
        assert!(issue_detail_from_value(&value).is_none());
    }

    // docs/0023 §2/D1/D2 — reaction groups parse on the issue and its
    // comments; the lock state and the repository permission ride along;
    // unknown contents and absent arrays degrade per node.
    #[test]
    fn issue_reactions_lock_and_permission_parse() {
        // viewerPermission rides the repository node, not the issue —
        // splice it in beside the issue object on the envelope.
        let body = issue_detail_body(
            r#"{
                "id": "I_5", "number": 12, "title": "t", "url": "u", "state": "OPEN",
                "viewerCanReact": true,
                "locked": true, "activeLockReason": "TOO_HEATED",
                "reactionGroups": [
                    {"content": "THUMBS_UP", "viewerHasReacted": true, "reactors": {"totalCount": 3}},
                    {"content": "EYES", "viewerHasReacted": false, "reactors": {"totalCount": 0}},
                    {"content": "NOT_A_THING", "viewerHasReacted": false, "reactors": {"totalCount": 9}}
                ],
                "comments": {"totalCount": 1, "nodes": [
                    {"id": "IC_1", "body": "c", "viewerCanReact": true,
                     "reactionGroups": [
                        {"content": "HEART", "viewerHasReacted": true, "reactors": {"totalCount": 2}}
                     ]}
                ]}
            }"#,
        )
        .replacen(
            r#""repository":{"issue""#,
            r#""repository":{"viewerPermission":"ADMIN","issue""#,
            1,
        );
        let env: Envelope<IssueDetailData> = serde_json::from_str(&body).unwrap();
        let d = issue_detail_from_value(&env.data.unwrap().repository.unwrap()).unwrap();
        assert!(d.viewer_can_react);
        assert!(d.locked);
        assert_eq!(d.active_lock_reason.as_deref(), Some("TOO_HEATED"));
        // The lock gate reads off the repository node (D5).
        assert_eq!(d.viewer_permission.as_deref(), Some("ADMIN"));
        // Unknown content skips its group alone; order preserved.
        assert_eq!(d.reaction_groups.len(), 2);
        assert_eq!(d.reaction_groups[0].content, ReactionContent::ThumbsUp);
        assert!(d.reaction_groups[0].viewer_has_reacted);
        assert_eq!(d.reaction_groups[0].total, 3);
        assert_eq!(d.reaction_groups[1].content, ReactionContent::Eyes);
        let c = &d.comments.nodes[0];
        assert!(c.viewer_can_react);
        assert_eq!(c.reaction_groups.len(), 1);
        assert_eq!(c.reaction_groups[0].content, ReactionContent::Heart);
        assert_eq!(c.reaction_groups[0].total, 2);
    }

    #[test]
    fn issue_reactions_degrade_absent_and_null_permission() {
        // Absent reaction/lock/permission fields → the degraded page:
        // no chips, unlocked, no lock rights (PR query parity).
        let body =
            issue_detail_body(r#"{"id":"I_6","number":2,"title":"t","url":"u","state":"OPEN"}"#);
        let env: Envelope<IssueDetailData> = serde_json::from_str(&body).unwrap();
        let d = issue_detail_from_value(&env.data.unwrap().repository.unwrap()).unwrap();
        assert!(!d.viewer_can_react);
        assert!(d.reaction_groups.is_empty());
        assert!(!d.locked);
        assert_eq!(d.active_lock_reason, None);
        assert_eq!(d.viewer_permission, None);
    }

    #[test]
    fn reaction_content_wire_round_trip_and_all() {
        // Every variant round-trips through its wire name; ALL is the
        // eight-value canonical order with no duplicates.
        assert_eq!(ReactionContent::ALL.len(), 8);
        for content in ReactionContent::ALL {
            assert_eq!(ReactionContent::parse(content.wire()), Some(content));
        }
        assert_eq!(ReactionContent::parse("NOPE"), None);
        let mut names: Vec<&str> = ReactionContent::ALL.iter().map(|c| c.wire()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 8);
        // Glyph/label are per-variant non-empty (D4).
        for content in ReactionContent::ALL {
            assert!(!content.glyph().is_empty());
            assert!(!content.label().is_empty());
        }
    }

    // docs/0022 §3/§7 — the detail payload carries node ids on labels and
    // assignees, the applied milestone, and the three picker gates; each
    // degrades independently (id-absent keeps display, kills the picker).
    #[test]
    fn issue_metadata_ids_milestone_and_gates_parse() {
        let body = issue_detail_body(
            r#"{
                "id": "I_9", "number": 7, "title": "t", "url": "u", "state": "OPEN",
                "viewerCanLabel": true, "viewerCanAssign": false,
                "viewerCanSetMilestone": true,
                "labels": {"totalCount": 2, "nodes": [
                    {"id": "L_1", "name": "bug", "color": "ff0000"},
                    {"name": "p1"}
                ]},
                "assignees": {"totalCount": 2, "nodes": [
                    {"id": "U_1", "login": "bob", "avatarUrl": "b"},
                    {"login": "no-id"}
                ]},
                "milestone": {"id": "M_1", "title": "v1.0", "state": "CLOSED"}
            }"#,
        );
        let env: Envelope<IssueDetailData> = serde_json::from_str(&body).unwrap();
        let d = issue_detail_from_value(&env.data.unwrap().repository.unwrap()).unwrap();
        assert!(d.viewer_can_label);
        assert!(!d.viewer_can_assign);
        assert!(d.viewer_can_set_milestone);
        assert_eq!(d.assignees_total, 2);
        // Ids ride the nodes; a node without one keeps rendering with
        // `id: None` (D3 disarms the picker, display is untouched).
        assert_eq!(d.labels[0].id.as_deref(), Some("L_1"));
        assert_eq!(d.labels[1].id, None);
        assert_eq!(d.assignees[1].login, "no-id");
        assert_eq!(d.assignees[1].id, None);
        let m = d.milestone.expect("milestone parses");
        assert_eq!(m.id, "M_1");
        assert_eq!(m.title, "v1.0");
        assert_eq!(m.state.as_deref(), Some("CLOSED"));

        // No milestone node → None; absent picker gates degrade off.
        let body =
            issue_detail_body(r#"{"id":"I_9","number":7,"title":"t","url":"u","state":"OPEN"}"#);
        let env: Envelope<IssueDetailData> = serde_json::from_str(&body).unwrap();
        let d = issue_detail_from_value(&env.data.unwrap().repository.unwrap()).unwrap();
        assert!(d.milestone.is_none());
        assert!(!d.viewer_can_label);
        assert!(!d.viewer_can_assign);
        assert!(!d.viewer_can_set_milestone);
    }

    // docs/0022 D5 — the picker source pages parse through the same
    // degradation rules: id-less or unrenderable nodes skip (the picker
    // exists to produce write targets, display coverage is the issue
    // payload's job).
    #[test]
    fn repo_picker_sources_parse_and_skip_idless_nodes() {
        let labels = repo_labels_from_value(
            &serde_json::from_value(serde_json::json!({
                "labels": {"nodes": [
                    {"id": "L_1", "name": "bug", "color": "ff0000"},
                    {"name": "no-id"},
                    {"id": "L_2", "name": "p1"}
                ]}
            }))
            .unwrap(),
        );
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0].name, "bug");
        assert_eq!(labels[1].id.as_deref(), Some("L_2"));

        let users = repo_assignable_users_from_value(
            &serde_json::from_value(serde_json::json!({
                "assignableUsers": {"nodes": [
                    {"id": "U_1", "login": "bob", "avatarUrl": "b"},
                    {"login": "no-id"}
                ]}
            }))
            .unwrap(),
        );
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].login, "bob");

        let milestones = repo_milestones_from_value(
            &serde_json::from_value(serde_json::json!({
                "milestones": {"nodes": [
                    {"id": "M_1", "title": "v1.0", "state": "OPEN"},
                    {"id": "M_2"}
                ]}
            }))
            .unwrap(),
        );
        assert_eq!(milestones.len(), 1);
        assert_eq!(milestones[0].state.as_deref(), Some("OPEN"));
    }

    // docs/0021 D2 — events parse from the union; every Assignee union
    // member reads as a login; unknown __typename and unrenderable nodes
    // skip; `events_capped` comes from filteredCount vs the fetched page.
    #[test]
    fn issue_events_parse_degrade_and_cap() {
        let body = issue_detail_body(
            r#"{
                "id": "I_3", "number": 9, "title": "t", "url": "u", "state": "OPEN",
                "timelineItems": {
                    "totalCount": 200, "filteredCount": 105,
                    "nodes": [
                        {"__typename": "LabeledEvent", "id": "E1",
                         "createdAt": "2026-09-01T01:00:00Z",
                         "actor": {"login": "alice"}, "label": {"name": "bug"}},
                        {"__typename": "AssignedEvent", "id": "E2",
                         "createdAt": "2026-09-01T02:00:00Z",
                         "actor": {"login": "alice"},
                         "assignee": {"__typename": "User", "login": "bob"}},
                        {"__typename": "AssignedEvent", "id": "E3",
                         "createdAt": "2026-09-01T03:00:00Z", "actor": null,
                         "assignee": {"__typename": "Bot", "login": "ci-bot"}},
                        {"__typename": "UnassignedEvent", "id": "E4",
                         "createdAt": "2026-09-01T04:00:00Z",
                         "actor": {"login": "alice"},
                         "assignee": {"__typename": "Organization", "login": "acme"}},
                        {"__typename": "AssignedEvent", "id": "E4b",
                         "createdAt": "2026-09-01T04:30:00Z",
                         "actor": {"login": "alice"},
                         "assignee": {"__typename": "Mannequin", "login": "ghost-user"}},
                        {"__typename": "UnlabeledEvent", "id": "E5",
                         "createdAt": "2026-09-01T05:00:00Z",
                         "actor": {"login": "alice"}, "label": {"name": "p1"}},
                        {"__typename": "ClosedEvent", "id": "E6",
                         "createdAt": "2026-09-01T06:00:00Z",
                         "actor": {"login": "alice"}, "stateReason": "NOT_PLANNED"},
                        {"__typename": "ClosedEvent", "id": "E6b",
                         "createdAt": "2026-09-01T06:30:00Z",
                         "actor": {"login": "alice"}, "stateReason": null},
                        {"__typename": "ReopenedEvent", "id": "E7",
                         "createdAt": "2026-09-01T07:00:00Z",
                         "actor": {"login": "bob"}},
                        {"__typename": "RenamedTitleEvent", "id": "E8",
                         "createdAt": "2026-09-01T08:00:00Z",
                         "actor": {"login": "bob"},
                         "previousTitle": "A", "currentTitle": "B"},
                        {"__typename": "MilestonedEvent", "id": "E9",
                         "createdAt": "2026-09-01T09:00:00Z",
                         "actor": {"login": "bob"}, "milestoneTitle": "v1.0"},
                        {"__typename": "DemilestonedEvent", "id": "E10",
                         "createdAt": "2026-09-01T10:00:00Z",
                         "actor": {"login": "bob"}, "milestoneTitle": "v1.0"},
                        {"__typename": "MysteryEvent", "id": "EX",
                         "createdAt": "2026-09-01T11:00:00Z"},
                        {"__typename": "AssignedEvent", "id": "EY",
                         "createdAt": "2026-09-01T12:00:00Z",
                         "actor": {"login": "alice"}, "assignee": null}
                    ]
                }
            }"#,
        );
        let env: Envelope<IssueDetailData> = serde_json::from_str(&body).unwrap();
        let d = issue_detail_from_value(&env.data.unwrap().repository.unwrap()).unwrap();
        // EX (unknown kind) and EY (assignee absent) skip; all else parses.
        assert_eq!(d.events.len(), 12);
        assert!(d.events_capped, "filteredCount 105 > 12 parsed rows");
        assert_eq!(
            d.events[0].kind,
            IssueEventKind::Labeled {
                label: "bug".into()
            }
        );
        // Every Assignee union member resolves to a login; a null actor
        // (ghost) stays None — the copy degrades to "Someone" (0021 D3).
        assert_eq!(
            d.events[1].kind,
            IssueEventKind::Assigned {
                login: "bob".into()
            }
        );
        assert!(d.events[2].actor_login.is_none());
        assert_eq!(
            d.events[2].kind,
            IssueEventKind::Assigned {
                login: "ci-bot".into()
            }
        );
        assert_eq!(
            d.events[3].kind,
            IssueEventKind::Unassigned {
                login: "acme".into()
            }
        );
        assert_eq!(
            d.events[4].kind,
            IssueEventKind::Assigned {
                login: "ghost-user".into()
            }
        );
        assert_eq!(
            d.events[5].kind,
            IssueEventKind::Unlabeled { label: "p1".into() }
        );
        assert_eq!(
            d.events[6].kind,
            IssueEventKind::Closed {
                reason: Some("NOT_PLANNED".into())
            }
        );
        assert_eq!(d.events[7].kind, IssueEventKind::Closed { reason: None });
        assert_eq!(d.events[8].kind, IssueEventKind::Reopened);
        assert_eq!(
            d.events[9].kind,
            IssueEventKind::Renamed {
                from: "A".into(),
                to: "B".into()
            }
        );
        assert_eq!(
            d.events[10].kind,
            IssueEventKind::Milestoned {
                title: "v1.0".into()
            }
        );
        assert_eq!(
            d.events[11].kind,
            IssueEventKind::Demilestoned {
                title: "v1.0".into()
            }
        );

        // No timelineItems at all → empty, uncapped (0007 D3).
        let body =
            issue_detail_body(r#"{"id":"I_2","number":1,"title":"t","url":"u","state":"OPEN"}"#);
        let env: Envelope<IssueDetailData> = serde_json::from_str(&body).unwrap();
        let d = issue_detail_from_value(&env.data.unwrap().repository.unwrap()).unwrap();
        assert!(d.events.is_empty());
        assert!(!d.events_capped);
    }
    // --- PR review threads (docs/0030 §5) --------------------------------

    fn threads_repo(pr: &str) -> serde_json::Value {
        serde_json::json!({ "pullRequest": serde_json::from_str::<serde_json::Value>(pr).unwrap() })
    }

    #[test]
    fn pr_threads_parse_happy_path() {
        let repo = threads_repo(
            r#"{"id":"PR_1","reviewThreads":{"totalCount":2,"nodes":[
                {"id":"T_1","path":"src/main.rs","line":14,"startLine":12,
                 "diffSide":"LEFT","subjectType":"LINE","isOutdated":false,
                 "isResolved":true,"resolvedBy":{"login":"rev"},
                 "viewerCanReply":true,"viewerCanResolve":true,
                 "viewerCanUnresolve":false,
                 "comments":{"totalCount":2,"nodes":[
                     {"id":"C_1","author":{"login":"a","avatarUrl":"u"},
                      "body":"first","createdAt":"2026-09-01T00:00:00Z",
                      "updatedAt":"2026-09-01T00:00:00Z","lastEditedAt":null,
                      "url":"https://x/1","viewerCanUpdate":true,
                      "viewerCanDelete":true,"viewerDidAuthor":true},
                     {"id":"C_2","body":"reply","createdAt":"2026-09-02T00:00:00Z",
                      "updatedAt":"2026-09-02T00:00:00Z","lastEditedAt":null,
                      "url":"https://x/2","viewerCanUpdate":false,
                      "viewerCanDelete":false,"viewerDidAuthor":false}]}}
            ]}}"#,
        );
        let t = pr_threads_from_value(&repo).unwrap();
        assert_eq!(t.total_count, 2);
        assert_eq!(t.nodes.len(), 1);
        let th = &t.nodes[0];
        assert_eq!(th.id, "T_1");
        assert_eq!(th.path, "src/main.rs");
        assert_eq!(th.line, Some(14));
        assert_eq!(th.start_line, Some(12));
        assert_eq!(th.side, DiffSide::Left);
        assert_eq!(th.subject, ThreadSubject::Line);
        assert!(th.is_resolved);
        assert!(!th.is_outdated);
        assert_eq!(th.resolved_by.as_deref(), Some("rev"));
        assert!(th.viewer_can_reply && th.viewer_can_resolve);
        assert!(!th.viewer_can_unresolve);
        assert_eq!(th.comments.len(), 2);
        assert_eq!(th.comments[0].author.as_ref().unwrap().login, "a");
        assert_eq!(th.comments[0].url.as_deref(), Some("https://x/1"));
        assert!(th.comments[1].author.is_none());
    }

    #[test]
    fn pr_threads_degrade_per_node() {
        // docs/0030 §5: a thread without id or path drops alone; a comment
        // without id/body drops alone; unknown diffSide/subjectType degrade
        // to Right/LINE; a missing connection yields an empty set, not an
        // error — the diff pane must not break (D1/D3).
        let repo = threads_repo(
            r#"{"id":"PR_1","reviewThreads":{"totalCount":3,"nodes":[
                {"path":"no-id.rs","line":1,"diffSide":"RIGHT","comments":{"nodes":[]}},
                {"id":"T_2","path":"src/lib.rs","line":3,
                 "diffSide":"WARP","subjectType":"PLANET","comments":{"nodes":[
                     {"body":"no id"},
                     {"id":"C_2"},
                     {"id":"C_3","body":"ok"}]}},
                {"id":"T_3","path":"file-level.rs","subjectType":"FILE",
                 "isOutdated":true,"comments":{"nodes":[]}}
            ]}}"#,
        );
        let t = pr_threads_from_value(&repo).unwrap();
        assert_eq!(t.total_count, 3, "server count kept even when nodes drop");
        assert_eq!(t.nodes.len(), 2);
        assert_eq!(t.nodes[0].side, DiffSide::Right, "unknown side degrades");
        assert_eq!(t.nodes[0].subject, ThreadSubject::Line);
        assert_eq!(t.nodes[0].comments.len(), 1, "bad comments drop alone");
        assert_eq!(t.nodes[0].comments[0].id, "C_3");
        assert_eq!(t.nodes[1].subject, ThreadSubject::File);
        assert!(t.nodes[1].is_outdated);
    }

    #[test]
    fn pr_threads_missing_connection_is_empty_not_none() {
        let repo = threads_repo(r#"{"id":"PR_1"}"#);
        let t = pr_threads_from_value(&repo).unwrap();
        assert_eq!(t.total_count, 0);
        assert!(t.nodes.is_empty());
        // A null pullRequest node is the only None case.
        assert!(pr_threads_from_value(&serde_json::json!({})).is_none());
    }
}
