//! #841: Disconnect asks before it drops unsent review drafts; an auth
//! loss drops them and says so.

use gpui::{Entity, TestAppContext, VisualTestContext};

use super::{discarded_copy, disconnect_prompt};
use crate::page::GithubPage;
use crate::page::shell_leave::tests::open_pr_page;
use crate::service::AuthStatus;

fn seed(page: &Entity<GithubPage>, vcx: &mut VisualTestContext, n: usize) {
    let pr = vcx.update(|_, cx| page.read(cx).pr.clone().expect("pr page"));
    pr.update(vcx, |p, _| {
        (0..n).for_each(|i| p.seed_review_draft(&format!("d{i}")))
    });
}

fn toasts(vcx: &mut VisualTestContext) -> Vec<String> {
    vcx.update(|_, cx| {
        dashboard_core::toasts::hub(cx)
            .read(cx)
            .log()
            .iter()
            .map(|e| e.toast.message.to_string())
            .collect()
    })
}

fn auth(page: &Entity<GithubPage>, vcx: &mut VisualTestContext) -> AuthStatus {
    vcx.update(|_, cx| page.read(cx).state.read(cx).auth.clone())
}

fn lose_auth(page: &Entity<GithubPage>, vcx: &mut VisualTestContext) {
    vcx.update(|window, cx| {
        let state = page.read(cx).state.clone();
        state.update(cx, |s, _| s.auth = AuthStatus::TokenRejected);
        page.update(cx, |p, cx| p.sync_from_state_in(window, cx));
    });
}

#[test]
fn copy_is_pluralised() {
    assert_eq!(discarded_copy(1), "1 unsent review draft discarded");
    assert_eq!(discarded_copy(3), "3 unsent review drafts discarded");
    assert_eq!(
        disconnect_prompt(1),
        "Disconnecting discards 1 unsent review draft."
    );
}

#[gpui::test]
fn disconnect_without_drafts_signs_out_at_once(cx: &mut TestAppContext) {
    let (page, vcx) = open_pr_page(cx);
    vcx.update(|window, cx| page.update(cx, |p, cx| p.request_disconnect(window, cx)));
    assert_eq!(auth(&page, vcx), AuthStatus::Disconnected);
}

#[gpui::test]
fn disconnect_with_drafts_asks_first(cx: &mut TestAppContext) {
    let (page, vcx) = open_pr_page(cx);
    seed(&page, vcx, 2);
    vcx.update(|window, cx| page.update(cx, |p, cx| p.request_disconnect(window, cx)));
    assert_eq!(auth(&page, vcx), AuthStatus::Connected, "still signed in");
    assert_eq!(
        vcx.update(|_, cx| page.read(cx).pending_disconnect_drafts(cx)),
        Some(2)
    );

    vcx.update(|window, cx| page.update(cx, |p, cx| p.cancel_disconnect(window, cx)));
    assert_eq!(auth(&page, vcx), AuthStatus::Connected);
    assert!(
        vcx.update(|_, cx| page.read(cx).pr.is_some()),
        "drafts kept"
    );
    assert_eq!(
        vcx.update(|_, cx| page.read(cx).pending_disconnect_drafts(cx)),
        None
    );
}

#[gpui::test]
fn a_confirmed_disconnect_is_quiet(cx: &mut TestAppContext) {
    let (page, vcx) = open_pr_page(cx);
    seed(&page, vcx, 1);
    vcx.update(|window, cx| page.update(cx, |p, cx| p.request_disconnect(window, cx)));
    vcx.update(|window, cx| page.update(cx, |p, cx| p.confirm_disconnect(window, cx)));
    assert_eq!(auth(&page, vcx), AuthStatus::Disconnected);
    assert!(vcx.update(|_, cx| page.read(cx).pr.is_none()));
    vcx.update(|window, cx| page.update(cx, |p, cx| p.sync_from_state_in(window, cx)));
    assert!(toasts(vcx).is_empty(), "the user already agreed");
}

#[gpui::test]
fn losing_auth_discards_the_drafts_and_says_so(cx: &mut TestAppContext) {
    let (page, vcx) = open_pr_page(cx);
    seed(&page, vcx, 1);
    lose_auth(&page, vcx);
    assert!(vcx.update(|_, cx| page.read(cx).pr.is_none()));
    assert_eq!(toasts(vcx), vec!["1 unsent review draft discarded"]);
}

#[gpui::test]
fn losing_auth_without_drafts_stays_silent(cx: &mut TestAppContext) {
    let (page, vcx) = open_pr_page(cx);
    lose_auth(&page, vcx);
    assert!(toasts(vcx).is_empty());
}
