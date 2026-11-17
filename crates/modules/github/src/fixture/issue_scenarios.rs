//! The committed issue scenarios (docs/0085 D9, #923) parse with the code's
//! own parser, served through the fixture transport by number, so a shape
//! that drifts from `gql.rs` fails here rather than as a blank page.

use std::path::{Path, PathBuf};

use super::FixtureHttp;
use crate::gql::{self, Envelope, IssueDetail, IssueDetailData, XrefState};
use crate::http::Http;

fn detail(number: i64) -> IssueDetail {
    let dir: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../debug/verify/fixtures/github");
    let vars = serde_json::json!({ "owner": "fixture-user", "name": "beacon", "number": number });
    let res = FixtureHttp::with_rate(dir, None)
        .post_graphql(gql::ISSUE_DETAIL_QUERY, &vars)
        .unwrap();
    assert_eq!(res.status, 200, "IssueDetail.{number}");
    let env: Envelope<IssueDetailData> =
        serde_json::from_str(res.body.as_deref().unwrap()).unwrap();
    assert!(env.errors.is_empty());
    env.data
        .and_then(|d| d.repository)
        .as_ref()
        .and_then(gql::issue_detail_from_value)
        .unwrap_or_else(|| panic!("IssueDetail.{number} doesn't parse"))
}

#[test]
fn the_open_issue_has_a_pr_that_will_close_it() {
    let d = detail(201);
    assert_eq!(d.state.as_deref(), Some("OPEN"));
    let closer = d.references.nodes.last().unwrap();
    assert!(closer.will_close && closer.source.is_pr);
    assert_eq!(closer.source.state, XrefState::Draft);
    assert_eq!(closer.source.repo, "fixture-user/beacon");
    let repos: Vec<_> = d
        .references
        .nodes
        .iter()
        .map(|e| e.source.repo.as_str())
        .collect();
    assert!(repos.contains(&"fixture-user/atlas") && repos.contains(&"fixture-org/elsewhere"));
    assert!(d.references.nodes.iter().any(|e| e.actor_login.is_none()));
    assert_eq!((d.comments.nodes.len(), d.events.len()), (1, 1));
}

#[test]
fn the_closed_issue_was_closed_by_a_merged_pr() {
    let d = detail(198);
    assert_eq!(d.state.as_deref(), Some("CLOSED"));
    let e = &d.references.nodes[0];
    assert!(e.will_close && e.source.state == XrefState::Merged);
}

#[test]
fn the_overflow_issue_carries_100_of_130_references() {
    let d = detail(202);
    assert_eq!(
        (d.references.total_count, d.references.nodes.len()),
        (130, 100)
    );
}
