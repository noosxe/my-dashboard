//! docs/0084 D2: the write-to-lane map. One pure function decides which
//! lanes a confirmed write refetches, so the call sites no longer pick
//! per write.

use gpui::Context;

use crate::pr_page::{BusyOp, PrDetailPage};
use crate::service::PendingKind;
use crate::usage::{Origin, Surface, Trigger};

/// What a write changes, grouped by the lanes it touches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WriteKind {
    /// Comment, reply, reaction, edit or delete on the timeline, and the
    /// description edit.
    Timeline,
    /// A review batch from the Files tab, a line comment or reply, an
    /// edit or delete of a review comment, a thread resolve or unresolve.
    Review,
    /// The metadata pickers (labels, assignees, milestone, reviewers).
    Picker,
    /// Merge, close, reopen, draft flips, auto-merge, queue, delete
    /// branch and update branch.
    State,
}

/// The lanes one write refetches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Lanes {
    pub(crate) detail: bool,
    pub(crate) threads: bool,
}

/// The write-to-lane table. Thread writes also refetch the detail
/// (#872): the review changes its `reviews`, decision and counts.
pub(crate) fn lanes_for(kind: WriteKind) -> Lanes {
    match kind {
        WriteKind::Timeline | WriteKind::Picker | WriteKind::State => Lanes {
            detail: true,
            threads: false,
        },
        WriteKind::Review => Lanes {
            detail: true,
            threads: true,
        },
    }
}

impl From<PendingKind> for WriteKind {
    fn from(kind: PendingKind) -> Self {
        match kind {
            PendingKind::Comment
            | PendingKind::Description
            | PendingKind::CommentEdit
            | PendingKind::CommentDelete
            | PendingKind::ReviewEdit
            | PendingKind::ReviewDelete
            | PendingKind::Review => WriteKind::Timeline,
            PendingKind::LineComment | PendingKind::ReviewReply | PendingKind::ReviewBatch => {
                WriteKind::Review
            }
            PendingKind::Merge
            | PendingKind::Close
            | PendingKind::ReadyForReview
            | PendingKind::ConvertToDraft
            | PendingKind::Reopen
            | PendingKind::DeleteBranch
            | PendingKind::EnableAutoMerge
            | PendingKind::DisableAutoMerge
            | PendingKind::Enqueue
            | PendingKind::Dequeue
            | PendingKind::UpdateBranch => WriteKind::State,
        }
    }
}

impl From<BusyOp> for WriteKind {
    fn from(op: BusyOp) -> Self {
        match op {
            BusyOp::Reaction => WriteKind::Timeline,
            BusyOp::ThreadResolve
            | BusyOp::ThreadUnresolve
            | BusyOp::ReviewCommentEdit
            | BusyOp::ReviewCommentDelete => WriteKind::Review,
            BusyOp::PrSetLabels
            | BusyOp::PrSetAssignees
            | BusyOp::PrSetMilestone
            | BusyOp::PrSetReviewers => WriteKind::Picker,
        }
    }
}

impl PrDetailPage {
    /// Refetch the lanes a confirmed write can have changed. A forced
    /// read, as every write's refetch is: no floor, only the gates.
    pub(in crate::pr_page) fn refetch_for_write(
        &mut self,
        kind: impl Into<WriteKind>,
        cx: &mut Context<Self>,
    ) {
        let lanes = lanes_for(kind.into());
        let origin = Origin::new(Surface::PrPage, Trigger::MutationRefetch);
        if lanes.threads {
            self.fetch_threads(origin, cx);
        }
        if lanes.detail {
            self.fetch_detail(origin, cx);
        }
    }
}

#[cfg(test)]
mod tests;
