//! Milestone (docs/0083 D10): one 26 px row with the title, a muted
//! "closed" after a closed milestone's title, "None" when empty. On open
//! PRs the section always renders, also for a viewer who can't edit it.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::{ActiveTheme, h_flex};

use super::metrics::ROW_H;
use super::section::{EditState, Section, Spec, edit_state, empty_line, finish, open_picker};
use super::{Ctx, picker_card};
use crate::gql::PrDetail;
use crate::pr_page::{PrDetailPage, PrMetaEditor, pr_milestone_arming};

const MILESTONE: Spec = Spec {
    section: Section::Milestone,
    id: "pr-milestone-panel",
    why_id: "pr-milestone-why",
    edit_id: "pr-milestones-edit",
    edit_label: "Edit milestone",
    on_edit: PrDetailPage::start_milestone_picker,
};

/// docs/0043 D4: triage-and-above with a write target — no data guard
/// applies to a single value.
pub(super) fn milestone_edit(d: &PrDetail, ctx: &Ctx) -> EditState {
    edit_state(ctx.editable, pr_milestone_arming(d), None, ctx.blocked)
}

pub(super) fn milestone(
    page: &PrDetailPage,
    d: &PrDetail,
    ctx: &Ctx,
    cx: &Context<PrDetailPage>,
) -> Option<AnyElement> {
    if let Some(PrMetaEditor::Milestone { rows, chosen }) = &page.meta_edit {
        let card = picker_card::milestone_card(page, d, rows, chosen, cx);
        return Some(open_picker(&MILESTONE, card, cx));
    }
    // Terminal PRs leave an empty section out (D6, D10).
    if ctx.terminal && d.milestone.is_none() {
        return None;
    }
    let body: AnyElement = match d.milestone.as_ref() {
        None => empty_line("pr-milestone-empty", "None", cx),
        Some(m) => h_flex()
            .id("pr-milestone")
            .debug_selector(|| "pr-milestone".to_string())
            .role(gpui::accesskit::Role::Label)
            .aria_label(match m.state.as_deref() {
                Some("CLOSED") => format!("{}, closed", m.title),
                _ => m.title.clone(),
            })
            .h(px(ROW_H))
            .gap_2()
            .items_center()
            .child(div().text_sm().child(m.title.clone()))
            .when(m.state.as_deref() == Some("CLOSED"), |row| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("closed"),
                )
            })
            .into_any_element(),
    };
    Some(finish(&MILESTONE, milestone_edit(d, ctx), body, page, cx))
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
    fn triage_viewers_can_edit_and_others_see_nothing() {
        let d = detail(json!({}));
        assert_eq!(milestone_edit(&d, &ctx()), EditState::Show);
        let d = detail(json!({"viewerPermission": "READ"}));
        assert_eq!(milestone_edit(&d, &ctx()), EditState::Hidden);
    }

    #[test]
    fn edit_hides_while_blocked() {
        let d = detail(json!({}));
        let busy = Ctx {
            blocked: true,
            ..ctx()
        };
        assert_eq!(milestone_edit(&d, &busy), EditState::Hidden);
    }
}
