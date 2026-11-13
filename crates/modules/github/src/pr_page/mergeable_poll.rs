//! An UNKNOWN mergeable state re-polls, a bounded number of times
//! (docs/0016 §6, #752).
//!
//! GitHub computes mergeability lazily, so the detail read right after a
//! push usually answers `UNKNOWN`, and Merge stays disabled until
//! something else refetches. A landing that carries it schedules one
//! re-poll after `POLL_DELAY`; the answer either resolves it or schedules
//! the next, up to `MAX_POLLS` in a row.
//!
//! No hot loop (#731): the budget is spent per poll and refilled only by a
//! landing that did not come from a poll (open, refresh, refocus, write
//! refetch, a new head). The pending timer is a task the page owns, so a
//! dropped page, a newer landing or a resolved state cancels it. A failed
//! poll has no landing, so it schedules nothing, and a paused GraphQL
//! lane skips the poll without spending more than its own try.

use std::time::Duration;

use gpui::{Context, Task};

use super::PrDetailPage;
use crate::gql;
use crate::service::Kind;
use crate::usage::{Origin, Surface, Trigger};

/// Re-polls after the landing that first answered UNKNOWN.
pub(super) const MAX_POLLS: u32 = 3;
/// The wait before each re-poll.
pub(super) const POLL_DELAY: Duration = Duration::from_secs(4);

#[derive(Default)]
pub(super) struct Poll {
    /// Polls fired since the last non-poll landing.
    tries: u32,
    /// A poll's read is on its way; the landing it produces is not a
    /// deliberate one and must not refill the budget.
    in_poll: bool,
    /// The pending timer. Dropping it cancels the poll.
    timer: Option<Task<()>>,
}

impl PrDetailPage {
    /// A detail landed: schedule the next re-poll while it says UNKNOWN
    /// and the budget lasts, stop the timer otherwise.
    pub(super) fn poll_unknown_mergeable(
        &mut self,
        detail: &gql::PrDetail,
        head_changed: bool,
        cx: &mut Context<Self>,
    ) {
        let from_poll = std::mem::take(&mut self.mergeable_poll.in_poll);
        if !from_poll || head_changed {
            self.mergeable_poll.tries = 0;
        }
        self.mergeable_poll.timer = None;
        let unknown = detail.mergeable.as_deref() == Some("UNKNOWN")
            && detail.state.as_deref() == Some("OPEN");
        if !unknown || self.mergeable_poll.tries >= MAX_POLLS {
            return;
        }
        self.mergeable_poll.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(POLL_DELAY).await;
            let _ = this.update(cx, |page, cx| page.fire_mergeable_poll(cx));
        }));
    }

    fn fire_mergeable_poll(&mut self, cx: &mut Context<Self>) {
        self.mergeable_poll.timer = None;
        self.mergeable_poll.tries += 1;
        if self.state.read(cx).gated_for(Kind::PrDetail.lane()) {
            return;
        }
        self.mergeable_poll.in_poll = true;
        self.fetch_detail(Origin::new(Surface::PrPage, Trigger::Recheck), cx);
    }
}

#[cfg(test)]
mod tests;
