//! The status block's auto-merge and merge-queue writes (docs/0083 D4,
//! #774, #775): Enable auto-merge, Disable auto-merge, Merge when ready
//! (`enqueuePullRequest`) and Remove from queue (`dequeuePullRequest`).
//! Each re-checks its gate (`merge_gate.rs`), sends through the store's
//! pending write (docs/0072 D4) and refetches on confirmation
//! (docs/0043 D2). Enable and Merge when ready are reached only through
//! their confirm; Disable and Remove from queue are one click, because
//! each is undone in one click. No polling follows a write: the queue's
//! own progress shows on the page's normal refreshes (docs/0052).

use gpui::{Context, Window};
use serde_json::{Value, json};

use super::PrDetailPage;
use super::merge_gate::{
    dequeue_enabled, disable_auto_enabled, enable_auto_enabled, queue_enabled,
};
use crate::service::{Mutation, PendingKind};

/// `EnablePullRequestAutoMergeInput`. The method rides along only off a
/// merge queue: with one, the server ignores it (schema description,
/// 2026-11-14), so the input leaves it out.
pub(super) fn enable_auto_merge_input(pull_request_id: &str, method: Option<&str>) -> Value {
    match method {
        Some(method) => json!({
            "input": { "pullRequestId": pull_request_id, "mergeMethod": method }
        }),
        None => json!({ "input": { "pullRequestId": pull_request_id } }),
    }
}

/// `DisablePullRequestAutoMergeInput` and `EnqueuePullRequestInput` take
/// the pull request's node id as `pullRequestId`.
pub(super) fn pull_request_input(pull_request_id: &str) -> Value {
    json!({ "input": { "pullRequestId": pull_request_id } })
}

/// `DequeuePullRequestInput` names it `id` (the pull request's, not the
/// queue entry's).
pub(super) fn dequeue_input(pull_request_id: &str) -> Value {
    json!({ "input": { "id": pull_request_id } })
}

impl PrDetailPage {
    /// Enable auto-merge with the selected method (none on a queued base).
    pub(super) fn enable_auto_merge_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail(cx).cloned() else {
            return;
        };
        if !enable_auto_enabled(&d, self.methods.is_empty(), self.writes_busy(cx)) {
            return;
        }
        let method = if d.queueing.queue_enabled {
            None
        } else {
            self.selected_method()
        };
        self.send_queue_write(
            PendingKind::EnableAutoMerge,
            Mutation::EnableAutoMerge,
            enable_auto_merge_input(&d.id, method.map(|m| m.as_str())),
            window,
            cx,
        );
    }

    /// Disable auto-merge. No confirm.
    pub(super) fn disable_auto_merge_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail(cx).cloned() else {
            return;
        };
        if !disable_auto_enabled(&d, self.writes_busy(cx)) {
            return;
        }
        self.send_queue_write(
            PendingKind::DisableAutoMerge,
            Mutation::DisableAutoMerge,
            pull_request_input(&d.id),
            window,
            cx,
        );
    }

    /// Merge when ready: add the PR to the base branch's merge queue.
    pub(super) fn enqueue_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail(cx).cloned() else {
            return;
        };
        if !queue_enabled(&d, self.writes_busy(cx)) {
            return;
        }
        self.send_queue_write(
            PendingKind::Enqueue,
            Mutation::Enqueue,
            pull_request_input(&d.id),
            window,
            cx,
        );
    }

    /// Remove from queue. No confirm.
    pub(super) fn dequeue_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail(cx).cloned() else {
            return;
        };
        if !dequeue_enabled(&d, self.writes_busy(cx)) {
            return;
        }
        self.send_queue_write(
            PendingKind::Dequeue,
            Mutation::Dequeue,
            dequeue_input(&d.id),
            window,
            cx,
        );
    }

    fn send_queue_write(
        &mut self,
        kind: PendingKind,
        mutation: Mutation,
        variables: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_store_mutation(
            window,
            kind,
            mutation,
            variables,
            Self::revalidate_after_store_mutation,
            cx,
        );
    }
}

#[cfg(test)]
mod tests;
