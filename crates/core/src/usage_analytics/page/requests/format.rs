//! The Requests view's own wording (docs/0082 D10, D21): facet and
//! measure labels, the log's status reasons, cost cells and times, and
//! the trend's bucket words. The wording both views share is
//! [`crate::usage_analytics::page::format`], re-exported here.

use crate::usage_analytics::surfaces::{CostCell, Measure, Span, StatusClass};
use crate::usage_records::Transport;

pub(in crate::usage_analytics::page) use crate::usage_analytics::page::format::*;

const DAY_MS: i64 = 24 * 60 * 60_000;

/// A transport as the facet and the tiles name it (D21).
pub(super) fn transport(t: Transport) -> &'static str {
    match t {
        Transport::Rest => "REST",
        Transport::Graphql => "GraphQL",
        Transport::Auth => "auth",
        Transport::Local => "local",
    }
}

/// The `Status class` facet's labels (D10).
pub(super) fn status_class(c: StatusClass) -> &'static str {
    match c {
        StatusClass::Success => "2xx",
        StatusClass::NotModified => "304",
        StatusClass::ClientError => "4xx",
        StatusClass::ServerError => "5xx",
        StatusClass::Network => "Network",
    }
}

/// The `Measure` picker's labels (D10).
pub(super) fn measure(m: Measure) -> &'static str {
    match m {
        Measure::Count => "Count",
        Measure::Cost => "Cost",
        Measure::P50 => "p50 duration",
        Measure::P90 => "p90 duration",
        Measure::Max => "Max duration",
        Measure::ErrorRate => "Error rate",
    }
}

/// The log's status cell (D10): the code with a plain-language reason.
/// The record keeps no failure kind, so a network failure reads
/// "Network error" without guessing whether it timed out.
pub(super) fn status(code: u16, class: StatusClass) -> String {
    if class == StatusClass::Network {
        return "Network error".into();
    }
    let reason = match code {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No content",
        301 => "Moved permanently",
        302 => "Found",
        304 => "Not modified",
        400 => "Bad request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not found",
        409 => "Conflict",
        410 => "Gone",
        422 => "Unprocessable",
        429 => "Too many requests",
        500 => "Server error",
        502 => "Bad gateway",
        503 => "Unavailable",
        504 => "Gateway timeout",
        _ => return code.to_string(),
    };
    format!("{code} {reason}")
}

/// The log's cost cell spells its unit, because the log mixes
/// transports (D10): "12 points", "1 request", "free (304)", "unknown",
/// "no budget". A costed request that cost nothing reads "free".
pub(super) fn cost_cell(c: CostCell) -> String {
    match c {
        CostCell::Requests(0) | CostCell::Points(0) => "free".into(),
        CostCell::Requests(n) => format!("{} request{}", count(n.into()), plural(n)),
        CostCell::Points(n) => format!("{} point{}", count(n.into()), plural(n)),
        CostCell::Free304 => "free (304)".into(),
        CostCell::Unknown => "unknown".into(),
        CostCell::NoBudget => "no budget".into(),
    }
}

fn plural(n: u32) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The log's time cell (D10): `15:11:30`, with the date for rows
/// outside the `as_of` day (`Nov 4, 23:58:02`), in the local zone at
/// `tz_offset_ms`.
pub(super) fn log_time(ts_ms: u64, as_of_ms: u64, tz_offset_ms: i64) -> String {
    let day = |t: u64| (t as i64 + tz_offset_ms).div_euclid(DAY_MS);
    let zone = chrono::FixedOffset::east_opt((tz_offset_ms / 1_000) as i32)
        .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).expect("UTC"));
    let Some(t) = chrono::DateTime::from_timestamp_millis(ts_ms as i64) else {
        return "—".into();
    };
    let t = t.with_timezone(&zone);
    if day(ts_ms) == day(as_of_ms) {
        t.format("%H:%M:%S").to_string()
    } else {
        t.format("%b %-d, %H:%M:%S").to_string()
    }
}

/// The trend's bucket, as an adjective (D10): "hourly bars".
pub(super) fn bucket_word(span: Span) -> &'static str {
    match span {
        Span::H1 => "5-minute",
        Span::H24 => "hourly",
        Span::D7 => "6-hour",
        Span::D30 | Span::D90 => "daily",
    }
}

/// The range in running text: "in the last 24 hours".
pub(super) fn span_phrase(span: Span) -> &'static str {
    range_phrase(crate::usage_analytics::surfaces::Preset::Trailing(span))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_carry_a_plain_reason() {
        assert_eq!(status(200, StatusClass::Success), "200 OK");
        assert_eq!(status(304, StatusClass::NotModified), "304 Not modified");
        assert_eq!(status(502, StatusClass::ServerError), "502 Bad gateway");
        assert_eq!(status(0, StatusClass::Network), "Network error");
        assert_eq!(status(418, StatusClass::ClientError), "418");
    }

    #[test]
    fn cost_cells_spell_their_unit() {
        assert_eq!(cost_cell(CostCell::Points(12)), "12 points");
        assert_eq!(cost_cell(CostCell::Points(1)), "1 point");
        assert_eq!(cost_cell(CostCell::Requests(1)), "1 request");
        assert_eq!(cost_cell(CostCell::Requests(0)), "free");
        assert_eq!(cost_cell(CostCell::Free304), "free (304)");
        assert_eq!(cost_cell(CostCell::Unknown), "unknown");
        assert_eq!(cost_cell(CostCell::NoBudget), "no budget");
    }

    #[test]
    fn log_times_gain_the_date_outside_the_as_of_day() {
        // 2026-11-05 15:12:00 UTC.
        let as_of = 1_793_891_520_000u64;
        assert_eq!(log_time(as_of - 30_000, as_of, 0), "15:11:30");
        assert_eq!(
            log_time(as_of - 16 * 3_600_000, as_of, 0),
            "Nov 4, 23:12:00"
        );
        // +2 h: the same instants are 17:11:30 and Nov 5, 01:12.
        let east = 2 * 3_600_000;
        assert_eq!(log_time(as_of - 30_000, as_of, east), "17:11:30");
        assert_eq!(log_time(as_of - 16 * 3_600_000, as_of, east), "01:12:00");
    }

    #[test]
    fn labels_follow_d21_casing() {
        assert_eq!(transport(Transport::Graphql), "GraphQL");
        assert_eq!(transport(Transport::Local), "local");
        assert_eq!(status_class(StatusClass::NotModified), "304");
        assert_eq!(measure(Measure::P90), "p90 duration");
        assert_eq!(bucket_word(Span::D7), "6-hour");
        assert_eq!(span_phrase(Span::H1), "in the last hour");
    }
}
