//! The Budget view (docs/0082 §5, D4–D9): the owner as a user, from
//! GitHub's reporting (§4 principle). Four bands in one scroll:
//! [`cards`] (D4), then, below the Budget selector, [`spend`] (D7),
//! [`history`] (D8) and [`avoidable`] (D9). Every band reads the data
//! the page derived off the UI thread (D11); a band whose inputs changed
//! shows its skeleton until its derivation lands (D18). Rendering only
//! lays out what was derived.

use gpui::prelude::*;
use gpui::{AnyElement, Context, Window, div};
use gpui_component::{ActiveTheme, h_flex, v_flex};

use super::derive::{Band, BudgetData};
use super::header::segmented_stop;
use super::range::budget_label;
use super::{UsagePage, UsageView, band_frame, derive, states};
use crate::usage_analytics::surfaces::Budget;

mod avoidable;
mod cards;
mod format;
mod history;
mod spend;
mod spend_table;

pub(super) use spend::Controls;

/// The skeletons' heights per band (docs/0002 D10: canvas sizes).
const CARDS_SKELETON_H: f32 = 180.0;
const WHERE_SKELETON_H: f32 = 320.0;
const HISTORY_SKELETON_H: f32 = 240.0;
const AVOIDABLE_SKELETON_H: f32 = 200.0;

/// The budget's name in running text (D21): "REST", "GraphQL".
fn budget_name(b: Budget) -> &'static str {
    budget_label(b)
}

/// The budget in element ids: `rest`, `graphql`.
fn budget_slug(b: Budget) -> &'static str {
    match b {
        Budget::Rest => "rest",
        Budget::Graphql => "graphql",
    }
}

impl UsagePage {
    /// The Budget view's body (D4–D9, D18).
    pub(super) fn render_budget(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let data: Option<BudgetData> = self.views.as_ref().and_then(|v| v.budget.clone());
        let preset = self.preset;
        let ready = |band: Band| self.band_ready(band);

        let cards = match &data {
            Some(d) if ready(Band::Cards) => {
                cards::band(&d.cards, d.key.as_of_ms, preset, cx).into_any_element()
            }
            _ => band_frame::skeleton("usage-cards-loading", CARDS_SKELETON_H),
        };
        let body = v_flex()
            .id("usage-budget-body")
            .w_full()
            .gap_6()
            .child(cards);
        // D18: with no records at all, the no-snapshot cards stay and
        // one page-level Empty replaces bands 2–4 and the selector.
        if self.rows.is_empty() {
            return body
                .child(states::page_empty(UsageView::Budget, cx))
                .into_any_element();
        }

        let selector = self.budget_selector(window, cx);
        let tz = derive::local_offset_ms(self.as_of_ms);
        let where_ = match (&data, &self.budget_controls) {
            (Some(d), Some(c)) if ready(Band::Where) => {
                spend::sync_table(c, &d.spend, self.spend, cx);
                spend::body(&d.spend, c, preset, self.spend, cx)
            }
            _ => band_frame::skeleton("usage-where-loading", WHERE_SKELETON_H),
        };
        let where_band = band_frame::section(
            "usage-where",
            "Where it went",
            self.budget_controls
                .as_ref()
                .map(|c| spend::query_bar(c, cx)),
            where_,
        );
        let history = match &data {
            Some(d) if ready(Band::History) => history::band(&d.history, preset, tz, cx),
            _ => band_frame::section(
                "usage-history",
                history::title(preset),
                None::<gpui::Div>,
                band_frame::skeleton("usage-history-loading", HISTORY_SKELETON_H),
            ),
        };
        let avoidable = match &data {
            Some(d) if ready(Band::Avoidable) => avoidable::band(&d.avoidable, preset, cx),
            _ => band_frame::section(
                "usage-avoidable",
                "Avoidable spend",
                None::<gpui::Div>,
                band_frame::skeleton("usage-avoidable-loading", AVOIDABLE_SKELETON_H),
            ),
        };
        body.child(selector)
            .child(where_band)
            .child(history)
            .child(avoidable)
            .into_any_element()
    }

    /// The view-level Budget selector (D7): between band 1 and band 2,
    /// because bands 2–4 all follow it. One Tab stop with arrow keys
    /// (D19).
    fn budget_selector(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let selected = Budget::all()
            .iter()
            .position(|b| *b == self.budget)
            .unwrap_or(0);
        let stop = segmented_stop(
            "usage-budget",
            "Budget",
            self.focus.as_ref().map(|f| &f.budget),
            Budget::all().iter().map(|b| budget_label(*b)).collect(),
            selected,
            |this, ix, cx| {
                if let Some(b) = Budget::all().get(ix) {
                    this.set_budget(*b, cx);
                }
            },
            window,
            cx,
        );
        h_flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Budget"),
            )
            .child(stop)
    }
}
