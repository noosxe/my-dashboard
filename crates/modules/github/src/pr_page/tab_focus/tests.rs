//! #818: focus inside the old tab moves to the strip on a tab change.

use gpui::{Focusable, TestAppContext};

use super::super::test_rig::{Rig, server};
use super::super::{PrDetailPage, PrTab};

#[test]
fn only_a_changed_tab_settles() {
    assert!(!super::tab_changed(PrTab::Checks, PrTab::Checks));
    assert!(super::tab_changed(PrTab::Conversation, PrTab::Checks));
}

fn rig(cx: &mut TestAppContext) -> Rig {
    Rig::new(cx, server("aaa", "MERGEABLE"), vec![])
}

fn switch(page: &gpui::Entity<PrDetailPage>, vcx: &mut gpui::VisualTestContext, tab: PrTab) {
    vcx.update(|_, cx| {
        page.update(cx, |p, cx| {
            p.tab = tab;
            cx.notify();
        })
    });
    vcx.update(|window, cx| window.draw(cx).clear(cx));
}

#[gpui::test]
fn focus_in_the_old_tab_moves_off_the_content(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (page, vcx) = rig.open(cx);
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    let comment = vcx.update(|window, cx| {
        let comment = page.read(cx).comment.read(cx).focus_handle(cx);
        comment.focus(window, cx);
        comment
    });
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    switch(&page, vcx, PrTab::Checks);
    vcx.update(|window, cx| {
        let content = page.read(cx).content_focus.clone();
        assert!(!comment.is_focused(window), "the comment box lost focus");
        assert!(
            !content.contains_focused(window, cx),
            "focus left the old tab's content"
        );
        assert!(window.focused(cx).is_some(), "focus landed on the strip");
    });
}

#[gpui::test]
fn focus_outside_the_content_stays_put(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (page, vcx) = rig.open(cx);
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    let page_focus = vcx.update(|window, cx| {
        let handle = page.read(cx).focus_handle.clone();
        handle.focus(window, cx);
        handle
    });
    switch(&page, vcx, PrTab::Commits);
    vcx.update(|window, _| assert!(page_focus.is_focused(window)));
}
