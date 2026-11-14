//! The Conversation tab's right rail (docs/0083 D1): a pinned status
//! block (verdict, gate rows, action row) and a scrolling metadata region
//! in one 288 px column.

mod action_row;
mod actions;
mod gate_row;
mod gates;
mod glyph;
mod labels;
mod metrics;
mod milestone;
mod people;
mod picker_card;
mod review_jump;
mod reviewer_row;
mod reviewers;
mod section;
mod status;
#[cfg(test)]
mod status_tests;
mod terminal;
#[cfg(test)]
mod tests;
mod update_branch;
mod verdict;

pub(super) use picker_card::{after_open, current_ids, milestone_unchanged};
pub(super) use section::Section;

use gpui::{
    AnyElement, Context, FocusHandle, InteractiveElement as _, IntoElement as _,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, Window, div,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable as _, h_flex, scroll::ScrollableElement as _, v_flex,
};

use super::{ErrorScope, PrDetailPage, PrMetaEditor, PrStateTone, pr_state_tone};
use crate::gql::PrDetail;

/// "+N more" expansions (D8, D9): page state, reset with the page when
/// another PR opens.
#[derive(Debug, Default)]
pub(super) struct Expanded {
    pub reviewers: bool,
    pub assignees: bool,
    pub labels: bool,
}

/// Where focus goes back to when a picker closes (docs/0083 D12): each
/// section's Edit button carries a handle, and closing a picker queues
/// the section; the next render applies it, once the button is back.
pub(super) struct EditFocus {
    handles: [FocusHandle; 4],
    restore: Option<Section>,
}

impl EditFocus {
    pub(super) fn new(cx: &mut Context<PrDetailPage>) -> Self {
        Self {
            handles: [
                cx.focus_handle(),
                cx.focus_handle(),
                cx.focus_handle(),
                cx.focus_handle(),
            ],
            restore: None,
        }
    }

    pub(super) fn handle(&self, section: Section) -> &FocusHandle {
        &self.handles[section.index()]
    }
}

impl PrDetailPage {
    /// Close the open picker and queue focus for its Edit button.
    pub(super) fn close_meta_editor(&mut self) {
        let section = match self.meta_edit.take() {
            Some(PrMetaEditor::Reviewers(_)) => Section::Reviewers,
            Some(PrMetaEditor::Assignees(_)) => Section::Assignees,
            Some(PrMetaEditor::Labels(_)) => Section::Labels,
            Some(PrMetaEditor::Milestone { .. }) => Section::Milestone,
            None => return,
        };
        self.rail_focus.restore = Some(section);
    }

    /// Applied at the top of render, so the Edit button that returns
    /// with the section is the one that takes focus.
    pub(super) fn apply_rail_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(section) = self.rail_focus.restore.take() {
            self.rail_focus.handle(section).focus(window, cx);
        }
    }
}

/// What every section's Edit rule needs from the page (D7).
pub(super) struct Ctx {
    /// The PR is open (or a draft): terminal and unknown states edit
    /// nothing.
    pub editable: bool,
    /// Merged or closed: empty sections are left out (D6, D10).
    pub terminal: bool,
    /// A write is pending, or an editor or picker is open (D7, #756).
    pub blocked: bool,
}

fn ctx(page: &PrDetailPage, d: &PrDetail, cx: &Context<PrDetailPage>) -> Ctx {
    let tone = pr_state_tone(d.state.as_deref(), d.draft);
    Ctx {
        editable: matches!(tone, PrStateTone::Open | PrStateTone::Draft),
        terminal: matches!(tone, PrStateTone::Merged | PrStateTone::Closed),
        blocked: page.writes_busy(cx) || page.any_meta_editor_open(),
    }
}

/// The rail: `h_full`, status region pinned, metadata region scrolling.
pub(super) fn render_rail(
    page: &PrDetailPage,
    d: &PrDetail,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    v_flex()
        .id("pr-rail")
        .w_72()
        .flex_none()
        .h_full()
        .min_h_0()
        .justify_start()
        .gap_3()
        .child(status::status_block(page, d, cx))
        .children(rail_error(page, cx))
        .child(metadata(page, d, cx))
        .into_any_element()
}

/// The one rail error slot (docs/0043 D2, 0083 D13): pinned under the
/// status region, above the scroll region, so a failed metadata write is
/// visible however far the metadata is scrolled.
fn rail_error(page: &PrDetailPage, cx: &Context<PrDetailPage>) -> Option<AnyElement> {
    page.mutation_error
        .as_ref()
        .filter(|e| e.scope == ErrorScope::Rail)
        .map(|e| {
            h_flex()
                .id("pr-rail-error")
                .flex_none()
                .gap_1()
                .items_start()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(Icon::new(IconName::CircleX).small())
                .child(div().min_w_0().child(e.message.clone()))
                .into_any_element()
        })
}

/// The scrolling region: the four sections in D7's order, 16 px apart,
/// no boxes and no hairlines.
fn metadata(page: &PrDetailPage, d: &PrDetail, cx: &Context<PrDetailPage>) -> AnyElement {
    let ctx = ctx(page, d, cx);
    // The sections are the scroll container's direct children, in this
    // order: `Section::index` addresses them for scroll-into-view.
    let sections = [
        people::reviewers(page, d, &ctx, cx),
        people::assignees(page, d, &ctx, cx),
        labels::labels(page, d, &ctx, cx),
        milestone::milestone(page, d, &ctx, cx),
    ];
    div()
        .flex_1()
        .min_h_0()
        .relative()
        .child(
            v_flex()
                .id("pr-rail-meta")
                .debug_selector(|| "pr-rail-meta".to_string())
                .role(gpui::accesskit::Role::Group)
                .aria_label("Pull request details")
                .size_full()
                // Flush with the status column's left edge; the right
                // inset keeps the Edit buttons clear of the scrollbar.
                .pr_3()
                .gap_4()
                .overflow_y_scroll()
                .track_scroll(&page.rail_scroll)
                .children(sections.into_iter().flatten())
                .vertical_scrollbar(&page.rail_scroll),
        )
        .into_any_element()
}

#[cfg(test)]
pub(super) mod test_support {
    use crate::gql::{self, PrDetail};
    use serde_json::{Value, json};

    /// A PR detail built through the real parser: an open PR, an admin
    /// viewer, one label, one assignee, one user review request and a
    /// milestone. `patch` overrides pull-request fields; `viewerPermission`
    /// rides on the repository wrapper node.
    pub(in crate::pr_page) fn detail(patch: Value) -> PrDetail {
        let mut pr = json!({
            "id": "PR_1", "number": 7, "title": "t", "url": "https://x/pull/7",
            "state": "OPEN", "isDraft": false,
            "createdAt": "2026-09-01T00:00:00Z",
            "viewerCanLabel": true, "viewerCanAssign": true,
            "author": {"login": "octo"},
            "labels": {"totalCount": 1, "nodes": [{"id": "L1", "name": "bug", "color": "ff0000"}]},
            "assignees": {"totalCount": 1, "nodes": [{"id": "U1", "login": "bob", "avatarUrl": "a"}]},
            "milestone": {"id": "M1", "title": "v1", "state": "OPEN"},
            "reviewRequests": {"totalCount": 1, "nodes": [
                {"requestedReviewer": {"__typename": "User", "id": "U2", "login": "carol"}}
            ]},
        });
        let mut permission = json!("ADMIN");
        if let Value::Object(patch) = patch {
            for (k, v) in patch {
                if k == "viewerPermission" {
                    permission = v;
                } else {
                    pr[&k] = v;
                }
            }
        }
        gql::pr_detail_from_value(&json!({"viewerPermission": permission, "pullRequest": pr}))
            .expect("rail test detail parses")
    }
}
