//! Tab leaves a multi-line text field instead of indenting (#733).
//!
//! The pinned gpui-component binds `tab` / `shift-tab` to its inline
//! indent actions in the `Input` key context, and a multi-line field
//! registers a handler for them, so the keys never reach the window's
//! focus traversal and the field is a keyboard trap. An `Unbind` of those
//! two actions in a deeper context (`<page> > Input`) removes the
//! library's binding for that page only; the keystroke then falls
//! through to the window's own Tab / Shift-Tab handling. (A `NoAction`
//! binding would also swallow the window's Tab binding.) Indenting stays
//! available through the library's block bindings (`ctrl-]` / `ctrl-[`,
//! `cmd-]` / `cmd-[` on macOS).

use gpui::{KeyBinding, Unbind};

/// Bindings that release Tab / Shift-Tab inside `page_context`'s fields.
pub(crate) fn bindings(page_context: &str) -> [KeyBinding; 2] {
    let context = format!("{page_context} > Input");
    [
        KeyBinding::new("tab", Unbind("input::IndentInline".into()), Some(&context)),
        KeyBinding::new(
            "shift-tab",
            Unbind("input::OutdentInline".into()),
            Some(&context),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        AppContext as _, Context, FocusHandle, InteractiveElement as _, IntoElement,
        ParentElement as _, Render, Styled as _, TestAppContext, Window, div,
    };
    use gpui_component::{
        Root,
        input::{Textarea, TextareaState},
    };

    struct Probe {
        area: gpui::Entity<TextareaState>,
        before: FocusHandle,
        after: FocusHandle,
    }

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .key_context("ProbePage")
                .size_full()
                .child(div().id("before").track_focus(&self.before).child("before"))
                .child(Textarea::new(&self.area))
                .child(div().id("after").track_focus(&self.after).child("after"))
        }
    }

    fn rig(
        cx: &mut TestAppContext,
        escape: bool,
    ) -> (
        FocusHandle,
        FocusHandle,
        FocusHandle,
        &mut gpui::VisualTestContext,
    ) {
        cx.update(gpui_component::init);
        if escape {
            cx.update(|cx| cx.bind_keys(bindings("ProbePage")));
        }
        let mut handles = None;
        let (_root, vcx) = cx.add_window_view(|window, cx| {
            let before = cx.focus_handle().tab_stop(true);
            let after = cx.focus_handle().tab_stop(true);
            let area = cx.new(|cx| TextareaState::new(window, cx));
            handles = Some((
                before.clone(),
                after.clone(),
                gpui::Focusable::focus_handle(&area, cx),
            ));
            let probe = cx.new(|_| Probe {
                area,
                before,
                after,
            });
            Root::new(probe, window, cx)
        });
        let (before, after, area) = handles.unwrap();
        (before, after, area, vcx)
    }

    fn focus_area(vcx: &mut gpui::VisualTestContext, before: &FocusHandle) {
        vcx.update(|window, cx| window.focus(before, cx));
        vcx.simulate_keystrokes("tab");
    }

    #[gpui::test]
    fn tab_leaves_the_textarea_forward_and_back(cx: &mut TestAppContext) {
        let (before, after, area, vcx) = rig(cx, true);
        focus_area(vcx, &before);
        vcx.update(|window, _| assert!(area.is_focused(window), "Tab enters the textarea"));
        vcx.simulate_input("x");
        vcx.simulate_keystrokes("tab");
        vcx.update(|window, _| assert!(after.is_focused(window), "Tab moves to the next stop"));
        vcx.simulate_keystrokes("shift-tab");
        vcx.simulate_keystrokes("shift-tab");
        vcx.update(|window, _| assert!(before.is_focused(window), "Shift-Tab walks back out"));
    }

    #[gpui::test]
    fn without_the_binding_tab_is_trapped(cx: &mut TestAppContext) {
        let (before, after, area, vcx) = rig(cx, false);
        focus_area(vcx, &before);
        vcx.update(|window, _| assert!(area.is_focused(window), "Tab enters the textarea"));
        vcx.simulate_keystrokes("tab");
        vcx.update(|window, _| assert!(!after.is_focused(window), "the library binding traps"));
    }
}
