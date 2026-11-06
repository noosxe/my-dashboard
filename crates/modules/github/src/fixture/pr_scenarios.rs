//! Every committed PR scenario (docs/0081 §12.1, #769) parses with the
//! code's own parsers, served through the fixture transport by number,
//! so a shape that drifts from `gql.rs` fails here rather than as a
//! blank page in a receipt.

use std::path::{Path, PathBuf};

use super::FixtureHttp;
use crate::api::{GatedRunsPage, PendingDeploymentNode, PrFile};
use crate::gql::{
    self, CheckState, Envelope, PrDetail, PrDetailData, PrThreads, PrThreadsData, RepoPickerData,
};
use crate::http::{Http, Verb};

const REPO: &str = "repos/fixture-user/beacon";

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../debug/verify/fixtures/github")
}

fn http() -> FixtureHttp {
    FixtureHttp::with_rate(fixture_dir(), None)
}

fn variables(number: i64) -> serde_json::Value {
    serde_json::json!({ "owner": "fixture-user", "name": "beacon", "number": number })
}

fn detail_envelope(number: i64) -> Envelope<PrDetailData> {
    let res = http()
        .post_graphql(gql::PR_DETAIL_QUERY, &variables(number))
        .unwrap();
    assert_eq!(res.status, 200, "PrDetail.{number}");
    serde_json::from_str(res.body.as_deref().unwrap()).unwrap()
}

fn detail(number: i64) -> PrDetail {
    let env = detail_envelope(number);
    assert!(env.errors.is_empty(), "PrDetail.{number}: {:?}", env.errors);
    env.data
        .and_then(|d| d.repository)
        .as_ref()
        .and_then(gql::pr_detail_from_value)
        .unwrap_or_else(|| panic!("PrDetail.{number} doesn't parse"))
}

fn threads(number: i64) -> PrThreads {
    let res = http()
        .post_graphql(gql::PR_THREADS_QUERY, &variables(number))
        .unwrap();
    assert_eq!(res.status, 200, "PrThreads.{number}");
    let env: Envelope<PrThreadsData> = serde_json::from_str(res.body.as_deref().unwrap()).unwrap();
    env.data
        .and_then(|d| d.repository)
        .as_ref()
        .and_then(gql::pr_threads_from_value)
        .unwrap_or_else(|| panic!("PrThreads.{number} doesn't parse"))
}

fn rest(path: &str) -> String {
    let url = format!("https://api.github.com/{REPO}/{path}");
    let res = http().request(Verb::Get, &url, None, None).unwrap();
    assert_eq!(res.status, 200, "{path}");
    res.body.unwrap()
}

fn sha(detail: &PrDetail) -> &str {
    detail.head_oid.as_deref().unwrap()
}

/// All seven rendering scenarios land, with the files, threads and
/// check-suites answers the page's other tabs ask for.
#[test]
fn every_scenario_parses_with_the_page_parsers() {
    for number in 101..=107 {
        let d = detail(number);
        assert_eq!(d.number, number);
        assert_eq!(d.url.rsplit('/').next(), Some(number.to_string().as_str()));
        let files: Vec<PrFile> =
            serde_json::from_str(&rest(&format!("pulls/{number}/files?per_page=100&page=1")))
                .unwrap();
        assert_eq!(Some(files.len() as i64), d.changed_files, "#{number} files");
        threads(number);
        let suites = rest(&format!("commits/{}/check-suites", sha(&d)));
        let suites: crate::api::CheckSuitesPage = serde_json::from_str(&suites).unwrap();
        assert!(suites.check_suites.is_empty());
    }
}

#[test]
fn the_populated_scenario_covers_every_section() {
    let d = detail(101);
    assert_eq!(d.merge_state_status.as_deref(), Some("BLOCKED"));
    assert_eq!((d.labels.len(), d.assignees.len()), (3, 2));
    assert!(d.milestone.is_some());
    assert_eq!(d.review_requests.len(), 3);
    assert!(d.review_requests.iter().any(|r| r.id.is_none()), "a team");
    let review_states: Vec<_> = d
        .reviews
        .nodes
        .iter()
        .filter_map(|r| r.state.as_deref())
        .collect();
    assert!(review_states.contains(&"APPROVED"));
    assert!(review_states.contains(&"CHANGES_REQUESTED"));
    let check_states: Vec<_> = d.checks.nodes.iter().map(|c| c.state).collect();
    for state in [
        CheckState::Success,
        CheckState::Failure,
        CheckState::Pending,
    ] {
        assert!(check_states.contains(&state), "{state:?}");
    }

    let t = threads(101);
    assert_eq!(t.nodes.len(), 4);
    assert!(t.nodes.iter().any(|n| n.is_outdated));
    assert!(t.nodes.iter().any(|n| n.is_resolved));
    assert!(threads(102).nodes.is_empty(), "the plain file answers");

    // The sweep's URL carries a query the fixture key drops; only this
    // PR's head matches its rows, and the probes tell the gates apart.
    let page: GatedRunsPage = serde_json::from_str(&rest(
        "actions/runs?branch=x&event=pull_request&status=action_required&per_page=50",
    ))
    .unwrap();
    let held: Vec<_> = page
        .workflow_runs
        .iter()
        .filter(|run| run.head_sha.as_deref() == Some(sha(&d)))
        .collect();
    assert_eq!(held.len(), 2);
    assert!(
        page.workflow_runs
            .iter()
            .all(|run| run.conclusion.as_deref() == Some("action_required"))
    );
    let probe = |id: u64| -> Vec<PendingDeploymentNode> {
        serde_json::from_str(&rest(&format!("actions/runs/{id}/pending_deployments"))).unwrap()
    };
    assert!(probe(held[0].id).is_empty(), "fork gate");
    assert_eq!(probe(held[1].id).len(), 1, "env gate");
}

/// #783: a PR whose only CI is gated has no rollup, and its waiting runs
/// are the whole Checks total.
#[test]
fn the_gated_only_scenario_has_only_waiting_runs() {
    let d = detail(109);
    assert_eq!(d.checks.total_count, 0);
    assert!(d.checks.nodes.is_empty());
    let page: GatedRunsPage = serde_json::from_str(&rest(
        "actions/runs?branch=x&event=pull_request&status=action_required&per_page=50",
    ))
    .unwrap();
    let held: Vec<_> = page
        .workflow_runs
        .iter()
        .filter(|run| run.head_sha.as_deref() == Some(sha(&d)))
        .collect();
    assert_eq!(held.len(), 2);
    for run in held {
        let probe = rest(&format!("actions/runs/{}/pending_deployments", run.id));
        let pending: Vec<PendingDeploymentNode> = serde_json::from_str(&probe).unwrap();
        assert!(pending.is_empty(), "fork gate");
    }
}

#[test]
fn the_overflow_scenario_ships_true_totals_over_capped_nodes() {
    let d = detail(102);
    assert_eq!(d.title.chars().count(), 200);
    assert_eq!((d.labels_total, d.labels.len()), (34, 20));
    assert_eq!(d.assignees_total, 12);
    assert_eq!(d.review_requests_total, 13);
    assert_eq!((d.checks.total_count, d.checks.nodes.len()), (137, 100));
    assert_eq!((d.commits.total_count, d.commits.nodes.len()), (137, 100));
    assert_eq!((d.comments.total_count, d.comments.nodes.len()), (240, 100));
}

#[test]
fn the_state_scenarios_read_as_named() {
    let draft = detail(103);
    assert!(draft.draft);
    assert_eq!(draft.body.as_deref(), Some(""));
    assert!(draft.checks.nodes.is_empty());

    let merged = detail(104);
    assert_eq!(merged.state.as_deref(), Some("MERGED"));
    assert!(merged.merge_methods.squash && !merged.merge_methods.merge);

    let closed = detail(105);
    assert_eq!(closed.state.as_deref(), Some("CLOSED"));
    assert_eq!(closed.viewer_permission.as_deref(), Some("READ"));
    assert_eq!(closed.mergeable.as_deref(), Some("CONFLICTING"));

    let clean = detail(106);
    assert_eq!(clean.state.as_deref(), Some("OPEN"));
    assert_eq!(clean.merge_state_status.as_deref(), Some("CLEAN"));
    assert_eq!(clean.viewer_permission.as_deref(), Some("READ"));
    assert!(!clean.viewer_can_update && !clean.viewer_can_label);
}

#[test]
fn the_edge_scenario_keeps_its_ghosts_and_duplicates() {
    let d = detail(107);
    assert!(d.author.is_none());
    assert_eq!(d.reviews.nodes.len(), 3, "no review drops");
    assert!(d.reviews.nodes.iter().any(|r| r.author.is_none()));
    let pending = d
        .reviews
        .nodes
        .iter()
        .find(|r| r.state.as_deref() == Some("PENDING"))
        .unwrap();
    assert!(pending.submitted_at.is_none());
    assert!(d.comments.nodes[0].author.is_none());
    let builds = d.checks.nodes.iter().filter(|c| c.name == "build").count();
    assert_eq!(builds, 2);
    assert!(d.commits.nodes[0].author_login.is_none());
}

/// The error scenario is what the read path turns into the load error:
/// a 200 with `errors` and no PR.
#[test]
fn the_error_scenario_carries_no_pr() {
    let env = detail_envelope(108);
    assert_eq!(env.errors.len(), 1);
    let parsed = env
        .data
        .and_then(|d| d.repository)
        .as_ref()
        .and_then(gql::pr_detail_from_value);
    assert!(parsed.is_none());
}

/// The lane lists every scenario on beacon; atlas's stays empty, which
/// its goldens and walk-derived ids depend on.
#[test]
fn the_lane_lists_the_scenarios_on_beacon_only() {
    let lane = |name: &str| -> serde_json::Value {
        let vars = serde_json::json!({ "owner": "fixture-user", "name": name });
        let res = http().post_graphql(gql::REPO_PULLS_QUERY, &vars).unwrap();
        serde_json::from_str(res.body.as_deref().unwrap()).unwrap()
    };
    let beacon = lane("beacon");
    let repo = &beacon["data"]["repository"];
    assert_eq!(repo["nameWithOwner"], "fixture-user/beacon");
    let rows = gql::repo_from_value(repo).unwrap().prs.nodes;
    let numbers: Vec<i64> = rows.iter().map(|r| r.number).collect();
    assert_eq!(numbers, (101..=109).collect::<Vec<_>>());
    let atlas = lane("atlas");
    assert_eq!(atlas["data"]["repository"]["pullRequests"]["totalCount"], 0);
}

/// The rail pickers' source pages parse into rows they can write back.
#[test]
fn the_picker_pages_parse() {
    let page = |query: &str| -> serde_json::Value {
        let res = http().post_graphql(query, &variables(101)).unwrap();
        let env: Envelope<RepoPickerData> =
            serde_json::from_str(res.body.as_deref().unwrap()).unwrap();
        env.data.and_then(|d| d.repository).unwrap()
    };
    assert_eq!(
        gql::repo_labels_from_value(&page(gql::REPO_LABELS_QUERY)).len(),
        62
    );
    assert_eq!(
        gql::repo_assignable_users_from_value(&page(gql::REPO_ASSIGNABLE_USERS_QUERY)).len(),
        52
    );
    assert_eq!(
        gql::repo_milestones_from_value(&page(gql::REPO_MILESTONES_QUERY)).len(),
        2
    );
}
