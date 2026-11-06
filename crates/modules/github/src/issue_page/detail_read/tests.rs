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
