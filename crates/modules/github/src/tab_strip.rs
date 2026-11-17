//! A named tab strip (#734, reworked at gpui-kit v0.10.0).
//!
//! The pinned `Tabs` owns the keyboard contract (ARIA tabs, manual
//! activation): the list is one Tab stop on the selected tab, Left/Right
//! (Up/Down on a vertical list) move focus and wrap, Home/End jump to the
//! ends, Enter/Space select, disabled tabs are skipped, and Tab leaves
//! the list. The strip adds an accessible name that carries the
//! selection ("Pull request sections: Commits") and a focus handle that
//! is not a Tab stop: the upstream tab does not take focus on a pointer
//! click, so the click hands focus to the strip, as in a browser. The
//! content the tab switch replaces may hold the focus, and a focus left
//! on an unmounted element makes Tab do nothing. The handle draws no
//! focus ring: the tab keeps the ring for keyboard focus.

use gpui::prelude::*;
use gpui::{App, Context, FocusHandle, SharedString, Window, accesskit::Role, div};
use gpui_component::ActiveTheme as _;
use gpui_component::tab::{Tab, TabBar};

/// The strip's focus handle, kept in window state under `id` so the
/// owning page needs no field for it. Not a Tab stop (see above).
pub(crate) fn focus_handle(id: &'static str, window: &mut Window, cx: &mut App) -> FocusHandle {
    window
        .use_keyed_state(SharedString::from(format!("{id}-focus")), cx, |_, cx| {
            cx.focus_handle().tab_stop(false)
        })
        .read(cx)
        .clone()
}

/// One tab's text (#796): the name, and an optional count drawn muted
/// beside it. The accessible name always carries the count in
/// parentheses ("Checks (0)") — also when `hide_zero` hides a zero — so
/// a tab with no count yet ("Checks") still reads differently from a
/// loaded empty one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabText {
    pub name: &'static str,
    pub count: Option<i64>,
    pub hide_zero: bool,
}

impl TabText {
    pub(crate) fn plain(name: &'static str) -> Self {
        Self {
            name,
            count: None,
            hide_zero: false,
        }
    }

    /// The count to draw, if any.
    pub(crate) fn shown_count(&self) -> Option<i64> {
        self.count.filter(|c| !(self.hide_zero && *c == 0))
    }

    pub(crate) fn accessible(&self) -> SharedString {
        match self.count {
            Some(c) => format!("{} ({c})", self.name).into(),
            None => self.name.into(),
        }
    }
}

/// One tab strip. `name` is the group's accessible name; `on_select`
/// runs for a pick by pointer or keyboard.
pub(crate) fn strip<V, F>(
    id: &'static str,
    name: &str,
    focus: &FocusHandle,
    labels: Vec<TabText>,
    selected: usize,
    on_select: F,
    cx: &Context<V>,
) -> impl IntoElement + use<V, F>
where
    V: 'static,
    F: Fn(&mut V, usize, &mut Window, &mut Context<V>) + 'static,
{
    let current = labels
        .get(selected)
        .map(TabText::accessible)
        .unwrap_or_default();
    let click_focus = focus.clone();
    let muted = cx.theme().muted_foreground;
    let bar = TabBar::new(SharedString::from(format!("{id}-bar")))
        .underline()
        .w_full()
        .selected_index(selected)
        .on_click(cx.listener(move |this, ix: &usize, window, cx| on_select(this, *ix, window, cx)))
        .children(labels.into_iter().map(|l| {
            let tab = Tab::new().label(l.name).aria_label(l.accessible());
            match l.shown_count() {
                Some(c) => tab.suffix(
                    div()
                        .ml_1()
                        .text_color(muted)
                        .child(SharedString::from(c.to_string())),
                ),
                None => tab,
            }
        }));
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .role(Role::Group)
        .aria_label(format!("{name}: {current}"))
        .w_full()
        .track_focus(focus)
        // The pointer press, not the click: a keyboard activation must
        // keep focus on the tab it came from.
        .capture_any_mouse_down(move |_, window, cx| window.focus(&click_focus, cx))
        .child(bar)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_counts_hide_only_where_asked_and_stay_in_the_accessible_name() {
        let t = |count, hide_zero| TabText {
            name: "Checks",
            count,
            hide_zero,
        };
        assert_eq!(t(Some(0), true).shown_count(), None);
        assert_eq!(t(Some(0), false).shown_count(), Some(0));
        assert_eq!(t(Some(4), true).shown_count(), Some(4));
        assert_eq!(t(None, true).shown_count(), None);
        assert_eq!(t(Some(0), true).accessible(), "Checks (0)");
        assert_eq!(t(None, true).accessible(), "Checks");
    }
    use gpui::{FocusHandle, Render, TestAppContext};
    use gpui_component::Root;

    struct Harness {
        selected: usize,
        strip: FocusHandle,
        before: FocusHandle,
        after: FocusHandle,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let labels = ["One", "Two", "Three"].map(TabText::plain).to_vec();
            div()
                .child(div().id("before").track_focus(&self.before).child("before"))
                .child(strip(
                    "probe-tabs",
                    "Probe sections",
                    &self.strip,
                    labels,
                    self.selected,
                    |this: &mut Self, ix, _, _| this.selected = ix,
                    cx,
                ))
                .child(div().id("after").track_focus(&self.after).child("after"))
        }
    }

    /// A full key press: GPUI activates a focused element on key release.
    fn press(vcx: &mut gpui::VisualTestContext, key: &'static str) {
        vcx.simulate_keystrokes(key);
        vcx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse(key).unwrap(),
        });
    }

    #[gpui::test]
    fn strip_is_one_stop_arrows_move_focus_and_enter_selects(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let mut probe = None;
        let (_root, vcx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| Harness {
                selected: 0,
                strip: cx.focus_handle().tab_stop(false),
                before: cx.focus_handle().tab_stop(true),
                after: cx.focus_handle().tab_stop(true),
            });
            probe = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = probe.unwrap();
        let (before, after) = vcx.read(|cx| {
            let h = view.read(cx);
            (h.before.clone(), h.after.clone())
        });
        let selected = |vcx: &mut gpui::VisualTestContext| vcx.read(|cx| view.read(cx).selected);
        vcx.update(|window, cx| window.focus(&before, cx));
        // One stop: Tab enters the strip on the selected tab.
        vcx.simulate_keystrokes("tab");
        vcx.update(|window, _| assert!(!before.is_focused(window) && !after.is_focused(window)));
        // Arrows move focus without selecting (manual activation).
        vcx.simulate_keystrokes("right right");
        assert_eq!(selected(vcx), 0, "moving focus does not select");
        press(vcx, "enter");
        assert_eq!(selected(vcx), 2, "Enter selects the focused tab");
        // Wraps at the end, and Home / End jump.
        vcx.simulate_keystrokes("right");
        press(vcx, "space");
        assert_eq!(selected(vcx), 0, "Right wraps to the first tab");
        vcx.simulate_keystrokes("end");
        press(vcx, "enter");
        assert_eq!(selected(vcx), 2);
        vcx.simulate_keystrokes("home");
        press(vcx, "enter");
        assert_eq!(selected(vcx), 0);
        // The next Tab leaves the strip.
        vcx.simulate_keystrokes("tab");
        vcx.update(|window, _| assert!(after.is_focused(window), "Tab leaves the strip"));
    }
}
