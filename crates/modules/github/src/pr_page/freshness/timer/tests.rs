//! The watch loop over the fake clock: each `tick` moves it past the
//! longest interval, so exactly one probe fires, and the transport
//! counts what leaves.

use std::time::{Duration, Instant};

use gpui::TestAppContext;

use super::*;
use crate::pr_page::test_rig::{NUMBER, REPO, Rig, server};
use crate::service::PrKey;

// --- the pure parts ---------------------------------------------------

#[test]
fn the_interval_follows_focus_and_what_is_moving() {
    let quiet = Cadence::default();
    let active = Cadence {
        window_active: true,
        ..quiet
    };
    assert_eq!(interval(active), ACTIVE);
    assert_eq!(interval(quiet), SHOWN);
    for hot in [
        Cadence {
            checks_running: true,
            ..active
        },
        Cadence {
            mergeable_unknown: true,
            ..active
        },
        Cadence {
            recent_change: true,
            ..active
        },
    ] {
        assert_eq!(interval(hot), HOT);
        // Unfocused, a Hot condition other than focus holds it at 30 s.
        assert_eq!(
            interval(Cadence {
                window_active: false,
                ..hot
            }),
            ACTIVE
        );
    }
}

#[test]
fn a_failing_probe_backs_off_up_to_a_cap() {
    assert_eq!(backoff(HOT, 0), HOT);
    assert_eq!(backoff(HOT, 1), Duration::from_secs(20));
    assert_eq!(backoff(ACTIVE, 2), Duration::from_secs(120));
    assert_eq!(backoff(SHOWN, 9), FAIL_CAP);
}

#[test]
fn a_checks_change_inside_the_gap_is_scheduled_not_dropped() {
    let now = Instant::now();
    let mut c = Coalesce::default();
    assert_eq!(c.checks_change(now), Refetch::Now, "nothing sent before");
    c.dispatched(now);
    assert!(!c.pending);

    let soon = now + Duration::from_secs(5);
    assert_eq!(
        c.checks_change(soon),
        Refetch::After(Duration::from_secs(15))
    );
    assert!(c.pending, "kept for its slot");

    let later = now + CHECKS_GAP;
    assert_eq!(c.checks_change(later), Refetch::Now);
    c.dispatched(later);
    assert!(!c.pending, "a read that went out covers it");
}

// --- the page ---------------------------------------------------------

fn pull(head: &str, review_comments: i64, updated: &str) -> String {
    serde_json::json!({
        "state": "open", "title": "probe", "head": { "sha": head },
        "review_comments": review_comments, "comments": 0, "updated_at": updated,
    })
    .to_string()
}

fn runs(status: &str) -> String {
    serde_json::json!({ "check_runs": [{ "status": status }] }).to_string()
}

/// A rig whose probe endpoints answer generation zero, a detail that
/// keeps the head the page opened on, and a page over it.
fn watched(
    cx: &mut TestAppContext,
) -> (
    Rig,
    gpui::Entity<PrDetailPage>,
    &mut gpui::VisualTestContext,
) {
    watched_over(cx, "aaa")
}

/// The same, with the head the detail answers after a refetch.
fn watched_over<'a>(
    cx: &'a mut TestAppContext,
    head_after: &'static str,
) -> (
    Rig,
    gpui::Entity<PrDetailPage>,
    &'a mut gpui::VisualTestContext,
) {
    let rig = Rig::new(
        cx,
        server("aaa", "MERGEABLE"),
        vec![server(head_after, "MERGEABLE")],
    );
    rig.serve("pulls", &pull("aaa", 3, "t0"));
    rig.serve("check-runs", &runs("completed"));
    let (page, vcx) = rig.open(cx);
    page.update(vcx, |p, cx| p.start_probing(cx));
    (rig, page, vcx)
}

fn advance(vcx: &mut gpui::VisualTestContext, secs: u64) {
    vcx.executor()
        .advance_clock(Duration::from_secs(secs) + Duration::from_millis(1));
    vcx.run_until_parked();
}

/// The first probe, which seeds.
fn seed(vcx: &mut gpui::VisualTestContext) {
    advance(vcx, FIRST_PROBE.as_secs());
}

/// One Active interval: exactly one probe while the page is quiet, up to
/// three once a change made it Hot.
fn tick(vcx: &mut gpui::VisualTestContext) {
    advance(vcx, ACTIVE.as_secs());
}

#[gpui::test]
fn the_first_200s_seed_and_an_idle_page_only_sees_304s(cx: &mut TestAppContext) {
    let (rig, _page, vcx) = watched(cx);
    seed(vcx);
    assert_eq!(rig.probe_statuses(None), [200, 200], "the seed");
    assert_eq!(rig.detail_reads(), 0, "the seed refetches nothing");
    for _ in 0..4 {
        tick(vcx);
    }
    let after_seed: Vec<u16> = rig.probe_statuses(None).split_off(2);
    assert_eq!(after_seed.len(), 8);
    assert!(after_seed.iter().all(|s| *s == 304), "{after_seed:?}");
    assert_eq!(rig.detail_reads(), 0);
    assert_eq!(rig.threads_reads(), 0);
    // Watch rows, no changed rows.
    assert!(rig.ops_for("watch").iter().all(|op| op == "pr-probe"));
    assert!(rig.ops_for("changed").is_empty());
}

#[gpui::test]
fn another_change_on_the_pr_refetches_the_detail_once(cx: &mut TestAppContext) {
    let (rig, _page, vcx) = watched(cx);
    seed(vcx);
    rig.serve("pulls", &pull("aaa", 3, "t1"));
    tick(vcx);
    assert_eq!(rig.detail_reads(), 1);
    assert_eq!(rig.ops_for("changed"), ["pr-detail"]);
    assert_eq!(rig.threads_reads(), 0);
    tick(vcx);
    assert_eq!(rig.detail_reads(), 1, "the next probes are 304s again");
    let pulls = rig.probe_statuses(Some("pulls"));
    assert_eq!(&pulls[..2], [200, 200]);
    assert!(pulls[2..].iter().all(|s| *s == 304), "{pulls:?}");
}

#[gpui::test]
fn a_new_head_moves_the_check_runs_probe_and_fans_out(cx: &mut TestAppContext) {
    let (rig, page, vcx) = watched_over(cx, "bbb");
    // Files and threads are loaded, so the fan-out has something to do.
    page.update(vcx, |p, cx| {
        p.fetch_files(Origin::new(Surface::PrPage, Trigger::Expand), cx);
        p.fetch_threads(Origin::new(Surface::PrPage, Trigger::Expand), cx);
    });
    vcx.run_until_parked();
    let (files, threads) = (rig.files_reads(), rig.threads_reads());
    seed(vcx);
    rig.serve("pulls", &pull("bbb", 3, "t1"));
    rig.serve("check-runs", &runs("completed"));
    tick(vcx);
    assert_eq!(rig.detail_reads(), 1, "one detail read for the head");
    assert_eq!(rig.files_reads(), files + 1, "the head fan-out");
    assert_eq!(rig.threads_reads(), threads + 1, "the head fan-out");
    let urls = rig.probe_urls();
    assert!(urls.iter().any(|u| u.contains("commits/aaa/check-runs")));
    assert!(
        urls.last()
            .is_some_and(|u| u.contains("commits/bbb/check-runs")),
        "{urls:?}"
    );
}

#[gpui::test]
fn a_review_comment_change_refetches_threads_only_when_loaded(cx: &mut TestAppContext) {
    let (rig, page, vcx) = watched(cx);
    seed(vcx);
    rig.serve("pulls", &pull("aaa", 4, "t1"));
    tick(vcx);
    assert_eq!(
        (rig.detail_reads(), rig.threads_reads()),
        (1, 0),
        "not loaded"
    );

    page.update(vcx, |p, cx| {
        p.fetch_threads(Origin::new(Surface::PrPage, Trigger::Expand), cx)
    });
    vcx.run_until_parked();
    let before = rig.threads_reads();
    rig.serve("pulls", &pull("aaa", 5, "t2"));
    tick(vcx);
    assert_eq!(rig.detail_reads(), 2);
    assert_eq!(rig.threads_reads(), before + 1);
    assert_eq!(
        rig.ops_for("changed"),
        ["pr-detail", "pr-detail", "pr-threads"]
    );
}

#[gpui::test]
fn a_check_runs_change_refetches_the_detail_and_coalesces_at_20_s(cx: &mut TestAppContext) {
    let (rig, _page, vcx) = watched(cx);
    seed(vcx);
    rig.serve("check-runs", &runs("in_progress"));
    tick(vcx);
    assert_eq!(rig.detail_reads(), 1, "the first goes straight out");

    // A second checks-only change right behind it waits for its slot.
    rig.serve("check-runs", &runs("completed"));
    vcx.executor().advance_clock(Duration::from_secs(11));
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1, "scheduled, not sent");
    vcx.executor().advance_clock(Duration::from_secs(21));
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 2, "and not dropped");
    assert_eq!(rig.ops_for("changed"), ["pr-detail", "pr-detail"]);
}

#[gpui::test]
fn one_detail_read_per_tick_whatever_changed(cx: &mut TestAppContext) {
    let (rig, _page, vcx) = watched(cx);
    seed(vcx);
    rig.serve("pulls", &pull("aaa", 9, "t1"));
    rig.serve("check-runs", &runs("in_progress"));
    tick(vcx);
    assert_eq!(rig.detail_reads(), 1);
}

#[gpui::test]
fn a_tick_is_skipped_while_a_lane_is_in_flight(cx: &mut TestAppContext) {
    let (rig, _page, vcx) = watched(cx);
    let set = |rig: &Rig, vcx: &mut gpui::VisualTestContext, on: bool| {
        vcx.update(|_, cx| {
            rig.state.update(cx, |s, _| {
                s.pulls
                    .get_mut(&PrKey::new(REPO, NUMBER))
                    .expect("the document")
                    .books
                    .in_flight = on;
            })
        });
    };
    set(&rig, vcx, true);
    seed(vcx);
    tick(vcx);
    assert!(rig.probe_statuses(None).is_empty(), "skipped");
    set(&rig, vcx, false);
    tick(vcx);
    assert_eq!(
        rig.probe_statuses(None),
        [200, 200],
        "back on the next tick"
    );
}

#[gpui::test]
fn only_a_watched_page_probes(cx: &mut TestAppContext) {
    let (rig, page, vcx) = watched(cx);
    // The module is not on screen.
    page.update(vcx, |p, _| p.set_module_shown(false));
    seed(vcx);
    tick(vcx);
    assert!(rig.probe_statuses(None).is_empty(), "module hidden");
    page.update(vcx, |p, _| p.set_module_shown(true));

    // The REST budget is paused (the pause gate).
    vcx.update(|_, cx| {
        rig.state.update(cx, |s, _| {
            s.rate_limited_until
                .arm(Lane::Rest, crate::usage::budget_now() + 3600)
        })
    });
    tick(vcx);
    assert!(rig.probe_statuses(None).is_empty(), "REST paused");
    // A GraphQL pause alone doesn't hold the REST probe.
    vcx.update(|_, cx| {
        rig.state.update(cx, |s, _| {
            s.rate_limited_until = Default::default();
            s.rate_limited_until
                .arm(Lane::Graphql, crate::usage::budget_now() + 3600)
        })
    });
    tick(vcx);
    assert_eq!(rig.probe_statuses(None), [200, 200]);

    // A closed PR does not probe.
    vcx.update(|_, cx| {
        rig.state.update(cx, |s, _| {
            s.rate_limited_until = Default::default();
            s.pulls
                .get_mut(&PrKey::new(REPO, NUMBER))
                .and_then(|e| e.detail.as_mut())
                .expect("the detail")
                .state = Some("MERGED".into());
        })
    });
    let seen = rig.probe_statuses(None).len();
    tick(vcx);
    assert_eq!(rig.probe_statuses(None).len(), seen, "merged");
}

#[gpui::test]
fn a_notification_naming_the_pr_sends_one_immediate_probe(cx: &mut TestAppContext) {
    let (rig, _page, vcx) = watched(cx);
    // Seed the notification diff, and the probe.
    rig.land_notification(vcx, NUMBER, "t1");
    seed(vcx);
    let probed = rig.probe_statuses(None).len();

    // The PR changed; no clock moves, yet the landing probes at once.
    rig.serve("pulls", &pull("aaa", 3, "t1"));
    rig.land_notification(vcx, NUMBER, "t2");
    vcx.run_until_parked();
    assert_eq!(
        rig.probe_statuses(None).len(),
        probed + 2,
        "pulls and check-runs"
    );
    assert_eq!(rig.detail_reads(), 1, "the probe found the change");
    assert_eq!(rig.ops_for("notification-landing"), Vec::<String>::new());
}
