//! docs/0084 D6: the merge queue's position has no REST field, so the
//! probe can't see it move. While the PR is queued, a tick that sent a
//! detail refetch for no other reason reads it: at most every 30 s, one
//! GraphQL point (F2). This amends docs/0083's "no queue polling".
//!
//! The same rule serves an UNKNOWN `mergeable` the REST side has already
//! resolved (the detail is stale and no probe will ever report the
//! change, because the first 200 only seeds the comparison): the Hot
//! tier ticks until the detail catches up.

use std::time::{Duration, Instant};

/// A queue (or UNKNOWN) refetch goes out at most this often.
pub(crate) const GAP: Duration = Duration::from_secs(30);

/// Why a tick with nothing to report still reads the detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Why {
    /// The PR is in a merge queue.
    Queued,
    /// The detail says `mergeable` UNKNOWN and REST says it is known.
    StaleUnknown,
}

#[derive(Debug, Default)]
pub(crate) struct QueueRefetch {
    /// When a probe last caused a detail read, for any reason.
    last: Option<Instant>,
}

impl QueueRefetch {
    /// A probe-caused detail read went out.
    pub(crate) fn dispatched(&mut self, now: Instant) {
        self.last = Some(now);
    }

    /// Whether a tick that refetched nothing should read the detail now.
    /// `quiet` is: no REST failure and no change reported, so the tick
    /// would otherwise send nothing.
    pub(crate) fn due(&self, now: Instant, why: Option<Why>, quiet: bool) -> bool {
        why.is_some()
            && quiet
            && self
                .last
                .is_none_or(|t| now.saturating_duration_since(t) >= GAP)
    }
}

#[cfg(test)]
mod tests;
