//! #818: focus must not outlive the tab that held it. A tab change
//! unmounts the old tab's elements; a focus handle that belonged to one
//! of them used to stay the window's focus, so Tab moved nothing and a
//! keyboard user was stranded. When the tab changes and focus was inside
//! the old tab's content, it moves to the tab strip — always present, and
//! the control that names the tab just entered.

use gpui::{Context, FocusHandle, Window};

use super::{PrDetailPage, PrTab};

impl PrDetailPage {
    /// Called once per render, before the new tab's content is built (the
    /// window's focus path is still the previous frame's, so it names the
    /// old tab's element). Focus elsewhere — the strip, the header, another
    /// window — is left alone.
    pub(super) fn settle_tab_focus(
        &mut self,
        tabs_focus: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !tab_changed(self.focus_tab, self.tab) {
            return;
        }
        self.focus_tab = self.tab;
        if self.content_focus.contains_focused(window, cx) {
            tabs_focus.focus(window, cx);
        }
    }
}

/// `true` when the rendered tab differs from the one the last frame held.
fn tab_changed(rendered: PrTab, current: PrTab) -> bool {
    rendered != current
}

#[cfg(test)]
mod tests;
