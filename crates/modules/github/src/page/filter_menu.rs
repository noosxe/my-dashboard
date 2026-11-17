//! The repo page's dropdown filter and sort buttons (docs/0042). Each choice
//! is a plain `PopupMenuItem::new(label)` row, so a screen reader and the
//! debug bridge see a named menu item (docs/0010 D10); the active choice is
//! the item's own checked state.

use gpui::*;
use gpui_component::{
    Sizable as _,
    button::{Button, ButtonVariants as _},
    menu::{DropdownMenu as _, PopupMenuItem},
};

use super::GithubPage;

impl GithubPage {
    /// One dropdown filter button (docs/0022-style PopupMenu): the label
    /// shows the active choice; a `•` prefix marks non-default state so
    /// a forgotten filter is visible even collapsed. `apply` runs
    /// deferred off the menu click (the docs/0023 D4 pattern).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn filter_menu_button<T: Clone + PartialEq + 'static>(
        &self,
        id: &'static str,
        label: String,
        non_default: bool,
        choices: Vec<(String, T)>,
        active: T,
        apply: fn(&mut Self, T, &mut Context<Self>),
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        Button::new(id)
            .debug_selector({
                let probe = id.to_string();
                move || probe.clone()
            })
            .ghost()
            .small()
            .label(if non_default {
                format!("• {label}")
            } else {
                label
            })
            .dropdown_menu(move |menu, _, _| {
                let mut next = menu;
                for (text, value) in choices.clone() {
                    let is_checked = value == active;
                    let weak = weak.clone();
                    next = next.item(
                        // A plain labelled item, so the row has an
                        // accessible name; the choice is its label and
                        // the active one carries the item's checked state.
                        PopupMenuItem::new(text).checked(is_checked).on_click(
                            move |_, window, cx| {
                                let weak = weak.clone();
                                let value = value.clone();
                                window.defer(cx, move |_, cx| {
                                    let _ = weak.update(cx, |this, cx| apply(this, value, cx));
                                });
                            },
                        ),
                    );
                }
                next
            })
    }
}
