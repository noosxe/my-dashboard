//! The usage page's shared wording (docs/0082 D21): counts, units,
//! shares, durations, sizes and range phrases, for both views. Pure, so
//! every band's copy is tested without a window. View-only copy lives
//! with its view (`budget/format.rs`, `requests/format.rs`).

use crate::usage_analytics::page::honesty;
use crate::usage_analytics::surfaces::{Budget, Preset, Span};

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

/// A cost against the requests that carried it (D21). GraphQL's two
/// numbers have different units: "4,227 points over 719 requests".
/// REST's are both requests, so the words say which is which: "159
/// requests counted against the limit, of 347 sent" (the sent total
/// includes the 304s that cost nothing). `named` puts the budget's
/// name before the unit ("159 REST requests …").
pub(in crate::usage_analytics::page) fn cost_of_sent(
    cost: u64,
    sent: u64,
    budget: Budget,
    named: bool,
) -> String {
    let name = match (named, budget) {
        (false, _) => "",
        (true, Budget::Rest) => "REST ",
        (true, Budget::Graphql) => "GraphQL ",
    };
    let spent = format!("{} {name}{}", count(cost), unit(cost, budget));
    match budget {
        Budget::Rest => format!("{spent} counted against the limit, of {} sent", count(sent)),
        Budget::Graphql => format!("{spent} over {}", requests(sent)),
    }
}

/// `1 request` / `1,318 requests`: request counts, whatever the budget.
pub(in crate::usage_analytics::page) fn requests(n: u64) -> String {
    format!("{} request{}", count(n), if n == 1 { "" } else { "s" })
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

/// An instant: `14:02`, or `Nov 5, 14:02` when the range is longer
/// than a day (D10a).
pub(in crate::usage_analytics::page) fn instant(ms: u64, dated: bool) -> String {
    if dated {
        honesty::format_date_time(ms)
    } else {
        honesty::format_clock(ms)
    }
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

/// A duration (D10): `88 ms`, `1.6 s`, `12 s`. Sub-10 s seconds keep
/// one decimal, so axis ticks read `1.0 s`, `1.5 s`.
pub(in crate::usage_analytics::page) fn duration(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms} ms")
    } else if ms < 10_000 {
        format!("{:.1} s", ms as f64 / 1_000.0)
    } else {
        format!("{} s", count((ms as f64 / 1_000.0).round() as u64))
    }
}

/// A body size, binary like the payload bands' edges: `966 B`, `6 KB`,
/// `1.2 MB`.
pub(in crate::usage_analytics::page) fn bytes(n: u64) -> String {
    const KB: u64 = 1_024;
    const MB: u64 = KB * KB;
    if n < KB {
        format!("{n} B")
    } else if n < MB {
        format!("{} KB", (n as f64 / KB as f64).round().max(1.0) as u64)
    } else {
        format!("{:.1} MB", n as f64 / MB as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cost_of_sent_never_reads_requests_over_requests() {
        assert_eq!(
            cost_of_sent(159, 347, Budget::Rest, true),
            "159 REST requests counted against the limit, of 347 sent"
        );
        assert_eq!(
            cost_of_sent(1, 1, Budget::Rest, false),
            "1 request counted against the limit, of 1 sent"
        );
        assert_eq!(
            cost_of_sent(4_227, 719, Budget::Graphql, true),
            "4,227 GraphQL points over 719 requests"
        );
        assert_eq!(
            cost_of_sent(1, 1, Budget::Graphql, false),
            "1 point over 1 request"
        );
    }

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
    fn shares_never_read_zero_or_whole() {
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
    fn durations_and_sizes_read_at_a_glance() {
        assert_eq!(duration(88), "88 ms");
        assert_eq!(duration(999), "999 ms");
        assert_eq!(duration(1_000), "1.0 s");
        assert_eq!(duration(1_640), "1.6 s");
        assert_eq!(duration(12_400), "12 s");
        assert_eq!(bytes(966), "966 B");
        assert_eq!(bytes(1_023), "1023 B");
        assert_eq!(bytes(1_024), "1 KB");
        assert_eq!(bytes(6_200), "6 KB");
        assert_eq!(bytes(1_300_000), "1.2 MB");
    }

    #[test]
    fn range_phrases_spell_the_span_out() {
        assert_eq!(range_phrase(Preset::CurrentWindow), "in this window");
        assert_eq!(range_phrase(Preset::Trailing(Span::H1)), "in the last hour");
        assert_eq!(
            range_phrase(Preset::Trailing(Span::D90)),
            "in the last 90 days"
        );
    }
}
