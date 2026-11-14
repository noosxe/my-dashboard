//! Generation 1 of the committed PR #101 (docs/0084 §8): the four
//! changes land together in the GraphQL detail and in the REST files the
//! freshness probe reads, and generation 0 stays what the receipts have
//! always seen.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::FixtureHttp;
use crate::gql::{self, Envelope, PrDetail, PrDetailData};
use crate::http::{Http, Verb};

const REPO: &str = "repos/fixture-user/beacon";
const OLD_SHA: &str = "0000065bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const NEW_SHA: &str = "0000065ccccccccccccccccccccccccccccccccc";

fn http_at(generation: u64) -> FixtureHttp {
    let dir: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../debug/verify/fixtures/github");
    let http = FixtureHttp::with_rate(dir, None);
    http.generation.bump(Some(generation));
    http
}

fn detail(http: &FixtureHttp) -> PrDetail {
    let vars = serde_json::json!({ "owner": "fixture-user", "name": "beacon", "number": 101 });
    let res = http.post_graphql(gql::PR_DETAIL_QUERY, &vars).unwrap();
    let env: Envelope<PrDetailData> = serde_json::from_str(res.body.as_deref().unwrap()).unwrap();
    env.data
        .and_then(|d| d.repository)
        .as_ref()
        .and_then(gql::pr_detail_from_value)
        .expect("PrDetail.101 parses")
}

fn rest(http: &FixtureHttp, path: &str) -> Value {
    let res = http
        .request(
            Verb::Get,
            &format!("https://api.github.com/{REPO}/{path}"),
            None,
            None,
        )
        .unwrap();
    assert_eq!(res.status, 200, "{path}");
    serde_json::from_str(&res.body.unwrap()).unwrap()
}

#[test]
fn the_detail_gains_a_comment_a_label_a_head_and_a_finished_check() {
    let (g0, g1) = (detail(&http_at(0)), detail(&http_at(1)));
    assert_eq!((g0.comments.nodes.len(), g1.comments.nodes.len()), (2, 3));
    let labels = |d: &PrDetail| d.labels.iter().map(|l| l.name.clone()).collect::<Vec<_>>();
    assert!(labels(&g0).contains(&"needs-review".to_string()));
    assert!(labels(&g1).contains(&"approved".to_string()));
    assert!(!labels(&g1).contains(&"needs-review".to_string()));
    assert_eq!(g0.head_oid.as_deref(), Some(OLD_SHA));
    assert_eq!(g1.head_oid.as_deref(), Some(NEW_SHA));
    assert_eq!((g0.changed_files, g1.changed_files), (Some(5), Some(6)));
    let test_state = |d: &PrDetail| {
        d.checks
            .nodes
            .iter()
            .find(|c| c.name == "test (workspace)")
            .map(|c| c.state)
    };
    assert_eq!(test_state(&g0), Some(gql::CheckState::Pending));
    assert_eq!(test_state(&g1), Some(gql::CheckState::Success));
}

#[test]
fn the_probe_endpoints_agree_with_the_detail_at_each_generation() {
    for generation in [0, 1] {
        let http = http_at(generation);
        let pull = rest(&http, "pulls/101");
        let sha = pull["head"]["sha"].as_str().unwrap().to_string();
        assert_eq!(sha, if generation == 0 { OLD_SHA } else { NEW_SHA });
        assert_eq!(Some(sha.as_str()), detail(&http).head_oid.as_deref());
        assert_eq!(pull["comments"], 2 + generation);
        assert_eq!(pull["changed_files"], 5 + generation);

        let runs = rest(&http, &format!("commits/{sha}/check-runs?per_page=100"));
        let test = runs["check_runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "test (workspace)")
            .unwrap();
        let (status, conclusion) = (test["status"].as_str(), test["conclusion"].as_str());
        if generation == 0 {
            assert_eq!((status, conclusion), (Some("in_progress"), None));
        } else {
            assert_eq!((status, conclusion), (Some("completed"), Some("success")));
        }
        assert_eq!(
            rest(&http, &format!("commits/{sha}/status"))["sha"],
            sha.as_str()
        );
        let files = rest(&http, "pulls/101/files?per_page=100&page=1");
        assert_eq!(files.as_array().unwrap().len() as u64, 5 + generation);
    }
}

#[test]
fn the_pull_labels_follow_the_generation() {
    let names = |generation| -> Vec<String> {
        rest(&http_at(generation), "pulls/101")["labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert!(names(0).contains(&"needs-review".to_string()));
    assert_eq!(names(1), ["bug", "area:parser", "approved"]);
}

/// Other PRs have no generation files: bumping changes nothing for them.
#[test]
fn other_prs_are_unchanged_by_a_bump() {
    let (a, b) = (http_at(0), http_at(3));
    let vars = serde_json::json!({ "owner": "fixture-user", "name": "beacon", "number": 102 });
    let body = |h: &FixtureHttp| h.post_graphql(gql::PR_DETAIL_QUERY, &vars).unwrap().body;
    assert_eq!(body(&a), body(&b));
}
