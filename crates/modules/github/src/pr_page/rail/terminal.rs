//! The verdict's sub-line (docs/0083 D2, D4, D6): history for merged and
//! closed PRs, and the queue and auto-merge details. It appears only when
//! it adds something the header doesn't say.

use super::verdict::Row;
use crate::gql::PrDetail;

/// A run of the sub-line; branch names are set in mono.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub text: String,
    pub mono: bool,
}

fn plain(text: impl Into<String>) -> Span {
    Span {
        text: text.into(),
        mono: false,
    }
}

fn mono(text: impl Into<String>) -> Span {
    Span {
        text: text.into(),
        mono: true,
    }
}

/// "3d ago", or "just now" for a fresh timestamp; `None` when the payload
/// has no parseable time (`relative_age` answers "").
fn ago(age: &str) -> Option<String> {
    match age {
        "" => None,
        "now" => Some("just now".into()),
        a => Some(format!("{a} ago")),
    }
}

/// Merged: "by <who> · <age> into <base>" (the base in mono). GitHub
/// exposes no merge method, so the line never says "how".
pub(crate) fn merged(d: &PrDetail, age: &str) -> Vec<Span> {
    let mut out = Vec::new();
    if let Some(who) = &d.ending.merged_by {
        out.push(plain(format!("by {who}")));
    }
    let when = ago(age);
    if !out.is_empty() && when.is_some() {
        out.push(plain(" · "));
    }
    if let Some(when) = when {
        out.push(plain(when));
    }
    if let Some(base) = d.base_ref.as_deref().filter(|b| !b.is_empty()) {
        if !out.is_empty() {
            out.push(plain(" "));
        }
        out.push(plain("into "));
        out.push(mono(base));
    }
    out
}

/// Closed: "by <closer> · <age> · not merged"; without a closer event the
/// "by" part drops.
pub(crate) fn closed(d: &PrDetail, age: &str) -> Vec<Span> {
    let mut parts = Vec::new();
    if let Some(who) = &d.ending.closed_by {
        parts.push(format!("by {who}"));
    }
    parts.extend(ago(age));
    parts.push("not merged".into());
    vec![plain(parts.join(" · "))]
}

fn entry_state(state: &str) -> String {
    state.to_lowercase().replace('_', " ")
}

fn method_word(method: &str) -> &'static str {
    match method {
        "SQUASH" => "squash",
        "REBASE" => "rebase",
        _ => "merge commit",
    }
}

/// Queued: "Position N · <entry state>".
pub(crate) fn queued(d: &PrDetail) -> Vec<Span> {
    let mut parts = Vec::new();
    if let Some(n) = d.queueing.queue_position {
        parts.push(format!("Position {n}"));
    }
    if let Some(state) = &d.queueing.queue_state {
        parts.push(entry_state(state));
    }
    if parts.is_empty() {
        return Vec::new();
    }
    vec![plain(parts.join(" · "))]
}

/// Auto-merge: "by <login> · <method>".
pub(crate) fn auto_merge(d: &PrDetail) -> Vec<Span> {
    let Some(a) = &d.queueing.auto_merge else {
        return Vec::new();
    };
    let mut parts = Vec::new();
    if let Some(who) = &a.enabled_by {
        parts.push(format!("by {who}"));
    }
    if let Some(m) = &a.merge_method {
        parts.push(method_word(m).to_string());
    }
    if parts.is_empty() {
        return Vec::new();
    }
    vec![plain(parts.join(" · "))]
}

/// The sub-line for a verdict row, or `None` for rows without one.
/// `age` is the relative age of the row's own timestamp.
pub(crate) fn sub_line(
    row: Row,
    d: &PrDetail,
    merged_age: &str,
    closed_age: &str,
) -> Option<Vec<Span>> {
    let spans = match row {
        Row::Merged => merged(d, merged_age),
        Row::Closed => closed(d, closed_age),
        Row::Queued => queued(d),
        Row::AutoMerge => auto_merge(d),
        _ => return None,
    };
    (!spans.is_empty()).then_some(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr_page::rail::test_support::detail;
    use serde_json::json;

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    fn merged_pr() -> PrDetail {
        detail(
            json!({"state": "MERGED", "merged": true, "baseRefName": "main",
            "mergedBy": {"login": "maint"}, "mergedAt": "2026-11-01T00:00:00Z"}),
        )
    }

    #[test]
    fn merged_reads_who_when_and_into_which_base() {
        let spans = merged(&merged_pr(), "3d");
        assert_eq!(text(&spans), "by maint · 3d ago into main");
        assert_eq!(
            spans
                .iter()
                .filter(|s| s.mono)
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>(),
            ["main"]
        );
        assert_eq!(
            text(&merged(&merged_pr(), "now")),
            "by maint · just now into main"
        );
    }

    #[test]
    fn merged_degrades_without_a_name_or_a_time() {
        let d = detail(json!({"state": "MERGED", "merged": true, "baseRefName": "main"}));
        assert_eq!(text(&merged(&d, "")), "into main");
        assert_eq!(text(&merged(&d, "2h")), "2h ago into main");
        assert_eq!(text(&merged(&merged_pr(), "")), "by maint into main");
    }

    #[test]
    fn closed_reads_the_closer_the_time_and_not_merged() {
        let d = detail(
            json!({"state": "CLOSED", "closedAt": "2026-11-01T00:00:00Z",
            "timelineItems": {"nodes": [{"actor": {"login": "closer"}, "createdAt": "x"}]}}),
        );
        assert_eq!(text(&closed(&d, "5d")), "by closer · 5d ago · not merged");
        let d = detail(json!({"state": "CLOSED"}));
        assert_eq!(text(&closed(&d, "5d")), "5d ago · not merged");
        assert_eq!(text(&closed(&d, "")), "not merged");
    }

    #[test]
    fn queue_and_auto_merge_details() {
        let d = detail(
            json!({"isInMergeQueue": true, "mergeQueueEntry": {"position": 2, "state": "AWAITING_CHECKS"}}),
        );
        assert_eq!(text(&queued(&d)), "Position 2 · awaiting checks");
        let d = detail(
            json!({"autoMergeRequest": {"enabledBy": {"login": "dev"}, "mergeMethod": "SQUASH"}}),
        );
        assert_eq!(text(&auto_merge(&d)), "by dev · squash");
        let d = detail(json!({"autoMergeRequest": {"enabledBy": null, "mergeMethod": "MERGE"}}));
        assert_eq!(text(&auto_merge(&d)), "merge commit");
    }

    #[test]
    fn only_the_four_rows_carry_a_sub_line() {
        let d = merged_pr();
        for row in [
            Row::Draft,
            Row::Conflicts,
            Row::Checking,
            Row::Blocked,
            Row::OutOfDate,
            Row::ChecksFailing,
            Row::Ready,
            Row::Unknown,
        ] {
            assert_eq!(sub_line(row, &d, "1d", "1d"), None, "{row:?}");
        }
        assert!(sub_line(Row::Merged, &d, "1d", "").is_some());
        assert!(sub_line(Row::Closed, &d, "", "1d").is_some());
        // A queued PR whose entry isn't loaded has nothing to add.
        assert_eq!(sub_line(Row::Queued, &d, "", ""), None);
    }
}
