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
