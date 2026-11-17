//! #785: one branch name in the header's meta line — mono, middle-truncated
//! when long, the full name in the tooltip, a click (or Enter) copies it.

use gpui::{App, ClipboardItem, ParentElement as _, SharedString, Styled as _, div};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};

use super::header_meta::branch::{MAX_CHARS, middle_truncate};

pub(super) fn branch_chip(id: &'static str, full: &str, mono: SharedString) -> Button {
    let copy = full.to_string();
    Button::new(id)
        .ghost()
        .xsmall()
        .child(
            div()
                .font_family(mono)
                .child(middle_truncate(full, MAX_CHARS)),
        )
        .tooltip(format!("{full} — click to copy"))
        .accessibility_label(format!("Copy branch name {full}"))
        .on_click(move |_, _, cx: &mut App| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
        })
}
