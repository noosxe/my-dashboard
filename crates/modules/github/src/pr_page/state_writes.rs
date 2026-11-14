//! The status block's state flips (docs/0083 D4, D5): Ready for review
//! (#771), Convert to draft (#772), Reopen (#770) and Delete branch (#773). Each re-checks its
//! gate (`merge_gate.rs`), sends `{ input: { pullRequestId } }` through
//! the store's pending write (docs/0072 D4) and refetches on
//! confirmation like Merge and Close (docs/0043 D2).

use gpui::{Context, Window};
use serde_json::{Value, json};

use super::PrDetailPage;
use super::merge_gate::{convert_enabled, delete_branch_enabled, ready_enabled, reopen_enabled};
use crate::service::{Mutation, PendingKind};

/// All three mutations take the PR's node id and nothing else (schema
/// introspection, 2026-11-14).
pub(super) fn state_input(pull_request_id: &str) -> Value {
    json!({ "input": { "pullRequestId": pull_request_id } })
}

/// `deleteRef` takes the head branch's `Ref` node id, not the PR's
/// (schema introspection, 2026-11-14).
pub(super) fn delete_branch_input(ref_id: &str) -> Value {
    json!({ "input": { "refId": ref_id } })
}

impl PrDetailPage {
    /// Mark a draft ready for review. No confirm: Convert to draft undoes
    /// it in one click (docs/0083 §2).
    pub(super) fn ready_for_review_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail(cx).cloned() else {
            return;
        };
        let busy = self.writes_busy(cx);
        if !ready_enabled(
            d.state.as_deref(),
            d.draft,
            d.viewer_can_update,
            d.id.is_empty(),
            busy,
        ) {
            return;
        }
        self.send_state_write(
            PendingKind::ReadyForReview,
            Mutation::ReadyForReview,
            &d.id,
            window,
            cx,
        );
    }

    /// Convert an open PR to a draft. Reached only through its confirm.
    pub(super) fn convert_to_draft_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail(cx).cloned() else {
            return;
        };
        let busy = self.writes_busy(cx);
        if !convert_enabled(
            d.state.as_deref(),
            d.draft,
            d.viewer_can_update,
            d.id.is_empty(),
            busy,
        ) {
            return;
        }
        self.send_state_write(
            PendingKind::ConvertToDraft,
            Mutation::ConvertToDraft,
            &d.id,
            window,
            cx,
        );
    }

    /// Reopen a closed PR. No confirm: Close undoes it (docs/0083 §2).
    pub(super) fn reopen_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail(cx).cloned() else {
            return;
        };
        let busy = self.writes_busy(cx);
        if !reopen_enabled(
            d.state.as_deref(),
            d.viewer_can_reopen,
            d.id.is_empty(),
            busy,
        ) {
            return;
        }
        self.send_state_write(PendingKind::Reopen, Mutation::Reopen, &d.id, window, cx);
    }

    /// Delete the head branch of a merged or closed PR. Reached only
    /// through its confirm (docs/0083 D5); the confirmation patches the
    /// cached document and refetches.
    pub(super) fn delete_branch_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail(cx).cloned() else {
            return;
        };
        let busy = self.writes_busy(cx);
        let Some(head) = d.head_branch.as_ref() else {
            return;
        };
        if !delete_branch_enabled(d.state.as_deref(), Some(head), busy) {
            return;
        }
        self.begin_store_mutation(
            window,
            PendingKind::DeleteBranch,
            Mutation::DeleteBranch,
            delete_branch_input(&head.id),
            Self::revalidate_after_store_mutation,
            cx,
        );
    }

    fn send_state_write(
        &mut self,
        kind: PendingKind,
        mutation: Mutation,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_store_mutation(
            window,
            kind,
            mutation,
            state_input(id),
            Self::revalidate_after_store_mutation,
            cx,
        );
    }
}

#[cfg(test)]
mod tests;
