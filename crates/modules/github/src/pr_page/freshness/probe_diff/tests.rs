use super::*;

fn body(edit: impl FnOnce(&mut serde_json::Value)) -> PrSnapshot {
    let mut v = serde_json::json!({
        "state": "open", "title": "t", "draft": false, "merged": false,
        "head": { "sha": "aaa" }, "base": { "sha": "bbb" },
        "labels": [{ "name": "bug" }, { "name": "area" }],
        "requested_reviewers": [{ "login": "alice" }],
        "comments": 2, "review_comments": 3,
        "mergeable": true, "mergeable_state": "clean",
        "auto_merge": null, "updated_at": "2026-01-01T00:00:00Z",
        "additions": 9
    });
    edit(&mut v);
    PrSnapshot::parse(&v.to_string()).expect("parses")
}

#[test]
fn identical_bodies_do_not_differ() {
    let a = body(|_| {});
    assert_eq!(a.diff(&body(|_| {})), PrChange::default());
    assert!(!PrChange::default().any());
}

#[test]
fn a_new_head_is_a_head_change_only() {
    let a = body(|_| {});
    let b = body(|v| v["head"]["sha"] = "ccc".into());
    assert_eq!(
        a.diff(&b),
        PrChange {
            head: true,
            ..Default::default()
        }
    );
    assert_eq!(b.head_sha(), Some("ccc"));
}

#[test]
fn the_review_comment_count_is_its_own_change() {
    let a = body(|_| {});
    let b = body(|v| v["review_comments"] = 4.into());
    assert_eq!(
        a.diff(&b),
        PrChange {
            review_comments: true,
            ..Default::default()
        }
    );
}

#[test]
fn every_other_shown_field_is_an_other_change() {
    let a = body(|_| {});
    type Edit = fn(&mut serde_json::Value);
    let edits: [(&str, Edit); 9] = [
        ("state", |v| v["state"] = "closed".into()),
        ("title", |v| v["title"] = "u".into()),
        ("draft", |v| v["draft"] = true.into()),
        ("label", |v| {
            v["labels"] = serde_json::json!([{ "name": "x" }])
        }),
        ("reviewer", |v| {
            v["requested_reviewers"] = serde_json::json!([])
        }),
        ("comments", |v| v["comments"] = 3.into()),
        ("mergeable", |v| v["mergeable_state"] = "behind".into()),
        ("auto merge", |v| {
            v["auto_merge"] = serde_json::json!({ "merge_method": "squash" })
        }),
        ("updated_at", |v| {
            v["updated_at"] = "2026-01-02T00:00:00Z".into()
        }),
    ];
    for (what, edit) in edits {
        let change = a.diff(&body(edit));
        assert_eq!(
            change,
            PrChange {
                other: true,
                ..Default::default()
            },
            "{what}"
        );
    }
}

#[test]
fn a_reordered_label_list_is_not_a_change() {
    let a = body(|_| {});
    let b = body(|v| v["labels"] = serde_json::json!([{ "name": "area" }, { "name": "bug" }]));
    assert!(!a.diff(&b).any());
}

#[test]
fn fields_the_page_does_not_show_are_ignored() {
    let a = body(|_| {});
    assert!(!a.diff(&body(|v| v["additions"] = 10.into())).any());
}

#[test]
fn a_body_that_is_not_a_pull_does_not_parse() {
    assert!(PrSnapshot::parse("[1,2]").is_none());
    assert!(PrSnapshot::parse("not json").is_none());
}

#[test]
fn checks_running_reads_any_unfinished_run() {
    let done = r#"{"total_count":1,"check_runs":[{"status":"completed"}]}"#;
    let busy =
        r#"{"total_count":2,"check_runs":[{"status":"completed"},{"status":"in_progress"}]}"#;
    let queued = r#"{"check_runs":[{"status":"queued"}]}"#;
    assert_eq!(checks_running(done), Some(false));
    assert_eq!(checks_running(busy), Some(true));
    assert_eq!(checks_running(queued), Some(true));
    assert_eq!(checks_running(r#"{"check_runs":[]}"#), Some(false));
    assert_eq!(checks_running("nope"), None);
}
