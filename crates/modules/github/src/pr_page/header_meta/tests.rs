use super::*;
use crate::gql;

const REPO: &str = "acme/widgets";

fn ago(days: i64) -> String {
    (jiff::Timestamp::now() - jiff::SignedDuration::from_hours(days * 24)).to_string()
}

fn summary(draft: bool, updated: Option<String>, url: &str) -> gql::PullRequest {
    gql::PullRequest {
        key: format!("{REPO}#7"),
        database_id: None,
        number: 7,
        title: "probe".into(),
        draft,
        state: None,
        author: None,
        updated_at: updated,
        url: url.into(),
        status: None,
        labels: None,
        review_requests: None,
    }
}

fn detail(state: &str, created: &str, updated: Option<&str>, url: &str) -> gql::PrDetail {
    let mut pr = serde_json::json!({
        "id": "PR_7", "number": 7, "title": "probe", "url": url,
        "state": state, "isDraft": false, "createdAt": created,
    });
    if let Some(u) = updated {
        pr["updatedAt"] = u.into();
    }
    gql::pr_detail_from_value(&serde_json::json!({ "pullRequest": pr })).expect("detail parses")
}

#[test]
fn nothing_known_claims_no_state_and_links_by_repo_and_number() {
    // #763: no summary, no detail (opened from a notification).
    let m = header_meta(None, None, REPO, 7);
    assert_eq!(m.tone, None);
    assert_eq!(m.updated, None);
    assert_eq!(m.url, "https://github.com/acme/widgets/pull/7");
}

#[test]
fn summary_gives_open_or_draft_and_its_url() {
    let m = header_meta(None, Some(&summary(false, None, "https://x/7")), REPO, 7);
    assert_eq!(m.tone, Some(PrStateTone::Open));
    assert_eq!(m.url, "https://x/7");
    let m = header_meta(None, Some(&summary(true, None, "")), REPO, 7);
    assert_eq!(m.tone, Some(PrStateTone::Draft));
    assert_eq!(m.url, "https://github.com/acme/widgets/pull/7");
}

#[test]
fn detail_state_wins_over_the_summary() {
    let d = detail("MERGED", &ago(9), None, "");
    let m = header_meta(Some(&d), Some(&summary(false, None, "")), REPO, 7);
    assert_eq!(m.tone, Some(PrStateTone::Merged));
}

#[test]
fn age_is_the_labelled_updated_time_and_does_not_jump_when_detail_lands() {
    // #764: summary says updated 1d ago, the PR was created 3d ago.
    let s = summary(false, Some(ago(1)), "u");
    let before = header_meta(None, Some(&s), REPO, 7);
    assert_eq!(before.updated.as_deref(), Some("updated 1d ago"));
    // Detail without updatedAt: the summary's time still rules.
    let d = detail("OPEN", &ago(3), None, "u");
    let after = header_meta(Some(&d), Some(&s), REPO, 7);
    assert_eq!(after.updated, before.updated);
    // Created time is never shown.
    let only_created = header_meta(Some(&d), None, REPO, 7);
    assert_eq!(only_created.updated, None);
}

#[test]
fn newer_of_detail_and_summary_updated_wins() {
    let s = summary(false, Some(ago(3)), "u");
    let d = detail("OPEN", &ago(9), Some(&ago(1)), "u");
    let m = header_meta(Some(&d), Some(&s), REPO, 7);
    assert_eq!(m.updated.as_deref(), Some("updated 1d ago"));
    let m = header_meta(Some(&d), Some(&summary(false, Some(ago(0)), "u")), REPO, 7);
    assert_eq!(m.updated.as_deref(), Some("updated just now"));
}

#[test]
fn head_label_names_the_fork_owner() {
    assert_eq!(super::head_label("fix", None), "fix");
    assert_eq!(super::head_label("fix", Some("fork-user")), "fork-user:fix");
}

#[test]
fn action_copy_follows_the_pr_state() {
    use PrStateTone::*;
    let c = |t, n| action_copy(Some(t), n, Some("main"), Some("closer"));
    assert_eq!(c(Open, Some(1)).as_deref(), Some("wants to merge 1 commit"));
    assert_eq!(
        c(Draft, Some(3)).as_deref(),
        Some("wants to merge 3 commits")
    );
    assert_eq!(
        c(Merged, Some(1)).as_deref(),
        Some("merged 1 commit into main")
    );
    assert_eq!(
        c(Merged, Some(4)).as_deref(),
        Some("merged 4 commits into main")
    );
    assert_eq!(c(Merged, None).as_deref(), Some("merged into main"));
    assert_eq!(
        c(Closed, Some(4)).as_deref(),
        Some("4 commits · closed by closer")
    );
    assert_eq!(
        action_copy(Some(Closed), Some(1), None, None).as_deref(),
        Some("1 commit · closed")
    );
    assert_eq!(
        action_copy(Some(Closed), None, None, None).as_deref(),
        Some("closed")
    );
    assert_eq!(c(Open, None), None);
    assert_eq!(
        action_copy(None, Some(2), None, None),
        Some("wants to merge 2 commits".into())
    );
}
