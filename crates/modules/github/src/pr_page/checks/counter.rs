//! How many checks a PR has (docs/0016 D7, docs/0066 D3, #783). Runs waiting
//! for approval count: they are CI the PR is held on, and GitHub's rollup
//! can't see them, so a PR whose only CI is gated would otherwise read
//! "Checks (0)". The tab and the header use this one total.

/// The rollup's server-side total plus the runs the gated sweep holds.
pub(crate) fn checks_total(rollup_total: i64, waiting: usize) -> i64 {
    rollup_total + waiting as i64
}

/// The tab's count. While the sweep is out and nothing has been found yet
/// the tab stays bare, so a gated-only PR never flashes a wrong "0"
/// (docs/0016 D3).
pub(crate) fn checks_tab_count(
    rollup_total: i64,
    waiting: usize,
    sweep_in_flight: bool,
) -> Option<i64> {
    let total = checks_total(rollup_total, waiting);
    (total > 0 || !sweep_in_flight).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_runs_count_towards_the_total() {
        assert_eq!(checks_total(7, 2), 9);
        assert_eq!(checks_total(0, 2), 2);
        assert_eq!(checks_total(137, 0), 137);
    }

    #[test]
    fn a_gated_only_pr_reads_its_waiting_runs_not_zero() {
        assert_eq!(checks_tab_count(0, 2, false), Some(2));
    }

    #[test]
    fn the_tab_only_says_zero_once_the_sweep_has_answered() {
        assert_eq!(checks_tab_count(0, 0, true), None);
        assert_eq!(checks_tab_count(0, 0, false), Some(0));
    }

    #[test]
    fn a_refetching_sweep_keeps_a_known_count_up() {
        assert_eq!(checks_tab_count(7, 0, true), Some(7));
        assert_eq!(checks_tab_count(7, 2, true), Some(9));
        assert_eq!(checks_tab_count(0, 2, true), Some(2));
    }
}
