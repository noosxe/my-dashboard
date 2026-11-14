//! docs/0084 D8: a landing that removes the node the user is editing
//! must not discard what they typed. The editor stays open, under a
//! "deleted on GitHub" notice, until the user closes it. Save is off: the
//! node is gone, so there is nothing to write to.

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, accesskit, div,
};
use gpui_component::{ActiveTheme as _, v_flex};

use crate::gql;
use crate::pr_page::{PrDetailPage, inline_edit_card, node_kind};

/// What the notice says.
pub(crate) const NOTICE: &str =
    "Deleted on GitHub. Your text stays here until you close the editor.";

/// Whether the node being edited is gone from the payload.
pub(crate) fn is_vanished(d: &gql::PrDetail, id: &str) -> bool {
    node_kind(&d.comments.nodes, &d.reviews.nodes, id).is_none()
}

impl PrDetailPage {
    /// The editor of a node that vanished, with its notice; `None` while
    /// the node is there (its own row shows the editor) or nothing is
    /// being edited.
    pub(in crate::pr_page) fn render_vanished_editor(
        &self,
        d: &gql::PrDetail,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let (id, editor) = self.timeline_edit.as_ref()?;
        if !is_vanished(d, id) {
            return None;
        }
        Some(
            v_flex()
                .id("pr-edit-vanished")
                .gap_1()
                .child(
                    div()
                        .id("pr-edit-vanished-notice")
                        .role(accesskit::Role::Status)
                        .aria_label(NOTICE)
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(NOTICE),
                )
                // `busy` greys Save only; Cancel stays the way out.
                .child(inline_edit_card(
                    "pr-edit-vanished-card",
                    "Editing a deleted item",
                    editor,
                    true,
                    cx.listener(|_, _, _, _| {}),
                    cx.listener(|this, _, window, cx| this.cancel_edit_row(window, cx)),
                    cx,
                ))
                .into_any_element(),
        )
    }
}
