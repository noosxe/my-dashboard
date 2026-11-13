//! #752: an UNKNOWN mergeable state re-polls a bounded number of times.

use std::time::Duration;

use gpui::TestAppContext;

use super::super::test_rig::{Rig, server};
use super::{MAX_POLLS, POLL_DELAY};
use crate::pr_page::{Surface, Trigger};
use crate::usage::Origin;

fn manual() -> Origin {
    Origin::new(Surface::PrPage, Trigger::Manual)
}

fn wait(vcx: &mut gpui::VisualTestContext, polls: u32) {
    for _ in 0..polls {
        vcx.executor()
            .advance_clock(POLL_DELAY + Duration::from_millis(1));
        vcx.run_until_parked();
    }
}

#[gpui::test]
fn unknown_repolls_a_bounded_number_of_times(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        server("aaa", "MERGEABLE"),
        vec![server("aaa", "UNKNOWN")],
    );
    let (page, vcx) = rig.open(cx);
    page.update(vcx, |p, cx| p.fetch_detail(manual(), cx));
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1);

    wait(vcx, MAX_POLLS + 5);
    assert_eq!(
        rig.detail_reads() as u32,
        1 + MAX_POLLS,
        "read plus bounded retries"
    );
    wait(vcx, 5);
    assert_eq!(rig.detail_reads() as u32, 1 + MAX_POLLS, "it stays stopped");

    // A deliberate refresh earns a fresh budget, still bounded.
    page.update(vcx, |p, cx| p.fetch_detail(manual(), cx));
    vcx.run_until_parked();
    wait(vcx, MAX_POLLS + 5);
    assert_eq!(rig.detail_reads() as u32, 2 + 2 * MAX_POLLS);
}

#[gpui::test]
fn resolving_stops_the_polling(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        server("aaa", "MERGEABLE"),
        vec![
            server("aaa", "UNKNOWN"),
            server("aaa", "UNKNOWN"),
            server("aaa", "MERGEABLE"),
        ],
    );
    let (page, vcx) = rig.open(cx);
    page.update(vcx, |p, cx| p.fetch_detail(manual(), cx));
    vcx.run_until_parked();
    wait(vcx, 8);
    assert_eq!(rig.detail_reads(), 3, "stops at the first resolved answer");
}

#[gpui::test]
fn a_dropped_page_stops_polling(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        server("aaa", "MERGEABLE"),
        vec![server("aaa", "UNKNOWN")],
    );
    rig.void_freshness(cx);
    let window = rig.open_droppable(cx);
    cx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1);
    rig.close(cx, window);
    cx.executor().advance_clock(POLL_DELAY * 10);
    cx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1);
}
