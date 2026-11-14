//! #721: the PR page's reads and writes obey their budget's pause
//! (docs/0050 D4 as amended by docs/0084 D1). A refused request costs a
//! budget that is already empty and tells the user nothing, so a paused
//! lane holds the dispatch and the page says so: the pause note beside
//! the freshness row, and, for a lane that has nothing to show yet, the
//! same words as the lane's error. A read that already landed keeps its
//! data; the pause note is the whole story.
//!
//! GraphQL lanes: detail, threads and every write. REST lanes: files, the
//! gated-runs sweep (#875) and the check-log chains.

use gpui::Context;

use super::PrDetailPage;
use crate::rate_pause::Lane;

impl PrDetailPage {
    /// Whether the detail read must wait out the GraphQL pause. A document
    /// that never landed shows the pause as its load error.
    pub(super) fn hold_detail_for_pause(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(note) = self.state.read(cx).paused_note(Lane::Graphql) else {
            return false;
        };
        let (repo, number) = (self.repo.clone(), self.number);
        self.state.update(cx, |s, _| {
            let nothing_shown = s
                .pr_entry(&repo, number)
                .is_some_and(|e| e.detail.is_none() && e.books.last_error.is_none());
            if nothing_shown {
                s.fail_detail_read(&repo, number, note);
            }
        });
        log::debug!("pr-detail: held — GraphQL budget paused");
        cx.notify();
        true
    }

    /// The same for the threads read (GraphQL).
    pub(super) fn hold_threads_for_pause(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(note) = self.state.read(cx).paused_note(Lane::Graphql) else {
            return false;
        };
        if self.threads.is_idle() {
            self.threads.fail(note);
        }
        log::debug!("pr-threads: held — GraphQL budget paused");
        cx.notify();
        true
    }

    /// The same for the files read (REST).
    pub(super) fn hold_files_for_pause(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(note) = self.state.read(cx).paused_note(Lane::Rest) else {
            return false;
        };
        if self.files.is_idle() {
            self.files.fail(note);
        }
        log::debug!("pr-files: held — REST budget paused");
        cx.notify();
        true
    }

    /// The note the freshness row shows: every paused budget, or nothing.
    pub(super) fn pause_note(&self, cx: &gpui::App) -> Option<String> {
        self.state.read(cx).paused_notes()
    }
}

#[cfg(test)]
mod tests;
