//! D19: focus survives a view switch. A switch unmounts the outgoing
//! view's controls; a focus handle left on one of them belongs to no
//! rendered element, so key dispatch falls back to the window root,
//! which is outside the shell's Tab binding: Tab stops moving and the
//! `tree --tab-stops` cycle never closes (#700). A pointer switch made
//! while a view control holds focus therefore hands focus to the view
//! switch, the control the user just used. A keyboard switch already
//! sits there.

use gpui::{App, Context, FocusHandle, Window};

use super::{UsagePage, UsageView};

impl UsagePage {
    /// The view switch's pick (D1, D19): switch, and keep focus on a
    /// rendered element.
    pub(super) fn switch_view(
        &mut self,
        view: UsageView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let orphaned = view != self.view
            && self
                .view_focus_handles(self.view, cx)
                .iter()
                .any(|h| h.contains_focused(window, cx));
        self.set_view(view, cx);
        if orphaned && let Some(f) = &self.focus {
            window.focus(&f.view, cx);
        }
    }

    /// The focus handles `view` owns, which a switch away unmounts.
    fn view_focus_handles(&self, view: UsageView, cx: &App) -> Vec<FocusHandle> {
        let mut handles = Vec::new();
        match view {
            UsageView::Budget => {
                handles.extend(self.focus.as_ref().map(|f| f.budget.clone()));
                if let Some(c) = &self.budget_controls {
                    handles.extend(c.focus_handles(cx));
                }
            }
            UsageView::Requests => {
                if let Some(f) = &self.focus {
                    handles.extend([f.transport.clone(), f.diagnostics.clone()]);
                }
                if let Some(c) = &self.requests_controls {
                    handles.extend(c.focus_handles(cx));
                }
            }
        }
        handles
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::TestAppContext;

    use super::*;
    use crate::usage_records::{self, MemorySink, UsageRecord, UsageStore, init_with, sink};

    #[gpui::test]
    fn a_pointer_switch_hands_orphaned_focus_to_the_view_switch(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let store = Arc::new(MemorySink::default());
        cx.update(|cx| init_with(cx, store.clone() as Arc<dyn UsageStore>));
        // One record, so the Budget view draws its query bar (D18).
        let mut r = UsageRecord::new("github", "pr-detail", "pr-page", "view-shown");
        r.ts_epoch_ms = usage_records::now_epoch_ms() - 60_000;
        r.status = 200;
        cx.update(|cx| sink(cx).record(r));
        let (page, window_cx) = cx.add_window_view(UsagePage::new);
        window_cx.run_until_parked();
        let select = window_cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            // D7's `Group by`, as a pointer pick leaves it.
            let select = page
                .read(cx)
                .budget_controls
                .as_ref()
                .unwrap()
                .focus_handles(cx)[0]
                .clone();
            window.focus(&select, cx);
            window.draw(cx).clear(cx);
            select
        });
        window_cx.update(|window, cx| {
            assert!(select.contains_focused(window, cx), "the select is drawn");
            page.update(cx, |p, cx| p.switch_view(UsageView::Requests, window, cx));
            let p = page.read(cx);
            assert_eq!(p.view, UsageView::Requests);
            assert!(p.focus.as_ref().unwrap().view.is_focused(window));
        });
        // A switch with focus elsewhere leaves focus where it is.
        window_cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let range = page.read(cx).focus.as_ref().unwrap().range.clone();
            window.focus(&range, cx);
            page.update(cx, |p, cx| p.switch_view(UsageView::Budget, window, cx));
            assert!(range.is_focused(window));
        });
    }
}
