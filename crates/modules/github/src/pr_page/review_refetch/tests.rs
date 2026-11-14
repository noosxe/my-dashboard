use gpui::TestAppContext;

use super::super::test_rig::{Rig, server};
use super::super::{LineDraft, PrDetailPage};
use crate::gql::DiffSide;

fn queue_one_draft(page: &mut PrDetailPage) {
    page.drafts.push(LineDraft {
        path: "src/lib.rs".into(),
        side: DiffSide::Right,
        line: 3,
        start: None,
        body: "nit".into(),
    });
}

#[gpui::test]
fn a_confirmed_review_batch_refetches_the_detail_and_the_threads(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        server("aaa", "MERGEABLE"),
        vec![server("aaa", "MERGEABLE")],
    );
    let window = rig.open_droppable(cx);
    cx.run_until_parked();
    cx.update(|cx| {
        window
            .update(cx, |p, window, cx| {
                queue_one_draft(p);
                p.submit_review(window, cx);
            })
            .unwrap();
    });
    cx.run_until_parked();

    assert_eq!(rig.mutations(), 1, "the batch was sent");
    assert_eq!(rig.threads_reads(), 1, "the threads refetch");
    assert_eq!(rig.detail_reads(), 1, "so does the detail");
    assert!(rig.stamped(cx), "the refetch stamps the document");
}
