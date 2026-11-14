//! The three gate rows under the verdict (docs/0083 D3): Reviews, Checks
//! and Mergeability, each an icon, a short label and an optional muted
//! value. This file is the pure row model; `gate_row.rs` draws it.

use super::verdict::{self, Glyph, Tone};
use crate::gql::{CheckCounts, CheckState, PrDetail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Reviews,
    Checks,
    Merge,
}

impl Kind {
    /// The probe key (docs/0083 D14).
    pub(crate) fn id(self) -> &'static str {
        match self {
            Kind::Reviews => "pr-rail-gate-reviews",
            Kind::Checks => "pr-rail-gate-checks",
            Kind::Merge => "pr-rail-gate-merge",
        }
    }
}

/// Where activating the row goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Go {
    /// A static row.
    Nowhere,
    /// Scroll the timeline to this review node (docs/0026 D9).
    Review(String),
    ChecksTab,
}

/// What sits at the row's trailing edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Trailing {
    None,
    /// "Resolve on GitHub": the PR's conflicts page.
    ResolveOnGithub,
    /// The Update split button (docs/0067), when the server gate is open.
    Update,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Gate {
    pub kind: Kind,
    pub glyph: Glyph,
    pub tone: Tone,
    pub label: String,
    pub value: Option<String>,
    pub go: Go,
    pub trailing: Trailing,
}

impl Gate {
    /// The a11y name: label, value and, for a navigating row, where it
    /// goes ("Checks: 1 failed, 1 running. Open the Checks tab").
    pub(crate) fn spoken(&self) -> String {
        let mut out = self.label.clone();
        if let Some(value) = &self.value {
            out.push_str(": ");
            out.push_str(&value.replace(" · ", ", "));
        }
        match self.go {
            Go::Nowhere => {}
            Go::Review(_) => out.push_str(". Show the review"),
            Go::ChecksTab => out.push_str(". Open the Checks tab"),
        }
        out
    }

    /// The reason this row gives for a blocked merge, if it blocks.
    pub(crate) fn blocking_reason(&self, d: &PrDetail) -> Option<String> {
        match self.kind {
            Kind::Reviews => match d.review_decision.as_deref() {
                Some("CHANGES_REQUESTED") => Some("changes requested".into()),
                Some("REVIEW_REQUIRED") => Some("review required".into()),
                _ => None,
            },
            Kind::Checks => {
                let failed = checks(d).failed;
                (failed > 0).then(|| {
                    format!(
                        "{failed} check{} failed",
                        if failed == 1 { "" } else { "s" }
                    )
                })
            }
            Kind::Merge => verdict::conflicts(d).then(|| "conflicts".into()),
        }
    }
}

/// The rows to show for this PR, in order. Merged PRs drop Mergeability;
/// closed PRs drop Checks and keep Mergeability only for conflicts (D6).
pub(crate) fn gates(d: &PrDetail, waiting: usize) -> Vec<Gate> {
    let terminal_merged = d.state.as_deref() == Some("MERGED") || d.ending.merged;
    let closed = d.state.as_deref() == Some("CLOSED");
    let mut rows = vec![reviews(d)];
    if !closed {
        rows.push(checks_row(d, waiting));
    }
    if !terminal_merged && let Some(row) = merge_row(d, closed) {
        rows.push(row);
    }
    rows
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

// ---- Reviews -------------------------------------------------------------

fn reviews(d: &PrDetail) -> Gate {
    let changes: Vec<_> = d
        .latest_opinionated_reviews
        .iter()
        .filter(|r| r.state == "CHANGES_REQUESTED")
        .collect();
    let approvals = d
        .latest_opinionated_reviews
        .iter()
        .filter(|r| r.state == "APPROVED")
        .count();
    let pending = d.review_requests_total.max(d.review_requests.len() as i64) as usize;
    let pending_value = (pending > 0).then(|| format!("{pending} pending"));
    let changes_value = match changes.as_slice() {
        [] => None,
        [one] => Some(one.login.clone()),
        many => Some(format!("{} reviewers", many.len())),
    };
    let approved_value = (approvals > 0).then(|| plural(approvals, "approval", "approvals"));

    // The decision first; GitHub's `null` (no review rule) falls back to
    // what the latest reviews show.
    let (glyph, tone, label, value) = match d.review_decision.as_deref() {
        Some("CHANGES_REQUESTED") => (
            Glyph::CircleX,
            Tone::Danger,
            "Changes requested",
            changes_value,
        ),
        Some("REVIEW_REQUIRED") => (
            Glyph::TriangleAlert,
            Tone::Warning,
            "Review required",
            pending_value,
        ),
        Some("APPROVED") => (
            Glyph::CircleCheck,
            Tone::Success,
            "Approved",
            approved_value,
        ),
        _ if !changes.is_empty() => (
            Glyph::TriangleAlert,
            Tone::Warning,
            "Changes requested",
            changes_value,
        ),
        _ if approvals > 0 => (
            Glyph::CircleCheck,
            Tone::Success,
            "Approved",
            approved_value,
        ),
        _ => (Glyph::Dash, Tone::Muted, "No reviews yet", pending_value),
    };
    // The newest change request, else the newest approval.
    let target = if !changes.is_empty() {
        newest(d, "CHANGES_REQUESTED")
    } else {
        newest(d, "APPROVED")
    };
    Gate {
        kind: Kind::Reviews,
        glyph,
        tone,
        label: label.into(),
        value,
        go: target.map_or(Go::Nowhere, Go::Review),
        trailing: Trailing::None,
    }
}

/// The node id of the newest review in `state` among the latest per
/// reviewer: the later one in the timeline's order when it is loaded,
/// else the first listed (the anchor then gives up quietly when the
/// review is outside the newest-100 window, docs/0016 §14).
fn newest(d: &PrDetail, state: &str) -> Option<String> {
    let ids: Vec<&str> = d
        .latest_opinionated_reviews
        .iter()
        .filter(|r| r.state == state && !r.id.is_empty())
        .map(|r| r.id.as_str())
        .collect();
    d.reviews
        .nodes
        .iter()
        .rev()
        .find(|r| ids.contains(&r.id.as_str()))
        .map(|r| r.id.clone())
        .or_else(|| ids.first().map(|id| id.to_string()))
}

// ---- Checks --------------------------------------------------------------

/// The counts behind the row: the query's exact counts, or, for a payload
/// persisted before they existed, the listed rows.
pub(crate) fn checks(d: &PrDetail) -> CheckCounts {
    if d.checks.counts.total() > 0 {
        return d.checks.counts;
    }
    let mut counts = CheckCounts::default();
    for c in &d.checks.nodes {
        match c.state {
            CheckState::Failure => counts.failed += 1,
            CheckState::Pending | CheckState::Waiting => counts.running += 1,
            CheckState::Success | CheckState::Neutral | CheckState::Cancelled => counts.passed += 1,
        }
    }
    counts
}

fn checks_row(d: &PrDetail, waiting: usize) -> Gate {
    let counts = checks(d);
    let waiting = waiting as u32;
    let rollup = d.rollup_state.as_deref();
    let nothing = counts.total() == 0 && waiting == 0;
    let (glyph, tone, label) = if counts.failed > 0 {
        (Glyph::CircleX, Tone::Danger, "Checks")
    } else if counts.running > 0 || waiting > 0 {
        (Glyph::Spinner, Tone::Warning, "Checks")
    } else if !nothing {
        (Glyph::CircleCheck, Tone::Success, "Checks passed")
    } else {
        // No counts at all: fall back to the rollup state, the signal a
        // payload without the lists still carries.
        match rollup {
            None => (Glyph::Dash, Tone::Muted, "No checks on this branch"),
            Some("SUCCESS") => (Glyph::CircleCheck, Tone::Success, "Checks passed"),
            Some("FAILURE" | "ERROR") => (Glyph::CircleX, Tone::Danger, "Checks"),
            Some("PENDING" | "EXPECTED") => (Glyph::Spinner, Tone::Warning, "Checks"),
            Some(_) => (Glyph::CircleAlert, Tone::Muted, "Checks status unknown"),
        }
    };
    let mut parts = Vec::new();
    for (n, word) in [
        (counts.failed, "failed"),
        (counts.running, "running"),
        (waiting, "waiting"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {word}"));
        }
    }
    let value = if !parts.is_empty() {
        Some(parts.join(" · "))
    } else if counts.total() > 0 {
        Some(format!("{} of {}", counts.passed, counts.total()))
    } else {
        None
    };
    Gate {
        kind: Kind::Checks,
        glyph,
        tone,
        label: label.into(),
        value,
        go: Go::ChecksTab,
        trailing: Trailing::None,
    }
}

// ---- Mergeability --------------------------------------------------------

fn merge_row(d: &PrDetail, closed: bool) -> Option<Gate> {
    let base = d.base_ref.as_deref().unwrap_or("the base branch");
    let can_update = d.viewer_can_update_branch && d.state.as_deref() == Some("OPEN");
    let row = |glyph, tone, label: String, trailing| Gate {
        kind: Kind::Merge,
        glyph,
        tone,
        label,
        value: None,
        go: Go::Nowhere,
        trailing,
    };
    if verdict::conflicts(d) {
        return Some(row(
            Glyph::CircleX,
            Tone::Danger,
            format!("Conflicts with {base}"),
            Trailing::ResolveOnGithub,
        ));
    }
    if closed {
        return None;
    }
    let update = if can_update {
        Trailing::Update
    } else {
        Trailing::None
    };
    Some(if verdict::checking(d) {
        row(
            Glyph::Spinner,
            Tone::Muted,
            "Checking for conflicts".into(),
            Trailing::None,
        )
    } else if d.merge_state_status.as_deref() == Some("BEHIND") {
        row(
            Glyph::TriangleAlert,
            Tone::Warning,
            format!("Out of date with {base}"),
            update,
        )
    } else if can_update {
        row(
            Glyph::CircleCheck,
            Tone::Success,
            format!("No conflicts, behind {base}"),
            update,
        )
    } else {
        row(
            Glyph::CircleCheck,
            Tone::Success,
            "No conflicts".into(),
            Trailing::None,
        )
    })
}

#[cfg(test)]
mod tests {
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
}
