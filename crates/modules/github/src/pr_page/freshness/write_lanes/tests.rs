use super::{Lanes, WriteKind, lanes_for};
use crate::pr_page::BusyOp;
use crate::service::PendingKind;

const DETAIL: Lanes = Lanes {
    detail: true,
    threads: false,
};
const BOTH: Lanes = Lanes {
    detail: true,
    threads: true,
};

#[test]
fn each_pending_write_maps_to_its_lanes() {
    use PendingKind::*;
    let table = [
        (Comment, DETAIL),
        (Description, DETAIL),
        (CommentEdit, DETAIL),
        (CommentDelete, DETAIL),
        (ReviewEdit, DETAIL),
        (ReviewDelete, DETAIL),
        (Review, DETAIL),
        (LineComment, BOTH),
        (ReviewReply, BOTH),
        (ReviewBatch, BOTH),
        (Merge, DETAIL),
        (Close, DETAIL),
        (ReadyForReview, DETAIL),
        (ConvertToDraft, DETAIL),
        (Reopen, DETAIL),
        (DeleteBranch, DETAIL),
        (EnableAutoMerge, DETAIL),
        (DisableAutoMerge, DETAIL),
        (Enqueue, DETAIL),
        (Dequeue, DETAIL),
        (UpdateBranch, DETAIL),
    ];
    for (kind, lanes) in table {
        assert_eq!(lanes_for(WriteKind::from(kind)), lanes, "{kind:?}");
    }
}

#[test]
fn each_page_modal_write_maps_to_its_lanes() {
    let table = [
        (BusyOp::Reaction, DETAIL),
        (BusyOp::PrSetLabels, DETAIL),
        (BusyOp::PrSetAssignees, DETAIL),
        (BusyOp::PrSetMilestone, DETAIL),
        (BusyOp::PrSetReviewers, DETAIL),
        (BusyOp::ThreadResolve, BOTH),
        (BusyOp::ThreadUnresolve, BOTH),
        (BusyOp::ReviewCommentEdit, BOTH),
        (BusyOp::ReviewCommentDelete, BOTH),
    ];
    for (op, lanes) in table {
        assert_eq!(lanes_for(WriteKind::from(op)), lanes, "{op:?}");
    }
}
