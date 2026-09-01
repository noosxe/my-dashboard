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
            notifications_url(2),
            "https://api.github.com/notifications?per_page=100&page=2"
        );
        assert_eq!(
            repo_notifications_url("o", "r", 1),
            "https://api.github.com/repos/o/r/notifications?per_page=100&page=1"
        );
    }
}
