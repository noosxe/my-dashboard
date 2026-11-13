//! #744: Esc closes the topmost open surface before the page, and a
//! page holding unsent review drafts asks before it goes.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use gpui_component::input::TextareaState;

use super::LeaveIntent;
use crate::drafts::DraftStore;
use crate::gql;
use crate::http::{Http, HttpError, HttpResult, Verb};
use crate::pr_page::{GateReview, LineDraft, PrDetailPage, PrPageEvent};
use crate::service::{AuthStatus, GithubCache, GithubState};

struct NoNetwork;

impl Http for NoNetwork {
    fn request(
        &self,
        _: Verb,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<HttpResult, HttpError> {
        Err(HttpError("no network in this test".into()))
    }
    fn post_graphql(&self, _: &str, _: &serde_json::Value) -> Result<HttpResult, HttpError> {
        Err(HttpError("no network in this test".into()))
    }
}

struct NoStore;

impl crate::auth::TokenStore for NoStore {
    fn save(&self, _: &crate::auth::StoredCredential) -> Result<(), String> {
        Ok(())
    }
    fn load(&self) -> Result<Option<crate::auth::StoredCredential>, String> {
        Ok(None)
    }
    fn delete(&self) -> Result<(), String> {
        Ok(())
    }
}

fn draft(body: &str) -> LineDraft {
    LineDraft {
        path: "src/lib.rs".into(),
        side: gql::DiffSide::Right,
        line: 3,
        start: None,
        body: body.into(),
    }
}

/// A page plus the log of the leave requests it emits.
fn open(
    cx: &mut TestAppContext,
) -> (
    Entity<PrDetailPage>,
    &mut VisualTestContext,
    Rc<RefCell<Vec<LeaveIntent>>>,
) {
    cx.update(dashboard_core::status::init);
    cx.update(gpui_component::init);
    cx.update(|cx| cx.background_executor().allow_parking());
    let state = cx.update(|cx| {
        cx.new(|_| {
            let mut s = GithubState::new(
                Arc::new(NoNetwork),
                Arc::new(NoStore),
                Default::default(),
                Default::default(),
                GithubCache::new(dashboard_core::paths::AppPaths::init()),
                DraftStore::at_tmp("pr-leave-guard"),
            );
            s.auth = AuthStatus::Connected;
            s
        })
    });
    let (page, vcx) = cx
        .add_window_view(|window, cx| PrDetailPage::new(state, "o/r".into(), 7, None, window, cx));
    let log = Rc::new(RefCell::new(Vec::new()));
    let sink = log.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&page, move |_, event: &PrPageEvent, _| {
            let PrPageEvent::Leave(intent) = event;
            sink.borrow_mut().push(*intent);
        })
        .detach();
    });
    (page, vcx, log)
}

fn esc(vcx: &mut VisualTestContext, page: &Entity<PrDetailPage>) {
    vcx.update(|window, cx| {
        page.update(cx, |p, cx| {
            if !p.cancel_top_editor(window, cx) {
                p.esc_leave(window, cx);
            }
        })
    });
}

#[gpui::test]
fn esc_closes_the_rebase_confirm_before_the_page(cx: &mut TestAppContext) {
    let (page, vcx, log) = open(cx);
    page.update(vcx, |p, _| p.update_rebase_confirm = true);
    esc(vcx, &page);
    page.update(vcx, |p, _| assert!(!p.update_rebase_confirm));
    assert!(log.borrow().is_empty(), "the page stays open");
    esc(vcx, &page);
    assert_eq!(*log.borrow(), [LeaveIntent::Close]);
}

#[gpui::test]
fn esc_closes_the_gate_review_before_the_page(cx: &mut TestAppContext) {
    let (page, vcx, log) = open(cx);
    vcx.update(|window, cx| {
        page.update(cx, |p, cx| {
            let comment = cx.new(|cx| TextareaState::new(window, cx));
            p.gate_review = Some(GateReview {
                run_id: 1,
                environments: Vec::new(),
                approvals: None,
                comment,
            });
        })
    });
    esc(vcx, &page);
    page.update(vcx, |p, _| assert!(p.gate_review.is_none()));
    assert!(log.borrow().is_empty(), "the page stays open");
    esc(vcx, &page);
    assert_eq!(*log.borrow(), [LeaveIntent::Close]);
}

#[gpui::test]
fn esc_closes_the_resolve_picker_before_the_page(cx: &mut TestAppContext) {
    let (page, vcx, log) = open(cx);
    page.update(vcx, |p, _| p.resolve_picker = Some("T_0".into()));
    esc(vcx, &page);
    page.update(vcx, |p, _| assert!(p.resolve_picker.is_none()));
    assert!(log.borrow().is_empty(), "the page stays open");
    esc(vcx, &page);
    assert_eq!(*log.borrow(), [LeaveIntent::Close]);
}

#[gpui::test]
fn drafts_turn_esc_into_a_discard_confirm(cx: &mut TestAppContext) {
    let (page, vcx, log) = open(cx);
    page.update(vcx, |p, _| p.drafts = vec![draft("a"), draft("b")]);
    esc(vcx, &page);
    assert!(log.borrow().is_empty(), "nothing is dropped unasked");
    assert!(vcx.debug_bounds("pr-discard-confirm").is_some());
    page.update(vcx, |p, _| assert_eq!(p.drafts.len(), 2));
    // Discard leaves.
    vcx.update(|_, cx| page.update(cx, |p, cx| p.confirm_discard(cx)));
    assert_eq!(*log.borrow(), [LeaveIntent::Close]);
}

#[gpui::test]
fn keep_editing_leaves_page_and_drafts_alone(cx: &mut TestAppContext) {
    let (page, vcx, log) = open(cx);
    page.update(vcx, |p, _| p.drafts = vec![draft("a")]);
    esc(vcx, &page);
    assert!(vcx.debug_bounds("pr-discard-confirm").is_some());
    // Esc on the confirm is Keep editing, not a second Esc to the page.
    esc(vcx, &page);
    assert!(log.borrow().is_empty());
    assert!(vcx.debug_bounds("pr-discard-confirm").is_none());
    page.update(vcx, |p, _| assert_eq!(p.drafts.len(), 1));
}

#[gpui::test]
fn crumb_guards_carry_their_intent(cx: &mut TestAppContext) {
    let (page, vcx, log) = open(cx);
    // No drafts: the host may go at once.
    let free = vcx.update(|window, cx| {
        page.update(cx, |p, cx| {
            p.guard_leave(LeaveIntent::RepoCrumb, window, cx)
        })
    });
    assert!(free);
    page.update(vcx, |p, _| p.drafts = vec![draft("a")]);
    let free = vcx.update(|window, cx| {
        page.update(cx, |p, cx| {
            p.guard_leave(LeaveIntent::ModuleRoot, window, cx)
        })
    });
    assert!(!free);
    vcx.update(|_, cx| page.update(cx, |p, cx| p.confirm_discard(cx)));
    assert_eq!(*log.borrow(), [LeaveIntent::ModuleRoot]);
}

#[gpui::test]
fn the_confirm_closes_when_the_drafts_are_gone(cx: &mut TestAppContext) {
    let (page, vcx, _log) = open(cx);
    page.update(vcx, |p, _| p.drafts = vec![draft("a")]);
    esc(vcx, &page);
    assert!(vcx.debug_bounds("pr-discard-confirm").is_some());
    // A batch confirmed under the confirm.
    page.update(vcx, |p, cx| {
        p.drafts.clear();
        cx.notify();
    });
    vcx.run_until_parked();
    let _ = vcx.debug_bounds("pr-discard-confirm");
    assert!(vcx.debug_bounds("pr-discard-confirm").is_none());
}
