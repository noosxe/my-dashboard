//! Reviewers and Assignees (docs/0083 D8): 26 px person rows, the
//! 8-row cap with "+N more", and the Edit rules of docs/0043 D3/D4. The
//! reviewer rows come from the merge in `reviewers.rs`.

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::{Sizable as _, avatar::Avatar, h_flex, tooltip::Tooltip, v_flex};

use super::metrics::ROW_H;
use super::reviewer_row::reviewer_row;
use super::section::{
    EditState, Reason, Section, Spec, capped, edit_state, empty_line, finish, more_button,
    open_picker, truncated,
};
use super::{Ctx, picker_card, reviewers};
use crate::gql::PrDetail;
use crate::pickers::PickerSource;
use crate::pickers::triage_gate;
use crate::pr_page::{PrDetailPage, PrMetaEditor, cap_row};

const REVIEWERS: Spec = Spec {
    section: Section::Reviewers,
    id: "pr-reviewers-panel",
    why_id: "pr-reviewers-why",
    edit_id: "pr-reviewers-edit",
    edit_label: "Edit reviewers",
    on_edit: PrDetailPage::start_reviewers_picker,
};

const ASSIGNEES: Spec = Spec {
    section: Section::Assignees,
    id: "pr-assignees-panel",
    why_id: "pr-assignees-why",
    edit_id: "pr-assignees-edit",
    edit_label: "Edit assignees",
    on_edit: PrDetailPage::start_assignees_picker,
};

/// docs/0043 D4: triage-and-above or the PR's author may request
/// reviews; a PR without a node id has no write target.
fn reviewers_gate(d: &PrDetail, viewer: Option<&str>) -> bool {
    let author = d
        .author
        .as_ref()
        .is_some_and(|a| Some(a.login.as_str()) == viewer);
    (triage_gate(d.viewer_permission.as_deref()) || author) && !d.id.is_empty()
}

/// docs/0043 D3's reviewer clause as a reason: every request must be a
/// user with an id, and the connection must be complete. Review outcomes
/// don't enter: the replace write sends requests only.
pub(super) fn reviewers_guard(d: &PrDetail) -> Option<Reason> {
    if d.review_requests.iter().any(|r| !r.is_user()) {
        return Some(Reason::TeamRequested);
    }
    if d.review_requests.iter().any(|r| r.id.is_none()) {
        return Some(Reason::Unreadable);
    }
    truncated(
        "reviewers",
        d.review_requests.len(),
        d.review_requests_total,
    )
}

pub(super) fn reviewers_edit(d: &PrDetail, viewer: Option<&str>, ctx: &Ctx) -> EditState {
    edit_state(
        ctx.editable,
        reviewers_gate(d, viewer),
        reviewers_guard(d),
        ctx.blocked,
    )
}

pub(super) fn assignees_guard(d: &PrDetail) -> Option<Reason> {
    if d.assignees.iter().any(|a| a.id.is_none()) {
        return Some(Reason::Unreadable);
    }
    truncated("assignees", d.assignees.len(), d.assignees_total)
}

pub(super) fn assignees_edit(d: &PrDetail, ctx: &Ctx) -> EditState {
    edit_state(
        ctx.editable,
        d.viewer_can_assign,
        assignees_guard(d),
        ctx.blocked,
    )
}

/// One person: avatar, then the name, which truncates with the full
/// text in a tooltip.
fn person_row(key: String, name: String, avatar: Option<String>) -> AnyElement {
    let tip = name.clone();
    let avatar = match avatar {
        Some(url) => Avatar::new().src(url),
        None => Avatar::new().name(name.clone()),
    };
    h_flex()
        .id(gpui::SharedString::from(key.clone()))
        .debug_selector(move || key.clone())
        .role(gpui::accesskit::Role::Label)
        .aria_label(name.clone())
        .h(px(ROW_H))
        .gap_2()
        .items_center()
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .child(avatar.xsmall())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_sm()
                .child(name),
        )
        .into_any_element()
}

pub(super) fn reviewers(
    page: &PrDetailPage,
    d: &PrDetail,
    ctx: &Ctx,
    cx: &Context<PrDetailPage>,
) -> Option<AnyElement> {
    if let Some(PrMetaEditor::Reviewers(draft)) = &page.meta_edit {
        let card = picker_card::checklist(
            page,
            d,
            "pr-reviewers",
            draft,
            PickerSource::Reviewers,
            &page.users_search,
            cx,
        );
        return Some(open_picker(&REVIEWERS, card, cx));
    }
    let rows = reviewers::merge(d);
    if ctx.terminal && rows.is_empty() {
        return None;
    }
    let viewer = page.viewer_login(cx);
    let state = reviewers_edit(d, viewer.as_deref(), ctx);
    // Requests beyond the loaded page are known only by count.
    let unloaded = (d.review_requests_total - d.review_requests.len() as i64).max(0);
    let cap = capped(
        rows.len(),
        rows.len() as i64 + unloaded,
        page.rail_expanded.reviewers,
    );
    let mut body = v_flex().gap_1();
    if rows.is_empty() {
        body = body.child(empty_line("pr-reviewers-empty", "None requested", cx));
    }
    for (ix, row) in rows.iter().take(cap.shown).enumerate() {
        body = body.child(reviewer_row(format!("pr-reviewer-{ix}"), row, cx));
    }
    if cap.more > 0 {
        body = body.child(more_button(
            "pr-reviewers-more",
            cap.more,
            "reviewers",
            |this, _, cx| {
                this.rail_expanded.reviewers = true;
                cx.notify();
            },
            cx,
        ));
    }
    if cap.on_github > 0 {
        body = body.child(cap_row("pr-reviewers-cap", cap.on_github, &d.url, cx));
    }
    Some(finish(&REVIEWERS, state, body.into_any_element(), page, cx))
}

pub(super) fn assignees(
    page: &PrDetailPage,
    d: &PrDetail,
    ctx: &Ctx,
    cx: &Context<PrDetailPage>,
) -> Option<AnyElement> {
    if let Some(PrMetaEditor::Assignees(draft)) = &page.meta_edit {
        let card = picker_card::checklist(
            page,
            d,
            "pr-assignees",
            draft,
            PickerSource::Assignees,
            &page.users_search,
            cx,
        );
        return Some(open_picker(&ASSIGNEES, card, cx));
    }
    if ctx.terminal && d.assignees.is_empty() {
        return None;
    }
    let state = assignees_edit(d, ctx);
    let loaded = d.assignees.len();
    let cap = capped(loaded, d.assignees_total, page.rail_expanded.assignees);
    let mut body = v_flex().gap_1();
    if loaded == 0 {
        body = body.child(empty_line("pr-assignees-empty", "None", cx));
    }
    for (ix, a) in d.assignees.iter().take(cap.shown).enumerate() {
        body = body.child(person_row(
            format!("pr-assignee-{ix}"),
            a.login.clone(),
            Some(a.avatar_url.clone()),
        ));
    }
    if cap.more > 0 {
        body = body.child(more_button(
            "pr-assignees-more",
            cap.more,
            "assignees",
            |this, _, cx| {
                this.rail_expanded.assignees = true;
                cx.notify();
            },
            cx,
        ));
    }
    if cap.on_github > 0 {
        body = body.child(cap_row("pr-assignees-cap", cap.on_github, &d.url, cx));
    }
    Some(finish(&ASSIGNEES, state, body.into_any_element(), page, cx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr_page::rail::test_support::detail;
    use serde_json::json;

    fn ctx() -> Ctx {
        Ctx {
            editable: true,
            terminal: false,
            blocked: false,
        }
    }

    #[test]
    fn reviewers_edit_shows_for_a_triage_viewer_with_clean_user_requests() {
        let d = detail(json!({}));
        assert_eq!(reviewers_edit(&d, Some("v"), &ctx()), EditState::Show);
    }

    #[test]
    fn a_team_request_hides_reviewers_edit_with_the_team_reason() {
        let d = detail(json!({"reviewRequests": {"totalCount": 2, "nodes": [
            {"requestedReviewer": {"__typename": "User", "id": "U1", "login": "a"}},
            {"requestedReviewer": {"__typename": "Team", "id": "T1", "name": "core"}}
        ]}}));
        assert_eq!(
            reviewers_edit(&d, Some("v"), &ctx()),
            EditState::Why(Reason::TeamRequested)
        );
    }

    #[test]
    fn a_non_author_without_triage_gets_no_reviewers_edit_and_no_reason() {
        let d = detail(json!({"viewerPermission": "READ", "author": {"login": "someone"}}));
        assert_eq!(reviewers_edit(&d, Some("v"), &ctx()), EditState::Hidden);
    }

    #[test]
    fn assignees_truncation_names_the_counts() {
        let d = detail(json!({"assignees": {"totalCount": 34, "nodes": [
            {"id": "U1", "login": "a", "avatarUrl": "x"}
        ]}}));
        assert_eq!(
            assignees_edit(&d, &ctx()),
            EditState::Why(Reason::Truncated {
                noun: "assignees",
                loaded: 1,
                total: 34
            })
        );
    }

    #[test]
    fn assignees_edit_is_hidden_while_blocked_and_on_terminal_prs() {
        let d = detail(json!({}));
        let busy = Ctx {
            blocked: true,
            ..ctx()
        };
        let done = Ctx {
            editable: false,
            terminal: true,
            blocked: false,
        };
        assert_eq!(assignees_edit(&d, &busy), EditState::Hidden);
        assert_eq!(assignees_edit(&d, &done), EditState::Hidden);
    }
}
