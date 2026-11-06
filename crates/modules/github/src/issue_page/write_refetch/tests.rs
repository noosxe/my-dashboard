//! #806 (the #739 port): a read older than a confirmed write must not
//! land, must not stamp fresh, and is replaced by exactly one read. Every
//! test drives a real page against a scripted transport and counts
//! requests there. The write confirms while the read is flying through
//! the same store and page calls the mutation's continuation makes.

use gpui::TestAppContext;

use crate::issue_page::test_rig::{NUMBER, REPO, Reply, Rig, detail_at_server, origin};
use crate::service::IssuePendingKind;
use crate::usage::{Trigger, budget_now};

/// The issue's scenario: the forced refetch is skipped while the earlier
/// read flies. That read must not land, and the follow-up must.
#[gpui::test]
fn write_during_a_read_gets_one_follow_up_that_shows_it(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        vec![detail_at_server(0, "OPEN"), detail_at_server(1, "OPEN")],
        true,
    );
    let (page, vcx) = rig.open(cx);
    rig.fly_detail(vcx, &page);
    rig.confirm_write(vcx, &page, IssuePendingKind::Comment);

    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 2, "the read, then one follow-up");
    assert_eq!(rig.comments(vcx), 1, "the final detail shows the write");
    assert!(rig.stamped(vcx), "the follow-up postdates the write");

    rig.poke_store(vcx);
    assert_eq!(rig.detail_reads(), 2, "settled: no further read");
}

/// A pause holds the follow-up, which shows the stale landing alone: no
/// body, no stamp. The first notification past the pause fires it.
#[gpui::test]
fn stale_read_does_not_stamp_fresh_and_a_paused_rerun_waits(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        vec![detail_at_server(0, "OPEN"), detail_at_server(1, "OPEN")],
        true,
    );
    let (page, vcx) = rig.open(cx);
    rig.fly_detail(vcx, &page);
    rig.confirm_write(vcx, &page, IssuePendingKind::Comment);
    rig.pause_graphql(vcx, budget_now() + 3600);

    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1, "the pause holds the follow-up");
    assert!(!rig.stamped(vcx), "the outdated read claims no freshness");
    assert!(!rig.in_flight(vcx), "the flight closed");

    rig.poke_store(vcx);
    assert_eq!(rig.detail_reads(), 1, "still paused");

    rig.pause_graphql(vcx, budget_now().saturating_sub(1));
    rig.poke_store(vcx);
    assert_eq!(rig.detail_reads(), 2, "the held follow-up fires once");
    assert_eq!(rig.comments(vcx), 1);
    assert!(rig.stamped(vcx));
}

/// The page-gone rule: the flight record clears with the page gone. The
/// outdated body does not land, the void survives, and the reopen
/// revalidates.
#[gpui::test]
fn page_gone_stale_read_keeps_the_void_and_the_reopen_revalidates(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        vec![detail_at_server(0, "OPEN"), detail_at_server(1, "OPEN")],
        true,
    );
    rig.void_freshness(cx);
    let window = rig.open_droppable(cx);
    rig.confirm_on_store(cx, IssuePendingKind::Comment);
    rig.close(cx, window);

    cx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1, "no page, no follow-up");
    assert!(!rig.in_flight(cx), "the flight closed");
    assert!(!rig.stamped(cx), "the void survives the landing");

    let (_page, vcx) = rig.open(cx);
    assert_eq!(rig.detail_reads(), 2, "the reopen is the one follow-up");
    assert_eq!(rig.comments(vcx), 1);
    assert!(rig.stamped(vcx));
}

/// A reopen during the outdated flight is skipped by the shared guard and
/// owns no landing: the mark on the document brings it the rerun.
#[gpui::test]
fn reopen_during_an_outdated_orphan_flight_gets_the_rerun(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        vec![detail_at_server(0, "OPEN"), detail_at_server(1, "OPEN")],
        true,
    );
    rig.void_freshness(cx);
    let window = rig.open_droppable(cx);
    rig.confirm_on_store(cx, IssuePendingKind::Comment);
    rig.close(cx, window);

    let _reopened = rig.open_droppable(cx);
    cx.run_until_parked();
    assert_eq!(rig.detail_reads(), 2);
    assert_eq!(rig.comments(cx), 1);
    assert!(rig.stamped(cx));
}

/// A confirmed close patches the state; the read that predates it must
/// not flip the document back to OPEN.
#[gpui::test]
fn stale_read_does_not_undo_a_patched_close(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, vec![detail_at_server(0, "OPEN")], true);
    rig.void_freshness(cx);
    let window = rig.open_droppable(cx);
    rig.confirm_on_store(cx, IssuePendingKind::Close);
    rig.close(cx, window);

    cx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1);
    let state = rig.with_entry(cx, |e| e.detail.as_ref().and_then(|d| d.state.clone()));
    assert_eq!(state.as_deref(), Some("CLOSED"));
    assert_eq!(
        rig.with_entry(cx, |e| e.summary.state.clone()).as_deref(),
        Some("CLOSED")
    );
    assert!(!rig.stamped(cx));
}

/// Same for a reopen over an older CLOSED body: the patched state stays
/// while the follow-up is held.
#[gpui::test]
fn stale_read_does_not_undo_a_patched_reopen(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, vec![detail_at_server(0, "CLOSED")], true);
    let (page, vcx) = rig.open(cx);
    rig.fly_detail(vcx, &page);
    rig.confirm_on_store(vcx, IssuePendingKind::Reopen);
    rig.pause_graphql(vcx, budget_now() + 3600);

    vcx.run_until_parked();
    let state = rig.with_entry(vcx, |e| e.detail.as_ref().and_then(|d| d.state.clone()));
    assert_eq!(state.as_deref(), Some("OPEN"));
}

/// No loop: the outdated read and its follow-up both fail, and nothing
/// re-dispatches, not on a later store write and not past the backoff.
#[gpui::test]
fn failing_reads_stop_after_the_one_follow_up(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, vec![Reply::Status(502)], true);
    let (page, vcx) = rig.open(cx);
    rig.fly_detail(vcx, &page);
    rig.confirm_write(vcx, &page, IssuePendingKind::Comment);

    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 2, "failure still earns the follow-up");

    rig.poke_store(vcx);
    rig.elapse_backoff(vcx);
    rig.poke_store(vcx);
    assert_eq!(rig.detail_reads(), 2);
    assert!(!rig.stamped(vcx));
}

/// Without a write, the guard's dedup is unchanged: a manual refresh
/// rides out the flight and its result lands.
#[gpui::test]
fn manual_refresh_during_a_read_still_rides_it_out(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, vec![detail_at_server(1, "OPEN")], true);
    let (page, vcx) = rig.open(cx);
    rig.fly_detail(vcx, &page);
    vcx.update(|_, cx| {
        page.update(cx, |p, cx| p.fetch_detail(origin(Trigger::Manual), cx));
    });

    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1);
    assert_eq!(rig.comments(vcx), 1);
    assert!(rig.stamped(vcx));
}

/// A write that failed changed nothing, so the flying read stays good.
#[gpui::test]
fn failed_store_write_leaves_the_flying_read_alone(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, vec![detail_at_server(1, "OPEN")], true);
    let (page, vcx) = rig.open(cx);
    rig.fly_detail(vcx, &page);
    vcx.update(|_, cx| {
        rig.state.update(cx, |s, _| {
            let token = s
                .begin_issue_pending(REPO, NUMBER, IssuePendingKind::Comment)
                .unwrap();
            assert!(s.fail_issue_pending(REPO, NUMBER, token, "rejected".into()));
        })
    });

    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1);
    assert_eq!(rig.comments(vcx), 1);
    assert!(rig.stamped(vcx));
}
