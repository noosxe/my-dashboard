//! Accessible names for multi-line text fields (#768).
//!
//! A bare `Textarea` exposes a text input with its value but no name,
//! so the audit flags it and a screen reader announces "edit text".
//! `named` projects a name through the pinned `InputAccessibility` API,
//! on the field's own focus node.

use gpui::{Role, SharedString};
use gpui_base::input::InputAccessibility;
use gpui_component::input::TextareaState;

pub(crate) trait Named: Sized {
    /// Name the field.
    fn named(self, label: impl Into<SharedString>) -> Self;
}

impl Named for TextareaState {
    fn named(mut self, label: impl Into<SharedString>) -> Self {
        self.set_accessibility(
            InputAccessibility::new(Some(Role::MultilineTextInput)).with_label(Some(label.into())),
        );
        self
    }
}
