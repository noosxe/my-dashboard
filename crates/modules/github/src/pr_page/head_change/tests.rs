//! #752: a head change outdates every lane that rides the old head, and
//! a line comment composed on the old patch is refused.

use gpui::TestAppContext;
use gpui_component::input::TextareaState;

use super::super::test_rig::{Rig, server};
use super::super::{LineComposer, PrDetailPage};
use crate::gql;
use crate::pr_page::{PendingKind, Surface, Trigger};
use crate::usage::Origin;
use gpui::Entity;

fn manual() -> Origin {
    Origin::new(Surface::PrPage, Trigger::Manual)
}

/// Load the Files and Threads lanes the way opening the tab does.
fn open_lanes(rig: &Rig, vcx: &mut gpui::VisualTestContext, page: &Entity<PrDetailPage>) {
    page.update(vcx, |p, cx| {
        p.fetch_files(manual(), cx);
        p.fetch_threads(manual(), cx);
    });
    vcx.run_until_parked();
    assert_eq!((rig.files_reads(), rig.threads_reads()), (1, 1));
}

#[gpui::test]
fn a_new_head_refetches_files_and_threads_once(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        server("aaa", "MERGEABLE"),
        vec![server("bbb", "MERGEABLE")],
    );
    let (page, vcx) = rig.open(cx);
    open_lanes(&rig, vcx, &page);

    page.update(vcx, |p, cx| p.fetch_detail(manual(), cx));
    vcx.run_until_parked();
    assert_eq!(rig.head(vcx).as_deref(), Some("bbb"));
    assert_eq!(rig.detail_reads(), 1);
    assert_eq!(rig.files_reads(), 2, "the file list follows the new head");
    assert_eq!(rig.threads_reads(), 2, "so do the threads");

    // The same head again is no new change.
    page.update(vcx, |p, cx| p.fetch_detail(manual(), cx));
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 2);
    assert_eq!((rig.files_reads(), rig.threads_reads()), (2, 2));
}

#[gpui::test]
fn update_branch_confirmation_fans_out(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        server("aaa", "MERGEABLE"),
        vec![server("bbb", "MERGEABLE")],
    );
    let (page, vcx) = rig.open(cx);
    open_lanes(&rig, vcx, &page);

    vcx.update(|_, cx| {
        rig.state.update(cx, |s, _| {
            let token = s
                .begin_pending(
                    super::super::test_rig::REPO,
                    super::super::test_rig::NUMBER,
                    PendingKind::UpdateBranch,
                )
                .unwrap();
            assert!(s.resolve_pending(
                super::super::test_rig::REPO,
                super::super::test_rig::NUMBER,
                token
            ));
        })
    });
    page.update_in(vcx, |p, window, cx| {
        p.revalidate_after_store_mutation(window, cx)
    });
    vcx.run_until_parked();
    assert_eq!((rig.files_reads(), rig.threads_reads()), (2, 2));
}

#[gpui::test]
fn lanes_nobody_opened_stay_untouched(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        server("aaa", "MERGEABLE"),
        vec![server("bbb", "MERGEABLE")],
    );
    let (page, vcx) = rig.open(cx);
    page.update(vcx, |p, cx| p.fetch_detail(manual(), cx));
    vcx.run_until_parked();
    assert_eq!(rig.head(vcx).as_deref(), Some("bbb"));
    assert_eq!((rig.files_reads(), rig.threads_reads()), (0, 0));
}

/// The comment API takes no commit id, so the line lands on whatever the
/// head is by then. The page refuses instead of guessing.
#[gpui::test]
fn a_line_comment_composed_on_the_old_patch_is_refused(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        server("aaa", "MERGEABLE"),
        vec![server("bbb", "MERGEABLE")],
    );
    let (page, vcx) = rig.open(cx);
    vcx.update(|window, cx| {
        page.update(cx, |p, cx| {
            p.open_line_composer("a.rs".into(), gql::DiffSide::Right, 3, window, cx);
            let editor = p.line_composer.as_ref().unwrap().editor.clone();
            editor.update(cx, |st, cx| st.set_value("note", window, cx));
        })
    });
    page.update(vcx, |p, cx| p.fetch_detail(manual(), cx));
    vcx.run_until_parked();

    vcx.update(|window, cx| page.update(cx, |p, cx| p.add_single_comment(window, cx)));
    vcx.run_until_parked();
    assert_eq!(rig.mutations(), 0, "nothing was posted");
    vcx.read(|cx| {
        let p = page.read(cx);
        assert!(p.line_composer.is_some(), "the text is kept");
        assert!(
            p.mutation_error
                .as_ref()
                .is_some_and(|e| e.message.contains("branch"))
        );
    });

    // A composer armed on the new head posts.
    vcx.update(|window, cx| {
        page.update(cx, |p, cx| {
            p.cancel_line_composer(window, cx);
            p.open_line_composer("a.rs".into(), gql::DiffSide::Right, 3, window, cx);
            let editor = p.line_composer.as_ref().unwrap().editor.clone();
            editor.update(cx, |st, cx| st.set_value("note", window, cx));
            p.add_single_comment(window, cx);
        })
    });
    vcx.run_until_parked();
    assert_eq!(rig.mutations(), 1);
    let _ = (TextareaState::new, std::mem::size_of::<LineComposer>());
}

/// A files read already flying predates the push: it must not land, and
/// exactly one replacement goes out (the #739 rule, for the files lane).
#[gpui::test]
fn a_files_read_flying_across_a_head_change_is_replaced_once(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        server("aaa", "MERGEABLE"),
        vec![server("aaa", "MERGEABLE")],
    );
    let (page, vcx) = rig.open(cx);
    open_lanes(&rig, vcx, &page);
    page.update(vcx, |p, cx| {
        p.fetch_files(manual(), cx);
        // The head-change refetch finds the guard taken.
        p.fetch_files(Origin::new(Surface::PrPage, Trigger::MutationRefetch), cx);
    });
    vcx.run_until_parked();
    assert_eq!(
        rig.files_reads(),
        3,
        "open, the flying read, one replacement"
    );
    vcx.run_until_parked();
    assert_eq!(rig.files_reads(), 3, "settled");
}
