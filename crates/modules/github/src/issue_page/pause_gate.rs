//! #887: the issue page's detail read obeys the GraphQL pause, as the PR
//! page's does (docs/0084 D1). A refused request costs a budget that is
//! already empty and tells the user nothing, so a paused lane holds the
//! dispatch; a document that never landed shows the pause note as its
//! load error, and one that already landed keeps its data. Writes and
//! picker reads hold through `rate_pause::run_unless_paused`.

use gpui::Context;

use super::IssueDetailPage;
use crate::rate_pause::Lane;

impl IssueDetailPage {
    /// Whether the detail read must wait out the GraphQL pause.
    pub(super) fn hold_detail_for_pause(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(note) = self.state.read(cx).paused_note(Lane::Graphql) else {
            return false;
        };
        let (repo, number) = (self.repo.clone(), self.number);
        self.state.update(cx, |s, _| {
            let nothing_shown = s
                .issue_entry(&repo, number)
                .is_some_and(|e| e.detail.is_none() && e.books.last_error.is_none());
            if nothing_shown {
                s.fail_issue_detail_read(&repo, number, note);
            }
        });
        log::debug!("issue-detail: held — GraphQL budget paused");
        cx.notify();
        true
    }
}

#[cfg(test)]
mod tests;
