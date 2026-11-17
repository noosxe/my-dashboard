//! #789: a card's Edit and Delete live in one `⋯` menu at the header's
//! trailing edge (after the age and the react control), not as buttons
//! beside the author's name. Delete is drawn in the danger colour.

use gpui::prelude::*;
use gpui::*;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
};

use super::PrDetailPage;

/// What a menu row does when picked.
#[derive(Clone)]
enum Pick {
    EditDescription,
    EditRow { id: String, body: String },
    DeleteRow { id: String },
}

struct Entry {
    label: &'static str,
    danger: bool,
    pick: Pick,
}

/// The header's trailing controls: the react control, then the menu.
pub(super) fn trailing_controls(
    react: Option<AnyElement>,
    menu: Option<AnyElement>,
) -> Option<AnyElement> {
    if react.is_none() && menu.is_none() {
        return None;
    }
    Some(
        h_flex()
            .gap_1()
            .items_center()
            .children(react)
            .children(menu)
            .into_any_element(),
    )
}

impl PrDetailPage {
    /// The `⋯` menu for one timeline row (0019 D3/D4): present only when
    /// the payload's viewer flags allow. `prefix` keys the bridge ids
    /// (`{prefix}-more`, rows `{prefix}-edit` / `{prefix}-delete`).
    pub(super) fn row_actions(
        &self,
        prefix: &str,
        node_id: &str,
        subject: &str,
        body: &str,
        (can_update, can_delete): (bool, bool),
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let mut entries = Vec::new();
        if can_update {
            entries.push(Entry {
                label: "Edit",
                danger: false,
                pick: Pick::EditRow {
                    id: node_id.to_string(),
                    body: body.to_string(),
                },
            });
        }
        if can_delete {
            entries.push(Entry {
                label: "Delete",
                danger: true,
                pick: Pick::DeleteRow {
                    id: node_id.to_string(),
                },
            });
        }
        self.card_menu(prefix, subject, entries, cx)
    }

    /// The description card's menu: Edit, when the viewer may update.
    pub(super) fn description_actions(
        &self,
        can_update: bool,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let entries = can_update
            .then_some(Entry {
                label: "Edit",
                danger: false,
                pick: Pick::EditDescription,
            })
            .into_iter()
            .collect();
        self.card_menu("pr-description", "the description", entries, cx)
    }

    fn card_menu(
        &self,
        prefix: &str,
        subject: &str,
        entries: Vec<Entry>,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if entries.is_empty() {
            return None;
        }
        let busy = self.busy.is_some();
        let weak = cx.entity().downgrade();
        let id = format!("{prefix}-more");
        let probe = id.clone();
        let wrap = format!("{prefix}-actions");
        Some(
            div()
                .flex_none()
                // Test-only probe key wrapping the menu button (0019 D8).
                .debug_selector(move || wrap.clone())
                .child(
                    Button::new(id)
                        .debug_selector(move || probe.clone())
                        .icon(Icon::new(IconName::Ellipsis))
                        .ghost()
                        .xsmall()
                        .tooltip("More actions")
                        .accessibility_label(format!("More actions for {subject}"))
                        .disabled(busy)
                        .dropdown_menu(move |menu, _, cx| {
                            // Labelled items, so each shows in `tree` as a
                            // named `MenuItem` (the element variant has no
                            // accessible name, docs/0067 D7). A named item
                            // cannot take a text colour, so Delete is told
                            // apart by a danger-tinted trash glyph; Edit
                            // carries the pencil.
                            let mut next = menu;
                            for e in &entries {
                                let pick = e.pick.clone();
                                let item_weak = weak.clone();
                                let icon = if e.danger {
                                    Icon::empty()
                                        .path("icons/trash.svg")
                                        .text_color(cx.theme().danger)
                                } else {
                                    crate::state_pill::icon(crate::state_pill::Glyph::Edit)
                                };
                                next = next.item(PopupMenuItem::new(e.label).icon(icon).on_click(
                                    move |_, window, cx| {
                                        // Defer: the click dispatches inside
                                        // the window's update (see the
                                        // reaction picker).
                                        let weak = item_weak.clone();
                                        let pick = pick.clone();
                                        window.defer(cx, move |window, cx| {
                                            let _ = weak.update(cx, |page, cx| match pick {
                                                Pick::EditDescription => {
                                                    page.start_edit_description(window, cx)
                                                }
                                                Pick::EditRow { id, body } => {
                                                    page.start_edit_row(id, body, window, cx)
                                                }
                                                Pick::DeleteRow { id } => {
                                                    page.start_delete_confirm(id, cx)
                                                }
                                            });
                                        });
                                    },
                                ));
                            }
                            next
                        }),
                )
                .into_any_element(),
        )
    }
}
