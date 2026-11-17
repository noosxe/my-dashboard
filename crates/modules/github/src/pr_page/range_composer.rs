//! Opening the line composer from the diff kit's own selection (#761).
//! The kit hands over a start and an end (`DiffLineRange`); GitHub's
//! `line`/`side` carry the END and `startLine`/`startSide` the START, so
//! the composer opens on the end with the start kept.

use gpui::{Context, Window};

use super::PrDetailPage;
use crate::gql::DiffSide;

/// `(end, start)` for a selection: the end anchor, and the start anchor
/// unless the selection is one line (which keeps single-line variables
/// byte-identical to a plain click). A same-side selection may arrive in
/// either order and is normalized to start <= end; a cross-side one is
/// already in patch order, which GitHub's `startSide`/`side` take as is.
pub(crate) fn range_anchors(
    start: (DiffSide, u32),
    end: (DiffSide, u32),
) -> ((DiffSide, u32), Option<(DiffSide, u32)>) {
    if start == end {
        return (end, None);
    }
    if start.0 == end.0 && start.1 > end.1 {
        return (start, Some(end));
    }
    (end, Some(start))
}

impl PrDetailPage {
    /// Arm the composer on the selected range. A one-line selection is a
    /// plain click (it may extend an open composer's range, #211).
    pub(crate) fn open_range_composer(
        &mut self,
        path: String,
        start: (DiffSide, u32),
        end: (DiffSide, u32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (end, start) = range_anchors(start, end);
        let Some(start) = start else {
            self.open_line_composer(path, end.0, end.1, window, cx);
            return;
        };
        if self.busy.is_some() {
            return;
        }
        // A fresh composer on the range end; the range replaces whatever
        // an earlier composer held.
        self.line_composer = None;
        self.open_line_composer(path, end.0, end.1, window, cx);
        if let Some(c) = self.line_composer.as_mut() {
            c.start = Some(start);
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DiffSide::{Left, Right};

    #[test]
    fn single_line_has_no_start() {
        assert_eq!(range_anchors((Right, 5), (Right, 5)), ((Right, 5), None));
    }

    #[test]
    fn same_side_range_is_ordered() {
        assert_eq!(
            range_anchors((Right, 3), (Right, 9)),
            ((Right, 9), Some((Right, 3)))
        );
        assert_eq!(
            range_anchors((Right, 9), (Right, 3)),
            ((Right, 9), Some((Right, 3)))
        );
    }

    #[test]
    fn cross_side_range_keeps_patch_order() {
        assert_eq!(
            range_anchors((Left, 4), (Right, 4)),
            ((Right, 4), Some((Left, 4)))
        );
    }
}
