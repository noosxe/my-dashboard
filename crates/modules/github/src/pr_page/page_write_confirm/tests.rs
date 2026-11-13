//! #807: a page-modal write confirms on the store, so a page dropped
//! mid-write cannot leave the pre-write document looking fresh.

use gpui::TestAppContext;
use std::sync::atomic::Ordering;

use super::super::test_rig::{Rig, server};
use crate::pr_page::BusyOp;
use crate::service::Mutation;

fn fire(
    rig: &Rig,
    cx: &mut TestAppContext,
    window: &gpui::WindowHandle<crate::pr_page::PrDetailPage>,
) {
    let _ = rig;
    cx.update(|cx| {
        window
            .update(cx, |p, window, cx| {
                p.busy = Some(BusyOp::PrSetLabels);
                p.spawn_mutation(
                    BusyOp::PrSetLabels,
                    Mutation::AddReaction,
                    serde_json::json!({}),
                    window,
                    cx,
                );
            })
            .unwrap();
    });
}

#[gpui::test]
fn a_write_confirmed_after_the_page_dropped_voids_freshness(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        server("aaa", "MERGEABLE"),
        vec![server("aaa", "MERGEABLE")],
    );
    let window = rig.open_droppable(cx);
    assert!(
        rig.stamped(cx),
        "the young document is fresh before the write"
    );
    fire(&rig, cx, &window);
    rig.close(cx, window);
    cx.run_until_parked();

    assert_eq!(rig.mutations(), 1);
    assert!(!rig.stamped(cx), "the confirmation voided the stamp");
    assert_eq!(rig.detail_reads(), 0, "no page, no refetch");

    let _reopened = rig.open_droppable(cx);
    cx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1, "the reopen revalidates");
    assert!(rig.stamped(cx));
}

#[gpui::test]
fn a_failed_write_voids_nothing(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        server("aaa", "MERGEABLE"),
        vec![server("aaa", "MERGEABLE")],
    );
    rig.http.mutation_status.store(500, Ordering::SeqCst);
    let window = rig.open_droppable(cx);
    fire(&rig, cx, &window);
    rig.close(cx, window);
    cx.run_until_parked();
    assert_eq!(rig.mutations(), 1);
    assert!(rig.stamped(cx), "a rejected write changed nothing");
}

/// #739's rule, with the page gone: a read flying while the write confirms
/// predates it and must not stamp the document fresh.
#[gpui::test]
fn a_read_flying_across_the_dropped_write_does_not_land(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        server("aaa", "MERGEABLE"),
        vec![server("aaa", "MERGEABLE")],
    );
    rig.void_freshness(cx);
    let window = rig.open_droppable(cx);
    fire(&rig, cx, &window);
    rig.close(cx, window);
    cx.run_until_parked();
    assert!(!rig.stamped(cx));
    let _reopened = rig.open_droppable(cx);
    cx.run_until_parked();
    assert!(rig.stamped(cx), "the replacement read lands");
}
