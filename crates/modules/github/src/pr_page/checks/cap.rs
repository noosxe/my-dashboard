//! The over-100 honesty rule (docs/0016 D7, #747): the query caps the rows
//! at `contexts(first: 100)`, so the rows can't speak for a bigger rollup.
//! The total and the verdict come from the server; the row counts are
//! labelled as covering only the rows that are listed.

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, IconName, Sizable, h_flex};

use crate::gql;
use crate::pr_page::checks::counter::checks_total;
use crate::pr_page::checks::header::{HeaderPart, PartTone};
use crate::pr_page::{ChecksCounts, PrDetailPage};

/// Is the list a part of the rollup? Also true when nodes dropped out of a
/// degraded payload (docs/0007 D3): they are just as unaccounted for.
pub(crate) fn is_capped(checks: &gql::PrChecks) -> bool {
    checks.total_count > checks.nodes.len() as i64
}

/// The overall verdict from `statusCheckRollup.state`. The rows can't give
/// it: a failure past the cap is on no row. The bool marks a failing verdict.
fn rollup_verdict(rollup: Option<&str>) -> (&'static str, bool) {
    match rollup {
        Some("SUCCESS") => ("all passed", false),
        Some("FAILURE") => ("some failed", true),
        Some("ERROR") => ("some errored", true),
        // "running" is the bucket word: a line could say it twice.
        Some("PENDING") => ("in progress", false),
        Some("EXPECTED") => ("queued", false),
        _ => ("status unknown", false),
    }
}

/// The line for a capped list. It carries the server's total and verdict and
/// scopes the buckets to the rows; a failing verdict leads, in tone (#792):
/// `"some failed · 137 checks · first 100: 10 failed · 90 passed"`. Runs
/// waiting for approval join the total, as on the tab (#783).
pub(crate) fn capped_parts(
    checks: &gql::PrChecks,
    rollup: Option<&str>,
    counts: &ChecksCounts,
    waiting: usize,
) -> Vec<HeaderPart> {
    let (verdict, failing) = rollup_verdict(rollup);
    let total = HeaderPart::new(
        format!("{} checks", checks_total(checks.total_count, waiting)),
        PartTone::Total,
    );
    let mut parts = if failing {
        vec![HeaderPart::new(verdict, PartTone::Danger), total]
    } else {
        vec![total, HeaderPart::new(verdict, PartTone::Muted)]
    };
    if waiting > 0 {
        parts.push(HeaderPart::new(
            format!("{waiting} waiting"),
            PartTone::Muted,
        ));
    }
    let buckets = [
        (counts.failed, "failed", PartTone::Danger),
        (counts.passed, "passed", PartTone::Muted),
        (counts.pending, "running", PartTone::Muted),
        (counts.muted, "skipped", PartTone::Muted),
        (counts.cancelled, "cancelled", PartTone::Muted),
    ]
    .into_iter()
    .filter(|(n, _, _)| *n > 0);
    for (i, (n, word, tone)) in buckets.enumerate() {
        let scope = if i == 0 {
            format!("first {}: ", counts.total)
        } else {
            String::new()
        };
        parts.push(HeaderPart::new(format!("{scope}{n} {word}"), tone));
    }
    parts
}

/// The caption beside the hand-off, in the Commits tab's words (D6). The
/// cap only limits the rollup's rows, so waiting runs are named apart: the
/// header's total includes them and the list always shows them.
pub(crate) fn cap_caption(checks: &gql::PrChecks, waiting: usize) -> String {
    let caption = format!(
        "Showing first {} of {} checks",
        checks.nodes.len(),
        checks.total_count
    );
    match waiting {
        0 => caption,
        n => format!("{caption}, plus {n} waiting"),
    }
}

/// Where the full list lives: the PR's own checks page.
pub(crate) fn checks_page_url(pr_url: &str) -> Option<String> {
    (!pr_url.is_empty()).then(|| format!("{}/checks", pr_url.trim_end_matches('/')))
}

/// The cap row, pinned under the header and reading as part of it (the
/// header drops its rule). On a 100-row list a row at the end would only be
/// seen after scrolling past what it qualifies.
pub(in crate::pr_page) fn render_cap_row(
    d: &gql::PrDetail,
    waiting: usize,
    cx: &Context<PrDetailPage>,
) -> Option<AnyElement> {
    if !is_capped(&d.checks) {
        return None;
    }
    let view_all = checks_page_url(&d.url).map(|url| {
        Button::new("pr-checks-view-all")
            .ghost()
            .xsmall()
            .icon(IconName::ExternalLink)
            .label("View all on GitHub")
            .accessibility_label(SharedString::from(format!(
                "View all {} checks on GitHub",
                d.checks.total_count
            )))
            .on_click(move |_, _, cx| cx.open_url(&url))
    });
    Some(
        h_flex()
            .id("pr-checks-truncated")
            .debug_selector(|| "pr-checks-truncated".into())
            .flex_none()
            .justify_between()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(cap_caption(&d.checks, waiting)),
            )
            .children(view_all)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gql::{CheckOrigin, CheckState, PrCheck, PrChecks};
    use crate::pr_page::checks::header::{header_parts, read};
    use crate::pr_page::checks_counts;

    fn listed(total: i64, states: &[(CheckState, usize)]) -> PrChecks {
        let nodes = states
            .iter()
            .flat_map(|(state, n)| {
                std::iter::repeat_n(
                    PrCheck {
                        name: "ci".into(),
                        state: *state,
                        details_url: None,
                        origin: CheckOrigin::Actions,
                        check_run_id: None,
                        suite_id: None,
                        workflow: None,
                        started_at: None,
                        completed_at: None,
                    },
                    *n,
                )
            })
            .collect();
        PrChecks {
            total_count: total,
            nodes,
            ..PrChecks::default()
        }
    }

    fn header(checks: &PrChecks, rollup: Option<&str>, waiting: usize) -> String {
        read(&header_parts(
            checks,
            rollup,
            &checks_counts(&checks.nodes),
            waiting,
        ))
    }

    #[test]
    fn an_uncapped_list_keeps_the_d7_line() {
        let checks = listed(8, &[(CheckState::Success, 7), (CheckState::Failure, 1)]);
        assert!(!is_capped(&checks));
        assert_eq!(
            header(&checks, Some("FAILURE"), 0),
            "1 failed · 8 checks · 7 passed"
        );
    }

    #[test]
    fn a_full_page_of_exactly_one_hundred_is_not_capped() {
        let checks = listed(100, &[(CheckState::Success, 100)]);
        assert!(!is_capped(&checks));
        assert_eq!(
            header(&checks, Some("SUCCESS"), 0),
            "100 checks · 100 passed"
        );
    }

    #[test]
    fn green_rows_over_a_failing_rollup_never_read_as_green() {
        // The issue's case: a failure at position 101.
        let checks = listed(137, &[(CheckState::Success, 100)]);
        assert!(is_capped(&checks));
        assert_eq!(
            header(&checks, Some("FAILURE"), 0),
            "some failed · 137 checks · first 100: 100 passed"
        );
        assert_eq!(cap_caption(&checks, 0), "Showing first 100 of 137 checks");
    }

    #[test]
    fn the_verdict_follows_every_rollup_state() {
        let checks = listed(137, &[(CheckState::Pending, 100)]);
        for (rollup, verdict) in [
            (Some("SUCCESS"), "137 checks · all passed"),
            (Some("FAILURE"), "some failed · 137 checks"),
            (Some("ERROR"), "some errored · 137 checks"),
            (Some("PENDING"), "137 checks · in progress"),
            (Some("EXPECTED"), "137 checks · queued"),
            (Some("NEW_STATE"), "137 checks · status unknown"),
            (None, "137 checks · status unknown"),
        ] {
            assert_eq!(
                header(&checks, rollup, 0),
                format!("{verdict} · first 100: 100 running")
            );
        }
    }

    #[test]
    fn a_failing_verdict_and_failed_rows_carry_the_tone() {
        let checks = listed(137, &[(CheckState::Success, 90), (CheckState::Failure, 10)]);
        let parts = header_parts(&checks, Some("FAILURE"), &checks_counts(&checks.nodes), 0);
        assert_eq!(
            read(&parts),
            "some failed · 137 checks · first 100: 10 failed · 90 passed"
        );
        assert_eq!(parts[0].tone, PartTone::Danger);
        assert_eq!(parts[2].tone, PartTone::Danger);
        assert_eq!(parts[3].tone, PartTone::Muted);
    }

    #[test]
    fn waiting_runs_ride_the_capped_line_outside_the_row_scope() {
        // docs/0066 D3: gated runs aren't rollup members or rows, but the
        // total counts them, as the tab does (#783).
        let checks = listed(137, &[(CheckState::Success, 100)]);
        assert_eq!(
            header(&checks, Some("PENDING"), 2),
            "139 checks · in progress · 2 waiting · first 100: 100 passed"
        );
        assert_eq!(
            cap_caption(&checks, 2),
            "Showing first 100 of 137 checks, plus 2 waiting"
        );
    }

    #[test]
    fn nodes_dropped_from_a_degraded_payload_count_as_capped() {
        let checks = listed(3, &[(CheckState::Success, 2)]);
        assert_eq!(
            header(&checks, Some("SUCCESS"), 0),
            "3 checks · all passed · first 2: 2 passed"
        );
        // Every node dropped: no empty row scope.
        let none = listed(3, &[]);
        assert_eq!(header(&none, Some("PENDING"), 0), "3 checks · in progress");
    }

    #[test]
    fn the_hand_off_goes_to_the_prs_checks_page() {
        assert_eq!(
            checks_page_url("https://github.com/o/r/pull/7").as_deref(),
            Some("https://github.com/o/r/pull/7/checks")
        );
        assert_eq!(checks_page_url(""), None);
    }
}
