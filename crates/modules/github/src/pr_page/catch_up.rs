//! The open page's rollup catch-up (docs/0072 D3): a summary landing
//! moved the rollup under the cached detail, so the page revalidates in
//! the background, at most once per arming.
//!
//! The trigger is the page's own store subscription, and a failed read
//! notifies the store too. Without the spent mark and the gates below,
//! a failing read re-triggers itself with no delay (#731).

use gpui::Context;

use super::PrDetailPage;
use crate::service::{Kind, PrEntry};
use crate::usage::{Origin, Surface, Trigger};

impl PrEntry {
    /// The document's half of the gate: drifted, this arming has no read
    /// charged to it yet, nothing is flying, and the read lane's failure
    /// backoff has run out (docs/0036 D2).
    pub(crate) fn catch_up_due(&self) -> bool {
        self.rollup_dirty
            && !self.rollup_catch_up_spent
            && !self.books.in_flight
            && dashboard_core::freshness::standard_backoff_elapsed(
                self.books.fail_streak,
                self.books.attempted_at,
            )
    }
}

impl PrDetailPage {
    /// Dispatch the catch-up read if it is due and the detail lane's
    /// gate is open (docs/0050 D4, per budget since docs/0082 Q10). A
    /// gated or backing-off arming stays unspent: the next store
    /// notification past the gate fires it.
    pub(super) fn catch_up(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let due = state
            .pr_entry(&self.repo, self.number)
            .is_some_and(PrEntry::catch_up_due)
            && !state.gated_for(Kind::PrDetail.lane());
        if due {
            self.fetch_detail(Origin::new(Surface::PrPage, Trigger::Drift), cx);
        }
    }
}

#[cfg(test)]
mod tests;
