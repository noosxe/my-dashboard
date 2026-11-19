//! The module's side of the shell's leave veto (#841, docs/0048): the
//! shell asks `can_leave` before a sidebar jump or history Back/Forward
//! commits. An open PR page with unsent review drafts answers no and
//! opens its discard confirm (docs/0043 D7); confirming runs the parked
//! continuation, which commits the navigation.

use gpui::{Context, Window};

use dashboard_core::module::LeaveResume;

use super::GithubPage;
use crate::pr_page::leave_guard::LeaveIntent;

impl GithubPage {
    /// `true` — nothing would be lost (no PR page, or no unsent drafts),
    /// the shell may go ahead. `false` — the confirm is open and `resume`
    /// is parked until the user answers. Back/Forward *within* the module
    /// (PR page to repo view) land here too, and tear the PR page down
    /// the same way, so they ask under the same rule.
    pub(super) fn shell_may_leave(
        &mut self,
        resume: LeaveResume,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.pr_may_leave(LeaveIntent::Shell, window, cx) {
            // A newer ask replaces an older parked one.
            self.shell_resume = Some(resume);
            return false;
        }
        true
    }

    /// The user confirmed the discard: run the parked navigation. It runs
    /// after this update ends — it re-enters the page through the shell's
    /// collapse/restore, which the current lease would forbid.
    pub(super) fn resume_shell_navigation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(resume) = self.shell_resume.take() {
            window.defer(cx, move |window, cx| resume(window, cx));
        }
    }
}

#[cfg(test)]
pub(super) mod tests;
