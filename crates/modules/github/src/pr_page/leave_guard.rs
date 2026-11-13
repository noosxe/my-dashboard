//! The confirm in front of leaving the PR page with unsent review
//! drafts (#744). Queued review comments, the finish-review summary and
//! the line composer's text live on the page, so every way of dropping
//! the page would drop them; the confirm reuses the Merge/Close confirm
//! pattern (docs/0016 D5, `merge_confirm.rs`). The drafts themselves are
//! not persisted here (#281).
//!
//! Paths that go through the guard: Esc with nothing else open, the repo
//! crumb, and the module crumb. Navigation that the shell has already
//! committed before the module hears of it (a sidebar jump to another
//! module, history Back/Forward) and sign-out cannot ask first.

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, accesskit, div,
};
use gpui_component::{
    ActiveTheme, Sizable as _,
    button::{Button, ButtonVariants},
    h_flex, v_flex,
};

use super::review_drafts::{discard_prompt, draft_count};
use super::{PrDetailPage, PrPageEvent};

/// Where the page is headed when it asks to be dropped; the host decides
/// what each destination means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeaveIntent {
    /// Esc: back to the repo view, selection kept.
    Close,
    /// The repo crumb: the repo view, recorded as a visit.
    RepoCrumb,
    /// The module crumb: back to the module root.
    ModuleRoot,
}

impl PrDetailPage {
    /// Unsent review drafts a departure would drop.
    pub(crate) fn review_draft_count(&self, cx: &Context<Self>) -> usize {
        let text = |editor: Option<&gpui::Entity<gpui_component::input::TextareaState>>| {
            editor
                .map(|e| e.read(cx).value().to_string())
                .unwrap_or_default()
        };
        draft_count(
            self.drafts.len(),
            &text(self.finish_summary.as_ref()),
            &text(self.line_composer.as_ref().map(|c| &c.editor)),
        )
    }

    /// Ask to leave. `true` — nothing would be lost, go ahead. `false` —
    /// the discard confirm is open; the page emits `Leave(intent)` when
    /// the user confirms.
    pub(crate) fn guard_leave(
        &mut self,
        intent: LeaveIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.review_draft_count(cx) == 0 {
            return true;
        }
        self.leave_confirm = Some(intent);
        // The click that asked may have come from outside the page.
        self.refocus_page(window, cx);
        cx.notify();
        false
    }

    /// Keep editing / Esc: the confirm closes, drafts and page stay.
    pub(super) fn cancel_leave_confirm(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let was_open = self.leave_confirm.take().is_some();
        if was_open {
            self.refocus_page(window, cx);
            cx.notify();
        }
        was_open
    }

    fn confirm_discard(&mut self, cx: &mut Context<Self>) {
        if let Some(intent) = self.leave_confirm.take() {
            cx.emit(PrPageEvent::Leave(intent));
        }
    }

    /// Esc closes the page, unless drafts need a word first.
    pub(super) fn esc_leave(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.guard_leave(LeaveIntent::Close, window, cx) {
            cx.emit(PrPageEvent::Leave(LeaveIntent::Close));
        }
    }

    /// Close the confirm when the drafts are gone (a batch confirmed
    /// under it); called every render.
    pub(super) fn sync_leave_confirm(&mut self, cx: &Context<Self>) {
        if self.leave_confirm.is_some() && self.review_draft_count(cx) == 0 {
            self.leave_confirm = None;
        }
    }

    pub(super) fn render_leave_confirm(&self, cx: &Context<Self>) -> Option<AnyElement> {
        self.leave_confirm?;
        let prompt = discard_prompt(self.review_draft_count(cx));
        Some(
            v_flex()
                .id("pr-discard-confirm")
                .debug_selector(|| "pr-discard-confirm".to_string())
                .role(accesskit::Role::Group)
                .aria_label(prompt.clone())
                .gap_2()
                .px_4()
                .py_2()
                .child(div().text_sm().text_color(cx.theme().warning).child(prompt))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("pr-discard-confirm-go")
                                .debug_selector(|| "pr-discard-confirm-go".to_string())
                                .label("Discard")
                                .danger()
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| this.confirm_discard(cx))),
                        )
                        .child(
                            Button::new("pr-discard-confirm-keep")
                                .debug_selector(|| "pr-discard-confirm-keep".to_string())
                                .label("Keep editing")
                                .ghost()
                                .small()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.cancel_leave_confirm(window, cx);
                                })),
                        ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests;
