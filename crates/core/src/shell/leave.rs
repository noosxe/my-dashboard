//! The module veto in front of shell-driven navigation (docs/0048 D3,
//! #841): a sidebar jump or history Back/Forward that would take the
//! user off a module's open page first asks the module, which may stop
//! it to confirm that unsent work can go. The navigation itself resumes
//! through the continuation the module was handed.

use gpui::{Context, Window};

use crate::module::LeaveResume;
use crate::nav::{self, Page};

use super::DashboardShell;

impl DashboardShell {
    /// Ask the module on screen whether the navigation `commit` stands for
    /// may go. `true` — nothing objects; the caller commits itself and
    /// `commit` is dropped unused. `false` — the module is asking the
    /// user; it runs `commit` (through [`LeaveResume`]) on a yes and drops
    /// it on a no.
    pub(super) fn leave_allowed(
        &mut self,
        commit: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Page::Module(id) = self.nav.read(cx).page.clone() else {
            return true;
        };
        let Some(module_nav) = self.registry.get(&id).and_then(|e| e.nav.clone()) else {
            return true;
        };
        let shell = cx.entity().downgrade();
        let resume: LeaveResume = Box::new(move |window, cx| {
            if let Some(shell) = shell.upgrade() {
                shell.update(cx, |this, cx| commit(this, window, cx));
            }
        });
        module_nav.can_leave(resume, window, cx)
    }

    /// Sidebar/crumb navigation, guarded: a jump away from the current
    /// module page asks the module first. Same-page jumps are the
    /// module's own re-entry and are never vetoed here.
    pub(super) fn navigate(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        if self.nav.read(cx).page != page {
            let target = page.clone();
            if !self.leave_allowed(
                move |this, window, cx| this.navigate_committed(target, window, cx),
                window,
                cx,
            ) {
                return;
            }
        }
        self.navigate_committed(page, window, cx);
    }

    pub(super) fn go_back(&mut self, _: &nav::Back, window: &mut Window, cx: &mut Context<Self>) {
        if self.nav.read(cx).can_back()
            && !self.leave_allowed(|this, w, cx| this.back_committed(w, cx), window, cx)
        {
            return;
        }
        self.back_committed(window, cx);
    }

    pub(super) fn go_forward(
        &mut self,
        _: &nav::Forward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.nav.read(cx).can_forward()
            && !self.leave_allowed(|this, w, cx| this.forward_committed(w, cx), window, cx)
        {
            return;
        }
        self.forward_committed(window, cx);
    }

    fn back_committed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(route) = self.nav.update(cx, |nav, cx| nav.back(cx)) {
            self.focus_root(window, cx);
            self.activate_route(&route, window, cx);
        }
    }

    fn forward_committed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(route) = self.nav.update(cx, |nav, cx| nav.forward(cx)) {
            self.focus_root(window, cx);
            self.activate_route(&route, window, cx);
        }
    }
}

#[cfg(test)]
mod tests;
