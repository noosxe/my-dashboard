//! The per-budget rate-limit pause (docs/0050 D4 as amended by docs/0082
//! §2.1 Q10): one `rate_limited_until` per primary bucket, each armed by
//! its own bucket's `remaining == 0` and each gating only its own
//! transport's fetches. An exhausted GraphQL budget no longer stops REST
//! traffic, and the reverse.
//!
//! The pause stays the only writer of pause behaviour; the chip and the
//! usage page read it next to the book's snapshots (0050 D4).

use serde::Serialize;

use crate::service::{Kind, RepoLane};

/// A primary budget's lane: the transport a fetch draws on (0050 D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Lane {
    /// REST `core`, in requests.
    Rest,
    /// GraphQL, in points.
    Graphql,
}

impl Lane {
    pub(crate) const ALL: [Lane; 2] = [Lane::Rest, Lane::Graphql];

    /// The budget's name in logs and copy (0082 D21).
    pub(crate) fn name(self) -> &'static str {
        match self {
            Lane::Rest => "REST",
            Lane::Graphql => "GraphQL",
        }
    }
}

impl Kind {
    /// The lane a fetch kind draws on. Every kind runs on exactly one
    /// transport (`run_spec`'s arms), so this is exhaustive with no
    /// fallback: a new kind must say which budget it spends.
    pub(crate) fn lane(self) -> Lane {
        match self {
            Kind::CardTotals
            | Kind::SessionInventory
            | Kind::RepoPulls
            | Kind::RepoIssues
            | Kind::RepoCommits
            | Kind::RepoCommitsMore
            | Kind::PrDetail
            | Kind::PrMutation(_)
            | Kind::PrThreads
            | Kind::IssueDetail
            | Kind::RepoLabels
            | Kind::RepoAssignableUsers
            | Kind::RepoMilestones
            | Kind::RepoBranches
            | Kind::RepoIssueTemplates
            | Kind::RepoPrTemplates
            | Kind::AccountPullRequests => Lane::Graphql,
            Kind::Notifications
            | Kind::RepoNotifications
            | Kind::PrFiles
            | Kind::JobLog
            | Kind::RepoTemplatesConfig
            | Kind::ThreadDetail
            | Kind::ThreadMutation(_)
            | Kind::RateLimit
            | Kind::GatedRuns
            | Kind::GateProbe
            | Kind::RunGateReview => Lane::Rest,
        }
    }
}

impl RepoLane {
    /// The budget a repo-view tab's fetch draws on (its spec's kind).
    pub(crate) fn budget(self) -> Lane {
        match self {
            RepoLane::Pulls | RepoLane::Issues | RepoLane::Commits => Lane::Graphql,
            RepoLane::Notifications => Lane::Rest,
        }
    }
}

/// One `rate_limited_until` per bucket, in UTC epoch seconds. Compared
/// against [`crate::usage::budget_now`] (docs/0081 §12.1), so a pinned
/// clock pins the gate too.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct RateLimitPause {
    pub rest: Option<u64>,
    pub graphql: Option<u64>,
}

impl RateLimitPause {
    /// The lane's pause deadline, in force or not.
    pub(crate) fn until(&self, lane: Lane) -> Option<u64> {
        match lane {
            Lane::Rest => self.rest,
            Lane::Graphql => self.graphql,
        }
    }

    fn slot(&mut self, lane: Lane) -> &mut Option<u64> {
        match lane {
            Lane::Rest => &mut self.rest,
            Lane::Graphql => &mut self.graphql,
        }
    }

    /// Arm `lane`'s pause until its bucket resets.
    pub(crate) fn arm(&mut self, lane: Lane, until: u64) {
        *self.slot(lane) = Some(until);
    }

    /// The lane's deadline while it is still in the future.
    pub(crate) fn in_force(&self, lane: Lane, now: u64) -> Option<u64> {
        self.until(lane).filter(|until| now < *until)
    }

    /// Whether `lane`'s fetches are paused at `now`.
    pub(crate) fn paused(&self, lane: Lane, now: u64) -> bool {
        self.in_force(lane, now).is_some()
    }

    /// Clear every pause whose window has passed; returns the lanes that
    /// resumed, so the caller logs each at its own reset.
    pub(crate) fn expire(&mut self, now: u64) -> Vec<Lane> {
        let mut resumed = Vec::new();
        for lane in Lane::ALL {
            let slot = self.slot(lane);
            if slot.is_some_and(|until| now >= until) {
                *slot = None;
                resumed.push(lane);
            }
        }
        resumed
    }

    /// A new credential or account: no pause carries over (docs/0011).
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Test seam: pause both lanes until `until`.
    #[cfg(test)]
    pub(crate) fn both(until: u64) -> Self {
        Self {
            rest: Some(until),
            graphql: Some(until),
        }
    }
}

mod arm;
mod classify;
pub(crate) use arm::null_read_copy;
pub(crate) use classify::{GqlFailure, classify_errors, pause_until, paused_copy};

#[cfg(test)]
mod tests;
