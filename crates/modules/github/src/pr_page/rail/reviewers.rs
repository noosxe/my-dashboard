//! The reviewer merge (docs/0083 D8): one row per reviewer from the
//! opinionated reviews, the latest reviews and the review requests.
//! Pure: no rendering, no page state.

use crate::gql::{LatestReview, PrDetail, ReviewerKind};

/// A reviewer's state, in display order: changes requested first (what
/// blocks), dismissed last (what no longer counts).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ReviewState {
    Changes,
    Approved,
    Commented,
    Pending,
    Dismissed,
}

impl ReviewState {
    /// The visible word beside the icon.
    pub(super) fn word(self) -> &'static str {
        match self {
            Self::Changes => "Changes",
            Self::Approved => "Approved",
            Self::Commented => "Commented",
            Self::Pending => "Pending",
            Self::Dismissed => "Dismissed",
        }
    }

    /// The phrase the row's accessible name uses (D14).
    pub(super) fn spoken(self) -> &'static str {
        match self {
            Self::Changes => "changes requested",
            Self::Approved => "approved",
            Self::Commented => "commented",
            Self::Pending => "review pending",
            Self::Dismissed => "dismissed",
        }
    }
}

/// One reviewer row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReviewerRow {
    pub kind: ReviewerKind,
    pub state: ReviewState,
    /// The login, or a team's name.
    pub name: String,
    pub avatar_url: Option<String>,
}

/// The state a review leaves its author in; a `PENDING` draft review is
/// not visible to anyone else, so it makes no row.
fn state_of(review: &LatestReview) -> Option<ReviewState> {
    match review.state.as_str() {
        "APPROVED" => Some(ReviewState::Approved),
        "CHANGES_REQUESTED" => Some(ReviewState::Changes),
        "COMMENTED" => Some(ReviewState::Commented),
        "DISMISSED" => Some(ReviewState::Dismissed),
        _ => None,
    }
}

/// Merge the three sources into one row per reviewer (D8). Opinionated
/// reviews come first, so a commenter row never replaces an opinion;
/// a request replaces whatever came before it (a re-request shows
/// Pending, as on github.com). Users, bots and mannequins dedupe by
/// login, case-insensitively; teams by node id.
pub(super) fn merge(d: &PrDetail) -> Vec<ReviewerRow> {
    let author = d.author.as_ref().map(|a| a.login.to_lowercase());
    let mut rows: Vec<(String, ReviewerRow)> = Vec::new();
    let mut put = |key: String, row: ReviewerRow, replace: bool| match rows
        .iter_mut()
        .find(|(k, _)| *k == key)
    {
        Some((_, old)) if replace => *old = row,
        Some(_) => {}
        None => rows.push((key, row)),
    };
    for r in d.latest_opinionated_reviews.iter().chain(&d.latest_reviews) {
        let Some(state) = state_of(r) else { continue };
        if Some(r.login.to_lowercase()) == author {
            continue;
        }
        put(
            r.login.to_lowercase(),
            ReviewerRow {
                kind: ReviewerKind::User,
                state,
                name: r.login.clone(),
                avatar_url: r.avatar_url.clone(),
            },
            false,
        );
    }
    for r in &d.review_requests {
        let (key, name) = if r.kind.is_team() {
            // A team without an id can't be told apart from another;
            // its name stands in.
            let id = r.team_id.as_deref().unwrap_or(&r.name);
            (format!("team:{id}"), r.name.clone())
        } else {
            if Some(r.login.to_lowercase()) == author {
                continue;
            }
            (r.login.to_lowercase(), r.login.clone())
        };
        put(
            key,
            ReviewerRow {
                kind: r.kind,
                state: ReviewState::Pending,
                name,
                avatar_url: r.avatar_url.clone(),
            },
            true,
        );
    }
    let mut rows: Vec<ReviewerRow> = rows.into_iter().map(|(_, row)| row).collect();
    rows.sort_by_cached_key(|r| (r.state, r.name.to_lowercase(), r.name.clone()));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr_page::rail::test_support::detail;
    use serde_json::{Value, json};

    fn review(login: &str, state: &str) -> Value {
        json!({"id": format!("R-{login}"), "state": state,
               "author": {"login": login, "avatarUrl": format!("https://x/{login}")}})
    }
    fn user(login: &str) -> Value {
        json!({"requestedReviewer": {"__typename": "User", "id": format!("U-{login}"),
               "login": login, "avatarUrl": format!("https://x/{login}")}})
    }

    /// A PR by `octo` with no requests and no reviews, patched.
    fn pr(patch: Value) -> PrDetail {
        let mut base = json!({"reviewRequests": {"totalCount": 0, "nodes": []}});
        for (k, v) in patch.as_object().unwrap() {
            base[k] = v.clone();
        }
        detail(base)
    }

    fn opinions(nodes: Vec<Value>) -> Value {
        json!({"latestOpinionatedReviews": {"nodes": nodes}})
    }
    fn latest(nodes: Vec<Value>) -> Value {
        json!({"latestReviews": {"nodes": nodes}})
    }
    fn requests(nodes: Vec<Value>) -> Value {
        json!({"reviewRequests": {"totalCount": nodes.len(), "nodes": nodes}})
    }
    fn all(parts: Vec<Value>) -> Value {
        let mut out = json!({});
        for p in parts {
            for (k, v) in p.as_object().unwrap() {
                out[k] = v.clone();
            }
        }
        out
    }
    fn summary(rows: &[ReviewerRow]) -> Vec<(String, ReviewState)> {
        rows.iter().map(|r| (r.name.clone(), r.state)).collect()
    }

    #[test]
    fn nothing_yields_no_rows() {
        assert!(merge(&pr(json!({}))).is_empty());
    }

    #[test]
    fn each_source_maps_to_its_state() {
        let d = pr(all(vec![
            opinions(vec![
                review("ann", "APPROVED"),
                review("bo", "CHANGES_REQUESTED"),
                review("cy", "DISMISSED"),
            ]),
            latest(vec![review("di", "COMMENTED")]),
            requests(vec![user("ed")]),
        ]));
        assert_eq!(
            summary(&merge(&d)),
            vec![
                ("bo".into(), ReviewState::Changes),
                ("ann".into(), ReviewState::Approved),
                ("di".into(), ReviewState::Commented),
                ("ed".into(), ReviewState::Pending),
                ("cy".into(), ReviewState::Dismissed),
            ]
        );
    }

    #[test]
    fn a_commenter_with_an_opinion_keeps_the_opinion() {
        // `latestReviews` repeats opinionated reviewers; the opinion wins.
        let d = pr(all(vec![
            opinions(vec![review("ann", "APPROVED")]),
            latest(vec![review("ann", "COMMENTED")]),
        ]));
        assert_eq!(
            summary(&merge(&d)),
            vec![("ann".into(), ReviewState::Approved)]
        );
    }

    #[test]
    fn a_re_request_after_a_review_shows_pending() {
        let d = pr(all(vec![
            opinions(vec![review("ann", "CHANGES_REQUESTED")]),
            latest(vec![review("ann", "CHANGES_REQUESTED")]),
            requests(vec![user("ann")]),
        ]));
        assert_eq!(
            summary(&merge(&d)),
            vec![("ann".into(), ReviewState::Pending)]
        );
    }

    #[test]
    fn a_pending_review_state_is_not_a_row() {
        let d = pr(latest(vec![review("ann", "PENDING")]));
        assert!(merge(&d).is_empty());
    }

    #[test]
    fn logins_dedupe_across_sources_ignoring_case() {
        let d = pr(all(vec![
            opinions(vec![review("Ann", "APPROVED")]),
            requests(vec![user("ann"), user("ann")]),
        ]));
        assert_eq!(merge(&d).len(), 1);
    }

    #[test]
    fn the_pr_author_never_appears() {
        let d = pr(all(vec![
            opinions(vec![review("octo", "APPROVED")]),
            latest(vec![review("OCTO", "COMMENTED")]),
            requests(vec![user("octo"), user("ann")]),
        ]));
        assert_eq!(
            summary(&merge(&d)),
            vec![("ann".into(), ReviewState::Pending)]
        );
    }

    #[test]
    fn rows_order_by_state_then_login_ignoring_case() {
        let d = pr(all(vec![
            opinions(vec![
                review("zed", "APPROVED"),
                review("Amy", "APPROVED"),
                review("bob", "APPROVED"),
            ]),
            requests(vec![user("yan"), user("Cat")]),
        ]));
        let names: Vec<_> = merge(&d).into_iter().map(|r| r.name).collect();
        assert_eq!(names, ["Amy", "bob", "zed", "Cat", "yan"]);
    }

    #[test]
    fn teams_dedupe_by_id_and_show_their_name_and_avatar() {
        let team = |id: &str, name: &str| {
            json!({"requestedReviewer": {"__typename": "Team", "id": id, "name": name,
                   "avatarUrl": "https://x/t"}})
        };
        let d = pr(requests(vec![
            team("T1", "core"),
            team("T1", "core"),
            team("T2", "core"),
        ]));
        let rows = merge(&d);
        assert_eq!(rows.len(), 2, "same name, different id stays two rows");
        assert!(rows.iter().all(|r| r.kind == ReviewerKind::Team));
        assert_eq!(rows[0].avatar_url.as_deref(), Some("https://x/t"));
    }

    #[test]
    fn a_team_named_like_a_user_does_not_swallow_the_user() {
        let d = pr(requests(vec![
            json!({"requestedReviewer": {"__typename": "Team", "id": "T1", "name": "ann"}}),
            user("ann"),
        ]));
        assert_eq!(merge(&d).len(), 2);
    }

    #[test]
    fn bots_and_mannequins_show_their_login_and_avatar() {
        let d = pr(requests(vec![
            json!({"requestedReviewer": {"__typename": "Bot", "id": "B1", "login": "copilot",
                   "avatarUrl": "https://x/b"}}),
            json!({"requestedReviewer": {"__typename": "Mannequin", "id": "M1", "login": "old",
                   "avatarUrl": "https://x/m"}}),
        ]));
        let rows = merge(&d);
        assert_eq!(rows[0].name, "copilot");
        assert_eq!(rows[0].kind, ReviewerKind::Bot);
        assert_eq!(rows[1].name, "old");
        assert_eq!(rows[1].avatar_url.as_deref(), Some("https://x/m"));
    }

    #[test]
    fn an_enterprise_team_has_no_avatar() {
        let d = pr(requests(vec![
            json!({"requestedReviewer": {"__typename": "EnterpriseTeam", "id": "E1", "name": "platform"}}),
        ]));
        let rows = merge(&d);
        assert_eq!(rows[0].name, "platform");
        assert!(rows[0].avatar_url.is_none());
    }

    #[test]
    fn the_row_words_and_spoken_phrases() {
        assert_eq!(ReviewState::Changes.word(), "Changes");
        assert_eq!(ReviewState::Changes.spoken(), "changes requested");
        assert_eq!(ReviewState::Pending.spoken(), "review pending");
    }
}
