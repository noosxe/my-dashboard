//! One gate row (docs/0083 D3, D14): icon, label, optional muted value.
//! A row with somewhere to go is a ghost button-like row with hover, a
//! focus ring and `Role::Link`; a static one is a plain content node.

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, accesskit, div, px,
};
use gpui_component::{
    ActiveTheme, Sizable as _,
    button::{Button, ButtonVariants},
    h_flex,
    tooltip::Tooltip,
};

use super::gates::{Gate, Go, Trailing};
use super::glyph::glyph;
use super::metrics::ROW_H;
use crate::pr_page::{PrDetailPage, PrTab};

/// What sits left of the trailing control: icon, label, value.
fn content(gate: &Gate, cx: &gpui::App) -> AnyElement {
    h_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .items_center()
        .child(glyph(gate.glyph, gate.tone, cx))
        .child(
            div()
                .id(SharedString::from(format!("{}-label", gate.kind.id())))
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_sm()
                .text_color(cx.theme().foreground)
                // A long base name truncates beside the Update button; the
                // full text is one hover away.
                .tooltip({
                    let label = gate.label.clone();
                    move |window, cx| Tooltip::new(label.clone()).build(window, cx)
                })
                .child(gate.label.clone()),
        )
        .children(gate.value.clone().map(|value| {
            div()
                .ml_auto()
                .flex_none()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(value)
        }))
        .into_any_element()
}

/// The row. `trailing` is the Update split button or the conflicts link;
/// `None` for the rows without one.
pub(super) fn gate_row(
    gate: &Gate,
    url: &str,
    trailing: Option<AnyElement>,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    let key = gate.kind.id();
    let spoken = SharedString::from(gate.spoken());
    let body = match &gate.go {
        Go::Nowhere => h_flex()
            .id(key)
            .debug_selector(move || key.to_string())
            .role(accesskit::Role::Label)
            .aria_label(spoken)
            .h(px(ROW_H))
            .min_w_0()
            .flex_1()
            .child(content(gate, cx))
            .into_any_element(),
        go => {
            let go = go.clone();
            let tooltip: SharedString = match go {
                Go::Review(_) => "Show the review in the conversation".into(),
                _ => "Open the Checks tab".into(),
            };
            div()
                .min_w_0()
                .flex_1()
                .debug_selector(move || key.to_string())
                .child(
                    Button::new(key)
                        .ghost()
                        .small()
                        .w_full()
                        .h(px(ROW_H))
                        .px_0()
                        .role(accesskit::Role::Link)
                        .accessibility_label(spoken)
                        .tooltip(tooltip)
                        .child(content(gate, cx))
                        .on_click(cx.listener(move |this, _, _, cx| match &go {
                            Go::ChecksTab => {
                                this.tab = PrTab::Checks;
                                cx.notify();
                            }
                            Go::Review(id) => this.jump_to_review(id, cx),
                            Go::Nowhere => {}
                        })),
                )
                .into_any_element()
        }
    };
    let link = (gate.trailing == Trailing::ResolveOnGithub).then(|| {
        let url = format!("{}/conflicts", url.trim_end_matches('/'));
        Button::new("pr-rail-resolve")
            .debug_selector(|| "pr-rail-resolve".to_string())
            .link()
            .small()
            .label("Resolve on GitHub")
            .tooltip("Resolve the conflicts on github.com")
            .on_click(move |_, _, cx| cx.open_url(&url))
            .into_any_element()
    });
    h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .child(body)
        .children(trailing.or(link))
        .into_any_element()
}
