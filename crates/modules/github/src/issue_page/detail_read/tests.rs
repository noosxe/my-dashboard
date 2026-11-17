//! #806 (the #743 port): a page dropped mid-flight must not strand the
//! document's flight record. Every test drives a real page against a
//! transport whose `IssueDetail` read is armed but unanswered until the
//! test runs the executors.

use gpui::TestAppContext;

use crate::issue_page::test_rig::{Rig, detail_at_server};

#[gpui::test]
fn page_dropped_mid_flight_still_lands_the_result(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, vec![detail_at_server(1, "OPEN")], false);
    let window = rig.open_droppable(cx);
    assert!(rig.in_flight(cx), "the open read is flying");

    rig.close(cx, window);
    cx.run_until_parked();

    assert!(!rig.in_flight(cx), "the flight record cleared");
    assert!(rig.has_detail(cx), "the result landed on the store");
}

#[gpui::test]
fn reopen_after_a_dropped_flight_shows_the_detail(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, vec![detail_at_server(1, "OPEN")], false);
    let window = rig.open_droppable(cx);
    rig.close(cx, window);
    cx.run_until_parked();

    let _window = rig.open_droppable(cx);
    cx.run_until_parked();
    assert!(!rig.in_flight(cx));
    assert!(rig.has_detail(cx), "the reopened document is no skeleton");
}

/// Reopen while the orphaned flight is still in the air: the shared guard
/// skips the new dispatch, so the orphan's landing is the only thing that
/// can clear the record.
#[gpui::test]
fn reopen_during_a_dropped_flight_recovers_when_it_lands(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, vec![detail_at_server(1, "OPEN")], false);
    let window = rig.open_droppable(cx);
    rig.close(cx, window);

    let _reopened = rig.open_droppable(cx);
    assert_eq!(rig.detail_reads(), 0, "nothing has answered yet");
    cx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1, "the guard skipped the duplicate");

    assert!(!rig.in_flight(cx));
    assert!(rig.has_detail(cx));
}

/// docs/0085 D7: GitHub nulls a `references` element whose non-null `source`
/// the viewer cannot read and adds an `errors` entry beside the data. The
/// read lands the issue and the readable rows; it does not fail.
#[gpui::test]
fn a_partial_errors_envelope_lands_the_issue_and_its_readable_references(cx: &mut TestAppContext) {
    let body = serde_json::json!({
        "data": {"repository": {"nameWithOwner": "o/r", "issue": {
            "id": "I_7", "number": 7, "title": "probe", "url": "u", "state": "OPEN",
            "createdAt": "2026-09-01T00:00:00Z",
            "references": {"filteredCount": 2, "nodes": [
                null,
                {"__typename": "CrossReferencedEvent", "id": "X1",
                 "createdAt": "2026-09-02T00:00:00Z", "actor": {"login": "octo-dev"},
                 "willCloseTarget": true,
                 "source": {"__typename": "PullRequest", "number": 9, "title": "Fix",
                            "state": "OPEN", "isDraft": false, "merged": false,
                            "url": "https://example.com/pull/9",
                            "repository": {"nameWithOwner": "o/r", "isPrivate": false}}}
            ]}
        }}},
        "errors": [{"message": "Resource not accessible", "type": "FORBIDDEN"}]
    })
    .to_string();
    let rig = Rig::new(
        cx,
        vec![crate::issue_page::test_rig::Reply::Body(body)],
        false,
    );
    let _window = rig.open_droppable(cx);
    cx.run_until_parked();

    assert!(rig.has_detail(cx), "the page survives the errors entry");
    let (rows, total) = rig.with_entry(cx, |e| {
        let r = &e.detail.as_ref().unwrap().references;
        (r.nodes.len(), r.total_count)
    });
    assert_eq!((rows, total), (1, 1), "the null element drops, uncounted");
}
