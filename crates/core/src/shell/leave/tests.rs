//! The veto contract (docs/0048 D3): a module that answers `false`
//! stops sidebar jumps and Back/Forward until it runs the continuation.

use std::cell::RefCell;
use std::sync::Arc;

use gpui::{AppContext as _, Context, IntoElement, Render, TestAppContext, Window, div};

use crate::module::{LeaveResume, Module, ModuleEntry, ModuleMeta, ModuleNav};
use crate::nav::{Navigator, Page, Route};
use crate::paths::AppPaths;
use crate::registry::ModuleRegistry;
use crate::shell::{DashboardShell, ShellState};

thread_local! {
    /// Parked continuation of the last vetoed navigation.
    static PARKED: RefCell<Option<LeaveResume>> = const { RefCell::new(None) };
    static VETO: RefCell<bool> = const { RefCell::new(true) };
    static LOG: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

struct Blank;

impl Render for Blank {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

struct VetoNav;

impl ModuleNav for VetoNav {
    fn can_leave(&self, resume: LeaveResume, _: &mut Window, _: &mut gpui::App) -> bool {
        if VETO.with(|v| *v.borrow()) {
            PARKED.with(|p| *p.borrow_mut() = Some(resume));
            false
        } else {
            true
        }
    }

    fn restore(
        &self,
        _: &Arc<dyn std::any::Any + Send + Sync>,
        _: &mut Window,
        _: &mut gpui::App,
    ) {
        LOG.with(|l| l.borrow_mut().push("restore"));
    }

    fn collapse(&self, _: &mut Window, _: &mut gpui::App) {
        LOG.with(|l| l.borrow_mut().push("collapse"));
    }
}

struct Fake;

impl Module for Fake {
    fn meta() -> ModuleMeta {
        ModuleMeta {
            id: "fake".into(),
            title: "Fake".into(),
            icon: None,
        }
    }

    fn init(
        _: &gpui::Entity<Navigator>,
        _: &mut Window,
        _: &AppPaths,
        cx: &mut gpui::App,
    ) -> ModuleEntry {
        ModuleEntry {
            meta: Self::meta(),
            overview: cx.new(|_| Blank).into(),
            main_page: cx.new(|_| Blank).into(),
            settings: None,
            nav: Some(Arc::new(VetoNav)),
            status_item: None,
        }
    }
}

fn setup(
    cx: &mut TestAppContext,
) -> (
    gpui::Entity<Navigator>,
    gpui::Entity<DashboardShell>,
    &mut gpui::VisualTestContext,
) {
    PARKED.with(|p| *p.borrow_mut() = None);
    VETO.with(|v| *v.borrow_mut() = true);
    LOG.with(|l| l.borrow_mut().clear());
    cx.update(|cx| {
        gpui_component::theme::init(cx);
        crate::settings::init(None, None, cx);
        crate::status::init(cx);
        crate::toasts::init(cx);
    });
    let nav = cx.update(|cx| cx.new(|_| Navigator::default()));
    let (shell, vcx) = {
        let nav = nav.clone();
        cx.add_window_view(|window, cx| {
            let registry = ModuleRegistry::new().with::<Fake>(&nav, window, AppPaths::init(), cx);
            DashboardShell::new(registry, nav, ShellState::default(), None, window, cx)
        })
    };
    vcx.update(|_, cx| {
        nav.update(cx, |nav, cx| {
            nav.go(Page::Module("fake".into()), cx);
            nav.visit(Route::module_place("fake", "deep", Arc::new(1u8)), cx);
        });
    });
    (nav, shell, vcx)
}

fn page_of(nav: &gpui::Entity<Navigator>, vcx: &mut gpui::VisualTestContext) -> Page {
    vcx.update(|_, cx| nav.read(cx).page.clone())
}

#[gpui::test]
fn a_vetoed_sidebar_jump_waits_for_the_resume(cx: &mut TestAppContext) {
    let (nav, shell, vcx) = setup(cx);
    vcx.update(|w, cx| shell.update(cx, |s, cx| s.navigate(Page::Settings, w, cx)));
    assert_eq!(page_of(&nav, vcx), Page::Module("fake".into()));
    assert!(LOG.with(|l| l.borrow().is_empty()), "no collapse yet");

    let resume = PARKED.with(|p| p.borrow_mut().take()).expect("parked");
    vcx.update(|w, cx| resume(w, cx));
    assert_eq!(page_of(&nav, vcx), Page::Settings);
    assert_eq!(LOG.with(|l| l.borrow().clone()), vec!["collapse"]);
}

#[gpui::test]
fn a_dropped_resume_cancels_the_jump(cx: &mut TestAppContext) {
    let (nav, shell, vcx) = setup(cx);
    vcx.update(|w, cx| shell.update(cx, |s, cx| s.navigate(Page::Settings, w, cx)));
    drop(PARKED.with(|p| p.borrow_mut().take()));
    assert_eq!(page_of(&nav, vcx), Page::Module("fake".into()));
}

#[gpui::test]
fn a_vetoed_back_keeps_the_cursor_until_resumed(cx: &mut TestAppContext) {
    let (nav, shell, vcx) = setup(cx);
    vcx.update(|w, cx| shell.update(cx, |s, cx| s.go_back(&crate::nav::Back, w, cx)));
    assert!(vcx.update(|_, cx| nav.read(cx).can_back()), "cursor unmoved");
    assert!(LOG.with(|l| l.borrow().is_empty()));

    let resume = PARKED.with(|p| p.borrow_mut().take()).expect("parked");
    vcx.update(|w, cx| resume(w, cx));
    assert!(vcx.update(|_, cx| nav.read(cx).can_forward()), "Back landed");
    // The resume committed without asking a second time.
    assert!(PARKED.with(|p| p.borrow().is_none()));
}

#[gpui::test]
fn without_a_veto_navigation_goes_straight_through(cx: &mut TestAppContext) {
    let (nav, shell, vcx) = setup(cx);
    VETO.with(|v| *v.borrow_mut() = false);
    vcx.update(|w, cx| shell.update(cx, |s, cx| s.navigate(Page::Settings, w, cx)));
    assert_eq!(page_of(&nav, vcx), Page::Settings);
    vcx.update(|w, cx| shell.update(cx, |s, cx| s.go_back(&crate::nav::Back, w, cx)));
    assert_eq!(LOG.with(|l| l.borrow().clone()), vec!["collapse", "restore"]);
}
