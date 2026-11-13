//! A read older than a confirmed write (docs/0072 D4, #739).
//!
//! A mutation's refetch shares the dispatch guard with every other read,
//! so it is skipped while one flies. That earlier read may predate the
//! write, and landing it would hide the write and stamp the document
//! fresh. So an outdated read never lands, and exactly one read replaces
//! it once the lane is free: arming any new read clears the mark, which
//! is also what stops a failing rerun from re-triggering itself.
//!
//! The detail mark lives on the document, because the confirmation and
//! the landing both outlive the page. The threads mark lives on the page,
//! like the threads record it guards.
//!
//! A manual refresh that hits the guard still rides out the flight: no
//! write made that read outdated.

use gpui::Context;

use super::PrDetailPage;
use crate::service::{Kind, PrEntry, PrKey};
use crate::usage::{Origin, Surface, Trigger};

impl PrEntry {
    /// The read that predates the write has ended and no other replaced it.
    fn outdated_rerun_due(&self) -> bool {
        self.books.read_outdated && !self.books.in_flight
    }
}

impl PrDetailPage {
    /// A write's detail refetch hit the guard. The store already marks
    /// the flight for writes that resolve through it; this covers the
    /// page-modal ones (labels, assignees, reviewers, reactions).
    pub(super) fn note_outdated_detail(&self, origin: Origin, cx: &mut Context<Self>) {
        if origin.trigger != Trigger::MutationRefetch {
            return;
        }
        let key = PrKey::new(self.repo.clone(), self.number);
        self.state.update(cx, |s, _| {
            if let Some(entry) = s.pulls.get_mut(&key) {
                entry.books.read_outdated = true;
            }
        });
    }

    /// A write's threads refetch hit the guard.
    pub(super) fn note_outdated_threads(&mut self, origin: Origin) {
        if origin.trigger == Trigger::MutationRefetch {
            self.threads_outdated = true;
        }
    }

    /// Close an outdated threads read without landing its body, leaving
    /// the mark for the rerun. Returns whether the read was outdated.
    pub(super) fn discard_outdated_threads(&mut self) -> bool {
        if self.threads_outdated {
            log::debug!("pr-threads: read predates a confirmed write, dropped");
            self.threads.in_flight = false;
        }
        self.threads_outdated
    }

    /// Dispatch the reads that outdated flights left owing. Failure backoff
    /// does not hold them, as it holds no write's refetch, but the lane's
    /// gate does (docs/0050 D4): a held rerun stays marked and the first
    /// store notification past the gate fires it, like a held catch-up.
    pub(super) fn rerun_outdated_reads(&mut self, cx: &mut Context<Self>) {
        let origin = Origin::new(Surface::PrPage, Trigger::MutationRefetch);
        let state = self.state.read(cx);
        let detail_due = state
            .pr_entry(&self.repo, self.number)
            .is_some_and(PrEntry::outdated_rerun_due)
            && !state.gated_for(Kind::PrDetail.lane());
        let threads_key = (
            Kind::PrThreads,
            Some(format!("{}#{}", self.repo, self.number)),
        );
        let threads_due = self.threads_outdated
            && !state.in_flight.contains(&threads_key)
            && !state.gated_for(Kind::PrThreads.lane());
        let files_due = self.files_rerun_due(cx);
        if detail_due {
            self.fetch_detail(origin, cx);
        }
        if files_due {
            self.fetch_files(origin, cx);
        }
        if threads_due {
            self.fetch_threads(origin, cx);
        }
    }
}

#[cfg(test)]
mod tests;
