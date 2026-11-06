//! Which run and job the log chain lands on (docs/0039 D1, #747). A name
//! match isn't enough: the name filter returns every same-named check on
//! the head, and one workflow run can hold same-named jobs. A row's log is
//! the log of that row or an error, never its namesake's.

use crate::api::{CheckRunNode, JobNode};
use crate::pr_page::checks::identity::CheckTarget;

const ACTIONS_SLUG: &str = "github-actions";

/// Step 1: the row's check run among the name-filtered runs. The exact id,
/// else the row's suite, else the only Actions run with this name: a row
/// from before a re-run carries an id the endpoint's `latest` filter no
/// longer lists, and when nothing tells several candidates apart the row
/// has to refresh rather than guess.
pub(crate) fn pick_check_run(
    runs: Vec<CheckRunNode>,
    target: &CheckTarget,
) -> Result<CheckRunNode, &'static str> {
    let mut actions: Vec<CheckRunNode> = runs
        .into_iter()
        .filter(|c| c.app.as_ref().and_then(|a| a.slug.as_deref()) == Some(ACTIONS_SLUG))
        .collect();
    if actions.is_empty() {
        return Err(
            "GitHub no longer lists this check on the latest commit. Refresh the page, or open it on GitHub.",
        );
    }
    let position = target
        .check_run_id
        .and_then(|id| actions.iter().position(|c| c.id == Some(id)))
        .or_else(|| {
            let suite = target.suite_id?;
            let mut same_suite = actions
                .iter()
                .enumerate()
                .filter(|(_, c)| c.check_suite.as_ref().map(|s| s.id) == Some(suite))
                .map(|(i, _)| i);
            match (same_suite.next(), same_suite.next()) {
                (Some(only), None) => Some(only),
                _ => None,
            }
        })
        .or((actions.len() == 1).then_some(0));
    match position {
        Some(i) => Ok(actions.swap_remove(i)),
        None => Err("Several checks share this name and this one has changed. Refresh the page."),
    }
}

/// The trailing id of a job's documented `check_run_url`.
fn linked_check_run(job: &JobNode) -> Option<u64> {
    job.check_run_url
        .as_deref()?
        .rsplit('/')
        .next()?
        .parse()
        .ok()
}

/// Step 3: the job linked to the picked check run, else the only job with
/// this exact name. Job and check run names match on both sides, matrix
/// suffixes included (docs/0039 D1).
pub(crate) fn pick_job(
    jobs: Vec<JobNode>,
    check_run_id: Option<u64>,
    name: &str,
) -> Result<JobNode, &'static str> {
    let mut named: Vec<JobNode> = Vec::new();
    for job in jobs {
        if check_run_id.is_some() && linked_check_run(&job) == check_run_id {
            return Ok(job);
        }
        if job.name == name {
            named.push(job);
        }
    }
    match named.len() {
        0 => Err(
            "This job isn't in the workflow run any more. It may have been renamed. Open the check on GitHub.",
        ),
        1 => Ok(named.remove(0)),
        _ => Err("Several jobs in this workflow run share this name. Open the check on GitHub."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: u64, suite: u64, slug: &str) -> CheckRunNode {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": "build", "check_suite": {"id": suite}, "app": {"slug": slug},
        }))
        .unwrap()
    }

    fn job(id: u64, name: &str, check_run: Option<u64>) -> JobNode {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": name,
            "check_run_url": check_run
                .map(|c| format!("https://api.github.com/repos/o/r/check-runs/{c}")),
        }))
        .unwrap()
    }

    fn target(id: Option<u64>, suite: Option<u64>) -> CheckTarget {
        CheckTarget::new(Some("h"), "build", id, suite)
    }

    fn picked(runs: Vec<CheckRunNode>, t: &CheckTarget) -> Result<Option<u64>, &'static str> {
        pick_check_run(runs, t).map(|c| c.id)
    }

    #[test]
    fn same_named_runs_resolve_by_id_not_position() {
        let runs = || vec![run(1, 10, "github-actions"), run(2, 20, "github-actions")];
        assert_eq!(picked(runs(), &target(Some(1), Some(10))), Ok(Some(1)));
        assert_eq!(picked(runs(), &target(Some(2), Some(20))), Ok(Some(2)));
    }

    #[test]
    fn a_third_party_namesake_never_wins() {
        let runs = vec![run(1, 10, "scanner"), run(2, 20, "github-actions")];
        assert_eq!(picked(runs, &target(Some(1), Some(10))), Ok(Some(2)));
        assert!(picked(vec![run(1, 10, "scanner")], &target(Some(1), None)).is_err());
    }

    #[test]
    fn a_stale_row_follows_its_suite_after_a_re_run() {
        let runs = vec![run(3, 10, "github-actions"), run(2, 20, "github-actions")];
        assert_eq!(picked(runs, &target(Some(1), Some(10))), Ok(Some(3)));
    }

    #[test]
    fn a_stale_unique_name_still_resolves() {
        let runs = vec![run(3, 99, "github-actions")];
        assert_eq!(picked(runs, &target(Some(1), Some(10))), Ok(Some(3)));
        let runs = vec![run(3, 99, "github-actions")];
        assert_eq!(picked(runs, &target(None, None)), Ok(Some(3)));
    }

    #[test]
    fn nothing_to_tell_namesakes_apart_is_an_error_not_a_guess() {
        let runs = || vec![run(3, 30, "github-actions"), run(4, 40, "github-actions")];
        assert!(picked(runs(), &target(Some(1), Some(10))).is_err());
        assert!(picked(runs(), &target(None, None)).is_err());
    }

    #[test]
    fn jobs_resolve_by_their_check_run_link() {
        let jobs = || {
            vec![
                job(50, "build", Some(1)),
                job(60, "build", Some(2)),
                job(70, "lint", Some(3)),
            ]
        };
        assert_eq!(pick_job(jobs(), Some(1), "build").unwrap().id, 50);
        assert_eq!(pick_job(jobs(), Some(2), "build").unwrap().id, 60);
    }

    #[test]
    fn a_job_without_a_link_matches_its_exact_name_when_unambiguous() {
        let jobs = || vec![job(50, "build (linux)", None), job(60, "build", None)];
        assert_eq!(pick_job(jobs(), Some(1), "build").unwrap().id, 60);
        assert!(
            pick_job(jobs(), None, "gone")
                .unwrap_err()
                .contains("renamed")
        );
        let twins = vec![job(50, "build", None), job(60, "build", None)];
        assert!(pick_job(twins, None, "build").is_err());
    }
}
