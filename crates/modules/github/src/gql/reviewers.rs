//! Review data on the PR detail (docs/0083 D8, D17): the requested
//! reviewers (`RequestedReviewer` union) and the latest review per
//! reviewer. Parsing is per node and tolerant (docs/0007 D3): a node
//! that doesn't fit drops alone.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The `RequestedReviewer` union arms (verified against GitHub's schema
/// 2026-11-14: `User`, `Team`, `Bot`, `Mannequin`, `EnterpriseTeam`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
pub enum ReviewerKind {
    #[default]
    User,
    Team,
    Bot,
    Mannequin,
    EnterpriseTeam,
}

impl ReviewerKind {
    /// Teams carry a `name`, everyone else a `login`.
    pub fn is_team(self) -> bool {
        matches!(self, Self::Team | Self::EnterpriseTeam)
    }
}

/// One requested reviewer on the detail page (docs/0043 D3, docs/0083
/// D8). `id` is the user node id a replace write can reproduce: only
/// users have it. `id: None` on a user means a degraded node. Every
/// other arm is display-only and forces the reviewer picker's Edit
/// hidden (D3's reviewer clause).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PrReviewRequest {
    pub id: Option<String>,
    /// Empty for teams.
    pub login: String,
    #[serde(default)]
    pub kind: ReviewerKind,
    /// A team's name; empty for the other arms.
    #[serde(default)]
    pub name: String,
    /// Node id of a team, the merge's dedupe key.
    #[serde(default)]
    pub team_id: Option<String>,
    /// Absent on `EnterpriseTeam` and on a degraded node.
    #[serde(default)]
    pub avatar_url: Option<String>,
}

impl PrReviewRequest {
    /// A user the picker's replace write can reproduce.
    pub fn is_user(&self) -> bool {
        self.kind == ReviewerKind::User
    }
}

/// The latest review of one reviewer (`latestOpinionatedReviews` and
/// `latestReviews` nodes). `state` is the `PullRequestReviewState` name.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LatestReview {
    pub id: String,
    pub login: String,
    pub avatar_url: Option<String>,
    pub state: String,
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// `reviewRequests.nodes` plus its `totalCount`. A null reviewer drops
/// out below `totalCount`, which keeps the truncation guard armed.
pub(crate) fn parse_review_requests(pr: &Value) -> (Vec<PrReviewRequest>, i64) {
    let requests: Vec<PrReviewRequest> = pr
        .pointer("/reviewRequests/nodes")
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| n.get("requestedReviewer").and_then(request_from_value))
                .collect()
        })
        .unwrap_or_default();
    let total = pr
        .pointer("/reviewRequests/totalCount")
        .and_then(Value::as_i64)
        .unwrap_or(requests.len() as i64);
    (requests, total)
}

fn request_from_value(rr: &Value) -> Option<PrReviewRequest> {
    if !rr.is_object() {
        return None;
    }
    let kind = match rr.get("__typename").and_then(Value::as_str)? {
        "User" => ReviewerKind::User,
        "Team" => ReviewerKind::Team,
        "Bot" => ReviewerKind::Bot,
        "Mannequin" => ReviewerKind::Mannequin,
        "EnterpriseTeam" => ReviewerKind::EnterpriseTeam,
        _ => return None,
    };
    let mut r = PrReviewRequest {
        kind,
        avatar_url: text(rr, "avatarUrl"),
        ..PrReviewRequest::default()
    };
    if kind.is_team() {
        r.name = text(rr, "name").unwrap_or_default();
        r.team_id = text(rr, "id");
    } else {
        r.login = text(rr, "login").unwrap_or_default();
        if kind == ReviewerKind::User {
            r.id = text(rr, "id");
        }
    }
    Some(r)
}

/// `latestOpinionatedReviews` / `latestReviews` nodes; a node without an
/// author login (a deleted account) drops out.
pub(crate) fn parse_latest_reviews(pr: &Value, key: &str) -> Vec<LatestReview> {
    pr.get(key)
        .and_then(|c| c.get("nodes"))
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| {
                    let author = n.get("author").filter(|a| a.is_object())?;
                    Some(LatestReview {
                        id: text(n, "id").unwrap_or_default(),
                        login: text(author, "login").filter(|l| !l.is_empty())?,
                        avatar_url: text(author, "avatarUrl"),
                        state: text(n, "state")?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn requests(nodes: Value, total: i64) -> (Vec<PrReviewRequest>, i64) {
        parse_review_requests(&json!({"reviewRequests": {"totalCount": total, "nodes": nodes}}))
    }

    #[test]
    fn a_user_arm_keeps_id_login_and_avatar() {
        let (r, total) = requests(
            json!([{"requestedReviewer": {"__typename": "User", "id": "U1", "login": "a", "avatarUrl": "https://x/a"}}]),
            1,
        );
        assert_eq!(total, 1);
        assert_eq!(r[0].kind, ReviewerKind::User);
        assert_eq!(r[0].id.as_deref(), Some("U1"));
        assert_eq!(r[0].login, "a");
        assert_eq!(r[0].avatar_url.as_deref(), Some("https://x/a"));
        assert!(r[0].is_user());
    }

    #[test]
    fn a_team_arm_keeps_name_id_and_avatar_and_no_user_id() {
        let (r, _) = requests(
            json!([{"requestedReviewer": {"__typename": "Team", "id": "T1", "name": "core", "avatarUrl": "https://x/t"}}]),
            1,
        );
        assert_eq!(r[0].kind, ReviewerKind::Team);
        assert_eq!(r[0].name, "core");
        assert_eq!(r[0].team_id.as_deref(), Some("T1"));
        assert_eq!(r[0].avatar_url.as_deref(), Some("https://x/t"));
        assert!(r[0].id.is_none(), "a team can't ride the user-id write");
        assert!(r[0].login.is_empty());
        assert!(!r[0].is_user());
    }

    #[test]
    fn an_enterprise_team_arm_has_a_name_and_no_avatar() {
        let (r, _) = requests(
            json!([{"requestedReviewer": {"__typename": "EnterpriseTeam", "id": "E1", "name": "platform"}}]),
            1,
        );
        assert_eq!(r[0].kind, ReviewerKind::EnterpriseTeam);
        assert_eq!(r[0].name, "platform");
        assert_eq!(r[0].team_id.as_deref(), Some("E1"));
        assert!(r[0].avatar_url.is_none());
        assert!(r[0].kind.is_team());
    }

    #[test]
    fn bot_and_mannequin_arms_keep_login_and_avatar_and_stay_display_only() {
        let (r, _) = requests(
            json!([
                {"requestedReviewer": {"__typename": "Bot", "id": "B1", "login": "copilot", "avatarUrl": "https://x/b"}},
                {"requestedReviewer": {"__typename": "Mannequin", "id": "M1", "login": "imported", "avatarUrl": "https://x/m"}}
            ]),
            2,
        );
        assert_eq!(r[0].kind, ReviewerKind::Bot);
        assert_eq!(r[1].kind, ReviewerKind::Mannequin);
        for x in &r {
            assert!(x.id.is_none() && !x.is_user() && !x.kind.is_team());
            assert!(x.avatar_url.is_some());
        }
        assert_eq!(r[0].login, "copilot");
        assert_eq!(r[1].login, "imported");
    }

    #[test]
    fn a_null_reviewer_and_an_unknown_arm_drop_out_below_the_total() {
        let (r, total) = requests(
            json!([
                {"requestedReviewer": null},
                {"requestedReviewer": {"__typename": "Robot", "login": "x"}},
                {"requestedReviewer": {"__typename": "User", "id": "U1", "login": "a"}}
            ]),
            3,
        );
        assert_eq!(r.len(), 1);
        assert_eq!(total, 3);
    }

    #[test]
    fn a_missing_connection_parses_empty() {
        let (r, total) = parse_review_requests(&json!({}));
        assert!(r.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn latest_reviews_parse_state_author_and_id() {
        let pr = json!({"latestReviews": {"nodes": [
            {"id": "R1", "state": "COMMENTED", "author": {"login": "a", "avatarUrl": "https://x/a"}},
            {"id": "R2", "state": "APPROVED", "author": null},
            {"id": "R3", "state": "APPROVED"}
        ]}});
        let r = parse_latest_reviews(&pr, "latestReviews");
        assert_eq!(r.len(), 1, "author-less nodes drop");
        assert_eq!(r[0].id, "R1");
        assert_eq!(r[0].login, "a");
        assert_eq!(r[0].state, "COMMENTED");
        assert!(parse_latest_reviews(&pr, "latestOpinionatedReviews").is_empty());
    }

    #[test]
    fn the_detail_parse_carries_decision_and_both_review_lists() {
        let d = crate::gql::pr_detail_from_value(&json!({"pullRequest": {
            "id": "PR_1", "number": 1, "title": "t", "url": "u",
            "reviewDecision": "CHANGES_REQUESTED",
            "latestOpinionatedReviews": {"nodes": [
                {"id": "R1", "state": "CHANGES_REQUESTED", "author": {"login": "b"}}
            ]},
            "latestReviews": {"nodes": [
                {"id": "R1", "state": "CHANGES_REQUESTED", "author": {"login": "b"}},
                {"id": "R2", "state": "COMMENTED", "author": {"login": "c"}}
            ]}
        }}))
        .expect("parses");
        assert_eq!(d.review_decision.as_deref(), Some("CHANGES_REQUESTED"));
        assert_eq!(d.latest_opinionated_reviews.len(), 1);
        assert_eq!(d.latest_reviews.len(), 2);
        let bare =
            crate::gql::pr_detail_from_value(&json!({"pullRequest": {"id": "P"}})).expect("parses");
        assert!(bare.review_decision.is_none() && bare.latest_reviews.is_empty());
    }
}
