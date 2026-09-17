//! Repo-view list filters & sort (docs/0042) — the pure engine.
//!
//! Filters and sorts the snapshot node lists already held in memory
//! (docs/0007 D2, ≤100 rows per box). No gpui types: everything here is
//! deterministic and unit-tested without a window.

use crate::api::Notification;
use crate::gql::{Issue, LabelSummary, PullRequest, RequestedReviewer};

/// Author scope for the PR and Issue boxes (docs/0042 D3): the issue's
/// asked minimum — mine vs others. A missing author (deleted account)
/// counts as *not mine*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum AuthorFilter {
    #[default]
    All,
    Mine,
    Others,
}

/// Review-request scope for the PR box (docs/0042 D3/D4): "requested of
/// me" matches a `User` review request whose login is the viewer's —
/// teams/mannequins/bots do not match in v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ReviewsFilter {
    #[default]
    All,
    RequestedOfMe,
}

/// Sort orders (docs/0042 D3): GitHub's wire order is updated-desc, so
/// that is the default everywhere. Ties break by number, descending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Sort {
    #[default]
    UpdatedDesc,
    UpdatedAsc,
    NumberDesc,
    NumberAsc,
}

/// The notifications box has no `#number` on its rows — only the
/// updated-at orders apply (docs/0042 D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum NotifSort {
    #[default]
    UpdatedDesc,
    UpdatedAsc,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PrFilter {
    pub author: AuthorFilter,
    pub reviews: ReviewsFilter,
    pub label: Option<String>,
    pub sort: Sort,
}

impl PrFilter {
    /// Any non-default dimension (drives the toolbar's Clear button).
    pub fn is_filtered(&self) -> bool {
        self.author != AuthorFilter::All
            || self.reviews != ReviewsFilter::All
            || self.label.is_some()
            || self.sort != Sort::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct IssueFilter {
    pub author: AuthorFilter,
    pub label: Option<String>,
    pub sort: Sort,
}

impl IssueFilter {
    pub fn is_filtered(&self) -> bool {
        self.author != AuthorFilter::All || self.label.is_some() || self.sort != Sort::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct NotifFilter {
    pub reason: Option<String>,
    pub kind: Option<String>,
    pub sort: NotifSort,
}

impl NotifFilter {
    pub fn is_filtered(&self) -> bool {
        self.reason.is_some() || self.kind.is_some() || self.sort != NotifSort::default()
    }
}

/// Session view state for the repo view's three boxes (docs/0042 D2).
/// The shared quick-filter text lives here too; the *label* choice
/// resets on repo switch (names are repo-local), everything else
/// survives. Not persisted across app restarts.
#[derive(Debug, Clone, Default)]
pub(crate) struct RepoViewFilters {
    pub prs: PrFilter,
    pub issues: IssueFilter,
    pub notifs: NotifFilter,
    pub search: String,
}

/// RFC-3339 → sort key; an unparseable/missing stamp sorts oldest
/// (defensive, matching `relative_age`'s treatment of blank stamps).
fn epoch(at: Option<&str>) -> i64 {
    at.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.timestamp())
        .unwrap_or(i64::MIN)
}

/// The shared quick filter (docs/0042 D5): case-insensitive substring
/// on the title, or an exact `#N` number match.
fn matches_search(title: &str, number: i64, search: &str) -> bool {
    let needle = search.trim();
    if needle.is_empty() {
        return true;
    }
    if let Some(rest) = needle.strip_prefix('#') {
        if let Ok(n) = rest.parse::<i64>() {
            return number == n;
        }
    }
    title.to_lowercase().contains(&needle.to_lowercase())
}

/// Mine needs *both* sides present and equal — a deleted author (None)
/// is never mine, and an unlanded viewer (None) matches nothing.
/// Others is the complement (docs/0042 D3).
fn author_matches(author: Option<&str>, filter: AuthorFilter, viewer: Option<&str>) -> bool {
    let mine = matches!((author, viewer), (Some(a), Some(v)) if a == v);
    match filter {
        AuthorFilter::All => true,
        AuthorFilter::Mine => mine,
        AuthorFilter::Others => !mine,
    }
}

/// "Requested of me" (docs/0042 D4): any review request on the PR is a
/// `User` request for the viewer's login.
pub(crate) fn pr_requested_of_me(pr: &PullRequest, viewer: Option<&str>) -> bool {
    let Some(viewer) = viewer else {
        return false;
    };
    pr.review_requests.as_ref().is_some_and(|rr| {
        rr.nodes.iter().any(|n| {
            matches!(
                &n.requested_reviewer,
                Some(RequestedReviewer::User { login }) if login == viewer
            )
        })
    })
}

/// Label names present in a node's fetched label set — the filter
/// menu's options (docs/0042 D3) and the label match key.
pub(crate) fn label_names(labels: Option<&LabelSummary>) -> Vec<&str> {
    labels
        .map(|l| l.nodes.iter().map(|n| n.name.as_str()).collect())
        .unwrap_or_default()
}

fn sort_prs(rows: &mut [&PullRequest], sort: Sort) {
    // Ties break by number, descending (docs/0042 D8) — deterministic
    // regardless of wire order.
    match sort {
        Sort::UpdatedDesc => rows.sort_by_key(|p| {
            (
                std::cmp::Reverse(epoch(p.updated_at.as_deref())),
                std::cmp::Reverse(p.number),
            )
        }),
        Sort::UpdatedAsc => {
            rows.sort_by_key(|p| (epoch(p.updated_at.as_deref()), std::cmp::Reverse(p.number)))
        }
        Sort::NumberDesc => rows.sort_by_key(|p| std::cmp::Reverse(p.number)),
        Sort::NumberAsc => rows.sort_by_key(|p| p.number),
    }
}

pub(crate) fn apply_prs<'a>(
    f: &PrFilter,
    nodes: &'a [PullRequest],
    viewer: Option<&str>,
    search: &str,
) -> Vec<&'a PullRequest> {
    let mut rows: Vec<&PullRequest> = nodes
        .iter()
        .filter(|p| matches_search(&p.title, p.number, search))
        .filter(|p| {
            author_matches(
                p.author.as_ref().map(|a| a.login.as_str()),
                f.author,
                viewer,
            )
        })
        .filter(|p| match f.reviews {
            ReviewsFilter::All => true,
            ReviewsFilter::RequestedOfMe => pr_requested_of_me(p, viewer),
        })
        .filter(|p| match &f.label {
            None => true,
            Some(label) => label_names(p.labels.as_ref()).contains(&label.as_str()),
        })
        .collect();
    sort_prs(&mut rows, f.sort);
    rows
}

pub(crate) fn apply_issues<'a>(
    f: &IssueFilter,
    nodes: &'a [Issue],
    viewer: Option<&str>,
    search: &str,
) -> Vec<&'a Issue> {
    let mut rows: Vec<&Issue> = nodes
        .iter()
        .filter(|i| matches_search(&i.title, i.number, search))
        .filter(|i| {
            author_matches(
                i.author.as_ref().map(|a| a.login.as_str()),
                f.author,
                viewer,
            )
        })
        .filter(|i| match &f.label {
            None => true,
            Some(label) => label_names(i.labels.as_ref()).contains(&label.as_str()),
        })
        .collect();
    match f.sort {
        Sort::UpdatedDesc => rows.sort_by_key(|i| {
            (
                std::cmp::Reverse(epoch(i.updated_at.as_deref())),
                std::cmp::Reverse(i.number),
            )
        }),
        Sort::UpdatedAsc => {
            rows.sort_by_key(|i| (epoch(i.updated_at.as_deref()), std::cmp::Reverse(i.number)))
        }
        Sort::NumberDesc => rows.sort_by_key(|i| std::cmp::Reverse(i.number)),
        Sort::NumberAsc => rows.sort_by_key(|i| i.number),
    }
    rows
}

/// Notifications filter by title substring only — the REST payload has
/// no `#number` field to match (docs/0042 D5).
pub(crate) fn apply_notifs<'a>(
    f: &NotifFilter,
    nodes: &'a [Notification],
    search: &str,
) -> Vec<&'a Notification> {
    let mut rows: Vec<&Notification> = nodes
        .iter()
        .filter(|n| matches_search(&n.subject.title, i64::MAX, search))
        .filter(|n| match &f.reason {
            None => true,
            Some(reason) => n.reason.as_deref() == Some(reason.as_str()),
        })
        .filter(|n| match &f.kind {
            None => true,
            Some(kind) => n.subject.kind == *kind,
        })
        .collect();
    match f.sort {
        NotifSort::UpdatedDesc => rows.sort_by_key(|n| {
            (
                std::cmp::Reverse(epoch(n.updated_at.as_deref())),
                n.id.clone(),
            )
        }),
        NotifSort::UpdatedAsc => {
            rows.sort_by_key(|n| (epoch(n.updated_at.as_deref()), n.id.clone()))
        }
    }
    rows
}

/// GroupBox title (docs/0042 D6): `"(5 of 23)"` while filtered, plain
/// `"(23)"` otherwise — the #87 cap hint keeps its total-based math.
pub(crate) fn box_title(prefix: &str, total: usize, visible: usize, filtered: bool) -> String {
    if filtered {
        format!("{prefix} ({visible} of {total})")
    } else {
        format!("{prefix} ({total})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gql::{Author, LabelRef, LabelSummary, ReviewRequestRef, ReviewRequestsSummary};

    fn author_of(login: &str) -> Option<Author> {
        Some(Author {
            login: login.to_string(),
            avatar_url: String::new(),
        })
    }

    fn pr(number: i64, title: &str, author: Option<&str>, updated_at: Option<&str>) -> PullRequest {
        PullRequest {
            key: format!("o/r#{number}"),
            database_id: None,
            number,
            title: title.to_string(),
            draft: false,
            state: None,
            author: author.and_then(author_of),
            updated_at: updated_at.map(str::to_string),
            url: "https://probe".into(),
            status: None,
            labels: None,
            review_requests: None,
        }
    }

    fn issue(number: i64, title: &str, author: Option<&str>, updated_at: Option<&str>) -> Issue {
        Issue {
            key: format!("o/r#{number}"),
            database_id: None,
            number,
            title: title.to_string(),
            author: author.and_then(author_of),
            updated_at: updated_at.map(str::to_string),
            url: "https://probe".into(),
            labels: None,
        }
    }

    fn with_labels(mut pr: PullRequest, names: &[&str]) -> PullRequest {
        pr.labels = Some(LabelSummary {
            total_count: names.len() as i64,
            nodes: names
                .iter()
                .map(|n| LabelRef {
                    name: n.to_string(),
                    color: Some("#d73a4a".into()),
                })
                .collect(),
        });
        pr
    }

    fn requested(mut pr: PullRequest, logins: &[&str]) -> PullRequest {
        pr.review_requests = Some(ReviewRequestsSummary {
            total_count: logins.len() as i64,
            nodes: logins
                .iter()
                .map(|l| ReviewRequestRef {
                    requested_reviewer: Some(RequestedReviewer::User {
                        login: l.to_string(),
                    }),
                })
                .collect(),
        });
        pr
    }

    fn titles(rows: &[&PullRequest]) -> Vec<String> {
        rows.iter().map(|p| p.title.clone()).collect()
    }

    #[test]
    fn author_filter_splits_mine_from_others() {
        let nodes = vec![
            pr(1, "mine", Some("me"), None),
            pr(2, "theirs", Some("you"), None),
            pr(3, "ghost", None, None),
        ];
        let f = PrFilter {
            author: AuthorFilter::Mine,
            ..Default::default()
        };
        assert_eq!(titles(&apply_prs(&f, &nodes, Some("me"), "")), ["mine"]);
        let f = PrFilter {
            author: AuthorFilter::Others,
            ..Default::default()
        };
        // A deleted author is "not mine" → Others (docs/0042 D3).
        // Order: no stamps → updated ties → number desc (docs/0042 D8).
        assert_eq!(
            titles(&apply_prs(&f, &nodes, Some("me"), "")),
            ["ghost", "theirs"]
        );
        // No viewer login (auth not landed): Mine matches nothing.
        let f = PrFilter {
            author: AuthorFilter::Mine,
            ..Default::default()
        };
        assert!(apply_prs(&f, &nodes, None, "").is_empty());
    }

    #[test]
    fn requested_of_me_matches_user_requests_only() {
        let mut nodes = vec![
            requested(pr(1, "me requested", None, None), &["me"]),
            requested(pr(2, "team only", None, None), &["someteam"]),
            pr(3, "plain", None, None),
        ];
        // A team request narrows to Other → no user login → no match.
        nodes[1].review_requests.as_mut().unwrap().nodes[0].requested_reviewer =
            Some(RequestedReviewer::Other);
        let f = PrFilter {
            reviews: ReviewsFilter::RequestedOfMe,
            ..Default::default()
        };
        assert_eq!(
            titles(&apply_prs(&f, &nodes, Some("me"), "")),
            ["me requested"]
        );
        assert!(apply_prs(&f, &nodes, None, "").is_empty());
    }

    #[test]
    fn label_filter_matches_exact_names_and_degrades_on_none() {
        let nodes = vec![
            with_labels(pr(1, "buggy", None, None), &["bug", "ui"]),
            pr(2, "bare", None, None),
        ];
        let f = PrFilter {
            label: Some("bug".into()),
            ..Default::default()
        };
        assert_eq!(titles(&apply_prs(&f, &nodes, None, "")), ["buggy"]);
        // None label nodes (pre-field cache) match no label choice.
        assert_eq!(label_names(nodes[1].labels.as_ref()).len(), 0);
    }

    #[test]
    fn sorts_order_by_updated_then_number_ties() {
        let nodes = vec![
            pr(1, "feb", None, Some("2026-02-01T00:00:00Z")),
            pr(2, "jan", None, Some("2026-01-01T00:00:00Z")),
            pr(3, "mar", None, Some("2026-03-01T00:00:00Z")),
            // Same stamp as mar — tie breaks by number desc (UpdatedDesc).
            pr(30, "mar-late-tie", None, Some("2026-03-01T00:00:00Z")),
        ];
        let f = PrFilter::default(); // UpdatedDesc
        assert_eq!(
            titles(&apply_prs(&f, &nodes, None, "")),
            ["mar-late-tie", "mar", "feb", "jan"]
        );
        let f = PrFilter {
            sort: Sort::UpdatedAsc,
            ..Default::default()
        };
        assert_eq!(
            titles(&apply_prs(&f, &nodes, None, "")),
            ["jan", "feb", "mar-late-tie", "mar"]
        );
        let f = PrFilter {
            sort: Sort::NumberAsc,
            ..Default::default()
        };
        assert_eq!(
            titles(&apply_prs(&f, &nodes, None, "")),
            ["feb", "jan", "mar", "mar-late-tie"]
        );
        // Missing stamps sort oldest (defensive, docs/0042 D8).
        let nodes = vec![
            pr(1, "no stamp", None, None),
            pr(2, "stamped", None, Some("2026-03-01T00:00:00Z")),
        ];
        let f = PrFilter::default();
        assert_eq!(
            titles(&apply_prs(&f, &nodes, None, "")),
            ["stamped", "no stamp"]
        );
    }

    #[test]
    fn search_matches_title_substring_and_exact_number() {
        let nodes = vec![
            pr(12, "Fix the Widget parser", None, None),
            pr(120, "Unrelated", None, None),
        ];
        assert_eq!(
            titles(&apply_prs(&PrFilter::default(), &nodes, None, "widget")),
            ["Fix the Widget parser"]
        );
        // Exact #N: #12 matches 12, not 120.
        assert_eq!(
            titles(&apply_prs(&PrFilter::default(), &nodes, None, "#12")),
            ["Fix the Widget parser"]
        );
        assert!(apply_prs(&PrFilter::default(), &nodes, None, "#99").is_empty());
        assert_eq!(
            titles(&apply_prs(&PrFilter::default(), &nodes, None, "")),
            ["Unrelated", "Fix the Widget parser"]
        );
    }

    #[test]
    fn combined_pr_filters_intersect() {
        let nodes = vec![
            with_labels(requested(pr(1, "hit", Some("me"), None), &["me"]), &["bug"]),
            with_labels(pr(2, "wrong author", Some("you"), None), &["bug"]),
            requested(pr(3, "no label", Some("me"), None), &["me"]),
        ];
        let f = PrFilter {
            author: AuthorFilter::Mine,
            reviews: ReviewsFilter::RequestedOfMe,
            label: Some("bug".into()),
            sort: Sort::default(),
        };
        assert_eq!(titles(&apply_prs(&f, &nodes, Some("me"), "")), ["hit"]);
        assert!(f.is_filtered());
        assert!(!PrFilter::default().is_filtered());
    }

    #[test]
    fn issue_engine_filters_and_sorts() {
        let nodes = vec![
            issue(1, "alpha", Some("me"), Some("2026-02-01T00:00:00Z")),
            issue(2, "beta", Some("you"), Some("2026-03-01T00:00:00Z")),
        ];
        let f = IssueFilter {
            author: AuthorFilter::Others,
            sort: Sort::NumberDesc,
            ..Default::default()
        };
        assert_eq!(
            apply_issues(&f, &nodes, Some("me"), "")
                .iter()
                .map(|i| i.title.as_str())
                .collect::<Vec<_>>(),
            ["beta"]
        );
        assert!(!IssueFilter::default().is_filtered());
        // Shared search hits issues too (docs/0042 D5).
        assert_eq!(
            apply_issues(&IssueFilter::default(), &nodes, None, "alp").len(),
            1
        );
    }

    #[test]
    fn notif_engine_filters_reason_kind_and_title() {
        let notif = |id: &str, kind: &str, reason: &str, title: &str| Notification {
            id: id.to_string(),
            subject: crate::api::Subject {
                title: title.to_string(),
                kind: kind.to_string(),
                url: None,
                latest_comment_url: None,
            },
            repository: crate::api::RepoRef {
                full_name: "o/r".into(),
            },
            updated_at: Some("2026-03-01T00:00:00Z".into()),
            unread: true,
            reason: Some(reason.to_string()),
        };
        let nodes = vec![
            notif("n1", "PullRequest", "review_requested", "PR one"),
            notif("n2", "Issue", "mention", "Issue two"),
        ];
        let f = NotifFilter {
            reason: Some("review_requested".into()),
            ..Default::default()
        };
        assert_eq!(apply_notifs(&f, &nodes, "").len(), 1);
        let f = NotifFilter {
            kind: Some("Issue".into()),
            ..Default::default()
        };
        assert_eq!(apply_notifs(&f, &nodes, "").len(), 1);
        assert_eq!(
            apply_notifs(&NotifFilter::default(), &nodes, "one").len(),
            1
        );
        assert!(!NotifFilter::default().is_filtered());
    }

    #[test]
    fn box_title_shows_n_of_m_while_filtered() {
        // docs/0042 D6.
        assert_eq!(box_title("Open PRs", 23, 5, true), "Open PRs (5 of 23)");
        assert_eq!(box_title("Open PRs", 23, 23, false), "Open PRs (23)");
        assert_eq!(
            box_title("Notifications", 11, 3, true),
            "Notifications (3 of 11)"
        );
    }
}
