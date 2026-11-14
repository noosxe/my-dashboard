//! Each state flip's gate and payload (docs/0083 D4, D5).

use gpui::{Entity, TestAppContext, VisualTestContext};
use serde_json::{Value, json};

use super::state_input;
use crate::pr_page::PrDetailPage;
use crate::pr_page::rail::test_support::detail;
use crate::pr_page::test_rig::{NUMBER, REPO, Rig, server};
use crate::service::PendingKind;

fn open(
    cx: &mut TestAppContext,
    patch: Value,
) -> (Rig, Entity<PrDetailPage>, &mut VisualTestContext) {
    let rig = Rig::new(
        cx,
        server("h1", "MERGEABLE"),
        vec![server("h1", "MERGEABLE")],
    );
    cx.update(|cx| {
        rig.state
            .update(cx, |s, _| s.land_detail(REPO, NUMBER, detail(patch)));
    });
    let (page, vcx) = rig.open(cx);
    (rig, page, vcx)
}

type Send = fn(&mut PrDetailPage, &mut gpui::Window, &mut gpui::Context<PrDetailPage>);

/// Call the write; report what it left pending synchronously.
fn fire(
    page: &Entity<PrDetailPage>,
    vcx: &mut VisualTestContext,
    send: Send,
) -> Option<PendingKind> {
    vcx.update(|window, cx| {
        page.update(cx, |p, cx| {
            send(p, window, cx);
            p.store_pending(cx)
        })
    })
}

#[test]
fn the_payload_is_the_node_id_alone() {
    assert_eq!(
        state_input("PR_9"),
        json!({"input": {"pullRequestId": "PR_9"}})
    );
}

#[gpui::test]
fn ready_for_review_posts_once_on_a_draft(cx: &mut TestAppContext) {
    let (rig, page, vcx) = open(cx, json!({"isDraft": true, "viewerCanUpdate": true}));
    assert_eq!(
        fire(&page, vcx, PrDetailPage::ready_for_review_pr),
        Some(PendingKind::ReadyForReview)
    );
    vcx.run_until_parked();
    assert_eq!(rig.mutations(), 1);
    // The confirmation patches the draft flag and refetches.
    assert!(rig.detail_reads() >= 1);
}

#[gpui::test]
fn ready_for_review_is_refused_off_its_gate(cx: &mut TestAppContext) {
    for patch in [
        json!({"isDraft": false, "viewerCanUpdate": true}),
        json!({"isDraft": true, "viewerCanUpdate": false}),
        json!({"isDraft": true, "viewerCanUpdate": true, "state": "CLOSED"}),
    ] {
        let (rig, page, vcx) = open(cx, patch.clone());
        assert_eq!(
            fire(&page, vcx, PrDetailPage::ready_for_review_pr),
            None,
            "{patch}"
        );
        vcx.run_until_parked();
        assert_eq!(rig.mutations(), 0, "{patch}");
    }
}

#[gpui::test]
fn convert_to_draft_posts_once_on_an_open_pr(cx: &mut TestAppContext) {
    let (rig, page, vcx) = open(cx, json!({"viewerCanUpdate": true}));
    assert_eq!(
        fire(&page, vcx, PrDetailPage::convert_to_draft_pr),
        Some(PendingKind::ConvertToDraft)
    );
    vcx.run_until_parked();
    assert_eq!(rig.mutations(), 1);
}

#[gpui::test]
fn convert_to_draft_is_refused_off_its_gate(cx: &mut TestAppContext) {
    for patch in [
        json!({"isDraft": true, "viewerCanUpdate": true}),
        json!({"viewerCanUpdate": false}),
        json!({"viewerCanUpdate": true, "state": "MERGED"}),
    ] {
        let (rig, page, vcx) = open(cx, patch.clone());
        assert_eq!(
            fire(&page, vcx, PrDetailPage::convert_to_draft_pr),
            None,
            "{patch}"
        );
        vcx.run_until_parked();
        assert_eq!(rig.mutations(), 0, "{patch}");
    }
}

#[gpui::test]
fn reopen_posts_once_on_a_closed_pr_the_viewer_can_reopen(cx: &mut TestAppContext) {
    let (rig, page, vcx) = open(cx, json!({"state": "CLOSED", "viewerCanReopen": true}));
    assert_eq!(
        fire(&page, vcx, PrDetailPage::reopen_pr),
        Some(PendingKind::Reopen)
    );
    vcx.run_until_parked();
    assert_eq!(rig.mutations(), 1);
    // The confirmation patches the state, so the PR reads open again.
    let state = page.read_with(vcx, |p, cx| p.detail(cx).and_then(|d| d.state.clone()));
    assert_eq!(state.as_deref(), Some("OPEN"));
}

#[gpui::test]
fn reopen_is_refused_off_its_gate(cx: &mut TestAppContext) {
    for patch in [
        json!({"state": "CLOSED", "viewerCanReopen": false}),
        json!({"state": "OPEN", "viewerCanReopen": true}),
        json!({"state": "MERGED", "viewerCanReopen": true}),
    ] {
        let (rig, page, vcx) = open(cx, patch.clone());
        assert_eq!(fire(&page, vcx, PrDetailPage::reopen_pr), None, "{patch}");
        vcx.run_until_parked();
        assert_eq!(rig.mutations(), 0, "{patch}");
    }
}

#[gpui::test]
fn a_pending_write_blocks_the_next_one(cx: &mut TestAppContext) {
    let (rig, page, vcx) = open(cx, json!({"isDraft": true, "viewerCanUpdate": true}));
    fire(&page, vcx, PrDetailPage::ready_for_review_pr);
    // Still pending (nothing parked yet): a second click is ignored.
    assert_eq!(
        fire(&page, vcx, PrDetailPage::ready_for_review_pr),
        Some(PendingKind::ReadyForReview)
    );
    vcx.run_until_parked();
    assert_eq!(rig.mutations(), 1);
}
