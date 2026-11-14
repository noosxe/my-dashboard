//! The Reviews gate row's jump (docs/0083 D3): scroll the timeline to the
//! review behind the row's state, on the docs/0026 D9 anchor.

use gpui::Context;

use crate::pr_page::conversation_window::is_pending_review;
use crate::pr_page::{CommentAnchor, PrDetailPage};

impl PrDetailPage {
    /// Arm the jump to review `id`. A review outside the newest-100
    /// window (docs/0016 §14), or a pending one the timeline doesn't
    /// show, gives up quietly, as the notification hand-off does.
    pub(in crate::pr_page) fn jump_to_review(&mut self, id: &str, cx: &mut Context<Self>) {
        let loaded = self.detail(cx).is_some_and(|d| {
            d.reviews
                .nodes
                .iter()
                .any(|r| r.id == id && !is_pending_review(r))
        });
        if !loaded {
            log::info!("pr-page anchor: review {id} not in the loaded conversation: no jump");
            return;
        }
        self.jump_done.set(false);
        self.anchor = CommentAnchor::Jumping(id.to_string());
        cx.notify();
    }
}
