//! docs/0085 on the issue page: rows merge by time with unique ids, the cap
//! row counts references, and activation emits or stays by target.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Modifiers, TestAppContext};

use crate::issue_page::test_rig::{Reply, Rig};
use crate::issue_page::{IssueDetailPage, IssuePageEvent};

fn detail_body(filtered: i64) -> String {
    let source = |kind: &str, n: i64, repo: &str, title: &str| {
        serde_json::json!({"__typename": kind, "number": n, "title": title,
            "state": "OPEN", "isDraft": false, "merged": false,
            "url": format!("https://example.com/{n}"),
            "repository": {"nameWithOwner": repo, "isPrivate": false}})
    };
    let event = |id: &str, at: &str, will_close: bool, src: serde_json::Value| {
        serde_json::json!({"__typename": "CrossReferencedEvent", "id": id,
            "createdAt": at, "actor": {"login": "octo-dev"},
            "willCloseTarget": will_close, "source": src})
    };
    serde_json::json!({"data": {"repository": {"nameWithOwner": "o/r", "issue": {
        "id": "I_7", "number": 7, "title": "probe", "url": "u", "state": "OPEN",
        "createdAt": "2026-09-01T00:00:00Z",
        "comments": {"totalCount": 1, "nodes": [{"id": "IC_1",
            "viewerCanUpdate": true, "author": {"login": "dave"}, "body": "hi",
            "createdAt": "2026-09-02T00:00:00Z"}]},
        "timelineItems": {"totalCount": 1, "filteredCount": 1, "nodes": [
            {"__typename": "LabeledEvent", "id": "E_1", "createdAt": "2026-09-01T12:00:00Z",
             "actor": {"login": "dave"}, "label": {"name": "bug"}}]},
        "references": {"filteredCount": filtered, "nodes": [
            event("X_1", "2026-09-02T00:00:00Z", true,
                  source("PullRequest", 9, "o/other", "Fix loader")),
            event("X_2", "2026-09-03T00:00:00Z", false,
                  source("Issue", 3, "fixture-org/elsewhere", "Related"))]}
    }}}})
    .to_string()
}

fn open(
    cx: &mut TestAppContext,
    filtered: i64,
) -> (
    Rig,
    gpui::Entity<IssueDetailPage>,
    &mut gpui::VisualTestContext,
    Rc<RefCell<Vec<String>>>,
) {
    let rig = Rig::new(cx, vec![Reply::Body(detail_body(filtered))], false);
    cx.update(|cx| {
        rig.state.update(cx, |s, _| {
            s.repos = vec![
                serde_json::from_value(serde_json::json!({
                    "key": "o/other", "database_id": 1, "name": "other",
                    "full_name": "o/other", "owner": {"login": "o"},
                    "has_issues_enabled": true,
                    "prs": {"total_count": 0, "nodes": []},
                    "issues": {"total_count": 0, "nodes": []}}))
                .unwrap(),
            ];
        })
    });
    let (page, vcx) = rig.open(cx);
    vcx.run_until_parked();
    let log = Rc::new(RefCell::new(Vec::new()));
    let sink = log.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&page, move |_, e: &IssuePageEvent, _| {
            if let IssuePageEvent::OpenSubject {
                repo,
                number,
                is_pr,
                title,
            } = e
            {
                sink.borrow_mut()
                    .push(format!("{repo}#{number} pr={is_pr} {title}"));
            }
        })
        .detach();
    });
    (rig, page, vcx, log)
}

fn y(vcx: &mut gpui::VisualTestContext, id: &'static str) -> gpui::Pixels {
    vcx.debug_bounds(id)
        .unwrap_or_else(|| panic!("{id} missing"))
        .origin
        .y
}

#[gpui::test]
fn rows_sit_in_time_order_with_ids_clear_of_the_event_rows(cx: &mut TestAppContext) {
    let (_rig, _page, vcx, _log) = open(cx, 2);
    // The label event is 09-01T12, the comment and X_1 tie at 09-02 (the
    // card first), X_2 is 09-03.
    assert!(y(vcx, "issue-event-0") < y(vcx, "issue-comment-0-actions"));
    assert!(y(vcx, "issue-comment-0-actions") < y(vcx, "issue-event-1"));
    assert!(y(vcx, "issue-event-1") < y(vcx, "issue-event-2"));
    assert!(
        vcx.debug_bounds("issue-events-more").is_none(),
        "all carried"
    );
}

#[gpui::test]
fn a_cut_window_joins_the_cap_row(cx: &mut TestAppContext) {
    let (_rig, _page, vcx, _log) = open(cx, 40);
    assert!(vcx.debug_bounds("issue-events-more").is_some());
}

#[gpui::test]
fn a_tracked_source_asks_the_host_and_an_untracked_one_stays(cx: &mut TestAppContext) {
    let (_rig, _page, vcx, log) = open(cx, 2);
    let tracked = vcx.debug_bounds("issue-event-1").unwrap();
    vcx.simulate_click(tracked.center(), Modifiers::default());
    assert_eq!(*log.borrow(), ["o/other#9 pr=true Fix loader"]);

    log.borrow_mut().clear();
    let untracked = vcx.debug_bounds("issue-event-2").unwrap();
    vcx.simulate_click(untracked.center(), Modifiers::default());
    assert!(log.borrow().is_empty(), "the browser path leaves the page");
}
