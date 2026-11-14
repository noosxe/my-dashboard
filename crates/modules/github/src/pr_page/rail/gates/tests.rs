//! Table tests for `gates.rs`.

use super::*;
use crate::pr_page::rail::test_support::detail;
use serde_json::{Value, json};

fn pr(patch: Value) -> PrDetail {
    let mut base = json!({"mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN", "baseRefName": "main",
        "reviewRequests": {"totalCount": 0, "nodes": []}});
    if let (Value::Object(b), Value::Object(p)) = (&mut base, patch) {
        b.extend(p);
    }
    detail(base)
}

fn row(d: &PrDetail, waiting: usize, kind: Kind) -> Gate {
    gates(d, waiting)
        .into_iter()
        .find(|g| g.kind == kind)
        .unwrap_or_else(|| panic!("{kind:?} row"))
}

fn latest(rows: &[(&str, &str, &str)]) -> Value {
    json!({"nodes": rows.iter().map(|(id, login, state)|
        json!({"id": id, "state": state, "author": {"login": login, "avatarUrl": "a"}})).collect::<Vec<_>>()})
}

type Summary<'a> = (Glyph, Tone, &'a str, Option<&'a str>);

fn summary(g: &Gate) -> Summary<'_> {
    (g.glyph, g.tone, g.label.as_str(), g.value.as_deref())
}

#[test]
fn reviews_table() {
    let one_request = json!({"totalCount": 2, "nodes": [
        {"requestedReviewer": {"__typename": "User", "id": "U", "login": "carol"}},
        {"requestedReviewer": {"__typename": "User", "id": "V", "login": "dan"}}]});
    let cases: Vec<(Value, Summary)> = vec![
        (
            json!({"reviewDecision": "CHANGES_REQUESTED", "latestOpinionatedReviews": latest(&[("R1", "bea", "CHANGES_REQUESTED")])}),
            (
                Glyph::CircleX,
                Tone::Danger,
                "Changes requested",
                Some("bea"),
            ),
        ),
        (
            json!({"reviewDecision": "CHANGES_REQUESTED", "latestOpinionatedReviews": latest(&[("R1", "bea", "CHANGES_REQUESTED"), ("R2", "cy", "CHANGES_REQUESTED")])}),
            (
                Glyph::CircleX,
                Tone::Danger,
                "Changes requested",
                Some("2 reviewers"),
            ),
        ),
        (
            json!({"reviewDecision": "REVIEW_REQUIRED", "reviewRequests": one_request}),
            (
                Glyph::TriangleAlert,
                Tone::Warning,
                "Review required",
                Some("2 pending"),
            ),
        ),
        (
            json!({"reviewDecision": "REVIEW_REQUIRED"}),
            (Glyph::TriangleAlert, Tone::Warning, "Review required", None),
        ),
        (
            json!({"reviewDecision": "APPROVED", "latestOpinionatedReviews": latest(&[("R1", "a", "APPROVED"), ("R2", "b", "APPROVED")])}),
            (
                Glyph::CircleCheck,
                Tone::Success,
                "Approved",
                Some("2 approvals"),
            ),
        ),
        (
            json!({"reviewDecision": "APPROVED", "latestOpinionatedReviews": latest(&[("R1", "a", "APPROVED")])}),
            (
                Glyph::CircleCheck,
                Tone::Success,
                "Approved",
                Some("1 approval"),
            ),
        ),
        (
            json!({"reviewDecision": null, "latestOpinionatedReviews": latest(&[("R1", "bea", "CHANGES_REQUESTED")])}),
            (
                Glyph::TriangleAlert,
                Tone::Warning,
                "Changes requested",
                Some("bea"),
            ),
        ),
        (
            json!({"reviewDecision": null, "latestOpinionatedReviews": latest(&[("R1", "a", "APPROVED")])}),
            (
                Glyph::CircleCheck,
                Tone::Success,
                "Approved",
                Some("1 approval"),
            ),
        ),
        (
            json!({"reviewDecision": null}),
            (Glyph::Dash, Tone::Muted, "No reviews yet", None),
        ),
        (
            json!({"reviewDecision": null, "reviewRequests": one_request}),
            (
                Glyph::Dash,
                Tone::Muted,
                "No reviews yet",
                Some("2 pending"),
            ),
        ),
    ];
    for (patch, want) in cases {
        let d = pr(patch.clone());
        assert_eq!(summary(&row(&d, 0, Kind::Reviews)), want, "{patch}");
    }
}

#[test]
fn the_reviews_row_goes_to_the_newest_change_request_else_approval() {
    let d = pr(json!({
        "reviewDecision": "CHANGES_REQUESTED",
        "latestOpinionatedReviews": latest(&[("PRR_1", "a", "APPROVED"), ("PRR_2", "b", "CHANGES_REQUESTED")]),
        "reviews": {"totalCount": 2, "nodes": [
            {"id": "PRR_1", "state": "APPROVED", "author": {"login": "a"}},
            {"id": "PRR_2", "state": "CHANGES_REQUESTED", "author": {"login": "b"}}]},
    }));
    assert_eq!(row(&d, 0, Kind::Reviews).go, Go::Review("PRR_2".into()));
    let d = pr(json!({
        "reviewDecision": "APPROVED",
        "latestOpinionatedReviews": latest(&[("PRR_1", "a", "APPROVED"), ("PRR_3", "c", "APPROVED")]),
        "reviews": {"totalCount": 2, "nodes": [
            {"id": "PRR_1", "state": "APPROVED", "author": {"login": "a"}},
            {"id": "PRR_3", "state": "APPROVED", "author": {"login": "c"}}]},
    }));
    assert_eq!(row(&d, 0, Kind::Reviews).go, Go::Review("PRR_3".into()));
    // Outside the loaded window: still a target, the anchor gives up.
    let d = pr(
        json!({"reviewDecision": "APPROVED", "latestOpinionatedReviews": latest(&[("PRR_9", "a", "APPROVED")])}),
    );
    assert_eq!(row(&d, 0, Kind::Reviews).go, Go::Review("PRR_9".into()));
    // Nothing behind the state: not interactive.
    let d = pr(json!({"reviewDecision": "REVIEW_REQUIRED"}));
    assert_eq!(row(&d, 0, Kind::Reviews).go, Go::Nowhere);
}

type Counts<'a> = &'a [(&'a str, u32)];

fn checks_pr(counts: (Counts, Counts), rollup: Option<&str>) -> PrDetail {
    let list = |l: &[(&str, u32)]| {
        l.iter()
            .map(|(s, c)| json!({"state": s, "count": c}))
            .collect::<Vec<_>>()
    };
    let total: u32 = counts.0.iter().chain(counts.1).map(|(_, c)| c).sum();
    pr(
        json!({"statusCheckRollup": rollup.map(|state| json!({"state": state, "contexts": {
        "totalCount": total, "nodes": [],
        "checkRunCountsByState": list(counts.0), "statusContextCountsByState": list(counts.1)}}))}),
    )
}

#[test]
fn checks_counts_table() {
    type Want = (Glyph, Tone, &'static str, Option<&'static str>);
    let cases: Vec<(PrDetail, usize, Want)> = vec![
        (
            checks_pr(
                (&[("FAILURE", 1), ("IN_PROGRESS", 1), ("SUCCESS", 4)], &[]),
                Some("FAILURE"),
            ),
            2,
            (
                Glyph::CircleX,
                Tone::Danger,
                "Checks",
                Some("1 failed · 1 running · 2 waiting"),
            ),
        ),
        (
            checks_pr(
                (&[("IN_PROGRESS", 2), ("SUCCESS", 1)], &[("PENDING", 1)]),
                Some("PENDING"),
            ),
            0,
            (Glyph::Spinner, Tone::Warning, "Checks", Some("3 running")),
        ),
        (
            checks_pr(
                (&[("SUCCESS", 5), ("SKIPPED", 1), ("NEUTRAL", 1)], &[]),
                Some("SUCCESS"),
            ),
            0,
            (
                Glyph::CircleCheck,
                Tone::Success,
                "Checks passed",
                Some("7 of 7"),
            ),
        ),
        (
            checks_pr((&[("SUCCESS", 3)], &[]), Some("SUCCESS")),
            1,
            (Glyph::Spinner, Tone::Warning, "Checks", Some("1 waiting")),
        ),
        // Only gated runs: the rollup can't see them.
        (
            pr(json!({"statusCheckRollup": null})),
            2,
            (Glyph::Spinner, Tone::Warning, "Checks", Some("2 waiting")),
        ),
        (
            pr(json!({"statusCheckRollup": null})),
            0,
            (Glyph::Dash, Tone::Muted, "No checks on this branch", None),
        ),
        // A rollup state this build doesn't know, with no counts.
        (
            checks_pr((&[], &[]), Some("MYSTERY")),
            0,
            (
                Glyph::CircleAlert,
                Tone::Muted,
                "Checks status unknown",
                None,
            ),
        ),
        // The rollup state alone still reads (payload without counts).
        (
            checks_pr((&[], &[]), Some("SUCCESS")),
            0,
            (Glyph::CircleCheck, Tone::Success, "Checks passed", None),
        ),
        (
            checks_pr((&[], &[]), Some("FAILURE")),
            0,
            (Glyph::CircleX, Tone::Danger, "Checks", None),
        ),
    ];
    for (i, (d, waiting, want)) in cases.into_iter().enumerate() {
        assert_eq!(summary(&row(&d, waiting, Kind::Checks)), want, "case {i}");
        assert_eq!(row(&d, waiting, Kind::Checks).go, Go::ChecksTab);
    }
}

#[test]
fn counts_come_from_the_exact_lists_when_the_page_is_capped() {
    // 137 checks, 100 listed: the lists decide, not the nodes.
    let d = checks_pr(
        (&[("SUCCESS", 127), ("IN_PROGRESS", 10)], &[]),
        Some("PENDING"),
    );
    assert_eq!(
        row(&d, 0, Kind::Checks).value.as_deref(),
        Some("10 running")
    );
}

fn merge(patch: Value) -> Option<Gate> {
    gates(&pr(patch), 0)
        .into_iter()
        .find(|g| g.kind == Kind::Merge)
}

#[test]
fn mergeability_table() {
    type Want = (Glyph, Tone, &'static str, Trailing);
    let cases: Vec<(Value, Want)> = vec![
        (
            json!({"mergeable": "CONFLICTING", "mergeStateStatus": "DIRTY"}),
            (
                Glyph::CircleX,
                Tone::Danger,
                "Conflicts with main",
                Trailing::ResolveOnGithub,
            ),
        ),
        (
            json!({"mergeStateStatus": "BEHIND", "viewerCanUpdateBranch": true}),
            (
                Glyph::TriangleAlert,
                Tone::Warning,
                "Out of date with main",
                Trailing::Update,
            ),
        ),
        (
            json!({"mergeStateStatus": "BEHIND"}),
            (
                Glyph::TriangleAlert,
                Tone::Warning,
                "Out of date with main",
                Trailing::None,
            ),
        ),
        (
            json!({"viewerCanUpdateBranch": true}),
            (
                Glyph::CircleCheck,
                Tone::Success,
                "No conflicts, behind main",
                Trailing::Update,
            ),
        ),
        (
            json!({}),
            (
                Glyph::CircleCheck,
                Tone::Success,
                "No conflicts",
                Trailing::None,
            ),
        ),
        (
            json!({"mergeable": "UNKNOWN"}),
            (
                Glyph::Spinner,
                Tone::Muted,
                "Checking for conflicts",
                Trailing::None,
            ),
        ),
    ];
    for (patch, (glyph, tone, label, trailing)) in cases {
        let g = merge(patch.clone()).expect("row");
        assert_eq!(
            (g.glyph, g.tone, g.label.as_str(), g.trailing),
            (glyph, tone, label, trailing),
            "{patch}"
        );
        assert_eq!(g.go, Go::Nowhere);
    }
}

#[test]
fn terminal_states_drop_rows() {
    let ids = |d: &PrDetail| gates(d, 0).iter().map(|g| g.kind).collect::<Vec<_>>();
    assert_eq!(
        ids(&pr(json!({}))),
        [Kind::Reviews, Kind::Checks, Kind::Merge]
    );
    assert_eq!(
        ids(&pr(json!({"isDraft": true}))),
        [Kind::Reviews, Kind::Checks, Kind::Merge]
    );
    // Merged: Mergeability is gone.
    assert_eq!(
        ids(&pr(json!({"state": "MERGED", "merged": true}))),
        [Kind::Reviews, Kind::Checks]
    );
    // Closed: Reviews, and Mergeability only for conflicts.
    assert_eq!(ids(&pr(json!({"state": "CLOSED"}))), [Kind::Reviews]);
    assert_eq!(
        ids(&pr(
            json!({"state": "CLOSED", "mergeable": "CONFLICTING", "mergeStateStatus": "DIRTY"})
        )),
        [Kind::Reviews, Kind::Merge]
    );
}

#[test]
fn a_navigating_row_says_where_it_goes() {
    let d = checks_pr(
        (&[("FAILURE", 1), ("IN_PROGRESS", 1)], &[]),
        Some("FAILURE"),
    );
    assert_eq!(
        row(&d, 2, Kind::Checks).spoken(),
        "Checks: 1 failed, 1 running, 2 waiting. Open the Checks tab"
    );
    let d = pr(json!({"reviewDecision": "REVIEW_REQUIRED"}));
    assert_eq!(row(&d, 0, Kind::Reviews).spoken(), "Review required");
}

#[test]
fn blocking_reasons_name_the_gates_that_hold_the_merge_back() {
    let d = pr(json!({
        "reviewDecision": "CHANGES_REQUESTED",
        "latestOpinionatedReviews": latest(&[("R1", "bea", "CHANGES_REQUESTED")]),
        "statusCheckRollup": {"state": "FAILURE", "contexts": {"totalCount": 1, "nodes": [],
            "checkRunCountsByState": [{"state": "FAILURE", "count": 1}]}}
    }));
    let reasons: Vec<_> = gates(&d, 0)
        .iter()
        .filter_map(|g| g.blocking_reason(&d))
        .collect();
    assert_eq!(reasons, ["changes requested", "1 check failed"]);
}
