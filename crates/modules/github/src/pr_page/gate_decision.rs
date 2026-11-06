//! Per-run record of the gate decision the user just made (docs/0066 D5,
//! #749): the row reads *approved, starting…* or *rejected* until the
//! next sweep stops listing the run. One record per run, so a rejected
//! run is never presented as starting.

/// What the user decided on a gated run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GateDecision {
    Approved,
    Rejected,
}

impl GateDecision {
    /// Maps the wire decision string the action path carries.
    pub(crate) fn parse(decision: &str) -> Self {
        if decision == "rejected" {
            Self::Rejected
        } else {
            Self::Approved
        }
    }

    /// The row's status suffix (docs/0066 D5).
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Approved => "approved, starting…",
            Self::Rejected => "rejected",
        }
    }
}

/// Decisions made on runs still listed by the last sweep.
#[derive(Debug, Default)]
pub(crate) struct GateDecisions(Vec<(u64, GateDecision)>);

impl GateDecisions {
    /// Records (or replaces) the decision for `run_id`.
    pub(crate) fn record(&mut self, run_id: u64, decision: GateDecision) {
        match self.0.iter_mut().find(|(id, _)| *id == run_id) {
            Some(entry) => entry.1 = decision,
            None => self.0.push((run_id, decision)),
        }
    }

    pub(crate) fn get(&self, run_id: u64) -> Option<GateDecision> {
        self.0.iter().find(|(id, _)| *id == run_id).map(|e| e.1)
    }

    /// Keeps only decisions whose run `listed` still reports.
    pub(crate) fn retain_listed(&mut self, listed: impl Fn(u64) -> bool) {
        self.0.retain(|(id, _)| listed(*id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_follows_the_decision() {
        assert_eq!(
            GateDecision::parse("approved").label(),
            "approved, starting…"
        );
        assert_eq!(GateDecision::parse("rejected").label(), "rejected");
    }

    #[test]
    fn mixed_runs_keep_their_own_state() {
        let mut d = GateDecisions::default();
        d.record(1, GateDecision::Approved);
        d.record(2, GateDecision::Rejected);
        assert_eq!(d.get(1), Some(GateDecision::Approved));
        assert_eq!(d.get(2), Some(GateDecision::Rejected));
        assert_eq!(d.get(3), None);
        d.retain_listed(|id| id == 2);
        assert_eq!(d.get(1), None);
        assert_eq!(d.get(2), Some(GateDecision::Rejected));
    }
}
