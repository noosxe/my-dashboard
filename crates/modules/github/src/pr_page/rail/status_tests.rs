//! Render-level tests of the status block (docs/0083 D1–D6): what the
//! rail shows for a PR state. The pure rules (verdict, gates, actions,
//! sub-lines) are table-tested next to their code.

use gpui::{Entity, TestAppContext, VisualTestContext};
use serde_json::{Value, json};

use super::test_support::detail;
use crate::pr_page::test_rig::{NUMBER, REPO, Rig, server};
use crate::pr_page::{CommentAnchor, PrDetailPage};

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

fn shown(vcx: &mut VisualTestContext, key: &'static str) -> bool {
    vcx.debug_bounds(key).is_some()
}

#[gpui::test]
fn a_blocked_open_pr_shows_verdict_gates_and_actions(cx: &mut TestAppContext) {
    let (_rig, _page, vcx) = open(
        cx,
        json!({"mergeable": "MERGEABLE", "mergeStateStatus": "BLOCKED", "viewerCanClose": true}),
    );
    for key in [
        "pr-rail-status",
        "pr-rail-verdict",
        "pr-rail-gate-reviews",
        "pr-rail-gate-checks",
        "pr-rail-gate-merge",
        "pr-merge",
        "pr-more",
    ] {
        assert!(shown(vcx, key), "{key}");
    }
    // The old panel's pieces are gone; Close waits in the closed menu.
    for key in [
        "pr-merge-panel",
        "pr-checks-summary",
        "pr-checks-summary-label",
        "pr-view-checks",
        "pr-close",
    ] {
        assert!(!shown(vcx, key), "{key}");
    }
}

#[gpui::test]
fn the_status_block_sits_above_the_scrolling_metadata(cx: &mut TestAppContext) {
    let (_rig, _page, vcx) = open(
        cx,
        json!({"mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN"}),
    );
    let status = vcx.debug_bounds("pr-rail-status").expect("status");
    let meta = vcx.debug_bounds("pr-rail-meta").expect("metadata");
    assert!(status.origin.y < meta.origin.y);
    let verdict = vcx.debug_bounds("pr-rail-verdict").expect("verdict");
    let merge = vcx.debug_bounds("pr-merge").expect("merge");
    assert!(verdict.origin.y < merge.origin.y, "verdict leads the block");
}

#[gpui::test]
fn a_merged_pr_has_history_and_no_controls(cx: &mut TestAppContext) {
    let (_rig, _page, vcx) = open(
        cx,
        json!({"state": "MERGED", "merged": true, "mergedBy": {"login": "maint"},
            "baseRefName": "main", "mergedAt": "2026-10-20T00:00:00Z"}),
    );
    assert!(shown(vcx, "pr-rail-verdict"));
    assert!(shown(vcx, "pr-rail-verdict-sub"));
    assert!(shown(vcx, "pr-rail-gate-reviews"));
    assert!(shown(vcx, "pr-rail-gate-checks"));
    assert!(!shown(vcx, "pr-rail-gate-merge"), "mergeability is hidden");
    assert!(!shown(vcx, "pr-merge"));
    assert!(!shown(vcx, "pr-more"), "no menu on a merged PR");
}

#[gpui::test]
fn a_closed_pr_shows_reviews_and_no_checks_or_menu(cx: &mut TestAppContext) {
    let (_rig, _page, vcx) = open(cx, json!({"state": "CLOSED", "viewerCanClose": true}));
    assert!(shown(vcx, "pr-rail-verdict-sub"));
    assert!(shown(vcx, "pr-rail-gate-reviews"));
    assert!(!shown(vcx, "pr-rail-gate-checks"));
    assert!(!shown(vcx, "pr-rail-gate-merge"), "no conflicts to show");
    assert!(!shown(vcx, "pr-more"));
}

#[gpui::test]
fn a_closed_pr_with_conflicts_keeps_the_conflicts_row(cx: &mut TestAppContext) {
    let (_rig, _page, vcx) = open(
        cx,
        json!({"state": "CLOSED", "mergeable": "CONFLICTING", "mergeStateStatus": "DIRTY"}),
    );
    assert!(shown(vcx, "pr-rail-gate-merge"));
    assert!(shown(vcx, "pr-rail-resolve"));
}

#[gpui::test]
fn a_reader_gets_the_verdict_and_gates_but_no_action_row(cx: &mut TestAppContext) {
    let (_rig, _page, vcx) = open(
        cx,
        json!({"viewerPermission": "READ", "viewerCanClose": false, "viewerCanUpdate": false}),
    );
    assert!(shown(vcx, "pr-rail-verdict"));
    assert!(shown(vcx, "pr-rail-gate-reviews"));
    assert!(!shown(vcx, "pr-merge"));
    assert!(!shown(vcx, "pr-more"));
}

#[gpui::test]
fn update_lives_in_the_mergeability_row(cx: &mut TestAppContext) {
    let (_rig, _page, vcx) = open(
        cx,
        json!({"id": "PR_1", "mergeable": "MERGEABLE", "mergeStateStatus": "BEHIND",
            "viewerCanUpdateBranch": true, "headRefOid": "abc123"}),
    );
    assert!(shown(vcx, "pr-update-branch"));
    let row = vcx.debug_bounds("pr-rail-gate-merge").expect("row");
    let button = vcx.debug_bounds("pr-update-branch").expect("button");
    assert!(
        button.origin.y >= row.origin.y && button.origin.y < row.origin.y + row.size.height,
        "the button sits on the row"
    );
}

#[gpui::test]
fn the_reviews_row_arms_the_timeline_jump(cx: &mut TestAppContext) {
    let node = |id: &str, state: &str| json!({"id": id, "state": state, "author": {"login": "b", "avatarUrl": "a"}});
    let (_rig, page, vcx) = open(
        cx,
        json!({
            "reviewDecision": "CHANGES_REQUESTED",
            "latestOpinionatedReviews": {"nodes": [node("PRR_2", "CHANGES_REQUESTED")]},
            "reviews": {"totalCount": 1, "nodes": [
                {"id": "PRR_2", "state": "CHANGES_REQUESTED", "author": {"login": "b"}}]},
        }),
    );
    assert!(shown(vcx, "pr-rail-gate-reviews"));
    // A review outside the loaded window gives up quietly.
    vcx.update(|_, cx| page.update(cx, |p, cx| p.jump_to_review("PRR_99", cx)));
    page.read_with(vcx, |p, _| assert_eq!(p.anchor, CommentAnchor::Idle));
    vcx.update(|_, cx| page.update(cx, |p, cx| p.jump_to_review("PRR_2", cx)));
    // The next frame mounts the card and the jump fires: the ring on it
    // is the Highlighted state.
    vcx.run_until_parked();
    page.read_with(vcx, |p, _| {
        assert!(
            matches!(&p.anchor, CommentAnchor::Jumping(id) | CommentAnchor::Highlighted(id) if id == "PRR_2"),
            "{:?}",
            p.anchor
        )
    });
}
