//! The Budget view's own wording (docs/0082 D4, D21): card titles, the
//! remaining percentage, countdowns and snapshot ages. The wording both
//! views share is [`crate::usage_analytics::page::format`], re-exported
//! here so the bands name one module.

use crate::usage_analytics::surfaces::Budget;

pub(in crate::usage_analytics::page) use crate::usage_analytics::page::format::*;

const MINUTE_MS: u64 = 60_000;
const HOUR_MS: u64 = 60 * MINUTE_MS;
const DAY_MS: u64 = 24 * HOUR_MS;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_remaining_share_rounds_down() {
        assert_eq!(remaining_pct(99.8), "99%");
        assert_eq!(remaining_pct(0.0), "0%");
        assert_eq!(remaining_pct(100.0), "100%");
        assert_eq!(budget_title(Budget::Graphql), "GraphQL points");
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
}
