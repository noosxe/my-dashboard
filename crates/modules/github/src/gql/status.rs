//! What the PR rail's status block reads beyond the merge state (docs/0083
//! D2–D6, D17): exact check counts, who merged or closed the PR and when,
//! and the merge-queue and auto-merge facts the verdict names. Parsing is
//! tolerant (docs/0007 D3): a missing or malformed field degrades to its
//! empty value, never to a parse failure.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Check counts folded into the buckets the Checks tab uses (docs/0016
/// D7), from `checkRunCountsByState` and `statusContextCountsByState`
/// (verified against GitHub's schema 2026-11-14). They are exact even when
/// the 100-node page of `contexts` is capped. `Waiting` runs from the
/// docs/0066 sweep are not here: GitHub's rollup can't see them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct CheckCounts {
    pub failed: u32,
    /// Running, queued, pending or expected.
    pub running: u32,
    /// Success, neutral, skipped, stale and cancelled: the Checks tab
    /// ranks cancelled rows with the passing ones.
    pub passed: u32,
}

impl CheckCounts {
    pub fn total(&self) -> u32 {
        self.failed + self.running + self.passed
    }

    fn add(&mut self, state: &str, count: u32) {
        match state {
            "FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" | "ERROR" => {
                self.failed += count
            }
            "IN_PROGRESS" | "QUEUED" | "PENDING" | "WAITING" | "EXPECTED" => self.running += count,
            // `COMPLETED` alone carries no verdict; it only appears for a
            // conclusion this build doesn't know, which isn't a failure.
            _ => self.passed += count,
        }
    }
}

/// Sum both lists of `contexts`; absent lists count zero.
pub(crate) fn parse_check_counts(pr: &Value) -> CheckCounts {
    let mut counts = CheckCounts::default();
    for key in ["checkRunCountsByState", "statusContextCountsByState"] {
        let list = pr
            .pointer(&format!("/statusCheckRollup/contexts/{key}"))
            .and_then(Value::as_array);
        for item in list.into_iter().flatten() {
            let state = item.get("state").and_then(Value::as_str);
            let count = item
                .get("count")
                .and_then(Value::as_u64)
                .and_then(|c| u32::try_from(c).ok());
            if let (Some(state), Some(count)) = (state, count) {
                counts.add(state, count);
            }
        }
    }
    counts
}

/// How a PR ended (docs/0083 D6). `merged_by` and `closed_by` are `None`
/// for a deleted account; GitHub exposes no merge method and no
/// `closedBy` field (the closer comes from the newest `ClosedEvent`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Ending {
    pub merged: bool,
    pub merged_at: Option<String>,
    pub merged_by: Option<String>,
    pub closed_at: Option<String>,
    pub closed_by: Option<String>,
}

pub(crate) fn parse_ending(pr: &Value) -> Ending {
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_string);
    let closer = pr.pointer("/timelineItems/nodes/0/actor/login");
    Ending {
        merged: pr.get("merged").and_then(Value::as_bool).unwrap_or(false),
        merged_at: text(pr.get("mergedAt")),
        merged_by: text(pr.pointer("/mergedBy/login")),
        closed_at: text(pr.get("closedAt")),
        closed_by: text(closer),
    }
}

/// Auto-merge and the merge queue (docs/0083 D2 rows 4 and 5, D4). Only
/// the verdict and its sub-line read these; the actions on them ship in
/// their own issues (#774, #775).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Queueing {
    pub in_merge_queue: bool,
    /// `MergeQueueEntryState` name, e.g. `AWAITING_CHECKS`.
    pub queue_state: Option<String>,
    pub queue_position: Option<i64>,
    pub auto_merge: Option<AutoMerge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AutoMerge {
    pub enabled_by: Option<String>,
    /// `PullRequestMergeMethod` name: `MERGE`, `SQUASH` or `REBASE`.
    pub merge_method: Option<String>,
}

pub(crate) fn parse_queueing(pr: &Value) -> Queueing {
    let text = |p: &str| pr.pointer(p).and_then(Value::as_str).map(str::to_string);
    Queueing {
        in_merge_queue: pr
            .get("isInMergeQueue")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        queue_state: text("/mergeQueueEntry/state"),
        queue_position: pr
            .pointer("/mergeQueueEntry/position")
            .and_then(Value::as_i64),
        auto_merge: pr
            .get("autoMergeRequest")
            .filter(|v| v.is_object())
            .map(|a| AutoMerge {
                enabled_by: a
                    .pointer("/enabledBy/login")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                merge_method: a
                    .get("mergeMethod")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn counts(runs: Value, statuses: Value) -> CheckCounts {
        parse_check_counts(&json!({"statusCheckRollup": {"contexts": {
            "checkRunCountsByState": runs, "statusContextCountsByState": statuses}}}))
    }

    #[test]
    fn counts_fold_into_the_tab_buckets() {
        let c = counts(
            json!([
                {"state": "SUCCESS", "count": 4},
                {"state": "SKIPPED", "count": 1},
                {"state": "NEUTRAL", "count": 1},
                {"state": "CANCELLED", "count": 1},
                {"state": "FAILURE", "count": 2},
                {"state": "TIMED_OUT", "count": 1},
                {"state": "ACTION_REQUIRED", "count": 1},
                {"state": "IN_PROGRESS", "count": 3},
                {"state": "QUEUED", "count": 2},
            ]),
            json!([
                {"state": "SUCCESS", "count": 1},
                {"state": "ERROR", "count": 1},
                {"state": "PENDING", "count": 1},
                {"state": "EXPECTED", "count": 1},
            ]),
        );
        assert_eq!(c.failed, 5);
        assert_eq!(c.running, 7);
        assert_eq!(c.passed, 8);
        assert_eq!(c.total(), 20);
    }

    #[test]
    fn a_missing_rollup_or_list_counts_zero() {
        assert_eq!(parse_check_counts(&json!({})), CheckCounts::default());
        assert_eq!(
            parse_check_counts(&json!({"statusCheckRollup": null})),
            CheckCounts::default()
        );
        assert_eq!(
            counts(json!([{"state": "SUCCESS"}]), json!(null)).total(),
            0
        );
    }

    #[test]
    fn ending_reads_merge_and_the_newest_closed_event() {
        let e = parse_ending(&json!({
            "merged": true, "mergedAt": "2026-11-01T00:00:00Z",
            "mergedBy": {"login": "maint"}, "closedAt": "2026-11-01T00:00:00Z",
            "timelineItems": {"nodes": [{"actor": {"login": "closer"}, "createdAt": "x"}]}
        }));
        assert!(e.merged);
        assert_eq!(e.merged_by.as_deref(), Some("maint"));
        assert_eq!(e.closed_by.as_deref(), Some("closer"));
        assert_eq!(e.merged_at.as_deref(), Some("2026-11-01T00:00:00Z"));
    }

    #[test]
    fn a_deleted_actor_or_empty_timeline_drops_the_name_only() {
        let e = parse_ending(&json!({
            "merged": false, "closedAt": "2026-11-01T00:00:00Z", "mergedBy": null,
            "timelineItems": {"nodes": [{"actor": null, "createdAt": "x"}]}
        }));
        assert!(!e.merged);
        assert_eq!(e.merged_by, None);
        assert_eq!(e.closed_by, None);
        assert_eq!(e.closed_at.as_deref(), Some("2026-11-01T00:00:00Z"));
        assert_eq!(parse_ending(&json!({})), Ending::default());
    }

    #[test]
    fn queueing_reads_the_queue_entry_and_auto_merge() {
        let q = parse_queueing(&json!({
            "isInMergeQueue": true,
            "mergeQueueEntry": {"position": 2, "state": "AWAITING_CHECKS"},
            "autoMergeRequest": {"enabledBy": {"login": "dev"}, "mergeMethod": "SQUASH"}
        }));
        assert!(q.in_merge_queue);
        assert_eq!(q.queue_position, Some(2));
        assert_eq!(q.queue_state.as_deref(), Some("AWAITING_CHECKS"));
        let a = q.auto_merge.expect("auto-merge");
        assert_eq!(a.enabled_by.as_deref(), Some("dev"));
        assert_eq!(a.merge_method.as_deref(), Some("SQUASH"));
        assert_eq!(
            parse_queueing(&json!({"autoMergeRequest": null})),
            Queueing::default()
        );
    }
}
