//! Each auto-merge and queue write's gate, payload and confirmation
//! patch (docs/0083 D4).

use gpui::{Entity, TestAppContext, VisualTestContext};
use serde_json::{Value, json};

use super::{dequeue_input, enable_auto_merge_input, pull_request_input};
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

/// What the store's confirmation leaves on the cached document before the
/// refetch lands: (auto-merge on, queued, position).
fn confirm(cx: &mut TestAppContext, patch: Value, kind: PendingKind) -> (bool, bool, Option<i64>) {
    let (rig, _page, _vcx) = open(cx, patch);
    cx.update(|cx| {
        rig.state.update(cx, |s, _| {
            let token = s.begin_pending(REPO, NUMBER, kind).expect("armed");
            assert!(s.resolve_pending(REPO, NUMBER, token));
            let q = &s
                .pr_entry(REPO, NUMBER)
                .and_then(|e| e.detail.as_ref())
                .expect("detail")
                .queueing;
            (q.auto_merge.is_some(), q.in_merge_queue, q.queue_position)
        })
    })
}

#[gpui::test]
fn confirmations_patch_what_the_write_is_sure_of(cx: &mut TestAppContext) {
    assert_eq!(
        confirm(cx, blocked(), PendingKind::EnableAutoMerge),
        (true, false, None)
    );
    let on = json!({
        "autoMergeRequest": {"enabledBy": {"login": "dev"}, "mergeMethod": "SQUASH"},
    });
    assert_eq!(
        confirm(cx, on, PendingKind::DisableAutoMerge),
        (false, false, None)
    );
    assert_eq!(
        confirm(cx, queue_base("CLEAN"), PendingKind::Enqueue),
        (false, true, None)
    );
    let queued = json!({
        "isInMergeQueue": true, "mergeQueueEntry": {"position": 2, "state": "QUEUED"},
    });
    assert_eq!(
        confirm(cx, queued, PendingKind::Dequeue),
        (false, false, None)
    );
}

#[test]
fn payloads() {
    // The method rides along off a queue, and only then.
    assert_eq!(
        enable_auto_merge_input("PR_9", Some("SQUASH")),
        json!({"input": {"pullRequestId": "PR_9", "mergeMethod": "SQUASH"}})
    );
    assert_eq!(
        enable_auto_merge_input("PR_9", None),
        json!({"input": {"pullRequestId": "PR_9"}})
    );
    // Disable and enqueue name the pull request `pullRequestId`; dequeue
    // names it `id`.
    assert_eq!(
        pull_request_input("PR_9"),
        json!({"input": {"pullRequestId": "PR_9"}})
    );
    assert_eq!(dequeue_input("PR_9"), json!({"input": {"id": "PR_9"}}));
}

fn blocked() -> Value {
    json!({
        "mergeable": "MERGEABLE", "mergeStateStatus": "BLOCKED",
        "viewerCanEnableAutoMerge": true, "autoMergeAllowed": true,
    })
}

#[gpui::test]
fn enable_auto_merge_posts_once_and_patches_it_on(cx: &mut TestAppContext) {
    let (rig, page, vcx) = open(cx, blocked());
    assert_eq!(
        fire(&page, vcx, PrDetailPage::enable_auto_merge_pr),
        Some(PendingKind::EnableAutoMerge)
    );
    vcx.run_until_parked();
    assert_eq!(rig.mutations(), 1);
    assert!(rig.detail_reads() >= 1, "the refetch flies");
}

#[gpui::test]
fn enable_auto_merge_is_refused_off_its_gate(cx: &mut TestAppContext) {
    for (key, value) in [
        ("autoMergeAllowed", json!(false)),
        ("viewerCanEnableAutoMerge", json!(false)),
        ("isDraft", json!(true)),
        ("state", json!("CLOSED")),
        ("isInMergeQueue", json!(true)),
    ] {
        let mut patch = blocked();
        patch[key] = value;
        let (rig, page, vcx) = open(cx, patch);
        assert_eq!(
            fire(&page, vcx, PrDetailPage::enable_auto_merge_pr),
            None,
            "{key}"
        );
        vcx.run_until_parked();
        assert_eq!(rig.mutations(), 0, "{key}");
    }
}

#[gpui::test]
fn disable_auto_merge_posts_once_and_patches_it_off(cx: &mut TestAppContext) {
    let on = json!({
        "autoMergeRequest": {"enabledBy": {"login": "dev"}, "mergeMethod": "SQUASH"},
        "viewerCanDisableAutoMerge": true,
    });
    let (rig, page, vcx) = open(cx, on);
    assert_eq!(
        fire(&page, vcx, PrDetailPage::disable_auto_merge_pr),
        Some(PendingKind::DisableAutoMerge)
    );
    vcx.run_until_parked();
    assert_eq!(rig.mutations(), 1);
}

#[gpui::test]
fn disable_auto_merge_is_refused_off_its_gate(cx: &mut TestAppContext) {
    for patch in [
        json!({"viewerCanDisableAutoMerge": true}),
        json!({
            "autoMergeRequest": {"enabledBy": null, "mergeMethod": "MERGE"},
            "viewerCanDisableAutoMerge": false,
        }),
    ] {
        let (rig, page, vcx) = open(cx, patch.clone());
        assert_eq!(
            fire(&page, vcx, PrDetailPage::disable_auto_merge_pr),
            None,
            "{patch}"
        );
        vcx.run_until_parked();
        assert_eq!(rig.mutations(), 0, "{patch}");
    }
}

fn queue_base(status: &str) -> Value {
    json!({
        "mergeable": "MERGEABLE", "mergeStateStatus": status,
        "isMergeQueueEnabled": true,
    })
}

#[gpui::test]
fn merge_when_ready_posts_once_and_patches_it_queued(cx: &mut TestAppContext) {
    let (rig, page, vcx) = open(cx, queue_base("CLEAN"));
    assert_eq!(
        fire(&page, vcx, PrDetailPage::enqueue_pr),
        Some(PendingKind::Enqueue)
    );
    vcx.run_until_parked();
    assert_eq!(rig.mutations(), 1);
}

#[gpui::test]
fn merge_when_ready_is_refused_unless_the_merge_gate_passes(cx: &mut TestAppContext) {
    for patch in [
        queue_base("BLOCKED"),
        queue_base("BEHIND"),
        queue_base("DIRTY"),
        queue_base("UNKNOWN"),
        json!({"mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN"}),
        {
            let mut p = queue_base("CLEAN");
            p["isInMergeQueue"] = json!(true);
            p
        },
        {
            let mut p = queue_base("CLEAN");
            p["isDraft"] = json!(true);
            p
        },
    ] {
        let (rig, page, vcx) = open(cx, patch.clone());
        assert_eq!(fire(&page, vcx, PrDetailPage::enqueue_pr), None, "{patch}");
        vcx.run_until_parked();
        assert_eq!(rig.mutations(), 0, "{patch}");
    }
}

#[gpui::test]
fn remove_from_queue_posts_once_and_patches_it_out(cx: &mut TestAppContext) {
    let (rig, page, vcx) = open(
        cx,
        json!({
            "isMergeQueueEnabled": true, "isInMergeQueue": true,
            "mergeQueueEntry": {"position": 2, "state": "QUEUED"},
        }),
    );
    assert_eq!(
        fire(&page, vcx, PrDetailPage::dequeue_pr),
        Some(PendingKind::Dequeue)
    );
    vcx.run_until_parked();
    assert_eq!(rig.mutations(), 1);
}

#[gpui::test]
fn remove_from_queue_is_refused_off_its_gate(cx: &mut TestAppContext) {
    for patch in [
        json!({"isMergeQueueEnabled": true}),
        json!({"isInMergeQueue": true, "viewerPermission": "READ"}),
    ] {
        let (rig, page, vcx) = open(cx, patch.clone());
        assert_eq!(fire(&page, vcx, PrDetailPage::dequeue_pr), None, "{patch}");
        vcx.run_until_parked();
        assert_eq!(rig.mutations(), 0, "{patch}");
    }
}

#[gpui::test]
fn a_pending_write_blocks_the_next_one(cx: &mut TestAppContext) {
    let (rig, page, vcx) = open(cx, blocked());
    fire(&page, vcx, PrDetailPage::enable_auto_merge_pr);
    assert_eq!(
        fire(&page, vcx, PrDetailPage::enable_auto_merge_pr),
        Some(PendingKind::EnableAutoMerge)
    );
    vcx.run_until_parked();
    assert_eq!(rig.mutations(), 1);
}
