//! Render-level rail tests: what the page shows for a given detail, and
//! the picker's no-op Save. The pure rules sit next to their code.

use gpui::{Entity, TestAppContext, VisualTestContext};
use serde_json::{Value, json};

use super::test_support::detail;
use crate::pr_page::test_rig::{NUMBER, REPO, Rig, server};
use crate::pr_page::{BusyOp, PrDetailPage};

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
    // `open` hands back a context borrowed from `cx`; the rig outlives it.
    (rig, page, vcx)
}

fn posts(rig: &Rig) -> usize {
    rig.http.mutations.load(std::sync::atomic::Ordering::SeqCst)
}

fn shown(vcx: &mut VisualTestContext, key: &'static str) -> bool {
    vcx.debug_bounds(key).is_some()
}

fn many_labels(n: usize, total: usize) -> Value {
    let nodes: Vec<_> = (0..n)
        .map(|i| json!({"id": format!("L{i}"), "name": format!("label-{i}"), "color": "1d76db"}))
        .collect();
    json!({"labels": {"totalCount": total, "nodes": nodes}})
}

#[gpui::test]
fn sections_render_in_d7_order(cx: &mut TestAppContext) {
    let (_rig, _page, vcx) = open(cx, json!({}));
    let mut ys = Vec::new();
    for key in [
        "pr-reviewers-panel",
        "pr-assignees-panel",
        "pr-labels-panel",
        "pr-milestone-panel",
    ] {
        ys.push(vcx.debug_bounds(key).expect(key).origin.y);
    }
    assert!(ys.windows(2).all(|w| w[0] < w[1]), "{ys:?}");
    assert!(shown(vcx, "pr-rail-meta"));
}

#[gpui::test]
fn edit_is_hidden_on_merged_and_closed_prs(cx: &mut TestAppContext) {
    for state in ["MERGED", "CLOSED"] {
        let (_rig, _page, vcx) = open(cx, json!({"state": state}));
        for key in [
            "pr-reviewers-edit",
            "pr-assignees-edit",
            "pr-labels-edit",
            "pr-milestones-edit",
        ] {
            assert!(!shown(vcx, key), "{key} on a {state} PR");
        }
        // The sections with something to show still render.
        assert!(shown(vcx, "pr-labels-panel"), "{state}");
    }
}

#[gpui::test]
fn open_prs_show_every_edit(cx: &mut TestAppContext) {
    let (_rig, _page, vcx) = open(cx, json!({}));
    for key in [
        "pr-reviewers-edit",
        "pr-assignees-edit",
        "pr-labels-edit",
        "pr-milestones-edit",
    ] {
        assert!(shown(vcx, key), "{key}");
    }
}

#[gpui::test]
fn edit_hides_while_a_write_is_pending(cx: &mut TestAppContext) {
    let (_rig, page, vcx) = open(cx, json!({}));
    vcx.update(|_, cx| {
        page.update(cx, |p, cx| {
            p.busy = Some(BusyOp::PrSetLabels);
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert!(!shown(vcx, "pr-labels-edit"));
    assert!(!shown(vcx, "pr-milestones-edit"));
    vcx.update(|_, cx| {
        page.update(cx, |p, cx| {
            p.busy = None;
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert!(shown(vcx, "pr-labels-edit"));
}

#[gpui::test]
fn empty_milestone_stays_on_open_prs_but_not_on_merged_ones(cx: &mut TestAppContext) {
    let (_rig, _page, vcx) = open(cx, json!({"milestone": null, "viewerPermission": "READ"}));
    assert!(shown(vcx, "pr-milestone-panel"), "open, empty, no rights");
    assert!(!shown(vcx, "pr-milestones-edit"));

    let (_rig, _page, vcx) = open(cx, json!({"milestone": null, "state": "MERGED"}));
    assert!(
        !shown(vcx, "pr-milestone-panel"),
        "merged and empty is left out"
    );
}

#[gpui::test]
fn truncated_labels_show_the_reason_instead_of_edit(cx: &mut TestAppContext) {
    let (_rig, _page, vcx) = open(cx, many_labels(20, 34));
    assert!(!shown(vcx, "pr-labels-edit"));
    assert!(shown(vcx, "pr-labels-why"));
    // Expanded or not, the shortfall is a github.com row.
    assert!(shown(vcx, "pr-labels-more"));
}

#[gpui::test]
fn labels_cap_at_eight_until_expanded(cx: &mut TestAppContext) {
    let (_rig, page, vcx) = open(cx, many_labels(11, 11));
    assert!(shown(vcx, "pr-labels-more"));
    assert!(!shown(vcx, "pr-labels-cap"));
    vcx.update(|_, cx| {
        page.update(cx, |p, cx| {
            p.rail_expanded.labels = true;
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert!(!shown(vcx, "pr-labels-more"));
}

#[gpui::test]
fn expanded_labels_end_with_the_github_row(cx: &mut TestAppContext) {
    let (_rig, page, vcx) = open(cx, many_labels(20, 34));
    vcx.update(|_, cx| {
        page.update(cx, |p, cx| {
            p.rail_expanded.labels = true;
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert!(shown(vcx, "pr-labels-cap"));
    assert!(!shown(vcx, "pr-labels-more"));
}

#[gpui::test]
fn team_request_shows_its_reason_line(cx: &mut TestAppContext) {
    let (_rig, _page, vcx) = open(
        cx,
        json!({"reviewRequests": {"totalCount": 1, "nodes": [
            {"requestedReviewer": {"__typename": "Team", "id": "T1", "name": "core"}}
        ]}}),
    );
    assert!(!shown(vcx, "pr-reviewers-edit"));
    assert!(shown(vcx, "pr-reviewers-why"));
}

#[gpui::test]
fn save_with_nothing_changed_posts_nothing(cx: &mut TestAppContext) {
    let (rig, page, vcx) = open(cx, json!({}));
    for (op, card) in [
        (BusyOp::PrSetLabels, "pr-labels-card"),
        (BusyOp::PrSetAssignees, "pr-assignees-card"),
        (BusyOp::PrSetReviewers, "pr-reviewers-card"),
        (BusyOp::PrSetMilestone, "pr-milestones-card"),
    ] {
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| match op {
                BusyOp::PrSetLabels => p.start_labels_picker(window, cx),
                BusyOp::PrSetAssignees => p.start_assignees_picker(window, cx),
                BusyOp::PrSetReviewers => p.start_reviewers_picker(window, cx),
                _ => p.start_milestone_picker(window, cx),
            })
        });
        vcx.run_until_parked();
        assert!(shown(vcx, card), "{card} opens");
        // The picker's source read lands first: it shares the transport's
        // "everything else" counter with writes.
        let before = posts(&rig);
        vcx.update(|window, cx| page.update(cx, |p, cx| p.save_meta(op, window, cx)));
        vcx.run_until_parked();
        assert!(!shown(vcx, card), "{card} closes on an unchanged save");
        assert_eq!(posts(&rig), before, "{card} posted a write");
        page.read_with(vcx, |p, _| assert!(p.busy.is_none()));
    }
}

#[gpui::test]
fn save_with_a_change_posts_one_write(cx: &mut TestAppContext) {
    let (rig, page, vcx) = open(cx, json!({}));
    vcx.update(|window, cx| page.update(cx, |p, cx| p.start_labels_picker(window, cx)));
    vcx.run_until_parked();
    let before = posts(&rig);
    vcx.update(|window, cx| {
        page.update(cx, |p, cx| {
            // The current label is row 0 (applied rows lead the list
            // while the source is still loading): uncheck it.
            p.toggle_meta_row(0, cx);
            p.save_meta(BusyOp::PrSetLabels, window, cx);
        })
    });
    vcx.run_until_parked();
    assert_eq!(posts(&rig), before + 1);
}
