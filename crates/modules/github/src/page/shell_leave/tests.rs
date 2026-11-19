//! #841: the shell's veto reaches the open PR page. Drafts hold the
//! navigation behind the discard confirm; no drafts let it through.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};

use dashboard_core::module::LeaveResume;
use dashboard_core::nav::Navigator;
use dashboard_core::paths::AppPaths;

use crate::auth::MockTokenStore;
use crate::drafts::DraftStore;
use crate::http::mock::MockHttp;
use crate::http::{Http, HttpError, SharedToken};
use crate::page::GithubPage;
use crate::service::{GithubCache, GithubState};

fn open_pr_page(cx: &mut TestAppContext) -> (Entity<GithubPage>, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(dashboard_core::status::init);
    let cx = cx.add_empty_window();
    let page = cx.update(|window, cx| {
        let failures = (0..6).map(|_| Err(HttpError("probe".into()))).collect();
        let client: Arc<dyn Http> = Arc::new(MockHttp::new(failures));
        let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
        let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
        let state = cx.new(|_| {
            GithubState::new(
                client,
                Arc::new(MockTokenStore::with("t0k".into())),
                shared,
                refresh,
                GithubCache::new(AppPaths::init()),
                DraftStore::at_tmp("shell-leave"),
            )
        });
        let nav = cx.new(|_| Navigator::default());
        let page = cx.new(|cx| GithubPage::new(state, nav, window, cx));
        let summary = crate::page::tests::pr(7, "probe", None);
        page.update(cx, |p, cx| {
            p.open_pr("acme/one", &summary, None, window, cx)
        });
        page
    });
    (page, cx)
}

fn probe() -> (LeaveResume, Rc<Cell<bool>>) {
    let ran = Rc::new(Cell::new(false));
    let flag = ran.clone();
    (Box::new(move |_, _| flag.set(true)), ran)
}

fn ask(page: &Entity<GithubPage>, vcx: &mut VisualTestContext, resume: LeaveResume) -> bool {
    vcx.update(|window, cx| page.update(cx, |p, cx| p.shell_may_leave(resume, window, cx)))
}

#[gpui::test]
fn no_drafts_lets_the_shell_through(cx: &mut TestAppContext) {
    let (page, vcx) = open_pr_page(cx);
    let (resume, ran) = probe();
    assert!(ask(&page, vcx, resume));
    assert!(!ran.get(), "the shell commits itself; the resume is unused");
}

#[gpui::test]
fn drafts_hold_the_navigation_until_discard_is_confirmed(cx: &mut TestAppContext) {
    let (page, vcx) = open_pr_page(cx);
    let pr = vcx.update(|_, cx| page.read(cx).pr.clone().expect("pr page"));
    pr.update(vcx, |p, _| p.seed_review_draft("a"));
    let (resume, ran) = probe();
    assert!(!ask(&page, vcx, resume), "drafts veto");
    vcx.run_until_parked();
    assert!(!ran.get(), "nothing resumes before the user answers");

    vcx.update(|_, cx| pr.update(cx, |p, cx| p.confirm_discard(cx)));
    vcx.run_until_parked();
    assert!(ran.get(), "Discard resumes the navigation");
}

#[gpui::test]
fn keep_editing_cancels_the_navigation(cx: &mut TestAppContext) {
    let (page, vcx) = open_pr_page(cx);
    let pr = vcx.update(|_, cx| page.read(cx).pr.clone().expect("pr page"));
    pr.update(vcx, |p, _| p.seed_review_draft("a"));
    let (resume, ran) = probe();
    assert!(!ask(&page, vcx, resume));
    vcx.update(|window, cx| pr.update(cx, |p, cx| p.cancel_leave_confirm(window, cx)));
    vcx.run_until_parked();
    assert!(!ran.get());
    assert!(vcx.update(|_, cx| page.read(cx).pr.is_some()), "page stays");
}

#[gpui::test]
fn a_module_without_a_pr_page_never_vetoes(cx: &mut TestAppContext) {
    let (page, vcx) = open_pr_page(cx);
    vcx.update(|_, cx| page.update(cx, |p, cx| p.close_pr(cx)));
    let (resume, _) = probe();
    assert!(ask(&page, vcx, resume));
}
