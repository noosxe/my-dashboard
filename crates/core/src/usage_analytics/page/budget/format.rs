//! The Budget view's wording (docs/0082 D4, D21): counts, percentages,
//! countdowns and range phrases. Pure, so every band's copy is tested
//! without a window.

use crate::usage_analytics::page::honesty;
use crate::usage_analytics::surfaces::{Budget, Preset, Span};

const MINUTE_MS: u64 = 60_000;
const HOUR_MS: u64 = 60 * MINUTE_MS;
const DAY_MS: u64 = 24 * HOUR_MS;

/// Thousands-grouped: `10,625`.
pub(in crate::usage_analytics::page) fn count(n: u64) -> String {
    honesty::fmt_count(n)
}

/// An amount in the budget's unit: `1 request`, `427 requests`,
/// `5 points` (D21).
pub(in crate::usage_analytics::page) fn amount(n: u64, budget: Budget) -> String {
    format!("{} {}", count(n), unit(n, budget))
}

/// The budget's unit, singular for one.
pub(in crate::usage_analytics::page) fn unit(n: u64, budget: Budget) -> &'static str {
    match (budget, n == 1) {
        (Budget::Rest, true) => "request",
        (Budget::Rest, false) => "requests",
        (Budget::Graphql, true) => "point",
        (Budget::Graphql, false) => "points",
    }
}

/// `1 request` / `1,318 requests`: request counts, whatever the budget.
pub(in crate::usage_analytics::page) fn requests(n: u64) -> String {
    format!("{} request{}", count(n), if n == 1 { "" } else { "s" })
}

/// The card title (D21): "REST requests", "GraphQL points".
pub(in crate::usage_analytics::page) fn budget_title(budget: Budget) -> &'static str {
    match budget {
        Budget::Rest => "REST requests",
        Budget::Graphql => "GraphQL points",
    }
}

/// What is left, as a whole percentage rounded **down**, so the card
/// never shows more budget than GitHub reported (4,990 of 5,000 reads
/// 99 %, not 100 %).
pub(in crate::usage_analytics::page) fn remaining_pct(pct: f64) -> String {
    format!("{}%", pct.clamp(0.0, 100.0).floor() as u64)
}

/// A share, rounded, that never reads 0 % or 100 % for a part that is
/// neither: `<1%`, `>99%`.
pub(in crate::usage_analytics::page) fn share(pct: Option<f64>) -> String {
    let Some(p) = pct else {
        return "—".into();
    };
    let r = p.round();
    if p > 0.0 && r < 1.0 {
        "<1%".into()
    } else if p < 100.0 && r >= 100.0 {
        ">99%".into()
    } else {
        format!("{}%", r as u64)
    }
}

/// An average cost with one decimal; `—` when nothing was costed.
pub(in crate::usage_analytics::page) fn avg(v: Option<f64>) -> String {
    v.map_or_else(|| "—".into(), |a| format!("{a:.1}"))
}

/// A duration in whole minutes, hours or days for the copy below.
fn span_words(ms: u64) -> String {
    if ms < HOUR_MS {
        format!("{} min", ms / MINUTE_MS)
    } else if ms < DAY_MS {
        let h = ms / HOUR_MS;
        let m = (ms % HOUR_MS) / MINUTE_MS;
        if m == 0 {
            format!("{h} h")
        } else {
            format!("{h} h {m} min")
        }
    } else {
        let d = ms / DAY_MS;
        format!("{d} day{}", if d == 1 { "" } else { "s" })
    }
}

/// The reset's countdown from `as_of` (D4): `in 13 min`, `in under a
/// minute`; a reset already behind `as_of` reads `13 min ago`.
pub(in crate::usage_analytics::page) fn countdown(reset_ms: u64, as_of_ms: u64) -> String {
    if reset_ms > as_of_ms {
        let d = reset_ms - as_of_ms;
        if d < MINUTE_MS {
            "in under a minute".into()
        } else {
            format!("in {}", span_words(d))
        }
    } else {
        let d = as_of_ms - reset_ms;
        if d < MINUTE_MS {
            "under a minute ago".into()
        } else {
            format!("{} ago", span_words(d))
        }
    }
}

/// The snapshot's age (D4, D21): "snapshot under a minute old", never
/// "0 min old".
pub(in crate::usage_analytics::page) fn snapshot_age(age_ms: u64) -> String {
    if age_ms < MINUTE_MS {
        "snapshot under a minute old".into()
    } else {
        format!("snapshot {} old", span_words(age_ms))
    }
}

/// The range a figure covers, as the summary lines say it.
pub(in crate::usage_analytics::page) fn range_phrase(preset: Preset) -> &'static str {
    match preset {
        Preset::CurrentWindow => "in this window",
        Preset::Trailing(Span::H1) => "in the last hour",
        Preset::Trailing(Span::H24) => "in the last 24 hours",
        Preset::Trailing(Span::D7) => "in the last 7 days",
        Preset::Trailing(Span::D30) => "in the last 30 days",
        Preset::Trailing(Span::D90) => "in the last 90 days",
    }
}

/// A window's span on the time axis: `Nov 5, 10:41–11:41`, with the
/// date when the range is longer than a day (D10a).
pub(in crate::usage_analytics::page) fn window_span(
    start_ms: u64,
    end_ms: u64,
    dated: bool,
) -> String {
    let start = if dated {
        honesty::format_date_time(start_ms)
    } else {
        honesty::format_clock(start_ms)
    };
    format!("{start}–{}", honesty::format_clock(end_ms))
}

/// A local day: `Nov 5`.
pub(in crate::usage_analytics::page) fn day(epoch_ms: u64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(epoch_ms as i64)
        .single()
        .map(|t| t.format("%b %-d").to_string())
        .unwrap_or_else(|| "—".into())
}

pub(in crate::usage_analytics::page) use honesty::format_clock as clock;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_name_the_budget_unit_and_agree_in_number() {
        assert_eq!(amount(1, Budget::Rest), "1 request");
        assert_eq!(amount(427, Budget::Rest), "427 requests");
        assert_eq!(amount(1, Budget::Graphql), "1 point");
        assert_eq!(amount(10_625, Budget::Graphql), "10,625 points");
        assert_eq!(requests(1), "1 request");
        assert_eq!(requests(1_318), "1,318 requests");
    }

    #[test]
    fn remaining_rounds_down_and_shares_never_read_zero_or_whole() {
        assert_eq!(remaining_pct(99.8), "99%");
        assert_eq!(remaining_pct(0.0), "0%");
        assert_eq!(remaining_pct(100.0), "100%");
        assert_eq!(share(Some(62.99)), "63%");
        assert_eq!(share(Some(0.3)), "<1%");
        assert_eq!(share(Some(0.0)), "0%");
        assert_eq!(share(Some(99.7)), ">99%");
        assert_eq!(share(Some(100.0)), "100%");
        assert_eq!(share(None), "—");
        assert_eq!(avg(Some(12.25)), "12.2");
        assert_eq!(avg(None), "—");
    }

    #[test]
    fn countdowns_and_ages_speak_in_minutes_never_zero() {
        let as_of = 10_000_000_000;
        assert_eq!(countdown(as_of + 13 * MINUTE_MS, as_of), "in 13 min");
        assert_eq!(countdown(as_of + 30_000, as_of), "in under a minute");
        assert_eq!(countdown(as_of - 5 * MINUTE_MS, as_of), "5 min ago");
        assert_eq!(snapshot_age(59_999), "snapshot under a minute old");
        assert_eq!(snapshot_age(2 * MINUTE_MS), "snapshot 2 min old");
        assert_eq!(
            snapshot_age(HOUR_MS + 5 * MINUTE_MS),
            "snapshot 1 h 5 min old"
        );
        assert_eq!(snapshot_age(3 * HOUR_MS), "snapshot 3 h old");
        assert_eq!(snapshot_age(2 * DAY_MS), "snapshot 2 days old");
    }

    #[test]
    fn range_phrases_spell_the_span_out() {
        assert_eq!(range_phrase(Preset::CurrentWindow), "in this window");
        assert_eq!(range_phrase(Preset::Trailing(Span::H1)), "in the last hour");
        assert_eq!(
            range_phrase(Preset::Trailing(Span::D90)),
            "in the last 90 days"
        );
        assert_eq!(budget_title(Budget::Graphql), "GraphQL points");
    }
}
