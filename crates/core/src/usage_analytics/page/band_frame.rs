//! What every band of both views shares (docs/0082 D18, D21): the titled
//! section, the muted summary line, the band-specific Empty, and the
//! skeleton a band shows while its derivation is in flight.

use gpui::prelude::*;
use gpui::{AnyElement, App, SharedString, div, px};
use gpui_component::{ActiveTheme, h_flex, skeleton::Skeleton, v_flex};

use crate::ui::{empty_state, section_header};

/// A band: its title (and the band's query bar, if any) over the body.
pub(super) fn section(
    id: &'static str,
    title: impl Into<SharedString>,
    bar: Option<impl IntoElement>,
    body: AnyElement,
) -> AnyElement {
    let title = title.into();
    v_flex()
        .id(id)
        .role(gpui::accesskit::Role::Group)
        .aria_label(title.clone())
        .w_full()
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_3()
                .flex_wrap()
                .child(section_header(title))
                .when_some(bar, |this, bar| this.child(div().ml_auto().child(bar))),
        )
        .child(body)
        .into_any_element()
}

/// The band's summary line.
pub(super) fn summary(text: String, cx: &App) -> impl IntoElement {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// The band's Empty (D18): one sentence and the next step.
pub(super) fn empty(id: &'static str, copy: String, cx: &App) -> impl IntoElement {
    let label = SharedString::from(copy);
    empty_state(label.clone(), cx)
        .id(id)
        .role(gpui::accesskit::Role::Label)
        .aria_label(label)
}

/// The band's skeleton while a derivation is in flight (D11, D18).
pub(super) fn skeleton(id: &'static str, height: f32) -> AnyElement {
    div()
        .id(id)
        .role(gpui::accesskit::Role::ProgressIndicator)
        .aria_label("Updating")
        .w_full()
        .child(Skeleton::new().w_full().h(px(height)))
        .into_any_element()
}
