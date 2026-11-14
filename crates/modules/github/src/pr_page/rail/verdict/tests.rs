//! Table tests for `verdict.rs`.

use super::*;
use crate::pr_page::rail::test_support::detail;
use serde_json::{Value, json};

fn open(patch: Value) -> Verdict {
    let mut base = json!({"mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN"});
    if let (Value::Object(b), Value::Object(p)) = (&mut base, patch) {
        b.extend(p);
    }
    verdict(&detail(base))
}

/// One case per row of D2's table, in table order, plus the
/// first-match-wins neighbours that could shadow it.
#[test]
fn one_case_per_d2_row() {
    let cases: Vec<(Value, Row, Glyph, Tone, &str)> = vec![
        (
            json!({"state": "MERGED", "merged": true}),
            Row::Merged,
            Glyph::Merged,
            Tone::Merged,
            "Merged",
        ),
        (
            json!({"state": "CLOSED"}),
            Row::Closed,
            Glyph::CircleX,
            Tone::Danger,
            "Closed",
        ),
        (
            json!({"isDraft": true}),
            Row::Draft,
            Glyph::Dash,
            Tone::Muted,
            "Draft",
        ),
        (
            json!({"isInMergeQueue": true}),
            Row::Queued,
            Glyph::Spinner,
            Tone::Warning,
            "Queued to merge",
        ),
        (
            json!({"autoMergeRequest": {"enabledBy": {"login": "dev"}, "mergeMethod": "SQUASH"}}),
            Row::AutoMerge,
            Glyph::Spinner,
            Tone::Warning,
            "Auto-merge enabled",
        ),
        (
            json!({"mergeable": "CONFLICTING"}),
            Row::Conflicts,
            Glyph::CircleX,
            Tone::Danger,
            "Conflicts",
        ),
        (
            json!({"mergeStateStatus": "DIRTY"}),
            Row::Conflicts,
            Glyph::CircleX,
            Tone::Danger,
            "Conflicts",
        ),
        (
            json!({"mergeStateStatus": "UNKNOWN"}),
            Row::Checking,
            Glyph::Spinner,
            Tone::Muted,
            "Checking…",
        ),
        (
            json!({"mergeable": "UNKNOWN"}),
            Row::Checking,
            Glyph::Spinner,
            Tone::Muted,
            "Checking…",
        ),
        (
            json!({"mergeStateStatus": null}),
            Row::Checking,
            Glyph::Spinner,
            Tone::Muted,
            "Checking…",
        ),
        (
            json!({"mergeable": null}),
            Row::Checking,
            Glyph::Spinner,
            Tone::Muted,
            "Checking…",
        ),
        (
            json!({"mergeStateStatus": "BLOCKED"}),
            Row::Blocked,
            Glyph::CircleX,
            Tone::Danger,
            "Merging is blocked",
        ),
        (
            json!({"mergeStateStatus": "BEHIND"}),
            Row::OutOfDate,
            Glyph::TriangleAlert,
            Tone::Warning,
            "Out of date",
        ),
        (
            json!({"mergeStateStatus": "UNSTABLE"}),
            Row::ChecksFailing,
            Glyph::TriangleAlert,
            Tone::Warning,
            "Mergeable, checks failing",
        ),
        (
            json!({}),
            Row::Ready,
            Glyph::CircleCheck,
            Tone::Success,
            "Ready to merge",
        ),
        (
            json!({"mergeStateStatus": "HAS_HOOKS"}),
            Row::Ready,
            Glyph::CircleCheck,
            Tone::Success,
            "Ready to merge",
        ),
        (
            json!({"mergeStateStatus": "DRAFT"}),
            Row::Unknown,
            Glyph::CircleAlert,
            Tone::Muted,
            "Merge status unknown",
        ),
        (
            json!({"mergeStateStatus": "SOMETHING_NEW"}),
            Row::Unknown,
            Glyph::CircleAlert,
            Tone::Muted,
            "Merge status unknown",
        ),
    ];
    for (patch, row, glyph, tone, word) in cases {
        let got = open(patch.clone());
        assert_eq!(
            (got.row, got.glyph, got.tone, got.word),
            (row, glyph, tone, word),
            "{patch}"
        );
        assert!(!got.sentence.is_empty());
    }
}

#[test]
fn earlier_rows_shadow_later_ones() {
    // A merged PR reports UNKNOWN mergeability on GitHub.
    let merged =
        open(json!({"state": "MERGED", "mergeable": "UNKNOWN", "mergeStateStatus": "UNKNOWN"}));
    assert_eq!(merged.row, Row::Merged);
    // A closed PR beats its stale DIRTY state.
    assert_eq!(
        open(json!({"state": "CLOSED", "mergeStateStatus": "DIRTY"})).row,
        Row::Closed
    );
    // A draft beats its BLOCKED state, and a queued PR its conflicts.
    assert_eq!(
        open(json!({"isDraft": true, "mergeStateStatus": "BLOCKED"})).row,
        Row::Draft
    );
    assert_eq!(
        open(json!({"isInMergeQueue": true, "mergeable": "CONFLICTING"})).row,
        Row::Queued
    );
    // Conflicts beat "checking": DIRTY with an UNKNOWN mergeable is still
    // a conflict (row 6 before row 7).
    assert_eq!(
        open(json!({"mergeStateStatus": "DIRTY", "mergeable": "UNKNOWN"})).row,
        Row::Conflicts
    );
}

#[test]
fn the_blocked_tooltip_joins_the_failing_gates() {
    let v = open(json!({"mergeStateStatus": "BLOCKED"}));
    assert_eq!(
        v.blocked_tooltip(&["changes requested".into(), "1 check failed".into()]),
        "Merging is blocked: changes requested, 1 check failed"
    );
    assert_eq!(
        v.blocked_tooltip(&[]),
        "Merging is blocked by required reviews or status checks"
    );
}
