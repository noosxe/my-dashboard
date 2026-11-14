//! The in-place picker card (docs/0083 D12). It replaces its section's
//! body, takes the filter's focus on open, keeps its checklist rows in a
//! scroll area of their own (max 240 px) and pins Save and Cancel under
//! them — so with 60 labels Save is still one click away (#732). Save is
//! disabled while the draft equals what the PR has (#756).

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, ClickEvent, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement as _, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, point, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _,
    avatar::Avatar,
    button::Button,
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState},
    scroll::ScrollableElement as _,
    spinner::Spinner,
    v_flex,
};

use super::metrics::PICKER_ROWS_MAX_H;
use super::section::{Section, empty_line};
use crate::gql::PrDetail;
use crate::pickers::{PickerDraft, PickerFetch, PickerRow, PickerSource, picker_row_matches};
use crate::pr_page::{BusyOp, PrDetailPage};

/// The ids the PR carries now for one picker — what "unchanged" means.
pub(crate) fn current_ids(d: &PrDetail, source: PickerSource) -> Vec<String> {
    match source {
        PickerSource::Labels => d.labels.iter().filter_map(|l| l.id.clone()).collect(),
        PickerSource::Assignees => d.assignees.iter().filter_map(|a| a.id.clone()).collect(),
        PickerSource::Reviewers => d
            .review_requests
            .iter()
            .filter_map(|r| r.id.clone())
            .collect(),
        PickerSource::Milestones => d.milestone.iter().map(|m| m.id.clone()).collect(),
    }
}

/// The milestone picker's "unchanged": the chosen id is the current one
/// (`None` = no milestone on both sides).
pub(crate) fn milestone_unchanged(chosen: Option<&str>, d: &PrDetail) -> bool {
    chosen == d.milestone.as_ref().map(|m| m.id.as_str())
}

/// Put a freshly opened picker where the reader can use it: its section's
/// title at the top of the scroll region, the checklist scrolled to its
/// first row and the filter (the first row, for the milestone) focused.
pub(crate) fn after_open(
    page: &mut PrDetailPage,
    section: Section,
    window: &mut Window,
    cx: &mut Context<PrDetailPage>,
) {
    page.picker_scroll.set_offset(point(px(0.), px(0.)));
    page.rail_scroll.scroll_to_top_of_item(section.index());
    let filter = match section {
        Section::Reviewers | Section::Assignees => Some(&page.users_search),
        Section::Labels => Some(&page.labels_search),
        Section::Milestone => None,
    };
    match filter {
        Some(search) => search.read(cx).focus_handle(cx).focus(window, cx),
        None => page.picker_row_focus.focus(window, cx),
    }
}

fn loading_line(cx: &Context<PrDetailPage>) -> AnyElement {
    h_flex()
        .gap_2()
        .items_center()
        .child(Spinner::new().small())
        .child(empty_line("pr-picker-loading", "Loading…", cx))
        .into_any_element()
}

fn error_line(msg: &str, cx: &Context<PrDetailPage>) -> AnyElement {
    div()
        .text_sm()
        .text_color(cx.theme().danger)
        .child(msg.to_string())
        .into_any_element()
}

/// The Save / Cancel row, pinned under the rows' scroll area.
fn actions(
    id: &'static str,
    save_id: &'static str,
    op: BusyOp,
    busy: bool,
    unchanged: bool,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    h_flex()
        .gap_2()
        .items_center()
        .pt_1()
        .child(
            Button::new(save_id)
                .debug_selector(move || save_id.to_string())
                .label("Save")
                .small()
                .disabled(busy || unchanged)
                .when(unchanged, |b| b.tooltip("No changes"))
                .on_click(cx.listener(move |this, _, window, cx| this.save_meta(op, window, cx))),
        )
        .child(
            Button::new(SharedString::from(format!("{id}-cancel")))
                .debug_selector(move || format!("{id}-cancel"))
                .label("Cancel")
                .small()
                .on_click(cx.listener(|this, _, _, cx| this.cancel_meta_editor(cx))),
        )
        .into_any_element()
}

/// The checklist rows in their own bounded scroll area.
fn rows_area(id: &str, page: &PrDetailPage, rows: Vec<AnyElement>) -> AnyElement {
    div()
        .id(SharedString::from(format!("{id}-rows")))
        .max_h(px(PICKER_ROWS_MAX_H))
        .overflow_y_scroll()
        .track_scroll(&page.picker_scroll)
        .child(v_flex().children(rows))
        .vertical_scrollbar(&page.picker_scroll)
        .into_any_element()
}

/// The shared checklist card (docs/0043 D5, 0022 D5 shape): one clickable
/// row per candidate — the row is the toggle target (its accesskit id is
/// the probe key), the checkbox is display-only.
pub(super) fn checklist(
    page: &PrDetailPage,
    d: &PrDetail,
    id: &'static str,
    draft: &PickerDraft,
    source: PickerSource,
    search: &Entity<InputState>,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    // #197: view-only type-ahead — the query is read live, rows are
    // hidden, draft arrays (and their indices) stay full so the save
    // still writes every checked row, filtered out or not.
    let query = search.read(cx).value().trim().to_string();
    let filtering = !query.is_empty();
    let visible = draft
        .rows
        .iter()
        .filter(|row| picker_row_matches(&query, &row.label))
        .count();
    let search_key = format!("{id}-search");
    let busy = page.busy.is_some();
    let loading = match source {
        PickerSource::Labels => matches!(page.labels_source, PickerFetch::Loading),
        PickerSource::Assignees | PickerSource::Reviewers => {
            matches!(page.users_source, PickerFetch::Loading)
        }
        PickerSource::Milestones => false,
    };
    let failed = match source {
        PickerSource::Labels => page.labels_source.failed_message(),
        PickerSource::Assignees | PickerSource::Reviewers => page.users_source.failed_message(),
        PickerSource::Milestones => None,
    };
    let (op, save_id) = match source {
        PickerSource::Reviewers => (BusyOp::PrSetReviewers, "pr-reviewers-save"),
        PickerSource::Labels => (BusyOp::PrSetLabels, "pr-labels-save"),
        PickerSource::Assignees => (BusyOp::PrSetAssignees, "pr-assignees-save"),
        PickerSource::Milestones => unreachable!("milestone uses milestone_card"),
    };
    let unchanged = draft.is_unchanged(&current_ids(d, source));
    let id_key = format!("{id}-card");
    let rows: Vec<AnyElement> = draft
        .rows
        .iter()
        .enumerate()
        .filter(|(_, row)| picker_row_matches(&query, &row.label))
        .map(|(ix, row)| {
            let checked = draft.checked.get(ix).copied().unwrap_or(false);
            let key = format!("{id}-row-{ix}");
            let probe = key.clone();
            let check = format!("{key}-check");
            h_flex()
                .id(SharedString::from(key))
                .debug_selector(move || probe.clone())
                .gap_2()
                .items_center()
                .px_2()
                .py_1()
                .rounded_sm()
                .hover(|s| s.bg(cx.theme().secondary))
                // docs/0049 D1: picker rows are listbox options — the
                // checkbox mirrors the selected state.
                .role(gpui::accesskit::Role::ListItem)
                .aria_label(row.label.clone())
                .aria_selected(checked)
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_meta_row(ix, cx)),
                )
                .child(
                    Checkbox::new(SharedString::from(check))
                        .checked(checked)
                        .small()
                        .tab_stop(false),
                )
                .when_some(row.avatar_url.clone(), |r, url| {
                    r.child(Avatar::new().src(url).xsmall())
                })
                .child(div().text_sm().child(row.label.clone()))
                .into_any_element()
        })
        .collect();
    v_flex()
        .id(SharedString::from(format!("{id}-card")))
        .debug_selector(move || id_key.clone())
        .gap_1()
        .when(loading, |col| col.child(loading_line(cx)))
        .when_some(failed, |col, msg| col.child(error_line(msg, cx)))
        .child(
            h_flex()
                .id(SharedString::from(format!("{id}-search")))
                .debug_selector(move || search_key.clone())
                .gap_2()
                .items_center()
                .child(Input::new(search).small())
                .when(filtering, |row| {
                    row.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{visible} of {}", draft.rows.len())),
                    )
                }),
        )
        .child(rows_area(id, page, rows))
        .when(filtering && visible == 0, |col| {
            col.child(empty_line("pr-picker-empty", "No matches", cx))
        })
        .child(actions(id, save_id, op, busy, unchanged, cx))
        .into_any_element()
}

/// The milestone picker (0022 D5 shape): single-select rows — clicking a
/// row chooses it, "No milestone" clears. Save posts `milestoneId` (null
/// clears); Cancel restores. No filter yet (#284), so the first row takes
/// the focus on open (D12).
pub(super) fn milestone_card(
    page: &PrDetailPage,
    d: &PrDetail,
    rows: &[PickerRow],
    chosen: &Option<String>,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    let busy = page.busy.is_some();
    let loading = matches!(page.milestones_source, PickerFetch::Loading);
    let unchanged = milestone_unchanged(chosen.as_deref(), d);
    let id_key = "pr-milestones-card".to_string();
    let rows: Vec<AnyElement> = rows
        .iter()
        .enumerate()
        .map(|(ix, row)| {
            let selected = chosen.as_deref() == Some(row.id.as_str())
                || (chosen.is_none() && row.id.is_empty());
            let key = format!("pr-milestones-row-{ix}");
            let probe = key.clone();
            let check = format!("{key}-check");
            h_flex()
                .id(SharedString::from(key))
                .debug_selector(move || probe.clone())
                .gap_2()
                .items_center()
                .px_2()
                .py_1()
                .rounded_sm()
                .hover(|s| s.bg(cx.theme().secondary))
                .when(ix == 0, |r| {
                    r.track_focus(&page.picker_row_focus).tab_stop(true)
                })
                .role(gpui::accesskit::Role::ListItem)
                .aria_label(row.label.clone())
                .aria_selected(selected)
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.pick_milestone_ix(ix, cx)),
                )
                .child(
                    Checkbox::new(SharedString::from(check))
                        .checked(selected)
                        .small()
                        .tab_stop(false),
                )
                .child(div().text_sm().child(row.label.clone()))
                .into_any_element()
        })
        .collect();
    v_flex()
        .id("pr-milestones-card")
        .debug_selector(move || id_key.clone())
        .gap_1()
        .when(loading, |col| col.child(loading_line(cx)))
        .when_some(page.milestones_source.failed_message(), |col, msg| {
            col.child(error_line(msg, cx))
        })
        .child(rows_area("pr-milestones", page, rows))
        .child(actions(
            "pr-milestones",
            "pr-milestones-save",
            BusyOp::PrSetMilestone,
            busy,
            unchanged,
            cx,
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr_page::rail::test_support::detail;
    use serde_json::json;

    #[test]
    fn current_ids_follow_each_source() {
        let d = detail(json!({}));
        assert_eq!(
            current_ids(&d, PickerSource::Labels),
            vec!["L1".to_string()]
        );
        assert_eq!(
            current_ids(&d, PickerSource::Milestones),
            vec!["M1".to_string()]
        );
        assert_eq!(
            current_ids(&d, PickerSource::Reviewers),
            vec!["U2".to_string()]
        );
        assert_eq!(
            current_ids(&d, PickerSource::Assignees),
            vec!["U1".to_string()]
        );
    }

    #[test]
    fn milestone_unchanged_compares_the_chosen_id() {
        let d = detail(json!({}));
        assert!(milestone_unchanged(Some("M1"), &d));
        assert!(!milestone_unchanged(Some("M2"), &d));
        assert!(!milestone_unchanged(None, &d));
        let none = detail(json!({"milestone": null}));
        assert!(milestone_unchanged(None, &none));
    }
}
