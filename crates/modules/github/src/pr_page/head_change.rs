//! A new head outdates what rides the old one (docs/0072 D3, #752).
//!
//! The detail refetch after Update branch, or any refresh that sees a
//! push, brings the new `headRefOid`. The Files list, the diff, the
//! threads and the check logs were read against the old one, and a
//! line comment composed on the old patch would land on whatever line
//! the new head has there: the thread mutation takes no commit id.
//!
//! - **Fan-out, once per head change.** The page remembers the head its
//!   last landing carried; a landing that differs refetches the lanes
//!   that are open (Idle lanes load on first open anyway). Both ride the
//!   shared dispatch guard (docs/0031); a read already flying predates
//!   the push, so a refetch that hits the guard marks it outdated and
//!   one replacement goes out, the #739 rule (`write_refetch.rs`).
//!   The diff follows the Files list on its next paint, and the check
//!   logs reset in `reconcile_check_logs` (#747).
//! - **Stale compose guard.** The page records the head a line composer
//!   or the first queued draft was started on. While either is in use
//!   and the head moved on, adding or submitting is refused inline and
//!   the text stays put. Discarding the drafts or arming a fresh composer
//!   on the new diff clears it.

use gpui::Context;

use super::{ErrorScope, MutationError, PrDetailPage};
use crate::gql;
use crate::usage::{Origin, Surface, Trigger};

/// The page's head bookkeeping.
#[derive(Default)]
pub(super) struct HeadWatch {
    /// The head the last detail landing (or the document the page opened
    /// over) carried.
    seen: Option<String>,
    /// The head the in-use composer / queued drafts were started on.
    composed: Option<String>,
    /// A head-change files refetch hit the guard behind an older read.
    files_outdated: bool,
}

const STALE_COMPOSE: &str = "The branch was updated after this comment was started, \
so its line may have moved. Discard it and comment again on the new diff.";

impl PrDetailPage {
    /// Seed from the document the page opened over, so the first landing
    /// compares against the head the page last showed.
    pub(super) fn seed_head_watch(&mut self, cx: &Context<Self>) {
        self.head_watch.seen = self.detail(cx).and_then(|d| d.head_oid.clone());
    }

    /// A detail landed: fan out when its head is not the one seen before.
    /// Returns whether the head changed.
    pub(super) fn note_landed_head(
        &mut self,
        detail: &gql::PrDetail,
        cx: &mut Context<Self>,
    ) -> bool {
        // A degraded payload without a head says nothing about it.
        let Some(head) = detail.head_oid.clone() else {
            return false;
        };
        let changed = self
            .head_watch
            .seen
            .as_ref()
            .is_some_and(|seen| *seen != head);
        self.head_watch.seen = Some(head);
        if changed {
            log::info!(
                "pr-head: {}#{} moved to a new head — refetching open lanes",
                self.repo,
                self.number
            );
            let origin = Origin::new(Surface::PrPage, Trigger::MutationRefetch);
            if !self.files.is_idle() {
                self.fetch_files(origin, cx);
            }
            if !self.threads.is_idle() {
                self.fetch_threads(origin, cx);
            }
        }
        changed
    }

    /// A head-change files refetch hit the guard.
    pub(super) fn note_outdated_files(&mut self, origin: Origin) {
        if origin.trigger == Trigger::MutationRefetch {
            self.head_watch.files_outdated = true;
        }
    }

    /// Any dispatched files read replaces the outdated one.
    pub(super) fn clear_outdated_files(&mut self) {
        self.head_watch.files_outdated = false;
    }

    /// Close an outdated files read without landing its body, leaving the
    /// mark for the rerun. Returns whether the read was outdated.
    pub(super) fn discard_outdated_files(&mut self) -> bool {
        if self.head_watch.files_outdated {
            log::debug!("pr-files: read predates a head change, dropped");
            self.files.in_flight = false;
        }
        self.head_watch.files_outdated
    }

    pub(super) fn files_rerun_due(&self, cx: &Context<Self>) -> bool {
        self.head_watch.files_outdated
            && !self.files.in_flight
            && !self
                .state
                .read(cx)
                .gated_for(crate::service::Kind::PrFiles.lane())
    }

    /// Record the head a composer or draft is started on. A composer
    /// replacing the previous one with nothing queued starts afresh.
    pub(super) fn note_compose_start(&mut self, cx: &Context<Self>) {
        let head = self.detail(cx).and_then(|d| d.head_oid.clone());
        if self.drafts.is_empty() || self.head_watch.composed.is_none() {
            self.head_watch.composed = head;
        }
    }

    /// Refuse a line comment write whose composer or drafts were started
    /// on an older head. Returns whether it refused.
    pub(super) fn refuse_stale_compose(&mut self, cx: &mut Context<Self>) -> bool {
        let in_use = self.line_composer.is_some() || !self.drafts.is_empty();
        let current = self.detail(cx).and_then(|d| d.head_oid.clone());
        let stale = in_use
            && self
                .head_watch
                .composed
                .as_ref()
                .is_some_and(|started| Some(started) != current.as_ref());
        if stale {
            self.mutation_error = Some(MutationError {
                scope: ErrorScope::Files,
                message: STALE_COMPOSE.into(),
            });
            cx.notify();
        }
        stale
    }
}

#[cfg(test)]
mod tests;
