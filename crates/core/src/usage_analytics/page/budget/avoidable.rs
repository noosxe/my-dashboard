//! Band 4 (docs/0082 D9): avoidable spend, the top five operations by
//! cost a revalidation, a timer or a failure paid for. Heuristics, not
//! proof, and this app's recorded spend (§4 principle): the summary says
//! both.

use gpui::prelude::*;
use gpui::{AnyElement, App, FontWeight, SharedString, div};
use gpui_component::{ActiveTheme, Sizable as _, h_flex, tag::Tag, v_flex};

use super::{band_frame, format};
use crate::usage_analytics::surfaces::{Avoidable, AvoidableCategory, AvoidableEntry, Preset};

/// The category tag (D9).
pub(super) fn category(c: AvoidableCategory) -> &'static str {
    match c {
        AvoidableCategory::Errors => "Errors",
        AvoidableCategory::RevalidationGap => "Revalidation gap",
        AvoidableCategory::TimerAndRefocus => "Timer and refocus",
    }
}

/// The entry's one-line reason.
pub(super) fn reason(e: &AvoidableEntry) -> String {
    match e.category {
        AvoidableCategory::Errors => "paid on requests that failed".into(),
        AvoidableCategory::RevalidationGap => {
            let checks = e.op_304s + e.op_200s;
            let pct = format::share((checks > 0).then(|| e.op_304s as f64 / checks as f64 * 100.0));
            format!("paid on 200s; this op came back 304 in {pct} of its checks")
        }
        AvoidableCategory::TimerAndRefocus => {
            "spent by a poll, refocus or probe, not by a page you opened".into()
        }
    }
}

/// The band's summary line (D9): heuristics, recorded spend, the share's
/// denominator.
pub(super) fn summary(a: &Avoidable, preset: Preset) -> String {
    let mut s = format!(
        "Heuristics, not proof: cost that a revalidation, a timer or a failure paid for, from this app's recorded spend. Share is of {} {} {} {}.",
        format::count(a.total_cost),
        super::budget_name(a.budget),
        format::unit(a.total_cost, a.budget),
        format::range_phrase(preset)
    );
    if a.total_entries > a.entries.len() {
        s.push_str(&format!(" Top {} of {}.", a.entries.len(), a.total_entries));
    }
    s
}

pub(super) fn empty_copy(a: &Avoidable, preset: Preset) -> String {
    format!(
        "No avoidable {} spend recorded {}.",
        super::budget_name(a.budget),
        format::range_phrase(preset)
    )
}

/// One row's accessible name: every visible piece, in order.
fn row_label(e: &AvoidableEntry, a: &Avoidable) -> String {
    format!(
        "{}, {}: {}. {}, {} of the range's spend",
        e.op,
        category(e.category),
        reason(e),
        format::amount(e.cost, a.budget),
        format::share(e.share_pct)
    )
}

fn row(ix: usize, e: &AvoidableEntry, a: &Avoidable, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let id = SharedString::from(format!("usage-avoidable-{ix}"));
    h_flex()
        .id(id.clone())
        .debug_selector(move || id.to_string())
        .role(gpui::accesskit::Role::ListItem)
        .aria_label(row_label(e, a))
        .w_full()
        .items_center()
        .gap_3()
        .px_1()
        .py_2()
        .border_b_1()
        .border_color(theme.border)
        .text_sm()
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .flex_none()
                .child(e.op.clone()),
        )
        .child(
            Tag::secondary()
                .small()
                .flex_none()
                .child(category(e.category)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.muted_foreground)
                .child(reason(e)),
        )
        .child(
            h_flex()
                .flex_none()
                .gap_1()
                .items_baseline()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format::count(e.cost)),
                )
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(format::unit(e.cost, a.budget)),
                ),
        )
        .child(
            div()
                .flex_none()
                .w_12()
                .text_right()
                .text_color(theme.muted_foreground)
                .child(format::share(e.share_pct)),
        )
}

/// Band 4: summary and the short list, or the band's Empty.
pub(super) fn band(a: &Avoidable, preset: Preset, cx: &App) -> AnyElement {
    let body = if a.entries.is_empty() {
        band_frame::empty("usage-avoidable-empty", empty_copy(a, preset), cx).into_any_element()
    } else {
        v_flex()
            .gap_2()
            .child(band_frame::summary(summary(a, preset), cx))
            .child(
                v_flex()
                    .id("usage-avoidable-list")
                    .role(gpui::accesskit::Role::List)
                    .aria_label("Avoidable spend")
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .children(a.entries.iter().enumerate().map(|(i, e)| row(i, e, a, cx))),
            )
            .into_any_element()
    };
    band_frame::section(
        "usage-avoidable",
        "Avoidable spend",
        None::<gpui::Div>,
        body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_analytics::surfaces::{Budget, Span};

    fn entry(category: AvoidableCategory) -> AvoidableEntry {
        AvoidableEntry {
            op: "notifications".into(),
            category,
            cost: 427,
            share_pct: Some(39.4),
            requests: 427,
            op_304s: 70,
            op_200s: 30,
        }
    }

    #[test]
    fn reasons_explain_each_category() {
        assert_eq!(
            reason(&entry(AvoidableCategory::RevalidationGap)),
            "paid on 200s; this op came back 304 in 70% of its checks"
        );
        assert_eq!(
            reason(&entry(AvoidableCategory::Errors)),
            "paid on requests that failed"
        );
        assert_eq!(
            category(AvoidableCategory::TimerAndRefocus),
            "Timer and refocus"
        );
    }

    #[test]
    fn the_summary_says_heuristics_and_recorded_spend() {
        let a = Avoidable {
            budget: Budget::Rest,
            unit: "requests".into(),
            bounds: None,
            total_cost: 1_085,
            entries: vec![entry(AvoidableCategory::Errors)],
            total_entries: 12,
        };
        let s = summary(&a, Preset::Trailing(Span::D7));
        assert!(s.starts_with("Heuristics, not proof: "));
        assert!(s.contains("this app's recorded spend"));
        assert!(s.contains("Share is of 1,085 REST requests in the last 7 days."));
        assert!(s.ends_with(" Top 1 of 12."));
        assert_eq!(
            row_label(&a.entries[0], &a),
            "notifications, Errors: paid on requests that failed. 427 requests, 39% of the range's spend"
        );
        assert_eq!(
            empty_copy(&a, Preset::CurrentWindow),
            "No avoidable REST spend recorded in this window."
        );
    }
}
