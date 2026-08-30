//! GitHub REST v3 payload structs and endpoint builders (docs/0005 §3.3, §7).
//!
//! Payload structs are deliberately minimal and tolerant: unknown fields are
//! ignored (serde default), and fields we merely display are `Option` where a
//! repo might omit them. GitHub's JSON is snake_case — no rename needed.

use serde::{Deserialize, Serialize};

pub const API_BASE: &str = "https://api.github.com";

/// `GET /user` — the authenticated account (personal org entry, docs/0005 Q10).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub login: String,
    pub avatar_url: String,
}

/// `GET /user/orgs` item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Org {
    pub login: String,
    #[allow(dead_code)] // consumed by the org dropdown UI layer
    pub avatar_url: String,
    #[allow(dead_code)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Owner {
    pub login: String,
    #[allow(dead_code)] // PR row avatars (UI layer)
    pub avatar_url: String,
}

/// `GET /user/repos` item (covers personal + org repos in one endpoint).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    pub id: u64,
    pub name: String,
    pub full_name: String,
    pub owner: Owner,
    pub description: Option<String>,
    #[serde(default)]
    pub private: bool,
    #[allow(dead_code)] // "recently pushed" ordering hint (UI layer)
    pub pushed_at: Option<String>,
}

/// `GET /repos/{owner}/{repo}/pulls?state=open` item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    pub id: u64,
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub draft: bool,
    pub user: Option<Owner>,
    pub updated_at: Option<String>,
    #[allow(dead_code)] // row click → browser (UI layer)
    pub html_url: String,
}

/// `GET /notifications` / repo-scoped item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub subject: Subject,
    pub repository: RepoRef,
    pub updated_at: Option<String>,
    #[serde(default = "default_true")]
    pub unread: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subject {
    pub title: String,
    /// GitHub calls this `type` ("PullRequest", "Issue", …) — renamed.
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoRef {
    #[allow(dead_code)] // grouping labels (UI layer)
    pub full_name: String,
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Endpoint builders — per_page=100 everywhere, page-driven pagination.
// ---------------------------------------------------------------------------

fn page_query(page: u32) -> String {
    format!("per_page=100&page={page}")
}

pub fn user_url() -> String {
    format!("{API_BASE}/user")
}

pub fn orgs_url(page: u32) -> String {
    format!("{API_BASE}/user/orgs?{}", page_query(page))
}

pub fn repos_url(page: u32) -> String {
    format!("{API_BASE}/user/repos?sort=pushed&{}", page_query(page))
}

pub fn pulls_url(owner: &str, repo: &str, page: u32) -> String {
    format!(
        "{API_BASE}/repos/{owner}/{repo}/pulls?state=open&{}",
        page_query(page)
    )
}

pub fn notifications_url(page: u32) -> String {
    format!("{API_BASE}/notifications?{}", page_query(page))
}

pub fn repo_notifications_url(owner: &str, repo: &str, page: u32) -> String {
    format!(
        "{API_BASE}/repos/{owner}/{repo}/notifications?{}",
        page_query(page)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user() {
        let u: User = serde_json::from_str(
            r#"{"login":"noosxe","id":1,"avatar_url":"https://a/o.png","extra":42}"#,
        )
        .unwrap();
        assert_eq!(u.login, "noosxe");
        assert_eq!(u.avatar_url, "https://a/o.png");
    }

    #[test]
    fn parses_repo_minimal_fields() {
        let r: Repo = serde_json::from_str(
            r#"{"id":7,"name":"my-dashboard","full_name":"noosxe/my-dashboard",
                "owner":{"login":"noosxe","avatar_url":"https://a/o.png",
                         "type":"User"},
                "private":true,
                "pushed_at":"2026-08-29T10:00:00Z",
                "language":"Rust","fork":false}"#,
        )
        .unwrap();
        assert!(r.private);
        assert_eq!(r.full_name, "noosxe/my-dashboard");
        assert_eq!(r.description, None);
    }

    #[test]
    fn parses_pull_request_with_draft_default() {
        let p: PullRequest = serde_json::from_str(
            r#"{"id":9,"number":123,"title":"feat: x",
                "user":{"login":"octo","avatar_url":"https://a/c.png"},
                "html_url":"https://github.com/o/r/pull/123",
                "updated_at":"2026-08-29T09:00:00Z","state":"open"}"#,
        )
        .unwrap();
        assert!(!p.draft);
        assert_eq!(p.user.unwrap().login, "octo");
    }

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
            repos_url(2),
            "https://api.github.com/user/repos?sort=pushed&per_page=100&page=2"
        );
        assert_eq!(
            pulls_url("o", "r", 1),
            "https://api.github.com/repos/o/r/pulls?state=open&per_page=100&page=1"
        );
        assert_eq!(
            repo_notifications_url("o", "r", 1),
            "https://api.github.com/repos/o/r/notifications?per_page=100&page=1"
        );
    }
}
