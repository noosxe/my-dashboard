//! The fixed header (docs/0082 D3): the view switch leads on the left
//! (Q13); the range presets and Refresh sit right-aligned on the same
//! row; the honesty line runs beneath. Both segmented controls are one
//! Tab stop each (the upstream `TabBar` owns the keyboard path, D19).

use gpui::prelude::*;
use gpui::{Context, FocusHandle, SharedString, Window, div};
use gpui_component::{
    ActiveTheme, IconName, Sizable as _,
    button::{Button, ButtonVariants},
    h_flex,
    tab::{Tab, TabBar},
};

use super::range::{self, preset_label};
use super::{UsagePage, UsageView, states};

impl UsagePage {
    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let presets = range::presets(self.view);
        let effective = self.effective_preset();
        let preset_ix = presets.iter().position(|p| *p == effective).unwrap_or(0);
        let view_ix = UsageView::all()
            .iter()
            .position(|v| *v == self.view)
            .unwrap_or(0);
        let focus = self.focus.as_ref();

        let view_switch = segmented_stop(
            "usage-view",
            "Usage view",
            focus.map(|f| &f.view),
            UsageView::all().iter().map(|v| v.label()).collect(),
            view_ix,
            |this, ix, window, cx| {
                if let Some(view) = UsageView::all().get(ix) {
                    this.switch_view(*view, window, cx);
                }
            },
            cx,
        );
        let range_switch = segmented_stop(
            "usage-range",
            "Range",
            focus.map(|f| &f.range),
            presets.iter().map(|p| preset_label(*p)).collect(),
            preset_ix,
            |this, ix, _, cx| {
                if let Some(p) = range::presets(this.view).get(ix) {
                    this.set_preset(*p, cx);
                }
            },
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

/// A segmented control (D19). The upstream `TabBar` inside draws the
/// segments, takes the pointer and owns the keyboard path (gpui-kit
/// v0.10.0: one Tab stop, arrows move focus, Enter/Space select). The
/// wrapper carries the accessible name and a focus handle that is not a
/// Tab stop: a pointer pick leaves the focus nowhere the upstream tab
/// takes it, so the view switch hands it to the wrapper (see `focus.rs`).
#[allow(clippy::too_many_arguments)]
pub(super) fn segmented_stop<F>(
    id: &'static str,
    name: &str,
    focus: Option<&FocusHandle>,
    labels: Vec<&'static str>,
    selected: usize,
    on_select: F,
    cx: &mut Context<UsagePage>,
) -> impl IntoElement + use<F>
where
    F: Fn(&mut UsagePage, usize, &mut Window, &mut Context<UsagePage>) + Clone + 'static,
{
    let current = labels.get(selected).copied().unwrap_or_default();
    let pick = on_select.clone();
    let bar = TabBar::new(SharedString::from(format!("{id}-tabs")))
        .segmented()
        .selected_index(selected)
        .on_click(cx.listener(move |this, ix: &usize, window, cx| {
            pick(this, *ix, window, cx);
            cx.notify();
        }))
        .children(labels.into_iter().map(|l| Tab::new().label(l)));
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .role(gpui::accesskit::Role::Group)
        .aria_label(format!("{name}: {current}"))
        .rounded(cx.theme().radius_lg)
        .when_some(focus, |this, f| this.track_focus(f))
        .child(bar)
}
