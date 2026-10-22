//! Home overview card (docs/0064 §4/D1): the live 180px preview + a
//! "Generate favicons" action into the module page. Observes the shared
//! model entity — same truth as the page (docs/0003 §4.1).

//! Home overview card (docs/0064 §4/D1): the live 180px preview + a
//! "Generate favicons" action into the module page. Observes the shared
//! model entity — same truth as the page (docs/0003 §4.1).

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Sizable as _,
    button::{Button, ButtonVariants},
    h_flex, v_flex,
};

use dashboard_core::nav::{Navigator, Page};
use dashboard_core::ui::{CardVariant, card};

use crate::page::FaviconModel;

pub(crate) struct FaviconCard {
    model: Entity<FaviconModel>,
    nav: Entity<Navigator>,
}

impl FaviconCard {
    pub fn new(
        model: Entity<FaviconModel>,
        nav: Entity<Navigator>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        Self { model, nav }
    }

    fn open_module(&mut self, cx: &mut Context<Self>) {
        self.nav.update(cx, |nav, cx| {
            nav.go(Page::Module("favicon".into()), cx);
        });
    }
}

impl Render for FaviconCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        let preview = model.stage_frame(180);
        let spec = model.spec();
        let summary = match &spec.source {
            crate::spec::Source::Text { chars, family, .. } => {
                format!("“{chars}” in {family}")
            }
            crate::spec::Source::Emoji { glyph, .. } => format!("{glyph} emoji"),
            crate::spec::Source::Image { w, h, .. } if *w > 0 => format!("{w}×{h} image"),
            crate::spec::Source::Image { .. } => "image source".into(),
            crate::spec::Source::Svg { w, h, .. } if *w > 0 => format!("{w}×{h} SVG"),
            crate::spec::Source::Svg { .. } => "SVG source".into(),
        };
        let selected = spec.platform_code();

        card(cx, CardVariant::Interactive)
            .id("favicon-card")
            .size_full()
            .role(accesskit::Role::ListItem)
            .aria_label(format!(
                "Open Favicon generator — {summary}, platforms {selected}"
            ))
            .on_click(cx.listener(|this, _, _, cx| this.open_module(cx)))
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .when_some(preview, |el, frame| {
                                el.child(img(frame).size(px(44.)).rounded_sm())
                            })
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("Favicon generator"),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(summary),
                                    ),
                            ),
                    )
                    .child(
                        h_flex().gap_2().child(
                            Button::new("favicon-card-open")
                                .label("Generate favicons")
                                .ghost()
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| this.open_module(cx))),
                        ),
                    ),
            )
    }
}
