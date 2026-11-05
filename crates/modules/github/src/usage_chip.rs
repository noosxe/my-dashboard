//! The status-bar usage chip (docs/0050 D5 as amended by docs/0082
//! D13), contributed through `ModuleEntry::status_item` and shown on
//! every page (Q9): `REST 3,712 · ⚠ GraphQL 1,140 Low`. GitHub's
//! reported numbers, verbatim (source follows audience, 0082 §4).
//!
//! - [`model`] derives what the chip shows: the book's snapshots with
//!   each budget's own pause laid over them (Q10), marked by the shared
//!   D5 ladder, plus the spoken label and the tooltip.
//! - [`critical`] keeps the D5 Critical toast to one per budget per
//!   window.
//!
//! This file is the view: a 1 Hz self-refresh and the render.

use gpui::prelude::*;
use gpui::{Context, Entity, FontWeight, Render, SharedString, Window, div};
use gpui_component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants},
    h_flex,
};

use dashboard_core::nav::Navigator;
use dashboard_core::usage_analytics::{ladder, live};

use crate::service::GithubState;
use crate::usage::{UsageLedger, budget_now};

mod critical;
mod model;

use critical::CriticalWatch;
use model::{ChipModel, Segment};

/// What a render shows; the refresh loop notifies only when it changes,
/// so a stable budget draws nothing (0050 D5).
fn content(model: &ChipModel, session: (u64, u64, u64, u64)) -> (Vec<Segment>, String) {
    let segments = model
        .budgets()
        .into_iter()
        .map(|(lane, chip)| model::segment(lane, chip))
        .collect();
    (segments, model::label(model, session))
}

/// Self-refreshing status chip. Created once in `lib.rs::init`; the
/// status-item closure shows it on every page (0082 Q9).
pub(crate) struct UsageChip {
    state: Entity<GithubState>,
    usage: UsageLedger,
    /// The chip is the button that opens the Budget view (D13).
    nav: Entity<Navigator>,
}

impl UsageChip {
    pub fn new(
        state: Entity<GithubState>,
        usage: UsageLedger,
        nav: Entity<Navigator>,
        cx: &mut Context<Self>,
    ) -> Self {
        // 1 Hz self-refresh (0050 D5): one short mutex read per second.
        // The same beat watches for the D5 Critical crossing, so the
        // toast fires wherever the app is. The chip lives as long as the
        // app (the status-item closure holds it), and so does this loop.
        let timer_state = state.clone();
        let timer_usage = usage.clone();
        cx.spawn(async move |this, cx| {
            let mut last: Option<(Vec<Segment>, String)> = None;
            let mut watch = CriticalWatch::default();
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let shown = cx.update(|cx| {
                    let (model, session) = current(&timer_state, &timer_usage, cx);
                    for alert in watch.observe(&model) {
                        dashboard_core::toasts::raise(cx, alert.toast());
                    }
                    content(&model, session)
                });
                if last.as_ref() != Some(&shown) {
                    last = Some(shown);
                    let _ = this.update(cx, |_, cx| cx.notify());
                }
            }
        })
        .detach();
        Self { state, usage, nav }
    }
}

/// The model and session counters as they stand.
fn current(
    state: &Entity<GithubState>,
    usage: &UsageLedger,
    cx: &gpui::App,
) -> (ChipModel, (u64, u64, u64, u64)) {
    let pause = state.read(cx).rate_limited_until;
    let book = usage.lock().unwrap();
    let now = budget_now();
    (
        ChipModel::of(&book.display(now), &pause, now),
        book.session_counters(),
    )
}

impl Render for UsageChip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (model, session) = current(&self.state, &self.usage, cx);
        let (segments, label) = content(&model, session);
        let theme = cx.theme();
        let (muted, foreground) = (theme.muted_foreground, theme.foreground);
        let mut row = h_flex().gap_1().items_center();
        for (i, seg) in segments.into_iter().enumerate() {
            if i > 0 {
                row = row.child(div().text_color(muted).child("·"));
            }
            // Q1: the D5 token colours the icon only; a marked budget's
            // words and number are `foreground`, semibold.
            row = row.child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .when_some(seg.icon, |this, state| {
                        this.child(
                            Icon::new(ladder::icon(state))
                                .xsmall()
                                .text_color(ladder::bar_color(Some(state), cx)),
                        )
                    })
                    .child(
                        div()
                            .when(seg.marked, |d| {
                                d.text_color(foreground).font_weight(FontWeight::SEMIBOLD)
                            })
                            .when(!seg.marked, |d| d.text_color(muted))
                            .child(SharedString::from(seg.text)),
                    ),
            );
        }
        let nav = self.nav.clone();
        Button::new("github-usage")
            .ghost()
            .xsmall()
            .accessibility_label(label)
            .tooltip(model::tooltip(&model))
            .child(row)
            .on_click(move |_, _, cx| live::open_budget_view(&nav, cx))
    }
}
