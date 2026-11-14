//! docs/0084 D4: which lanes a probe's findings refetch. Pure and
//! table-tested; the page acts on the plan.

use super::probe::Findings;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LanePlan {
    /// Refetch the detail: at most once per tick, whatever combination
    /// changed.
    pub(crate) detail: bool,
    /// Refetch the threads too (only when they are loaded).
    pub(crate) threads: bool,
    /// Nothing but check runs or statuses moved: this refetch is
    /// rate-limited (Q4) and scheduled when it comes too soon.
    pub(crate) checks_only: bool,
}

/// The lane map. A new head refetches the detail alone: its landing runs
/// the head fan-out (Files and threads, if loaded) and the probe already
/// follows the new commit.
pub(crate) fn plan(f: &Findings, threads_loaded: bool) -> LanePlan {
    if f.pr.head {
        return LanePlan {
            detail: true,
            ..LanePlan::default()
        };
    }
    if f.pr.review_comments {
        return LanePlan {
            detail: true,
            threads: threads_loaded,
            ..LanePlan::default()
        };
    }
    if f.pr.other {
        return LanePlan {
            detail: true,
            ..LanePlan::default()
        };
    }
    if f.checks {
        return LanePlan {
            detail: true,
            checks_only: true,
            ..LanePlan::default()
        };
    }
    LanePlan::default()
}

#[cfg(test)]
mod tests;
