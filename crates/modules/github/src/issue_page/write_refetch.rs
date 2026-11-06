//! A read older than a confirmed write (docs/0072 D4, #806 — the issue
//! page's side of #739, `pr_page/write_refetch.rs`).
//!
//! A mutation's refetch shares the dispatch guard with every other read,
//! so it is skipped while one flies. That earlier read may predate the
//! write, and landing it would hide the write (or undo a patched
//! close/reopen) and stamp the document fresh. So an outdated read never
//! lands, and exactly one read replaces it once the lane is free: arming
//! any new read clears the mark, which is also what stops a failing rerun
//! from re-triggering itself.
//!
//! The mark lives on the document (`DetailBooks::read_outdated`), because
//! the confirmation and the landing both outlive the page. The issue page
//! has one read lane — comments and timeline ride the detail query — so
//! there is no second record to guard.
//!
//! A manual refresh that hits the guard still rides out the flight: no
//! write made that read outdated.

use gpui::Context;

use super::IssueDetailPage;
use crate::service::{IssueEntry, Kind, PrKey};
use crate::usage::{Origin, Surface, Trigger};

impl IssueEntry {
    /// The read that predates the write has ended and no other replaced it.
    fn outdated_rerun_due(&self) -> bool {
        self.books.read_outdated && !self.books.in_flight
    }
}

impl IssueDetailPage {
    /// A write's detail refetch hit the guard. The store already marks
    /// the flight for writes that confirm through it; this covers a
    /// refetch that is skipped for any other reason.
    pub(super) fn note_outdated_detail(&self, origin: Origin, cx: &mut Context<Self>) {
        if origin.trigger != Trigger::MutationRefetch {
            return;
        }
        let key = PrKey::new(self.repo.clone(), self.number);
        self.state.update(cx, |s, _| {
            if let Some(entry) = s.issues.get_mut(&key) {
                entry.books.read_outdated = true;
            }
        });
    }

    /// Dispatch the read an outdated flight left owing. Failure backoff
    /// does not hold it, as it holds no write's refetch, but the lane's
    /// gate does (docs/0050 D4): a held rerun stays marked and the first
    /// store notification past the gate fires it.
    pub(super) fn rerun_outdated_reads(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let due = state
            .issue_entry(&self.repo, self.number)
            .is_some_and(IssueEntry::outdated_rerun_due)
            && !state.gated_for(Kind::IssueDetail.lane());
        if due {
            self.fetch_detail(
                Origin::new(Surface::IssuePage, Trigger::MutationRefetch),
                cx,
            );
        }
    }
}

#[cfg(test)]
mod tests;
