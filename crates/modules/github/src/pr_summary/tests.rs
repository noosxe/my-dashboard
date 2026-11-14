//! #873 and #874 over the real store: detail landings refresh the
//! summary, a late list landing never undoes a newer state, and a
//! synthesized notification row never blanks a landed one.

use std::sync::Arc;

use crate::auth::MockTokenStore;
use crate::drafts::DraftStore;
use crate::gql;
use crate::http::mock::MockHttp;
use crate::http::{Http, SharedToken};
use crate::service::{AuthStatus, GithubCache, GithubState, PendingKind};

const REPO: &str = "acme/one";
const N: i64 = 482;
const T1: &str = "2026-11-01T10:00:00Z";
const T2: &str = "2026-11-01T11:00:00Z";
const T3: &str = "2026-11-01T12:00:00Z";

fn store() -> GithubState {
    let shared: SharedToken = Arc::new(std::sync::RwLock::new(Some("t0k".into())));
    let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
    let client: Arc<dyn Http> = Arc::new(MockHttp::new(vec![]));
    let mut s = GithubState::new(
        client,
        Arc::new(MockTokenStore::with("t0k".into())),
        shared,
        refresh,
        GithubCache::at_tmp("pr-summary"),
        DraftStore::at_tmp("pr-summary"),
    );
    s.auth = AuthStatus::Connected;
    s
}

fn row(state: &str, updated_at: &str) -> gql::PullRequest {
    serde_json::from_value(serde_json::json!({
        "key": format!("{REPO}#{N}"),
        "database_id": null,
        "number": N,
        "title": "listed",
        "draft": false,
        "state": state,
        "author": null,
        "updated_at": updated_at,
        "url": format!("https://github.com/{REPO}/pull/{N}"),
        "status": "SUCCESS",
        "labels": {"totalCount": 1, "nodes": [{"name": "bug", "color": "d73a4a"}]},
        "review_requests": {"totalCount": 0, "nodes": []},
    }))
    .expect("row parses")
}

fn detail(state: &str, updated_at: &str) -> gql::PrDetail {
    gql::pr_detail_from_value(&serde_json::json!({
        "pullRequest": {
            "id": "PR_482", "number": N, "title": "renamed on the page", "url": "u",
            "state": state, "isDraft": true, "createdAt": "2026-10-01T00:00:00Z",
            "updatedAt": updated_at,
            "labels": {"totalCount": 2, "nodes": [
                {"id": "L1", "name": "bug", "color": "d73a4a"},
                {"id": "L2", "name": "docs", "color": "0075ca"}
            ]},
            "reviewRequests": {"totalCount": 1, "nodes": [
                {"requestedReviewer": {"__typename": "User", "id": "U1", "login": "reviewer-a", "avatarUrl": "a"}}
            ]},
        }
    }))
    .expect("detail parses")
}

fn summary(s: &GithubState) -> gql::PullRequest {
    s.pr_entry(REPO, N).expect("the document").summary.clone()
}

#[test]
fn a_detail_landing_refreshes_the_summary_row() {
    let mut s = store();
    s.land_summary(REPO, &row("OPEN", T1));
    s.land_detail(REPO, N, detail("MERGED", T2));

    let sum = summary(&s);
    assert_eq!(
        sum.state.as_deref(),
        Some("MERGED"),
        "merged by someone else"
    );
    assert_eq!(sum.title, "renamed on the page");
    assert!(sum.draft);
    assert_eq!(sum.updated_at.as_deref(), Some(T2));
    let labels = sum.labels.expect("labels follow the detail");
    assert_eq!(labels.total_count, 2);
    assert_eq!(labels.nodes[1].name, "docs");
    let reviewers = sum.review_requests.expect("review requests follow");
    assert_eq!(reviewers.total_count, 1);
    assert_eq!(
        reviewers.nodes[0].requested_reviewer,
        Some(gql::RequestedReviewer::User {
            login: "reviewer-a".into()
        })
    );
}

#[test]
fn a_late_list_landing_cannot_undo_a_state_the_page_saw() {
    let mut s = store();
    s.land_summary(REPO, &row("OPEN", T1));
    s.land_detail(REPO, N, detail("MERGED", T2));

    // A list read dispatched before the merge lands afterwards.
    s.land_summary(REPO, &row("OPEN", T1));
    assert_eq!(summary(&s).state.as_deref(), Some("MERGED"));
    assert_eq!(summary(&s).title, "renamed on the page");
}

#[test]
fn a_late_list_landing_cannot_undo_a_confirmed_write() {
    let mut s = store();
    s.land_summary(REPO, &row("OPEN", T1));
    let token = s.begin_pending(REPO, N, PendingKind::Merge).unwrap();
    assert!(s.resolve_pending(REPO, N, token));
    assert_eq!(summary(&s).state.as_deref(), Some("MERGED"));

    // The same-stamp, pre-merge row: a state change always moves
    // `updatedAt`, so an equal stamp with another state is the old read.
    s.land_summary(REPO, &row("OPEN", T1));
    assert_eq!(summary(&s).state.as_deref(), Some("MERGED"));
}

#[test]
fn a_newer_list_landing_wins_and_an_older_one_still_moves_the_rollup() {
    let mut s = store();
    s.land_summary(REPO, &row("OPEN", T2));
    s.land_detail(REPO, N, detail("CLOSED", T2));

    // Reopened since: newer stamp, newer state.
    s.land_summary(REPO, &row("OPEN", T3));
    assert_eq!(summary(&s).state.as_deref(), Some("OPEN"));

    // CI moves without touching `updatedAt`: an equal-stamp row still
    // carries the new rollup.
    let mut ci = row("OPEN", T3);
    ci.status = Some("FAILURE".into());
    s.land_summary(REPO, &ci);
    assert_eq!(summary(&s).status.as_deref(), Some("FAILURE"));
}

fn synthesized() -> gql::PullRequest {
    gql::PullRequest {
        key: format!("{REPO}#{N}"),
        database_id: None,
        number: N,
        title: "from a notification".into(),
        draft: false,
        state: None,
        author: None,
        updated_at: None,
        url: String::new(),
        status: None,
        labels: None,
        review_requests: None,
    }
}

#[test]
fn a_notification_open_keeps_the_landed_row() {
    let mut s = store();
    s.land_summary(REPO, &row("OPEN", T1));
    s.land_detail(REPO, N, detail("OPEN", T1));
    s.land_summary(REPO, &row("OPEN", T1));
    let before = summary(&s);

    s.seed_summary(REPO, &synthesized());

    let after = summary(&s);
    assert_eq!(
        after.state.as_deref(),
        Some("OPEN"),
        "still in the Open list"
    );
    assert_eq!(after.url, before.url);
    assert_eq!(after.status, before.status);
    assert_eq!(after.labels, before.labels);
    assert_eq!(after.title, before.title);
    let entry = s.pr_entry(REPO, N).unwrap();
    assert!(!entry.rollup_dirty, "no needless catch-up read");
}

#[test]
fn a_notification_open_of_an_unknown_pr_inserts_the_row() {
    let mut s = store();
    s.seed_summary(REPO, &synthesized());
    assert_eq!(summary(&s).title, "from a notification");
}
