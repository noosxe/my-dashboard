//! Band 1 (docs/0082 D4): twin budget cards, side by side down to about
//! 640 px of content (Q2). GitHub's reporting, verbatim (0050 D1, §4
//! principle); the meta line's recorded spend is the one figure from
//! this app's records, and it says so ("recorded").

use gpui::prelude::*;
use gpui::{App, FontWeight, SharedString, div, px};
use gpui_component::{ActiveTheme, Icon, Sizable as _, h_flex, progress::Progress, v_flex};

use super::format;
use crate::ui::{CardVariant, card, section_header};
use crate::usage_analytics::ladder;
use crate::usage_analytics::surfaces::{
    BudgetCard, BudgetCards, Preset, Projection, ThresholdState,
};

/// A card's minimum width: two fit side by side from about 640 px of
/// content (Q2, D20); narrower, the row wraps into a stack.
const CARD_MIN_W: f32 = 300.0;

/// What one card says. Pure: the render only lays it out.
#[derive(Debug, PartialEq)]
pub(super) struct CardCopy {
    pub title: &'static str,
    /// The D5 badge, right of the title; `None` for Normal.
    pub badge: Option<ThresholdState>,
    /// `("3,712", "left of 5,000")`; `None` without a snapshot.
    pub headline: Option<(String, String)>,
    /// The `Progress` value (0–100) and its percentage label.
    pub bar: Option<(f32, String)>,
    /// `("Resets 14:00", "(in 13 min)")`.
    pub reset: Option<(String, String)>,
    /// The projection (D6) or the line that keeps the card's shape.
    pub status: String,
    /// "as of 13:47 · snapshot 2 min old".
    pub meta: String,
    /// "153 requests recorded in the last 24 hours": the meta line's
    /// second row (D4's sketch), this app's own figure.
    pub recorded: String,
}

impl CardCopy {
    pub(super) fn of(card: &BudgetCard, as_of_ms: u64, preset: Preset) -> Self {
        let budget = card.budget;
        let exhausted = matches!(card.projection, Projection::Exhausted { .. });
        let recorded = format!(
            "{} recorded {}",
            format::amount(card.recorded.known_cost, budget),
            format::range_phrase(preset)
        );
        let as_of = format!("as of {}", format::clock(as_of_ms));
        let Some(snap) = &card.snapshot else {
            return CardCopy {
                title: format::budget_title(budget),
                badge: None,
                headline: None,
                bar: None,
                reset: None,
                status: "No response from GitHub in this range yet".into(),
                meta: as_of,
                recorded,
            };
        };
        let headline = (
            format::count(u64::from(snap.remaining)),
            snap.limit.map_or_else(
                || "left".into(),
                |l| format!("left of {}", format::count(u64::from(l))),
            ),
        );
        // Exhausted shows an empty bar (D4), whatever the last number.
        let bar = snap.remaining_pct.map(|pct| {
            let pct = if exhausted { 0.0 } else { pct };
            (pct as f32, format::remaining_pct(pct))
        });
        let reset = snap.reset_ms.map(|r| {
            let verb = if snap.window_open { "Resets" } else { "Reset" };
            (
                format!("{verb} {}", format::clock(r)),
                format!("({})", format::countdown(r, as_of_ms)),
            )
        });
        let status = match card.projection {
            Projection::NoSnapshot => "No response from GitHub in this range yet".into(),
            Projection::WindowClosed => {
                "This window has reset; GitHub has refilled the budget since".into()
            }
            Projection::Unavailable => "GitHub reported no reset or limit to project from".into(),
            Projection::Exhausted { .. } => format!(
                "GitHub paused {} requests; they resume at the reset",
                super::budget_name(budget)
            ),
            Projection::NotEnoughData { .. } => {
                "Not enough responses in this window to project".into()
            }
            Projection::OnTrack { .. } => "On track".into(),
            Projection::RunsOut { at_ms, .. } => {
                format!("≈ runs out ~{} at this pace", format::clock(at_ms))
            }
        };
        CardCopy {
            title: format::budget_title(budget),
            badge: card.state.filter(|s| ladder::word(*s).is_some()),
            headline: Some(headline),
            bar,
            reset,
            status,
            meta: format!("{as_of} · {}", format::snapshot_age(snap.snapshot_age_ms)),
            recorded,
        }
    }

    /// The card's accessible name: every visible line, in order.
    pub(super) fn label(&self) -> String {
        let mut bits = vec![self.title.to_string()];
        if let Some(state) = self.badge.and_then(ladder::word) {
            bits.push(state.to_string());
        }
        if let Some((n, of)) = &self.headline {
            bits.push(format!("{n} {of}"));
        }
        if let Some((_, pct)) = &self.bar {
            bits.push(format!("{pct} left"));
        }
        if let Some((at, when)) = &self.reset {
            bits.push(format!("{at} {when}"));
        }
        bits.push(self.status.clone());
        bits.push(self.meta.clone());
        bits.push(self.recorded.clone());
        bits.join(". ")
    }
}

/// Band 1: both cards, REST first.
pub(super) fn band(
    cards: &BudgetCards,
    as_of_ms: u64,
    preset: Preset,
    cx: &App,
) -> impl IntoElement {
    h_flex()
        .id("usage-cards")
        .w_full()
        .flex_wrap()
        .items_stretch()
        .gap_3()
        .children(
            cards
                .cards
                .iter()
                .map(|c| budget_card(c, CardCopy::of(c, as_of_ms, preset), cx)),
        )
}

fn budget_card(c: &BudgetCard, copy: CardCopy, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let id = SharedString::from(format!("usage-card-{}", super::budget_slug(c.budget)));
    let label = copy.label();
    let badge = copy.badge.and_then(|s| ladder::word(s).map(|w| (s, w)));
    card(cx, CardVariant::Quiet)
        .id(id.clone())
        .debug_selector(move || id.to_string())
        .role(gpui::accesskit::Role::Group)
        .aria_label(label)
        .flex_1()
        .flex_basis(px(CARD_MIN_W))
        .min_w(px(CARD_MIN_W))
        .p_4()
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .child(section_header(copy.title))
                .when_some(badge, |this, (state, word)| {
                    this.child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(
                                Icon::new(ladder::icon(state))
                                    .small()
                                    .text_color(ladder::bar_color(Some(state), cx)),
                            )
                            .child(word),
                    )
                }),
        )
        .when_some(copy.headline, |this, (n, of)| {
            this.child(
                h_flex()
                    .items_baseline()
                    .gap_2()
                    .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child(n))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(of)),
            )
        })
        .when_some(copy.bar, |this, (value, pct)| {
            this.child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        Progress::new(format!("usage-card-bar-{}", super::budget_slug(c.budget)))
                            .value(value)
                            .color(ladder::bar_color(c.state, cx))
                            .accessibility_label(format!("{pct} left"))
                            .flex_1(),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(pct),
                    ),
            )
        })
        .child(
            v_flex()
                .gap_0p5()
                .text_sm()
                .when_some(copy.reset, |this, (at, when)| {
                    this.child(
                        h_flex()
                            .gap_1()
                            .child(at)
                            .child(div().text_color(theme.muted_foreground).child(when)),
                    )
                })
                .child(div().child(copy.status)),
        )
        .child(
            div()
                .mt_auto()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(div().child(copy.meta))
                .child(div().child(copy.recorded)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_analytics::surfaces::{Budget, BudgetSnapshot, Span, SpendTotals};

    const AS_OF: u64 = 10_000_000_000;
    const MIN: u64 = 60_000;

    fn card(projection: Projection, state: Option<ThresholdState>) -> BudgetCard {
        BudgetCard {
            budget: Budget::Graphql,
            snapshot: Some(BudgetSnapshot {
                remaining: 1_140,
                limit: Some(5_000),
                remaining_pct: Some(22.8),
                reset_ms: Some(AS_OF + 5 * MIN),
                snapshot_ts_ms: AS_OF - MIN,
                snapshot_age_ms: MIN,
                window_open: true,
            }),
            state,
            projection,
            window_drop: None,
            recorded: SpendTotals {
                requests: 40,
                known_cost: 10_625,
                unknown: 0,
            },
            bounds: None,
        }
    }

    const H24: Preset = Preset::Trailing(Span::H24);

    #[test]
    fn a_card_states_github_numbers_verbatim_with_the_projection() {
        let c = CardCopy::of(
            &card(
                Projection::RunsOut {
                    at_ms: AS_OF + 3 * MIN,
                    pace_per_min: 400.0,
                },
                Some(ThresholdState::Low),
            ),
            AS_OF,
            H24,
        );
        assert_eq!(c.title, "GraphQL points");
        assert_eq!(c.badge, Some(ThresholdState::Low));
        assert_eq!(c.headline, Some(("1,140".into(), "left of 5,000".into())));
        assert_eq!(c.bar, Some((22.8, "22%".into())));
        assert_eq!(c.reset.as_ref().map(|r| r.1.as_str()), Some("(in 5 min)"));
        assert!(c.status.starts_with("≈ runs out ~"), "{}", c.status);
        assert!(c.meta.ends_with(" · snapshot 1 min old"), "{}", c.meta);
        assert_eq!(c.recorded, "10,625 points recorded in the last 24 hours");
    }

    #[test]
    fn normal_shows_no_badge_and_the_guard_keeps_the_shape() {
        let c = CardCopy::of(
            &card(
                Projection::NotEnoughData {
                    responses: 2,
                    elapsed_ms: MIN,
                },
                Some(ThresholdState::Normal),
            ),
            AS_OF,
            H24,
        );
        assert_eq!(c.badge, None);
        assert_eq!(c.status, "Not enough responses in this window to project");
    }

    #[test]
    fn exhausted_says_github_paused_and_empties_the_bar() {
        let c = CardCopy::of(
            &card(
                Projection::Exhausted {
                    resumes_at_ms: AS_OF + 5 * MIN,
                },
                Some(ThresholdState::Exhausted),
            ),
            AS_OF,
            H24,
        );
        assert_eq!(
            c.status,
            "GitHub paused GraphQL requests; they resume at the reset"
        );
        assert_eq!(c.bar, Some((0.0, "0%".into())));
        assert!(
            c.label()
                .contains("GraphQL points. Exhausted. 1,140 left of 5,000")
        );
    }

    #[test]
    fn no_snapshot_never_shows_zero() {
        let mut empty = card(Projection::NoSnapshot, None);
        empty.snapshot = None;
        let c = CardCopy::of(&empty, AS_OF, Preset::CurrentWindow);
        assert_eq!(c.headline, None);
        assert_eq!(c.bar, None);
        assert_eq!(c.status, "No response from GitHub in this range yet");
        assert_eq!(c.recorded, "10,625 points recorded in this window");
    }

    #[test]
    fn a_closed_window_keeps_its_numbers_and_says_it_reset() {
        let mut closed = card(Projection::WindowClosed, None);
        if let Some(s) = closed.snapshot.as_mut() {
            s.window_open = false;
            s.reset_ms = Some(AS_OF - 5 * MIN);
        }
        let c = CardCopy::of(&closed, AS_OF, H24);
        assert_eq!(c.reset.as_ref().map(|r| r.1.as_str()), Some("(5 min ago)"));
        assert!(c.reset.unwrap().0.starts_with("Reset "));
        assert!(c.status.starts_with("This window has reset"));
    }
}
