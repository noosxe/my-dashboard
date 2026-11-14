use gpui::TestAppContext;

use super::super::test_rig::{Rig, server};
use crate::rate_pause::Lane;
use crate::usage::{Origin, Surface, Trigger, budget_now};

fn rig(cx: &mut TestAppContext) -> Rig {
    Rig::new(
        cx,
        server("aaa", "MERGEABLE"),
        vec![server("aaa", "MERGEABLE")],
    )
}

fn pause(rig: &Rig, cx: &mut gpui::VisualTestContext, lane: Lane) {
    rig.state.update(cx, |s, _| {
        s.rate_limited_until = Default::default();
        s.rate_limited_until.arm(lane, budget_now() + 3600);
    });
}

fn refetch(
    page: &gpui::Entity<crate::pr_page::PrDetailPage>,
    cx: &mut gpui::VisualTestContext,
    t: Trigger,
) {
    page.update(cx, |p, cx| {
        p.fetch_detail(Origin::new(Surface::PrPage, t), cx)
    });
    cx.run_until_parked();
}

#[gpui::test]
fn the_sweep_is_booked_under_the_triggering_read(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (page, vcx) = rig.open(cx);
    refetch(&page, vcx, Trigger::Manual);
    // Opening over the cached document swept once under view-shown.
    assert_eq!(rig.runs_reads(), 2);
    // The empty sweep's suites fallback is booked under the same trigger.
    let mut triggers = rig.gated_triggers();
    triggers.dedup();
    assert_eq!(triggers, ["view-shown", "manual"]);
}

#[gpui::test]
fn a_paused_rest_budget_holds_the_sweep_back(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (page, vcx) = rig.open(cx);
    let opened = rig.runs_reads();
    pause(&rig, vcx, Lane::Rest);
    refetch(&page, vcx, Trigger::Manual);
    assert_eq!(rig.detail_reads(), 1, "the GraphQL detail still flies");
    assert_eq!(rig.runs_reads(), opened, "the REST sweep does not");
}

#[gpui::test]
fn a_graphql_pause_does_not_hold_the_rest_sweep(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (page, vcx) = rig.open(cx);
    // The detail read is a GraphQL request and is held by its own pause
    // elsewhere; here the detail has landed and the sweep is re-armed
    // directly, which only looks at the REST lane.
    refetch(&page, vcx, Trigger::Manual);
    let before = rig.runs_reads();
    pause(&rig, vcx, Lane::Graphql);
    page.update(vcx, |p, cx| p.fetch_gated(Trigger::Manual, cx));
    vcx.run_until_parked();
    assert_eq!(rig.runs_reads(), before + 1);
}

#[test]
fn the_sweep_runs_on_open_refresh_head_change_and_a_newly_waiting_run() {
    use super::runs_after_landing as runs;
    // Open and Refresh sweep; every other landing doesn't.
    assert!(runs(Trigger::ViewShown, false, None, 0));
    assert!(runs(Trigger::Manual, false, Some(0), 0));
    for t in [
        Trigger::Refocus,
        Trigger::MutationRefetch,
        Trigger::Recheck,
        Trigger::TabEntry,
        Trigger::NotificationLanding,
        Trigger::Changed,
    ] {
        assert!(!runs(t, false, Some(1), 1), "{t:?}");
    }
    // A new head sweeps under any trigger.
    assert!(runs(Trigger::Changed, true, Some(0), 0));
    // A rise in failed checks (`action_required` runs) does too.
    assert!(runs(Trigger::Changed, false, Some(0), 1));
    assert!(
        !runs(Trigger::Changed, false, Some(2), 1),
        "a fall is no run waiting"
    );
    assert!(
        !runs(Trigger::Changed, false, None, 3),
        "no earlier landing to compare"
    );
}

#[gpui::test]
fn a_probe_driven_or_write_refetch_does_not_sweep(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (page, vcx) = rig.open(cx);
    let opened = rig.runs_reads();
    refetch(&page, vcx, Trigger::MutationRefetch);
    refetch(&page, vcx, Trigger::Changed);
    refetch(&page, vcx, Trigger::Refocus);
    assert_eq!(rig.detail_reads(), 3);
    assert_eq!(rig.runs_reads(), opened, "no sweep");
}
