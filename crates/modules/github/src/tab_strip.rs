//! A tab strip that is one keyboard stop (#734).
//!
//! The pinned `TabBar` draws the tabs (role `Tab`, selected state, set
//! position) and takes the pointer, but has no keyboard path. The strip
//! wraps it in a focusable `TabList` node: one Tab stop, Left/Right
//! (Up/Down) move to the neighbouring tab, Home/End jump to the ends,
//! selection follows focus (automatic activation) and never wraps. Tab
//! and Shift-Tab pass through so the walk continues past the strip.

use gpui::prelude::*;
use gpui::{App, Context, FocusHandle, KeyDownEvent, SharedString, Window, accesskit::Role, div};
use gpui_component::{
    ThemeStyled as _,
    tab::{Tab, TabBar},
};

/// The strip's focus handle, kept in window state under `id` so the
/// owning page needs no field for it.
pub(crate) fn focus_handle(id: &'static str, window: &mut Window, cx: &mut App) -> FocusHandle {
    window
        .use_keyed_state(SharedString::from(format!("{id}-focus")), cx, |_, cx| {
            cx.focus_handle().tab_stop(true)
        })
        .read(cx)
        .clone()
}

/// The tab an arrow / Home / End key moves to from `selected` among
/// `count`: clamped, never wrapping; `None` for keys the strip ignores.
pub(crate) fn key_target(key: &str, selected: usize, count: usize) -> Option<usize> {
    let last = count.saturating_sub(1);
    Some(match key {
        "left" | "up" => selected.saturating_sub(1),
        "right" | "down" => (selected + 1).min(last),
        "home" => 0,
        "end" => last,
        _ => return None,
    })
}

/// One tab strip. `name` is the list's accessible name; `on_select`
/// runs for pointer clicks and keys alike.
#[allow(clippy::too_many_arguments)]
pub(crate) fn strip<V, F>(
    id: &'static str,
    name: &str,
    focus: &FocusHandle,
    labels: Vec<SharedString>,
    selected: usize,
    on_select: F,
    window: &Window,
    cx: &Context<V>,
) -> impl IntoElement + use<V, F>
where
    V: 'static,
    F: Fn(&mut V, usize, &mut Window, &mut Context<V>) + Clone + 'static,
{
    let count = labels.len();
    let current = labels.get(selected).cloned().unwrap_or_default();
    let pick = on_select.clone();
    let click_focus = focus.clone();
    let bar = TabBar::new(SharedString::from(format!("{id}-bar")))
        .underline()
        .w_full()
        .selected_index(selected)
        .on_click(cx.listener(move |this, ix: &usize, window, cx| {
            // A click takes focus onto the strip, as in a browser: the
            // content it replaces may hold the focus, and a focus left on
            // an unmounted element makes Tab do nothing.
            window.focus(&click_focus, cx);
            pick(this, *ix, window, cx)
        }))
        .children(labels.into_iter().map(|l| Tab::new().label(l)));
    let focused = focus.is_focused(window);
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .role(Role::TabList)
        .aria_label(format!("{name}: {current}"))
        .w_full()
        .track_focus(focus)
        .when(focused, |this| this.focus_ring_style(window, cx))
        .on_key_down(cx.listener(move |this, ev: &KeyDownEvent, window, cx| {
            let Some(next) = key_target(ev.keystroke.key.as_str(), selected, count) else {
                return;
            };
            cx.stop_propagation();
            if next != selected {
                on_select(this, next, window, cx);
                cx.notify();
            }
        }))
        .child(bar)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Render, TestAppContext};
    use gpui_component::Root;

    struct Harness {
        selected: usize,
        focus: FocusHandle,
        after: FocusHandle,
    }

    impl Render for Harness {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let labels = ["One", "Two", "Three"].map(SharedString::from).to_vec();
            div()
                .child(strip(
                    "probe-tabs",
                    "Probe sections",
                    &self.focus,
                    labels,
                    self.selected,
                    |this: &mut Self, ix, _, _| this.selected = ix,
                    window,
                    cx,
                ))
                .child(div().id("after").track_focus(&self.after).child("after"))
        }
    }

    #[gpui::test]
    fn strip_is_one_stop_and_arrows_select(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let mut probe = None;
        let (_root, vcx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| Harness {
                selected: 0,
                focus: cx.focus_handle().tab_stop(true),
                after: cx.focus_handle().tab_stop(true),
            });
            probe = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = probe.unwrap();
        let (focus, after) = vcx.read(|cx| {
            let h = view.read(cx);
            (h.focus.clone(), h.after.clone())
        });
        vcx.update(|window, cx| window.focus(&focus, cx));
        let selected = |vcx: &mut gpui::VisualTestContext| vcx.read(|cx| view.read(cx).selected);
        vcx.simulate_keystrokes("right right right");
        assert_eq!(selected(vcx), 2, "clamped at the last tab");
        vcx.simulate_keystrokes("home");
        assert_eq!(selected(vcx), 0);
        vcx.simulate_keystrokes("end left");
        assert_eq!(selected(vcx), 1);
        vcx.update(|window, _| assert!(focus.is_focused(window), "focus stays on the strip"));
        vcx.simulate_keystrokes("tab");
        vcx.update(|window, _| assert!(after.is_focused(window), "Tab leaves the strip"));
    }

    #[test]
    fn keys_move_within_the_strip_and_clamp() {
        assert_eq!(key_target("right", 0, 4), Some(1));
        assert_eq!(key_target("down", 3, 4), Some(3), "no wrap at the end");
        assert_eq!(key_target("left", 0, 4), Some(0), "no wrap at the start");
        assert_eq!(key_target("up", 2, 4), Some(1));
        assert_eq!(key_target("home", 3, 4), Some(0));
        assert_eq!(key_target("end", 1, 4), Some(3));
        assert_eq!(key_target("tab", 1, 4), None, "Tab passes through");
    }
}
