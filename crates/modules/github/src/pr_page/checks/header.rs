//! The Checks tab's summary line (docs/0016 D7, #792). Failure leads, in
//! tone; the total comes next; the buckets stay quiet. The line is a list of
//! parts so the render can tone each one and the tests can read it whole.

use gpui::{AnyElement, App, FontWeight, IntoElement, ParentElement, SharedString, Styled, div};
use gpui_component::{ActiveTheme, h_flex};

use crate::gql;
use crate::pr_page::ChecksCounts;
use crate::pr_page::checks::cap;
use crate::pr_page::checks::counter::checks_total;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PartTone {
    Danger,
    Total,
    Muted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeaderPart {
    pub text: String,
    pub tone: PartTone,
}

impl HeaderPart {
    pub(crate) fn new(text: impl Into<String>, tone: PartTone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }
}

fn total_part(total: i64) -> HeaderPart {
    let plural = if total == 1 { "" } else { "s" };
    HeaderPart::new(format!("{total} check{plural}"), PartTone::Total)
}

/// An uncapped list: `"1 failed · 8 checks · 7 passed"`. `waiting` is the
/// gated-runs count (docs/0066 D3): not rollup members, but the total
/// includes them so the line agrees with the tab's counter (#783).
fn listed_parts(counts: &ChecksCounts, waiting: usize) -> Vec<HeaderPart> {
    let mut parts = Vec::new();
    if counts.failed > 0 {
        parts.push(HeaderPart::new(
            format!("{} failed", counts.failed),
            PartTone::Danger,
        ));
    }
    parts.push(total_part(checks_total(counts.total as i64, waiting)));
    parts.extend(
        [
            (counts.passed, "passed"),
            (counts.pending, "running"),
            (waiting, "waiting"),
            (counts.muted, "skipped"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, word)| HeaderPart::new(format!("{n} {word}"), PartTone::Muted)),
    );
    parts
}

/// The line's parts. A capped list takes its total and verdict from the
/// server (`checks::cap`).
pub(crate) fn header_parts(
    checks: &gql::PrChecks,
    rollup: Option<&str>,
    counts: &ChecksCounts,
    waiting: usize,
) -> Vec<HeaderPart> {
    if cap::is_capped(checks) {
        cap::capped_parts(checks, rollup, counts, waiting)
    } else {
        listed_parts(counts, waiting)
    }
}

pub(in crate::pr_page) fn render_header_line(parts: Vec<HeaderPart>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (danger, foreground, muted) = (theme.danger, theme.foreground, theme.muted_foreground);
    let mut line = h_flex().flex_wrap().gap_1p5().text_sm().text_color(muted);
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            line = line.child(div().child("·"));
        }
        let text = SharedString::from(part.text);
        line = line.child(match part.tone {
            PartTone::Danger => div()
                .text_color(danger)
                .font_weight(FontWeight::SEMIBOLD)
                .child(text),
            PartTone::Total => div().text_color(foreground).child(text),
            PartTone::Muted => div().child(text),
        });
    }
    line.into_any_element()
}

/// The parts as the line reads, for tests.
#[cfg(test)]
pub(crate) fn read(parts: &[HeaderPart]) -> String {
    parts
        .iter()
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(passed: usize, failed: usize, pending: usize, muted: usize) -> ChecksCounts {
        ChecksCounts {
            total: passed + failed + pending + muted,
            passed,
            failed,
            pending,
            muted,
            waiting: 0,
        }
    }

    fn line(c: &ChecksCounts, waiting: usize) -> String {
        read(&listed_parts(c, waiting))
    }

    #[test]
    fn the_failure_count_leads_in_tone() {
        let parts = listed_parts(&counts(7, 1, 0, 0), 0);
        assert_eq!(read(&parts), "1 failed · 8 checks · 7 passed");
        assert_eq!(parts[0].tone, PartTone::Danger);
        assert_eq!(parts[1].tone, PartTone::Total);
        assert_eq!(parts[2].tone, PartTone::Muted);
    }

    #[test]
    fn without_failures_the_total_leads_and_zero_buckets_drop() {
        assert_eq!(line(&counts(1, 0, 0, 0), 0), "1 check · 1 passed");
        let parts = listed_parts(&counts(3, 0, 1, 0), 0);
        assert_eq!(parts[0].tone, PartTone::Total);
        assert_eq!(read(&parts), "4 checks · 3 passed · 1 running");
    }

    #[test]
    fn every_bucket_has_its_word() {
        assert_eq!(
            line(&counts(1, 1, 1, 1), 0),
            "1 failed · 4 checks · 1 passed · 1 running · 1 skipped"
        );
    }

    #[test]
    fn waiting_runs_join_the_total_as_on_the_tab() {
        // docs/0066 D3, #783.
        assert_eq!(
            line(&counts(1, 1, 0, 0), 2),
            "1 failed · 4 checks · 1 passed · 2 waiting"
        );
        assert_eq!(line(&ChecksCounts::default(), 2), "2 checks · 2 waiting");
        assert_eq!(line(&ChecksCounts::default(), 1), "1 check · 1 waiting");
        assert_eq!(line(&ChecksCounts::default(), 0), "0 checks");
    }
}
