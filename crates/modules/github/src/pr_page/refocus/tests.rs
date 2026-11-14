//! #871: refocus refetches the detail past the 15 s floor, and a voided
//! stamp on a landed document counts as stale.

use std::time::{Duration, Instant};

use gpui::TestAppContext;

use super::super::test_rig::{NUMBER, REPO, Rig, server};
use crate::service::{FOCUS_MIN_AGE, PrKey};

/// Backdate the landed document's stamp, as if it landed `age` ago.
fn age_detail(rig: &Rig, cx: &mut TestAppContext, age: Duration) {
    cx.update(|cx| {
        rig.state.update(cx, |s, _| {
            s.pulls
                .get_mut(&PrKey::new(REPO, NUMBER))
                .expect("the document")
                .detail_at = Some(Instant::now() - age);
        })
    });
}

#[gpui::test]
fn refocus_at_20_s_refetches(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        server("aaa", "MERGEABLE"),
        vec![server("bbb", "MERGEABLE")],
    );
    let (page, vcx) = rig.open(cx);
    assert_eq!(rig.detail_reads(), 0, "a young document opens quietly");

    // Inside the floor: nothing.
    age_detail(&rig, vcx, Duration::from_secs(5));
    page.update(vcx, |p, cx| p.refresh_if_stale(FOCUS_MIN_AGE, cx));
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 0, "refocus under the floor is quiet");

    // Past the floor but well inside the 5-minute reopen TTL.
    age_detail(&rig, vcx, Duration::from_secs(20));
    page.update(vcx, |p, cx| p.refresh_if_stale(FOCUS_MIN_AGE, cx));
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1, "refocus at 20 s refetches");
    assert_eq!(rig.head(vcx).as_deref(), Some("bbb"));
}

#[gpui::test]
fn a_voided_stamp_refetches(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        server("aaa", "MERGEABLE"),
        vec![server("bbb", "MERGEABLE")],
    );
    let (page, vcx) = rig.open(cx);
    rig.void_freshness(vcx);
    assert!(!rig.stamped(vcx));

    page.update(vcx, |p, cx| p.refresh_if_stale(FOCUS_MIN_AGE, cx));
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1, "refocus heals a voided document");
    assert!(rig.stamped(vcx), "the landing stamps it again");
    assert_eq!(rig.head(vcx).as_deref(), Some("bbb"));
}

#[test]
fn only_floor_passing_and_deliberate_triggers_bypass_the_ttl() {
    use crate::usage::Trigger::*;
    for t in [Refocus, Manual, MutationRefetch, Recheck] {
        assert!(super::bypasses_reopen_ttl(t), "{t:?}");
    }
    for t in [ViewShown, Bootstrap, Expand] {
        assert!(!super::bypasses_reopen_ttl(t), "{t:?}");
    }
}
