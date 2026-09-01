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
//! with a stderr note, never failing the whole snapshot.

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
            author { login }
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
        author { login }
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Open issues — groundwork for the future issues UI (doc 0007 §2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueSummary {
    #[allow(dead_code)] // consumed by the issues UI (follow-up)
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
    #[allow(dead_code)] // consumed by the issues UI (follow-up)
    pub key: String,
    #[allow(dead_code)]
    pub database_id: Option<i64>,
    #[allow(dead_code)]
    pub number: i64,
    #[allow(dead_code)]
    pub title: String,
    #[allow(dead_code)]
    pub author: Option<Author>,
    pub updated_at: Option<String>,
    #[allow(dead_code)]
    pub url: String,
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

/// Parse a list of raw nodes tolerantly: per-item failures log to stderr
/// and degrade that item alone ([DG] — never the whole snapshot).
fn parse_nodes<T: serde::de::DeserializeOwned>(values: &[serde_json::Value]) -> Vec<T> {
    values
        .iter()
        .filter_map(|v| match serde_json::from_value::<T>(v.clone()) {
            Ok(node) => Some(node),
            Err(e) => {
                eprintln!("[github] snapshot: skipping malformed node: {e}");
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
                      "author":{{"login":"octo"}},"updatedAt":"u","url":"iu"}}]}}}}"#,
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
}
