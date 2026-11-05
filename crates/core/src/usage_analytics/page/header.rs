//! The fixed header (docs/0082 D3): the view switch leads on the left
//! (Q13); the range presets and Refresh sit right-aligned on the same
//! row; the honesty line runs beneath. Both segmented controls are one
//! Tab stop each with arrow keys inside (D19).

use gpui::prelude::*;
use gpui::{Context, FocusHandle, KeyDownEvent, SharedString, Window, div};
use gpui_component::{
    ActiveTheme, IconName, Sizable as _, ThemeStyled as _,
    button::{Button, ButtonVariants},
    h_flex,
    tab::{Tab, TabBar},
};

use super::range::{self, preset_label};
use super::{UsagePage, UsageView, states};

impl UsagePage {
    pub(super) fn render_header(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let presets = range::presets(self.view);
        let effective = self.effective_preset();
        let preset_ix = presets.iter().position(|p| *p == effective).unwrap_or(0);
        let view_ix = UsageView::all()
            .iter()
            .position(|v| *v == self.view)
            .unwrap_or(0);
        let focus = self.focus.clone();

        let view_switch = segmented_stop(
            "usage-view",
            "Usage view",
            focus.as_ref().map(|f| &f.0),
            UsageView::all().iter().map(|v| v.label()).collect(),
            view_ix,
            |this, ix, cx| {
                if let Some(view) = UsageView::all().get(ix) {
                    this.set_view(*view, cx);
                }
            },
            window,
            cx,
        );
        let range_switch = segmented_stop(
            "usage-range",
            "Range",
            focus.as_ref().map(|f| &f.1),
            presets.iter().map(|p| preset_label(*p)).collect(),
            preset_ix,
            |this, ix, cx| {
                if let Some(p) = range::presets(this.view).get(ix) {
                    this.set_preset(*p, cx);
                }
            },
            window,
            cx,
        );
        let refresh = Button::new("usage-refresh")
            .debug_selector(|| "usage-refresh".to_string())
            .icon(IconName::RotateCw)
            .ghost()
            .small()
            .tooltip("Refresh — re-stamp 'as of' and re-read the history")
            // docs/0049 D1: icon-only buttons get an explicit
            // accessible name.
            .accessibility_label("Refresh")
            .on_click(cx.listener(|this, _, _, cx| {
                this.refresh(cx);
                cx.notify();
            }));

        let controls = h_flex()
            .items_center()
            .gap_3()
            .flex_wrap()
            .child(view_switch)
            .child(
                h_flex()
                    .ml_auto()
                    .items_center()
                    .gap_2()
                    .child(range_switch)
                    .child(refresh),
            );

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(controls)
            .child(self.render_honesty(cx))
    }

    /// The honesty row (D3, 0054 D2). A Status node (docs/0049): it
    /// changes under the viewer, and it doubles as the bridge-visible
    /// freeze signal.
    fn render_honesty(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let line = self.honesty().line();
        let degraded = self.degraded_shown();
        let status_label = if degraded {
            format!("{line} · {}", states::DEGRADED_COPY)
        } else {
            line.clone()
        };
        h_flex()
            .id("usage-honesty")
            .role(gpui::accesskit::Role::Status)
            .aria_label(status_label)
            .flex_wrap()
            .items_center()
            .gap_x_2()
            .text_sm()
            .child(div().text_color(cx.theme().muted_foreground).child(line))
            .when(degraded, |this| this.child(states::degraded_chip(cx)))
    }
}

/// A segmented control as one Tab stop (D19): the focusable wrapper
/// carries the a11y node (docs/0010 D10: id + focus + role) and the
/// arrow keys; the upstream `TabBar` inside draws the segments and
/// takes the pointer. Upstream's `TabBar` has no keyboard path.
#[allow(clippy::too_many_arguments)]
fn segmented_stop<F>(
    id: &'static str,
    name: &str,
    focus: Option<&FocusHandle>,
    labels: Vec<&'static str>,
    selected: usize,
    on_select: F,
    window: &Window,
    cx: &mut Context<UsagePage>,
) -> impl IntoElement + use<F>
where
    F: Fn(&mut UsagePage, usize, &mut Context<UsagePage>) + Clone + 'static,
{
    let count = labels.len();
    let current = labels.get(selected).copied().unwrap_or_default();
    let pick = on_select.clone();
    let bar = TabBar::new(SharedString::from(format!("{id}-tabs")))
        .segmented()
        .selected_index(selected)
        .on_click(cx.listener(move |this, ix: &usize, _, cx| {
            pick(this, *ix, cx);
            cx.notify();
        }))
        .children(labels.into_iter().map(|l| Tab::new().label(l)));
    let focused = focus.is_some_and(|f| f.is_focused(window));
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .role(gpui::accesskit::Role::TabList)
        .aria_label(format!("{name}: {current}"))
        .rounded(cx.theme().radius_lg)
        .when_some(focus, |this, f| this.track_focus(f))
        .when(focused, |this| this.focus_ring_style(window, cx))
        .on_key_down(cx.listener(move |this, ev: &KeyDownEvent, _, cx| {
            let Some(next) = arrow_target(ev.keystroke.key.as_str(), selected, count) else {
                return;
            };
            cx.stop_propagation();
            if next != selected {
                on_select(this, next, cx);
                cx.notify();
            }
        }))
        .child(bar)
}

/// The segment an arrow key moves to from `selected` among `count`
/// (D19): clamped, never wrapping; `None` for keys the control ignores.
fn arrow_target(key: &str, selected: usize, count: usize) -> Option<usize> {
    let last = count.saturating_sub(1);
    Some(match key {
        "left" | "up" => selected.saturating_sub(1),
        "right" | "down" => (selected + 1).min(last),
        "home" => 0,
        "end" => last,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::arrow_target;

    #[test]
    fn arrows_move_within_the_control_and_clamp() {
        assert_eq!(arrow_target("right", 0, 6), Some(1));
        assert_eq!(arrow_target("down", 5, 6), Some(5), "no wrap at the end");
        assert_eq!(arrow_target("left", 0, 6), Some(0), "no wrap at the start");
        assert_eq!(arrow_target("up", 3, 6), Some(2));
        assert_eq!(arrow_target("home", 4, 6), Some(0));
        assert_eq!(arrow_target("end", 1, 6), Some(5));
        // Tab and everything else pass through: the control is one stop.
        assert_eq!(arrow_target("tab", 1, 6), None);
    }
}
