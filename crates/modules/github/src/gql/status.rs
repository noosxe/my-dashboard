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

/// Auto-merge and the merge queue (docs/0083 D2 rows 4 and 5, D4): what
/// the verdict and its sub-line read, and the gates of Enable auto-merge,
/// Disable auto-merge, Merge when ready and Remove from queue (#774,
/// #775). Every gate degrades to `false`, which hides its control.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Queueing {
    pub in_merge_queue: bool,
    /// `PullRequest.isMergeQueueEnabled`: the base branch has a merge
    /// queue. Read from the pull request, so it answers for this PR's own
    /// base without a second `Repository.mergeQueue(branch:)` lookup.
    #[serde(default)]
    pub queue_enabled: bool,
    /// `Repository.autoMergeAllowed`.
    #[serde(default)]
    pub auto_merge_allowed: bool,
    /// `viewerCanEnableAutoMerge`.
    #[serde(default)]
    pub viewer_can_enable: bool,
    /// `viewerCanDisableAutoMerge`.
    #[serde(default)]
    pub viewer_can_disable: bool,
    /// `MergeQueueEntryState` name, e.g. `AWAITING_CHECKS`.
    pub queue_state: Option<String>,
    pub queue_position: Option<i64>,
    pub auto_merge: Option<AutoMerge>,
}

impl Queueing {
    /// The facts a confirmed write is sure of, patched in until the
    /// refetch brings the rest (the author, method, position and entry
    /// state are learnable only from the server).
    pub fn after_enable(&mut self) {
        self.auto_merge = Some(AutoMerge {
            enabled_by: None,
            merge_method: None,
        });
    }

    pub fn after_disable(&mut self) {
        self.auto_merge = None;
    }

    pub fn after_enqueue(&mut self) {
        self.in_merge_queue = true;
        self.queue_state = None;
        self.queue_position = None;
    }

    pub fn after_dequeue(&mut self) {
        self.in_merge_queue = false;
        self.queue_state = None;
        self.queue_position = None;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AutoMerge {
    pub enabled_by: Option<String>,
    /// `PullRequestMergeMethod` name: `MERGE`, `SQUASH` or `REBASE`.
    pub merge_method: Option<String>,
}

/// `pr` is the pull-request node, `repo` the repository node that holds it
/// (`autoMergeAllowed` lives there).
pub(crate) fn parse_queueing(pr: &Value, repo: &Value) -> Queueing {
    let text = |p: &str| pr.pointer(p).and_then(Value::as_str).map(str::to_string);
    let flag = |node: &Value, name: &str| node.get(name).and_then(Value::as_bool).unwrap_or(false);
    Queueing {
        in_merge_queue: flag(pr, "isInMergeQueue"),
        queue_enabled: flag(pr, "isMergeQueueEnabled"),
        auto_merge_allowed: flag(repo, "autoMergeAllowed"),
        viewer_can_enable: flag(pr, "viewerCanEnableAutoMerge"),
        viewer_can_disable: flag(pr, "viewerCanDisableAutoMerge"),
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
        let q = parse_queueing(
            &json!({
                "isInMergeQueue": true,
                "mergeQueueEntry": {"position": 2, "state": "AWAITING_CHECKS"},
                "autoMergeRequest": {"enabledBy": {"login": "dev"}, "mergeMethod": "SQUASH"}
            }),
            &json!({}),
        );
        assert!(q.in_merge_queue);
        assert_eq!(q.queue_position, Some(2));
        assert_eq!(q.queue_state.as_deref(), Some("AWAITING_CHECKS"));
        let a = q.auto_merge.expect("auto-merge");
        assert_eq!(a.enabled_by.as_deref(), Some("dev"));
        assert_eq!(a.merge_method.as_deref(), Some("SQUASH"));
        assert_eq!(
            parse_queueing(&json!({"autoMergeRequest": null}), &json!({})),
            Queueing::default()
        );
    }

    #[test]
    fn confirmed_writes_patch_what_they_are_sure_of() {
        let mut q = Queueing::default();
        q.after_enable();
        assert!(q.auto_merge.is_some());
        q.after_disable();
        assert_eq!(q.auto_merge, None);
        q.after_enqueue();
        assert!(q.in_merge_queue);
        q.queue_position = Some(3);
        q.queue_state = Some("QUEUED".into());
        q.after_dequeue();
        assert_eq!(q.queue_position, None);
        assert_eq!(q, Queueing::default());
    }

    #[test]
    fn queueing_reads_the_gates_and_degrades_to_hidden() {
        let q = parse_queueing(
            &json!({
                "isMergeQueueEnabled": true,
                "viewerCanEnableAutoMerge": true,
                "viewerCanDisableAutoMerge": true,
            }),
            &json!({"autoMergeAllowed": true}),
        );
        assert!(q.queue_enabled && q.auto_merge_allowed);
        assert!(q.viewer_can_enable && q.viewer_can_disable);
        // Absent or malformed flags are `false`: the controls stay hidden.
        let q = parse_queueing(
            &json!({"isMergeQueueEnabled": "yes", "viewerCanEnableAutoMerge": null}),
            &json!({"autoMergeAllowed": 1}),
        );
        assert_eq!(q, Queueing::default());
    }
}
