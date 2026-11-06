//! One check row (docs/0016 D7, docs/0039 D3, #792): chevron, state, name,
//! workflow, time, and the hand-off to GitHub. On a row with a log the whole
//! row is the toggle; it stays one real button, so keyboard and screen reader
//! reach it like any other control.

use gpui::{
    AnyElement, App, Context, InteractiveElement, IntoElement, ParentElement, SharedString, Styled,
    div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable as _, h_flex, v_flex};

use crate::gql;
use crate::pr_page::checks::identity::CheckTarget;
use crate::pr_page::checks::timing::check_timing;
use crate::pr_page::{PrDetailPage, check_visual};

/// The word a screen reader gets for what the row's icon shows.
fn state_word(state: gql::CheckState) -> &'static str {
    match state {
        gql::CheckState::Success => "passed",
        gql::CheckState::Failure => "failed",
        gql::CheckState::Pending => "running",
        gql::CheckState::Waiting => "waiting",
        gql::CheckState::Neutral => "skipped",
    }
}

/// The row's name with its workflow: `"build in Release"`. Names repeat, so
/// every accessible name on the row carries both.
fn spoken_name(check: &gql::PrCheck) -> String {
    match &check.workflow {
        Some(workflow) => format!("{} in {workflow}", check.name),
        None => check.name.clone(),
    }
}

/// The toggle's accessible name. It stands in for the row's whole content,
/// and it tells same-named rows apart.
pub(crate) fn toggle_label(check: &gql::PrCheck, expanded: bool) -> String {
    let verb = if expanded { "Hide" } else { "View" };
    let mut label = format!("{verb} log for {}", spoken_name(check));
    label.push_str(&format!(", {}", state_word(check.state)));
    if let Some(timing) = check_timing(check) {
        label.push_str(&format!(", {timing}"));
    }
    label
}

/// What the row shows, toggle or not: the chevron column stays reserved so
/// names align down the list, gated rows included.
fn row_content(check: &gql::PrCheck, chevron: Option<IconName>, cx: &App) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let (icon, color) = check_visual(check.state, cx);
    h_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .items_center()
        .child(match chevron {
            Some(chevron) => Icon::new(chevron)
                .size_3()
                .flex_none()
                .text_color(muted)
                .into_any_element(),
            None => div().flex_none().size_3().into_any_element(),
        })
        // #397: running rows spin, GitHub-style; Spinner covers
        // reduced-motion (static, zero frames) and keeps the animation
        // inside the glyph's box.
        .child(if check.state == gql::CheckState::Pending {
            Spinner::new().icon(icon).color(color).into_any_element()
        } else {
            Icon::new(icon)
                .size_4()
                .flex_none()
                .text_color(color)
                .into_any_element()
        })
        .child(
            div()
                .text_sm()
                .min_w_0()
                .truncate()
                .child(check.name.clone()),
        )
        .children(check.workflow.clone().map(|workflow| {
            div()
                .text_xs()
                .min_w_0()
                .truncate()
                .text_color(muted)
                .child(workflow)
        }))
        .child(div().flex_1())
        .children(
            check_timing(check)
                .map(|timing| div().flex_none().text_xs().text_color(muted).child(timing)),
        )
}

impl PrDetailPage {
    pub(in crate::pr_page) fn render_check_row(
        &self,
        i: usize,
        check: &gql::PrCheck,
        head_oid: Option<&str>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let target = CheckTarget::of(head_oid, check);
        let expanded = self.expanded_check.as_ref() == Some(&target.key);
        // docs/0039 D3 as amended by docs/0076 D1: Actions-origin rows
        // expand in every running state — the live watch owns in-progress
        // ones. Status contexts and third-party checks keep the external-link
        // affordance; docs/0066 Waiting rows stay with the gated-run flow.
        let loggable =
            check.origin == gql::CheckOrigin::Actions && check.state != gql::CheckState::Waiting;
        let main = if loggable {
            let chevron = if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            };
            let page = cx.entity();
            let toggle_target = target.clone();
            // The wrapper carries the probe selector (docs/0010 D10 id
            // discipline lives on the Button itself).
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                // Inset so the scroll container doesn't clip the focus ring;
                // this plus the button's px_2p5 is the plain row's px_3.
                .p_0p5()
                .debug_selector(move || format!("check-log-toggle-{i}"))
                .child(
                    Button::new(SharedString::from(format!("check-log-toggle-{i}")))
                        .ghost()
                        .xsmall()
                        .w_full()
                        .h_full()
                        .px_2p5()
                        // docs/0049 D1: expand/collapse semantics.
                        .accessibility_label(toggle_label(check, expanded))
                        .toggled(expanded)
                        .child(row_content(check, Some(chevron), cx))
                        .on_click(move |_, _, cx| {
                            page.update(cx, |this, cx| this.toggle_check_log(&toggle_target, cx))
                        }),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .px_3()
                .child(row_content(check, None, cx))
                .into_any_element()
        };
        let row = h_flex()
            .id(SharedString::from(format!("pr-check-{i}")))
            // A button's text size tightens its line height: one fixed height
            // keeps toggle rows, plain rows and gated rows in rhythm.
            .h_10()
            .items_center()
            .pr_3()
            .child(main)
            .child(match check.details_url.clone() {
                Some(url) => Button::new(SharedString::from(format!("pr-check-open-{i}")))
                    .ghost()
                    .xsmall()
                    .icon(IconName::ExternalLink)
                    .tooltip("View details")
                    .accessibility_label(format!("Open {} on GitHub", spoken_name(check)))
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .into_any_element(),
                // Keeps the time column aligned on a row with no link.
                None => div().flex_none().size_5().into_any_element(),
            });
        v_flex()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(row)
            .children(
                (expanded && loggable).then(|| self.render_check_log_pane(i, check, &target, cx)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gql::{CheckOrigin, CheckState, PrCheck};

    fn check(workflow: Option<&str>) -> PrCheck {
        PrCheck {
            name: "build".into(),
            state: CheckState::Failure,
            details_url: None,
            origin: CheckOrigin::Actions,
            check_run_id: None,
            suite_id: None,
            workflow: workflow.map(str::to_string),
            started_at: Some("2026-10-23T10:00:00Z".into()),
            completed_at: Some("2026-10-23T10:01:42Z".into()),
        }
    }

    #[test]
    fn the_toggle_says_what_the_row_shows() {
        assert_eq!(
            toggle_label(&check(Some("Release")), false),
            "View log for build in Release, failed, 1m 42s"
        );
        assert_eq!(
            toggle_label(&check(None), true),
            "Hide log for build, failed, 1m 42s"
        );
    }

    #[test]
    fn same_named_rows_have_different_names() {
        assert_ne!(
            toggle_label(&check(Some("CI")), false),
            toggle_label(&check(Some("Release")), false)
        );
    }
}
