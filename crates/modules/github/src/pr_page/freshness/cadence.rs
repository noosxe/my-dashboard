//! docs/0084 D5: the probe interval, one pure function. The page's loop
//! (`timer.rs`) gathers the inputs, asks, and arms its one timer with the
//! answer; nothing here reads a clock or the store.
//!
//! The tiers:
//!
//! | Tier | Condition | Interval |
//! |---|---|---|
//! | Hot | window active, and something is moving | 10 s |
//! | Active | window active, otherwise | 30 s |
//! | Shown | window inactive | 60 s, or 30 s while something is moving |
//!
//! then, in order: the interval doubles for every 30 quiet minutes up to
//! 5 minutes (Settled); the budget guard's Shown cap; Low usage
//! multiplies by 4; failures back off; a secondary limit waits at least
//! 60 s; `X-Poll-Interval` is a floor. Some inputs end probing instead:
//! a closed PR (Q5), an hour unfocused (Unattended, Q2), a stopped
//! guard, and Critical usage.

use std::time::Duration;

use dashboard_core::usage_analytics::surfaces::ThresholdState;

use super::budget::GuardState;

/// Window active, and something is moving (D5, Hot).
pub(crate) const HOT: Duration = Duration::from_secs(10);
/// Window active, otherwise.
pub(crate) const ACTIVE: Duration = Duration::from_secs(30);
/// Window inactive: the second-screen case (docs/0036 D1).
pub(crate) const SHOWN: Duration = Duration::from_secs(60);
/// A change landed this recently keeps the page Hot.
pub(crate) const RECENT_CHANGE: Duration = Duration::from_secs(120);
/// Every this much quiet time doubles the interval (Settled).
pub(crate) const SETTLE_STEP: Duration = Duration::from_secs(30 * 60);
/// Settling stops doubling here.
pub(crate) const SETTLED_CAP: Duration = Duration::from_secs(5 * 60);
/// Unfocused for longer than this, nobody is looking (Unattended).
pub(crate) const UNATTENDED_AFTER: Duration = Duration::from_secs(60 * 60);
/// A secondary limit waits at least this long (F7, #751).
pub(crate) const SECONDARY_FLOOR: Duration = Duration::from_secs(60);
/// Low usage (docs/0082 D5) slows every interval by this factor.
pub(crate) const LOW_FACTOR: u32 = 4;
/// A failing probe backs off by doubling, up to this.
const FAIL_CAP: Duration = Duration::from_secs(300);

/// What the interval is a function of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cadence {
    pub(crate) window_active: bool,
    /// How long the window has been inactive; zero while it is active.
    pub(crate) unfocused_for: Duration,
    /// The PR is open. A closed or merged one doesn't probe (Q5).
    pub(crate) open: bool,
    /// A check is running or queued.
    pub(crate) checks_running: bool,
    /// `mergeable` is UNKNOWN: GitHub is still computing it.
    pub(crate) mergeable_unknown: bool,
    /// The PR is in a merge queue (D6).
    pub(crate) queued: bool,
    /// Time since a change was last seen; `None` when none has been
    /// since the page opened.
    pub(crate) since_change: Option<Duration>,
    /// Quiet time: since the last change, refocus or Refresh, or since
    /// the page opened. It settles the interval.
    pub(crate) quiet_for: Duration,
    /// The budget guard's verdict for this page (D7).
    pub(crate) guard: GuardState,
    /// The worse of the two budgets on the usage ladder (D7).
    pub(crate) usage: ThresholdState,
    /// `X-Poll-Interval`, the floor, when GitHub sent one.
    pub(crate) poll_floor: Option<Duration>,
    /// Failed ticks in a row.
    pub(crate) fail_streak: u32,
    /// The last failure was a rate limit.
    pub(crate) limited: bool,
}

impl Default for Cadence {
    /// A focused, open page that has just opened.
    fn default() -> Self {
        Self {
            window_active: true,
            unfocused_for: Duration::ZERO,
            open: true,
            checks_running: false,
            mergeable_unknown: false,
            queued: false,
            since_change: None,
            quiet_for: Duration::ZERO,
            guard: GuardState::Open,
            usage: ThresholdState::Normal,
            poll_floor: None,
            fail_streak: 0,
            limited: false,
        }
    }
}

/// Why the page isn't probing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    /// MERGED or CLOSED: events and Refresh cover it.
    Closed,
    /// Unfocused for more than an hour.
    Unattended,
    /// The budget guard tripped twice in an hour.
    Guard,
    /// A budget is at Critical or exhausted.
    Usage,
}

/// What the page does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Next {
    /// Probe after this wait.
    After(Duration),
    /// Send nothing. The loop keeps looking locally, so whatever lifted
    /// the stop resumes it.
    Idle(Stop),
}

impl Cadence {
    /// Something other than focus keeps the page Hot.
    pub(crate) fn moving(&self) -> bool {
        self.checks_running
            || self.mergeable_unknown
            || self.queued
            || self.since_change.is_some_and(|d| d < RECENT_CHANGE)
    }

    /// The tier's interval before settling, the guard and usage.
    fn tier(&self) -> Duration {
        match (self.window_active, self.moving()) {
            (true, true) => HOT,
            (true, false) => ACTIVE,
            (false, true) => ACTIVE,
            (false, false) => SHOWN,
        }
    }

    /// The interval of this state, or why there is none.
    pub(crate) fn next(&self) -> Next {
        if !self.open {
            return Next::Idle(Stop::Closed);
        }
        if self.guard == GuardState::Stopped {
            return Next::Idle(Stop::Guard);
        }
        if self.usage >= ThresholdState::Critical {
            return Next::Idle(Stop::Usage);
        }
        if !self.window_active && self.unfocused_for > UNATTENDED_AFTER {
            return Next::Idle(Stop::Unattended);
        }
        let mut wait = self.tier();
        // Settled: double for every 30 quiet minutes, up to the cap.
        let steps = (self.quiet_for.as_secs() / SETTLE_STEP.as_secs()).min(16) as u32;
        if steps > 0 {
            wait = (wait * 2u32.pow(steps)).min(SETTLED_CAP.max(wait));
        }
        if self.guard == GuardState::Shown {
            wait = wait.max(SHOWN);
        }
        if self.usage == ThresholdState::Low {
            wait *= LOW_FACTOR;
        }
        if self.fail_streak > 0 {
            wait = backoff(wait, self.fail_streak);
        }
        if self.limited {
            wait = wait.max(SECONDARY_FLOOR);
        }
        if let Some(floor) = self.poll_floor {
            wait = wait.max(floor);
        }
        Next::After(wait)
    }
}

/// The delay after `streak` failures in a row.
fn backoff(base: Duration, streak: u32) -> Duration {
    (base * 2u32.pow(streak.min(4))).min(FAIL_CAP.max(base))
}

#[cfg(test)]
mod tests;
