//! #788: the "add reaction" control. It sits in the card header, muted,
//! and only that: the chips row below the body exists only when chips do
//! (`render_reaction_row`). The control stays a normal Tab stop.

use gpui::prelude::*;
use gpui::*;
use gpui_component::{
    Disableable as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
};

use super::{PrDetailPage, gql};
use crate::issue_page::reaction_direction;

impl PrDetailPage {
    /// The header's add-reaction button, `None` when the viewer cannot react.
    pub(super) fn render_reaction_add(
        &self,
        prefix: &str,
        subject_id: &str,
        subject: &str,
        groups: &[gql::ReactionGroup],
        can_react: bool,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if !can_react {
            return None;
        }
        let busy = self.busy.is_some();
        let subject_id = subject_id.to_string();
        let groups = groups.to_vec();
        let weak = cx.entity().downgrade();
        let picker_id = format!("{prefix}-add");
        let probe = picker_id.clone();
        Button::new(picker_id)
            .debug_selector(move || probe.clone())
            .ghost()
            .xsmall()
            .icon(Icon::empty().path("icons/smile-plus.svg"))
            .tooltip("Add reaction")
            .accessibility_label(format!("Add reaction to {subject}"))
            .disabled(busy)
            .dropdown_menu(move |menu, _, _| {
                // docs/0023 D4 (§10 amendment): the static
                // eight-row picker rides the + button as a
                // PopupMenu. Rows freeze at open — built from
                // this render's snapshot, so a refetch mid-pick
                // repaints chips, never the open menu — and a
                // row commits on click (the mutation is the
                // commit; no Save ritual).
                let mut next = menu;
                for content in gql::ReactionContent::ALL {
                    let armed = reaction_direction(&groups, content);
                    let count = groups
                        .iter()
                        .find(|g| g.content == content)
                        .map(|g| g.total)
                        .unwrap_or(0);
                    let subject = subject_id.clone();
                    let item_weak = weak.clone();
                    let probe = format!("pr-reaction-pick-{}", content.wire());
                    next = next.item(
                        PopupMenuItem::element(move |_, _| {
                            // Probe-visible row (0010 D10) — the
                            // plain Item variant is not.
                            let key = probe.clone();
                            h_flex()
                                .debug_selector(move || key.clone())
                                .gap_2()
                                .child(format!(
                                    "{} {} ({})",
                                    content.glyph(),
                                    content.label(),
                                    count
                                ))
                                .when(armed, |row| row.child("· you"))
                        })
                        .checked(armed)
                        .disabled(busy)
                        .on_click(move |_, window, cx| {
                            // The click dispatches inside the window's
                            // update — any re-entrant window update here
                            // fails ("window not found"). Defer to the end
                            // of the effect cycle, then post from the live
                            // context (the page.rs spawn pattern, inline).
                            let weak = item_weak.clone();
                            let subject = subject.clone();
                            window.defer(cx, move |window, cx| {
                                let _ = weak.update(cx, |page, cx| {
                                    page.toggle_reaction(subject, armed, content, window, cx)
                                });
                            });
                        }),
                    );
                }
                next
            })
            .into_any_element()
            .into()
    }
}
