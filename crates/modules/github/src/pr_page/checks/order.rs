//! The order the Checks tab lists its rows in (docs/0016 D7, #792): what
//! needs attention first. Failed, then running, then everything else. Inside
//! a rank rows cluster by workflow, unlabelled ones last, then by name; the
//! run id settles namesakes, so a refresh doesn't shuffle rows on the server's
//! whim.

use crate::gql::{CheckState, PrCheck};

fn attention_rank(state: CheckState) -> u8 {
    match state {
        CheckState::Failure => 0,
        CheckState::Pending | CheckState::Waiting => 1,
        CheckState::Success | CheckState::Neutral => 2,
    }
}

/// The rows in display order. The list stays the server's; only the view
/// sorts it. Over the cap this orders the listed rows only.
pub(crate) fn display_order(nodes: &[PrCheck]) -> Vec<&PrCheck> {
    let mut rows: Vec<&PrCheck> = nodes.iter().collect();
    rows.sort_by(|a, b| {
        attention_rank(a.state)
            .cmp(&attention_rank(b.state))
            .then_with(|| a.workflow.is_none().cmp(&b.workflow.is_none()))
            .then_with(|| a.workflow.cmp(&b.workflow))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.check_run_id.cmp(&b.check_run_id))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gql::CheckOrigin;

    fn row(name: &str, state: CheckState, workflow: Option<&str>, id: u64) -> PrCheck {
        PrCheck {
            name: name.into(),
            state,
            details_url: None,
            origin: CheckOrigin::Actions,
            check_run_id: Some(id),
            suite_id: None,
            workflow: workflow.map(str::to_string),
            started_at: None,
            completed_at: None,
        }
    }

    fn names(nodes: &[PrCheck]) -> Vec<&str> {
        display_order(nodes)
            .iter()
            .map(|c| c.name.as_str())
            .collect()
    }

    #[test]
    fn failed_rows_lead_then_running_then_the_rest() {
        let nodes = [
            row("passed", CheckState::Success, None, 1),
            row("skipped", CheckState::Neutral, None, 2),
            row("running", CheckState::Pending, None, 3),
            row("failed", CheckState::Failure, None, 4),
        ];
        assert_eq!(names(&nodes), ["failed", "running", "passed", "skipped"]);
    }

    #[test]
    fn failure_outranks_the_workflow_cluster() {
        let nodes = [
            row("a", CheckState::Success, Some("Alpha"), 1),
            row("z", CheckState::Failure, Some("Zeta"), 2),
        ];
        assert_eq!(names(&nodes), ["z", "a"]);
    }

    #[test]
    fn inside_a_rank_rows_cluster_by_workflow_with_unlabelled_last() {
        let nodes = [
            row("status", CheckState::Success, None, 1),
            row("test", CheckState::Success, Some("Beta"), 2),
            row("lint", CheckState::Success, Some("Alpha"), 3),
            row("build", CheckState::Success, Some("Beta"), 4),
        ];
        assert_eq!(names(&nodes), ["lint", "build", "test", "status"]);
    }

    #[test]
    fn namesakes_keep_a_stable_order_whatever_the_server_sends() {
        let a = row("build", CheckState::Success, Some("CI"), 1);
        let b = row("build", CheckState::Success, Some("CI"), 2);
        let ids = |nodes: &[PrCheck]| -> Vec<Option<u64>> {
            display_order(nodes)
                .iter()
                .map(|c| c.check_run_id)
                .collect()
        };
        assert_eq!(ids(&[a.clone(), b.clone()]), ids(&[b, a]));
    }
}
