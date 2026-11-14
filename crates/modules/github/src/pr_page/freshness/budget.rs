//! docs/0084 D7: the page's watch budget. Over a rolling hour a watched
//! page may spend 600 counted REST requests (200s, never 304s) and 300
//! GraphQL points on `Watch` and `Changed` traffic (Q1). The first time
//! either ceiling is reached the page drops to the Shown interval; a
//! second time within the hour stops the probe until a refocus or
//! Refresh (`clear`).
//!
//! The guard's only own counter is this one; the usage thresholds
//! (Low, Critical, Exhausted) are read from the usage book by the caller
//! (`usage_chip::worst_state`) and enter the cadence function directly.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Counted REST requests per rolling hour.
pub(crate) const REST_CEILING: u32 = 600;
/// GraphQL points per rolling hour.
pub(crate) const POINTS_CEILING: u32 = 300;
/// The rolling window.
pub(crate) const WINDOW: Duration = Duration::from_secs(60 * 60);

/// What the guard says about the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GuardState {
    Open,
    /// One breach in the last hour: probe at the Shown interval.
    Shown,
    /// Two breaches in the last hour: no probe until refocus or Refresh.
    Stopped,
}

/// A rolling sum of what was spent, oldest first.
#[derive(Debug, Default)]
struct Spend(VecDeque<(Instant, u32)>);

impl Spend {
    fn add(&mut self, now: Instant, n: u32) {
        self.0.push_back((now, n));
    }

    fn total(&mut self, now: Instant) -> u32 {
        while self
            .0
            .front()
            .is_some_and(|(t, _)| now.saturating_duration_since(*t) >= WINDOW)
        {
            self.0.pop_front();
        }
        self.0.iter().map(|(_, n)| n).sum()
    }
}

#[derive(Debug, Default)]
pub(crate) struct Guard {
    rest: Spend,
    points: Spend,
    /// The ceilings are currently reached; a breach is the crossing, so
    /// one stretch over the line is one breach.
    over: bool,
    /// When each breach of the last hour happened.
    breaches: VecDeque<Instant>,
    stopped: bool,
}

impl Guard {
    /// Counted REST requests the page's watch traffic just spent.
    pub(crate) fn spend_rest(&mut self, now: Instant, n: u32) {
        self.over = self.reached(now);
        self.rest.add(now, n);
        self.settle(now);
    }

    /// GraphQL points the page's watch traffic just spent.
    pub(crate) fn spend_points(&mut self, now: Instant, n: u32) {
        self.over = self.reached(now);
        self.points.add(now, n);
        self.settle(now);
    }

    fn reached(&mut self, now: Instant) -> bool {
        self.rest.total(now) >= REST_CEILING || self.points.total(now) >= POINTS_CEILING
    }

    /// Called after a spend: crossing the line from below is a breach.
    fn settle(&mut self, now: Instant) {
        let reached = self.reached(now);
        self.expire(now);
        if reached && !self.over {
            self.breaches.push_back(now);
            // Only a new crossing stops it: a resumed page with two
            // breaches still in the hour runs at Shown until the next.
            if self.breaches.len() >= 2 {
                self.stopped = true;
            }
        }
        self.over = reached;
    }

    fn expire(&mut self, now: Instant) {
        while self
            .breaches
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= WINDOW)
        {
            self.breaches.pop_front();
        }
    }

    pub(crate) fn state(&mut self, now: Instant) -> GuardState {
        self.expire(now);
        if self.stopped {
            GuardState::Stopped
        } else if self.breaches.is_empty() {
            GuardState::Open
        } else {
            GuardState::Shown
        }
    }

    /// A refocus or Refresh lifts the stop only. The spend and the
    /// breaches stay: the hour is rolling, and a user who switches windows
    /// now and then must not reset it. A resumed page runs at Shown while a
    /// breach is inside the hour, and the next crossing stops it again.
    pub(crate) fn resume(&mut self) {
        self.stopped = false;
    }

    /// Spent in the last hour, for the page's tests and stage 5's label.
    #[cfg(test)]
    pub(crate) fn spent(&mut self, now: Instant) -> (u32, u32) {
        (self.rest.total(now), self.points.total(now))
    }

    /// The probe is stopped by the guard until refocus or Refresh.
    #[cfg(test)]
    pub(crate) fn stopped(&self) -> bool {
        self.stopped
    }
}

#[cfg(test)]
mod tests;
