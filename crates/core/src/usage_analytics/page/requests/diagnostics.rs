//! Band 4 (docs/0082 D10, Q8): Diagnostics, a disclosure collapsed by
//! default, above the log. Its content is docs/0054 §6's (#337, #410),
//! unchanged: the duration histogram, the payload × duration grid,
//! contention, and the 304-vs-200 comparison. The disclosure is one Tab
//! stop and reports its expanded state (D19); Enter and Space toggle it
//! through gpui's keyboard click on a focused clickable element.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, FontWeight, SharedString, Window, div, px};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable as _, ThemeStyled as _, h_flex, v_flex};

use super::format;
use super::grid::{GridCell, GridCol, grid};
use crate::usage_analytics::page::UsagePage;
use crate::usage_analytics::page::band_frame;
use crate::usage_analytics::page::chart::{self, ValueFormat, hbars, steps};
use crate::usage_analytics::surfaces::Span;
use crate::usage_analytics::surfaces::diagnostics::{
    Contention, DURATION_LABELS, Diagnostics, PayloadGrid, QUEUE_NOTABLE_MS, RevalidationOp,
    SIZE_LABELS,
};

/// What the collapsed disclosure says it holds.
const CONTENTS: &str = "Duration histogram · payload × duration · contention · 304 vs 200";

/// The histogram's summary line.
pub(super) fn histogram_summary(d: &Diagnostics) -> String {
    let slow: u64 = d.histogram.counts[4..].iter().sum();
    format!(
        "{} by duration band; {} took 1 s or more.",
        format::requests(d.requests),
        format::count(slow)
    )
}

/// The payload grid's summary: what is left out, and why.
pub(super) fn payload_summary(p: &PayloadGrid) -> String {
    let base = "Requests per duration band and response size";
    if p.unmeasured == 0 {
        format!("{base}.")
    } else {
        format!(
            "{base}; {} had no measured body (304s and network errors) and are not in the grid.",
            format::requests(p.unmeasured)
        )
    }
}

/// Contention's two lines: the peak, and the queue waits.
pub(super) fn contention_lines(c: &Contention, span: Span) -> (String, String) {
    let dated = span.ms() > 24 * 3_600_000;
    let peak = match c.peak_at_ms {
        Some(at) if c.peak_in_flight > 0 => format!(
            "At most {} in flight at once, first at {}: {}.",
            format::count(c.peak_in_flight),
            format::instant(at, dated),
            c.peak_ops.join(", ")
        ),
        _ => "Nothing was in flight.".into(),
    };
    let queue = match (c.queue_p50_ms, c.queue_p90_ms) {
        (Some(p50), Some(p90)) => format!(
            "Queue wait p50 {}, p90 {}; {} of {} waits were {} or longer.",
            format::duration(p50.into()),
            format::duration(p90.into()),
            format::count(c.queued_notable),
            format::count(c.queued),
            format::duration(QUEUE_NOTABLE_MS.into())
        ),
        _ => "No queue waits were recorded.".into(),
    };
    (peak, queue)
}

fn revalidation_rows(ops: &[RevalidationOp]) -> Vec<Vec<GridCell>> {
    ops.iter()
        .map(|o| {
            let delta = match o.delta_ms {
                d if d > 0 => format!("{} faster", format::duration(d as u64)),
                d if d < 0 => format!("{} slower", format::duration(d.unsigned_abs())),
                _ => "same".into(),
            };
            vec![
                GridCell::new(o.op.clone()),
                GridCell::new(format::count(o.full)),
                GridCell::new(format::duration(o.full_median_ms.into())),
                GridCell::new(format::count(o.revalidated)),
                GridCell::new(format::duration(o.revalidated_median_ms.into())),
                GridCell::new(delta),
                GridCell::new(
                    o.bytes_saved_est
                        .map_or_else(|| "—".into(), |b| format!("≈ {}", format::bytes(b))),
                ),
            ]
        })
        .collect()
}

fn subsection(title: &'static str, summary: String, body: AnyElement, cx: &App) -> AnyElement {
    v_flex()
        .gap_2()
        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
        .child(band_frame::summary(summary, cx))
        .child(body)
        .into_any_element()
}

/// Where the band's data sits in time: the span read back from `as_of`.
#[derive(Debug, Clone, Copy)]
pub(super) struct Frame {
    pub span: Span,
    pub as_of_ms: u64,
    pub tz_offset_ms: i64,
}

/// The open disclosure's body.
fn body(d: &Diagnostics, frame: Frame, cx: &App) -> AnyElement {
    let Frame {
        span,
        as_of_ms,
        tz_offset_ms,
    } = frame;
    let rows: Vec<(String, f64)> = DURATION_LABELS
        .iter()
        .zip(&d.histogram.counts)
        .map(|(l, n)| (l.to_string(), *n as f64))
        .collect();
    let max = rows.iter().map(|(_, v)| *v).fold(0.0, f64::max);
    let hist_label = format!(
        "Bar chart of requests by duration band: {}",
        rows.iter()
            .map(|(k, v)| format!("{k} {}", format::count(*v as u64)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let histogram = div()
        .id("usage-histogram-chart")
        .role(gpui::accesskit::Role::Label)
        .aria_label(hist_label)
        .w_full()
        .h(px(hbars::height(rows.len())))
        .child(hbars::HBarChart {
            rows,
            ticks: chart::value_ticks(max),
            unit: "requests",
        })
        .into_any_element();

    let mut cols = vec![GridCol {
        name: "Duration",
        width: 112.,
        right: false,
    }];
    cols.extend(SIZE_LABELS.iter().map(|l| GridCol {
        name: l,
        width: 104.,
        right: true,
    }));
    let payload_rows = DURATION_LABELS
        .iter()
        .zip(&d.payload.cells)
        .map(|(l, cells)| {
            std::iter::once(GridCell::new(l.to_string()))
                .chain(cells.iter().map(|n| GridCell::new(format::count(*n))))
                .collect()
        })
        .collect();
    let payload = grid(
        "usage-payload-grid",
        "Requests by duration band and response size".into(),
        &cols,
        payload_rows,
        cx,
    );

    let (peak, queue) = contention_lines(&d.contention, span);
    let steps_data: Vec<steps::Step> = d
        .contention
        .buckets
        .iter()
        .map(|b| steps::Step {
            start_ms: b.start_ms,
            end_ms: b.end_ms,
            value: Some(b.max_in_flight as f64),
        })
        .collect();
    let in_flight_max = steps_data
        .iter()
        .filter_map(|s| s.value)
        .fold(0.0, f64::max);
    let buckets = d.contention.buckets.clone();
    let dated = span.ms() > 24 * 3_600_000;
    let contention_chart = chart::frame(
        "usage-contention-chart",
        format!(
            "Step chart of the most requests in flight per {} bucket; at most {}",
            format::bucket_word(span),
            format::count(d.contention.peak_in_flight)
        ),
        steps::StepsChart {
            id: "usage-contention-plot",
            from_ms: as_of_ms.saturating_sub(span.ms()),
            to_ms: as_of_ms,
            tz_offset_ms,
            steps: steps_data,
            ticks: chart::value_ticks(in_flight_max),
            format: ValueFormat::Count,
            tip: Rc::new(move |ix| {
                let b = buckets.get(ix)?;
                Some((
                    format::window_span(b.start_ms, b.end_ms, dated),
                    vec![("Most in flight".into(), format::count(b.max_in_flight))],
                ))
            }),
        }
        .into_any_element(),
    )
    .into_any_element();
    let contention = v_flex()
        .gap_2()
        .child(band_frame::summary(queue, cx))
        .child(contention_chart)
        .into_any_element();

    let reval_cols = [
        GridCol {
            name: "Operation",
            width: 180.,
            right: false,
        },
        GridCol {
            name: "200s",
            width: 72.,
            right: true,
        },
        GridCol {
            name: "200 median",
            width: 100.,
            right: true,
        },
        GridCol {
            name: "304s",
            width: 72.,
            right: true,
        },
        GridCol {
            name: "304 median",
            width: 100.,
            right: true,
        },
        GridCol {
            name: "Difference",
            width: 120.,
            right: true,
        },
        GridCol {
            name: "Bytes saved",
            width: 112.,
            right: true,
        },
    ];
    let reval = if d.revalidation.is_empty() {
        band_frame::empty(
            "usage-revalidation-empty",
            "No operation came back both 200 and 304 in this selection.".into(),
            cx,
        )
        .into_any_element()
    } else {
        grid(
            "usage-revalidation",
            "Median duration of full responses against revalidations, per operation".into(),
            &reval_cols,
            revalidation_rows(&d.revalidation),
            cx,
        )
    };

    v_flex()
        .id("usage-diagnostics-body")
        .gap_6()
        .pt_2()
        .child(subsection("Duration histogram", histogram_summary(d), histogram, cx))
        .child(subsection("Payload × duration", payload_summary(&d.payload), payload, cx))
        .child(subsection("Contention", peak, contention, cx))
        .child(subsection(
            "304 vs 200",
            "Whether revalidating is also faster, per operation with both in range. Bytes saved is estimated from the 200s' bodies.".into(),
            reval,
            cx,
        ))
        .into_any_element()
}

impl UsagePage {
    /// The Diagnostics band: the disclosure, and its body when open
    /// (or its skeleton while the selection re-derives).
    pub(super) fn diagnostics_band(
        &self,
        data: Option<(&Diagnostics, Frame)>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.diagnostics_open;
        let focus = self.focus.as_ref().map(|f| f.diagnostics.clone());
        let focused = focus.as_ref().is_some_and(|f| f.is_focused(window));
        let theme = cx.theme();
        let toggle = h_flex()
            .id("usage-diagnostics-toggle")
            .debug_selector(|| "usage-diagnostics-toggle".to_string())
            .role(gpui::accesskit::Role::Button)
            .aria_label("Diagnostics")
            .aria_description(CONTENTS)
            .aria_expanded(open)
            .when_some(focus.as_ref(), |this, f| this.track_focus(f))
            .when(focused, |this| this.focus_ring_style(window, cx))
            .rounded(theme.radius)
            .items_center()
            .gap_2()
            .py_1()
            .cursor_pointer()
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .small()
                .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Diagnostics"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(CONTENTS)),
            )
            .on_click(cx.listener(|this, _, _, cx| this.toggle_diagnostics(cx)));
        let content = open.then(|| match data {
            Some((d, frame)) => body(d, frame, cx),
            None => {
                band_frame::skeleton("usage-diagnostics-loading", super::DIAGNOSTICS_SKELETON_H)
            }
        });
        v_flex()
            .id("usage-diagnostics")
            .role(gpui::accesskit::Role::Group)
            .aria_label("Diagnostics")
            .w_full()
            .child(toggle)
            .children(content)
            .into_any_element()
    }

    fn toggle_diagnostics(&mut self, cx: &mut Context<Self>) {
        self.diagnostics_open = !self.diagnostics_open;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_analytics::surfaces::diagnostics::{ContentionBucket, DurationHistogram};

    fn diag() -> Diagnostics {
        Diagnostics {
            histogram: DurationHistogram {
                counts: vec![10, 20, 30, 15, 4, 1],
                requests: 80,
            },
            payload: PayloadGrid {
                cells: vec![vec![0; 5]; 6],
                unmeasured: 7,
            },
            contention: Contention {
                peak_in_flight: 0,
                peak_at_ms: None,
                peak_ops: Vec::new(),
                buckets: vec![ContentionBucket {
                    start_ms: 0,
                    end_ms: 10,
                    max_in_flight: 0,
                }],
                queued: 0,
                queued_notable: 0,
                queue_p50_ms: None,
                queue_p90_ms: None,
            },
            revalidation: Vec::new(),
            requests: 80,
        }
    }

    #[test]
    fn summaries_count_the_tail_and_what_the_grid_leaves_out() {
        let d = diag();
        assert_eq!(
            histogram_summary(&d),
            "80 requests by duration band; 5 took 1 s or more."
        );
        assert_eq!(
            payload_summary(&d.payload),
            "Requests per duration band and response size; 7 requests had no measured body (304s and network errors) and are not in the grid."
        );
        assert_eq!(
            contention_lines(&d.contention, Span::H24),
            (
                "Nothing was in flight.".to_string(),
                "No queue waits were recorded.".to_string()
            )
        );
    }

    #[test]
    fn contention_reads_the_peak_and_the_queue() {
        let mut c = diag().contention;
        c.peak_in_flight = 4;
        c.peak_at_ms = Some(1_793_891_520_000);
        c.peak_ops = vec!["pr-detail".into(), "repo-pulls".into()];
        c.queued = 900;
        c.queued_notable = 12;
        c.queue_p50_ms = Some(3);
        c.queue_p90_ms = Some(40);
        let (peak, queue) = contention_lines(&c, Span::H24);
        assert!(
            peak.starts_with("At most 4 in flight at once, first at "),
            "{peak}"
        );
        assert!(peak.ends_with(": pr-detail, repo-pulls."), "{peak}");
        assert_eq!(
            queue,
            "Queue wait p50 3 ms, p90 40 ms; 12 of 900 waits were 100 ms or longer."
        );
    }

    #[test]
    fn revalidation_rows_say_which_side_was_faster() {
        let op = RevalidationOp {
            op: "notifications".into(),
            full: 2,
            full_median_ms: 300,
            revalidated: 5,
            revalidated_median_ms: 100,
            delta_ms: 200,
            bytes_saved_est: Some(25_000),
        };
        let texts: Vec<String> = revalidation_rows(&[op])[0]
            .iter()
            .map(|c| c.text.clone())
            .collect();
        assert_eq!(
            texts,
            [
                "notifications",
                "2",
                "300 ms",
                "5",
                "100 ms",
                "200 ms faster",
                "≈ 24 KB"
            ]
        );
    }
}
