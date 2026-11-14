//! docs/0084 D5-D7 over the page: the loop asks the cadence function,
//! the guard and the usage book, and the transport counts what leaves.
//! `watch.skew` moves the loop's own clock (Settled, Unattended and the
//! guard's hour), while `advance` moves the executor's timers.

use std::time::Duration;

use gpui::TestAppContext;

use super::tests::{advance, runs, seed, tick, watched};
use crate::gql;
use crate::pr_page::freshness::cadence::{ACTIVE, HOT, SECONDARY_FLOOR, SHOWN, Stop};
use crate::pr_page::test_rig::{NUMBER, REPO, Rig, server};
use crate::pr_page::{PrDetailPage, Surface, Trigger};
use crate::rate_pause::Lane;
use crate::service::PrKey;
use crate::usage::{BudgetSnapshot, Origin, budget_now};

type Vcx = gpui::VisualTestContext;
type Page = gpui::Entity<PrDetailPage>;

fn minutes(m: u64) -> Duration {
    Duration::from_secs(m * 60)
}

fn armed(page: &Page, vcx: &mut Vcx) -> Duration {
    page.read_with(vcx, |p, _| p.watch.armed_for)
}

fn stop(page: &Page, vcx: &mut Vcx) -> Option<Stop> {
    page.read_with(vcx, |p, _| p.probe_stop())
}

/// Move the loop's clock forward.
fn skew(page: &Page, vcx: &mut Vcx, by: Duration) {
    page.update(vcx, |p, _| p.watch.skew += by);
}

fn probes(rig: &Rig) -> usize {
    rig.probe_statuses(None).len()
}

fn set_usage(rig: &Rig, vcx: &mut Vcx, remaining: u32) {
    vcx.update(|_, cx| {
        rig.state.update(cx, |s, _| {
            let now = budget_now();
            let snap = BudgetSnapshot {
                limit: 5000,
                remaining,
                reset_at: now + 3600,
            };
            s.usage.lock().unwrap().seed_snapshots(snap, snap, now);
        })
    });
}

fn edit_detail(rig: &Rig, vcx: &mut Vcx, f: impl FnOnce(&mut gql::PrDetail)) {
    vcx.update(|_, cx| {
        rig.state.update(cx, |s, _| {
            f(s.pulls
                .get_mut(&PrKey::new(REPO, NUMBER))
                .and_then(|e| e.detail.as_mut())
                .expect("the detail"));
        })
    });
}

fn queued(rig: &Rig, vcx: &mut Vcx) {
    edit_detail(rig, vcx, |d| d.queueing.in_merge_queue = true);
}

fn set_active(rig: &Rig, vcx: &mut Vcx, active: bool) {
    vcx.update(|_, cx| rig.state.update(cx, |s, _| s.window_active = active));
}

fn refocus(page: &Page, vcx: &mut Vcx) {
    page.update(vcx, |p, cx| {
        p.refresh_if_stale(crate::service::FOCUS_MIN_AGE, cx)
    });
    vcx.run_until_parked();
}

fn refresh(page: &Page, vcx: &mut Vcx) {
    page.update(vcx, |p, cx| {
        p.fetch_detail(Origin::new(Surface::PrPage, Trigger::Manual), cx)
    });
    vcx.run_until_parked();
}

// --- tiers over time ---------------------------------------------------

#[gpui::test]
fn a_quiet_page_settles_and_a_refocus_wakes_it(cx: &mut TestAppContext) {
    let (_rig, page, vcx) = watched(cx);
    seed(vcx);
    assert_eq!(armed(&page, vcx), ACTIVE);
    skew(&page, vcx, minutes(31));
    tick(vcx);
    assert_eq!(armed(&page, vcx), ACTIVE * 2, "30 quiet minutes");
    skew(&page, vcx, minutes(100));
    advance(vcx, 61);
    assert_eq!(armed(&page, vcx), Duration::from_secs(300), "the cap");
    refocus(&page, vcx);
    assert_eq!(armed(&page, vcx), ACTIVE, "refocus brings it back");
}

#[gpui::test]
fn an_hour_unfocused_stops_the_probe_until_the_window_is_back(cx: &mut TestAppContext) {
    let (rig, page, vcx) = watched(cx);
    set_active(&rig, vcx, false);
    seed(vcx);
    advance(vcx, 61);
    let sent = probes(&rig);
    assert!(sent > 2, "Shown still probes");
    assert_eq!(stop(&page, vcx), None);

    skew(&page, vcx, minutes(61));
    advance(vcx, 61);
    advance(vcx, 61);
    assert_eq!(probes(&rig), sent, "nothing goes out after an hour");
    assert_eq!(stop(&page, vcx), Some(Stop::Unattended));

    set_active(&rig, vcx, true);
    refocus(&page, vcx);
    advance(vcx, 31);
    assert!(probes(&rig) > sent, "back at its tier");
    assert_eq!(stop(&page, vcx), None);
}

#[gpui::test]
fn a_closed_pr_stops_with_its_reason(cx: &mut TestAppContext) {
    let (rig, page, vcx) = watched(cx);
    seed(vcx);
    edit_detail(&rig, vcx, |d| d.state = Some("MERGED".into()));
    let sent = probes(&rig);
    advance(vcx, 31);
    assert_eq!(probes(&rig), sent);
    assert_eq!(stop(&page, vcx), Some(Stop::Closed));
}

#[gpui::test]
fn x_poll_interval_is_a_floor(cx: &mut TestAppContext) {
    let (rig, page, vcx) = watched(cx);
    *rig.http.probe_rate.lock().unwrap() = Some(crate::http::RateLimit {
        limit: 5000,
        remaining: 4900,
        reset_epoch: budget_now() + 3600,
        retry_after: None,
        poll_interval: Some(90),
    });
    seed(vcx);
    assert_eq!(armed(&page, vcx), Duration::from_secs(90));
}

#[gpui::test]
fn a_secondary_limit_waits_a_minute_and_backs_off(cx: &mut TestAppContext) {
    let (rig, page, vcx) = watched(cx);
    *rig.http.probe_force.lock().unwrap() = Some(403);
    seed(vcx);
    assert_eq!(probes(&rig), 1, "the tick stops at the first failure");
    assert_eq!(armed(&page, vcx), SECONDARY_FLOOR);
}

// --- merge queue and UNKNOWN (D6, D5) -----------------------------------

#[gpui::test]
fn a_queued_pr_reads_the_detail_every_30_s_while_probes_say_304(cx: &mut TestAppContext) {
    let (rig, _page, vcx) = watched(cx);
    queued(&rig, vcx);
    seed(vcx);
    assert_eq!(rig.detail_reads(), 0, "the seed tick reads nothing");
    tick(vcx);
    assert_eq!(rig.detail_reads(), 0, "inside 30 s of the open's read");
    let page = _page;
    skew(&page, vcx, Duration::from_secs(31));
    queued(&rig, vcx);
    tick(vcx);
    assert_eq!(rig.detail_reads(), 1, "due");
    assert_eq!(rig.ops_for("changed"), ["pr-detail"]);
    queued(&rig, vcx);
    tick(vcx);
    assert_eq!(rig.detail_reads(), 1, "spaced by 30 s");
    skew(&page, vcx, Duration::from_secs(31));
    queued(&rig, vcx);
    tick(vcx);
    assert_eq!(rig.detail_reads(), 2);
    // The probes themselves were 304s throughout.
    assert!(rig.probe_statuses(None).iter().skip(2).all(|s| *s == 304));
}

#[gpui::test]
fn an_unqueued_quiet_page_reads_nothing(cx: &mut TestAppContext) {
    let (rig, page, vcx) = watched(cx);
    seed(vcx);
    skew(&page, vcx, Duration::from_secs(120));
    tick(vcx);
    assert_eq!(rig.detail_reads(), 0);
}

fn pull_with_mergeable(mergeable: Option<bool>, updated: &str) -> String {
    serde_json::json!({
        "state": "open", "title": "probe", "head": { "sha": "aaa" },
        "review_comments": 0, "comments": 0, "updated_at": updated,
        "mergeable": mergeable, "mergeable_state": if mergeable.is_some() { "clean" } else { "unknown" },
    })
    .to_string()
}

fn mergeable_of(page: &Page, vcx: &mut Vcx) -> Option<String> {
    page.read_with(vcx, |p, cx| p.detail(cx).and_then(|d| d.mergeable.clone()))
}

/// A landed UNKNOWN resolves without the old re-poll: the Hot tier probes
/// every 10 s, sees GitHub's recompute move the PR's ETag, and refetches.
#[gpui::test]
fn unknown_resolves_through_the_hot_tier(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        server("aaa", "UNKNOWN"),
        vec![server("aaa", "MERGEABLE")],
    );
    rig.serve("pulls", &pull_with_mergeable(None, "t0"));
    rig.serve("check-runs", &runs("completed"));
    let (page, vcx) = rig.open(cx);
    page.update(vcx, |p, cx| p.start_probing(cx));
    seed(vcx);
    assert_eq!(armed(&page, vcx), HOT, "UNKNOWN is Hot");
    assert_eq!(mergeable_of(&page, vcx).as_deref(), Some("UNKNOWN"));

    advance(vcx, 11);
    assert_eq!(rig.detail_reads(), 0, "still computing: 304");
    rig.serve("pulls", &pull_with_mergeable(Some(true), "t1"));
    advance(vcx, 11);
    assert_eq!(rig.detail_reads(), 1, "the recompute moved the ETag");
    assert_eq!(mergeable_of(&page, vcx).as_deref(), Some("MERGEABLE"));
    assert_eq!(armed(&page, vcx), HOT, "a change keeps it Hot for 2 min");
    advance(vcx, 11);
    assert_eq!(rig.detail_reads(), 1, "and it stops there");
    assert!(rig.ops_for("recheck").is_empty());
}

/// The first 200 only seeds the comparison, so a detail that said UNKNOWN
/// over a PR REST already knows would wait forever for a change: the
/// queue rule reads it.
#[gpui::test]
fn unknown_over_a_known_pr_is_read_once(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        server("aaa", "UNKNOWN"),
        vec![server("aaa", "MERGEABLE")],
    );
    rig.serve("pulls", &pull_with_mergeable(Some(true), "t0"));
    rig.serve("check-runs", &runs("completed"));
    let (page, vcx) = rig.open(cx);
    page.update(vcx, |p, cx| p.start_probing(cx));
    seed(vcx);
    skew(&page, vcx, Duration::from_secs(31));
    advance(vcx, 11);
    assert_eq!(rig.detail_reads(), 1);
    assert_eq!(mergeable_of(&page, vcx).as_deref(), Some("MERGEABLE"));
    advance(vcx, 11);
    advance(vcx, 11);
    assert_eq!(rig.detail_reads(), 1);
}

// --- the guard (D7) ------------------------------------------------------

/// Spend `n` counted requests at the loop's clock.
fn spend(page: &Page, vcx: &mut Vcx, n: u32) {
    page.update(vcx, |p, _| {
        let t = p.watch.now();
        p.watch.guard.spend_rest(t, n);
    });
}

fn guard_case(cx: &mut TestAppContext, wake: fn(&Page, &mut Vcx)) {
    let (rig, page, vcx) = watched(cx);
    seed(vcx);
    // 300 requests, then 300 more half an hour later: the first breach.
    spend(&page, vcx, 300);
    skew(&page, vcx, minutes(30));
    spend(&page, vcx, 300);
    tick(vcx);
    assert_eq!(armed(&page, vcx), SHOWN, "the first breach: Shown");
    assert_eq!(stop(&page, vcx), None);

    // The early 300 leave the window, 300 more cross the line again
    // inside the hour of the first breach: the second.
    skew(&page, vcx, minutes(31));
    spend(&page, vcx, 300);
    advance(vcx, 61);
    let sent = probes(&rig);
    advance(vcx, 61);
    assert_eq!(probes(&rig), sent, "stopped");
    assert_eq!(stop(&page, vcx), Some(Stop::Guard));

    wake(&page, vcx);
    advance(vcx, 31);
    assert!(probes(&rig) > sent, "resumed");
    assert_eq!(stop(&page, vcx), None);
}

#[gpui::test]
fn the_guard_drops_to_shown_then_stops_until_refocus(cx: &mut TestAppContext) {
    guard_case(cx, refocus);
}

#[gpui::test]
fn the_guard_drops_to_shown_then_stops_until_refresh(cx: &mut TestAppContext) {
    guard_case(cx, refresh);
}

#[gpui::test]
fn a_probe_200_counts_against_the_ceiling_and_a_304_does_not(cx: &mut TestAppContext) {
    let (rig, page, vcx) = watched(cx);
    seed(vcx);
    let spent = |page: &Page, vcx: &mut Vcx| {
        page.update(vcx, |p, _| {
            let t = p.watch.now();
            p.watch.guard.spent(t)
        })
    };
    assert_eq!(spent(&page, vcx), (2, 0), "the seed: two 200s");
    tick(vcx);
    tick(vcx);
    assert_eq!(spent(&page, vcx), (2, 0), "304s are free");
    rig.serve(
        "pulls",
        &crate::pr_page::freshness::timer::tests::pull("aaa", 3, "t1"),
    );
    tick(vcx);
    assert_eq!(
        spent(&page, vcx),
        (3, 1),
        "a 200 and the detail read it caused"
    );
}

// --- usage thresholds (D7) ----------------------------------------------

#[gpui::test]
fn low_usage_multiplies_the_interval_by_four(cx: &mut TestAppContext) {
    let (rig, page, vcx) = watched(cx);
    set_usage(&rig, vcx, 1_000);
    seed(vcx);
    assert_eq!(armed(&page, vcx), ACTIVE * 4);
}

#[gpui::test]
fn critical_stops_the_probe_and_recovery_resumes_it(cx: &mut TestAppContext) {
    let (rig, page, vcx) = watched(cx);
    seed(vcx);
    set_usage(&rig, vcx, 400);
    advance(vcx, 31);
    advance(vcx, 61);
    assert_eq!(probes(&rig), 2, "only the seed went out");
    assert_eq!(stop(&page, vcx), Some(Stop::Usage));
    set_usage(&rig, vcx, 5_000);
    advance(vcx, 61);
    assert!(probes(&rig) > 2, "the next beat probes again");
    assert_eq!(stop(&page, vcx), None);
}

#[gpui::test]
fn an_exhausted_budget_sends_nothing_on_either_lane(cx: &mut TestAppContext) {
    let (rig, page, vcx) = watched(cx);
    seed(vcx);
    vcx.update(|_, cx| {
        rig.state.update(cx, |s, _| {
            s.rate_limited_until.arm(Lane::Graphql, budget_now() + 3600)
        })
    });
    advance(vcx, 31);
    advance(vcx, 61);
    assert_eq!(probes(&rig), 2);
    assert_eq!(stop(&page, vcx), Some(Stop::Usage));
}
