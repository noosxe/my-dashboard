//! Band 2 (docs/0082 D10, D10a): one trend of the selected measure over
//! clock-aligned buckets on a proportional time axis. Counts and cost
//! are columns, percentiles and the error rate step lines with a
//! `chart_1` area (Q14). `Measure: cost` is **this app's recorded spend
//! over time** for one quota transport (Q7, #709), and its title and
//! summary say so (§4 principle).

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App};
use gpui_component::ActiveTheme as _;

use super::format;
use crate::usage_analytics::page::chart::{
    self, ValueFormat,
    columns::{ColumnMark, ColumnsChart, Tip},
    steps::{Step, StepsChart},
};
use crate::usage_analytics::surfaces::{Budget, Measure, RequestTrend, Span, TrendBucket};
use crate::usage_records::Transport;

/// The band title for `measure` (D21), known before the data lands so
/// the skeleton keeps it.
pub(super) fn title(measure: Measure) -> &'static str {
    match measure {
        Measure::Count => "Requests over time",
        Measure::Cost => "Recorded spend over time",
        Measure::P50 => "p50 duration over time",
        Measure::P90 => "p90 duration over time",
        Measure::Max => "Max duration over time",
        Measure::ErrorRate => "Error rate over time",
    }
}

/// Counts and cost are bars; the rest are step lines (D10).
fn bars(m: Measure) -> bool {
    matches!(m, Measure::Count | Measure::Cost)
}

fn value_format(m: Measure) -> ValueFormat {
    match m {
        Measure::Count | Measure::Cost => ValueFormat::Count,
        Measure::P50 | Measure::P90 | Measure::Max => ValueFormat::Duration,
        Measure::ErrorRate => ValueFormat::Percent,
    }
}

/// The bucket's value for this measure: a count is never missing.
fn value(b: &TrendBucket, m: Measure) -> Option<f64> {
    match m {
        Measure::Count => Some(b.requests as f64),
        _ => b.value,
    }
}

/// The bucket's span in words, dated for ranges over a day (D10a).
fn bucket_span(b: &TrendBucket, span: Span) -> String {
    format::window_span(b.start_ms, b.end_ms, span.ms() > 24 * 3_600_000)
}

/// The highest bucket, earliest on a tie.
fn peak(t: &RequestTrend) -> Option<(&TrendBucket, f64)> {
    t.buckets
        .iter()
        .filter_map(|b| value(b, t.measure).map(|v| (b, v)))
        .fold(None, |best, (b, v)| match best {
            Some((_, bv)) if bv >= v => best,
            _ => Some((b, v)),
        })
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// The band's summary line: the total or the peak, the range, the
/// bucket and its mark. Cost names its budget's unit and says it is this
/// app's recorded spend (§4 principle).
pub(super) fn summary(t: &RequestTrend, span: Span, transport: Option<Transport>) -> String {
    let range = format::span_phrase(span);
    let bucket = format::bucket_word(span);
    let fmt = value_format(t.measure);
    let peak_words = peak(t)
        .filter(|(_, v)| *v > 0.0)
        .map(|(b, v)| format!("; highest {} in {}", fmt.label(v), bucket_span(b, span)))
        .unwrap_or_default();
    match t.measure {
        Measure::Count => format!(
            "{} {range}, {bucket} bars{peak_words}.",
            format::requests(t.requests)
        ),
        Measure::Cost => {
            let budget = transport.and_then(Budget::of).unwrap_or(Budget::Rest);
            let cost: f64 = t.buckets.iter().filter_map(|b| b.value).sum();
            let unknown: u64 = t.groups.iter().map(|g| g.unknown).sum();
            let mut s = format!(
                "This app's recorded spend over time: {} {range}, {bucket} bars{peak_words}.",
                format::cost_of_sent(cost as u64, t.requests, budget, true)
            );
            if unknown > 0 {
                s.push_str(&format!(
                    " {} had no reported cost.",
                    format::requests(unknown)
                ));
            }
            s
        }
        Measure::ErrorRate => format!(
            "{} error rate across {} {range}: {} failed{peak_words}.",
            capitalised(bucket),
            format::requests(t.requests),
            format::count(t.errors)
        ),
        m => format!(
            "{} {} across {} {range}{peak_words}.",
            capitalised(bucket),
            format::measure(m),
            format::requests(t.requests)
        ),
    }
}

/// The chart's single a11y summary (D19).
fn chart_label(t: &RequestTrend, span: Span) -> String {
    let kind = if bars(t.measure) {
        "Bar chart"
    } else {
        "Step chart"
    };
    let fmt = value_format(t.measure);
    let mut s = format!(
        "{kind} of {} per {} bucket, {} buckets",
        format::measure(t.measure).to_lowercase(),
        format::bucket_word(span),
        t.buckets.len()
    );
    if let Some((b, v)) = peak(t) {
        s.push_str(&format!(
            "; highest {} in {}",
            fmt.label(v),
            bucket_span(b, span)
        ));
    }
    s
}

fn tip(t: &RequestTrend, span: Span) -> Rc<dyn Fn(usize) -> Option<Tip>> {
    let buckets = t.buckets.clone();
    let measure = t.measure;
    Rc::new(move |ix| {
        let b = buckets.get(ix)?;
        let mut rows = vec![("Requests".to_string(), format::count(b.requests))];
        if measure != Measure::Count {
            let v =
                value(b, measure).map_or_else(|| "—".into(), |v| value_format(measure).label(v));
            rows.insert(0, (format::measure(measure).to_string(), v));
        }
        Some((bucket_span(b, span), rows))
    })
}

/// The trend chart for `t`, in its a11y frame.
pub(super) fn chart(t: &RequestTrend, span: Span, tz_offset_ms: i64, cx: &App) -> AnyElement {
    let series = cx.theme().chart_1;
    let max = t
        .buckets
        .iter()
        .filter_map(|b| value(b, t.measure))
        .fold(0.0, f64::max);
    let ticks = chart::value_ticks(max);
    let format = value_format(t.measure);
    let plot = if bars(t.measure) {
        ColumnsChart {
            id: "usage-trend-plot",
            from_ms: t.bounds.from_ms,
            to_ms: t.bounds.to_ms,
            tz_offset_ms,
            marks: t
                .buckets
                .iter()
                .map(|b| ColumnMark {
                    start_ms: b.start_ms,
                    end_ms: b.end_ms,
                    value: value(b, t.measure),
                    color: series,
                })
                .collect(),
            ticks,
            format,
            guides: Vec::new(),
            off_scale: None,
            tip: tip(t, span),
        }
        .into_any_element()
    } else {
        StepsChart {
            id: "usage-trend-plot",
            from_ms: t.bounds.from_ms,
            to_ms: t.bounds.to_ms,
            tz_offset_ms,
            steps: t
                .buckets
                .iter()
                .map(|b| Step {
                    start_ms: b.start_ms,
                    end_ms: b.end_ms,
                    value: b.value,
                })
                .collect(),
            ticks,
            format,
            tip: tip(t, span),
        }
        .into_any_element()
    };
    chart::frame("usage-trend-chart", chart_label(t, span), plot).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_analytics::surfaces::Bounds;

    const H: u64 = 3_600_000;
    /// 2026-11-05 00:00 UTC.
    const DAY0: u64 = 1_793_836_800_000;

    fn trend(measure: Measure, values: &[Option<f64>]) -> RequestTrend {
        RequestTrend {
            bounds: Bounds {
                from_ms: DAY0,
                to_ms: DAY0 + values.len() as u64 * H,
            },
            measure,
            cost_allowed: measure == Measure::Cost,
            bucket_ms: H,
            buckets: values
                .iter()
                .enumerate()
                .map(|(i, v)| TrendBucket {
                    start_ms: DAY0 + i as u64 * H,
                    end_ms: DAY0 + (i as u64 + 1) * H,
                    requests: 10,
                    value: *v,
                })
                .collect(),
            groups: Vec::new(),
            requests: 10 * values.len() as u64,
            errors: 3,
        }
    }

    #[test]
    fn percentile_summaries_name_the_bucket_and_the_peak() {
        let t = trend(Measure::P90, &[Some(800.0), None, Some(1_900.0)]);
        let s = summary(&t, Span::H24, None);
        assert!(
            s.starts_with(
                "Hourly p90 duration across 30 requests in the last 24 hours; highest 1.9 s in "
            ),
            "{s}"
        );
        assert!(
            chart_label(&t, Span::H24).starts_with(
                "Step chart of p90 duration per hourly bucket, 3 buckets; highest 1.9 s"
            )
        );
    }

    #[test]
    fn the_cost_trend_is_this_apps_recorded_spend() {
        let t = trend(Measure::Cost, &[Some(4.0), Some(8.0)]);
        let s = summary(&t, Span::H24, Some(Transport::Graphql));
        assert!(
            s.starts_with(
                "This app's recorded spend over time: 12 GraphQL points over 20 requests in the last 24 hours, hourly bars; highest 8 in "
            ),
            "{s}"
        );
        assert_eq!(title(Measure::Cost), "Recorded spend over time");
        let rest = summary(&t, Span::H24, Some(Transport::Rest));
        assert!(rest.contains("12 REST requests counted against the limit, of 20 sent"));
    }

    #[test]
    fn counts_and_error_rates_read_their_totals() {
        let count = summary(&trend(Measure::Count, &[None, None]), Span::H1, None);
        // The bucket's clock time is local; the words around it are not.
        assert!(
            count.starts_with("20 requests in the last hour, 5-minute bars; highest 10 in "),
            "{count}"
        );
        let err = summary(&trend(Measure::ErrorRate, &[Some(0.0)]), Span::D7, None);
        assert_eq!(
            err,
            "6-hour error rate across 10 requests in the last 7 days: 3 failed."
        );
        assert!(bars(Measure::Cost) && !bars(Measure::Max));
    }
}
