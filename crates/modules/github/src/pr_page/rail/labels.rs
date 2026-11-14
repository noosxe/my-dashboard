//! Labels (docs/0083 D9): the pills hug and wrap in one container (#799),
//! capped at 8 with an inline "+N more". The pill builder stays
//! `page::label_pill` (the docs/0002 D5 data-colour exception).

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};

use super::section::{
    EditState, Reason, Section, Spec, capped, edit_state, empty_line, finish, more_button,
    open_picker, truncated,
};
use super::{Ctx, picker_card};
use crate::gql::PrDetail;
use crate::pickers::PickerSource;
use crate::pr_page::{PrDetailPage, PrMetaEditor, cap_row};

const LABELS: Spec = Spec {
    section: Section::Labels,
    id: "pr-labels-panel",
    why_id: "pr-labels-why",
    edit_id: "pr-labels-edit",
    edit_label: "Edit labels",
    on_edit: PrDetailPage::start_labels_picker,
};

pub(super) fn labels_guard(d: &PrDetail) -> Option<Reason> {
    if d.labels.iter().any(|l| l.id.is_none()) {
        return Some(Reason::Unreadable);
    }
    truncated("labels", d.labels.len(), d.labels_total)
}

pub(super) fn labels_edit(d: &PrDetail, ctx: &Ctx) -> EditState {
    edit_state(
        ctx.editable,
        d.viewer_can_label,
        labels_guard(d),
        ctx.blocked,
    )
}

pub(super) fn labels(
    page: &PrDetailPage,
    d: &PrDetail,
    ctx: &Ctx,
    cx: &Context<PrDetailPage>,
) -> Option<AnyElement> {
    if let Some(PrMetaEditor::Labels(draft)) = &page.meta_edit {
        let card = picker_card::checklist(
            page,
            d,
            "pr-labels",
            draft,
            PickerSource::Labels,
            &page.labels_search,
            cx,
        );
        return Some(open_picker(&LABELS, card, cx));
    }
    if ctx.terminal && d.labels.is_empty() {
        return None;
    }
    let state = labels_edit(d, ctx);
    let loaded = d.labels.len();
    let cap = capped(loaded, d.labels_total, page.rail_expanded.labels);
    let surface = cx.theme().tokens.group_box.into();
    let body: AnyElement = if loaded == 0 {
        empty_line("pr-labels-empty", "None", cx)
    } else {
        // One wrap container that holds the pills themselves (#799).
        let names: Vec<&str> = d
            .labels
            .iter()
            .take(cap.shown)
            .map(|l| l.name.as_str())
            .collect();
        let mut pills = h_flex()
            .id("pr-labels")
            .role(gpui::accesskit::Role::Label)
            .aria_label(names.join(", "))
            .flex_wrap()
            .gap_1();
        for l in d.labels.iter().take(cap.shown) {
            pills = pills.child(crate::page::label_pill(
                &l.name,
                l.color.as_deref(),
                surface,
                cx,
            ));
        }
        if cap.more > 0 {
            pills = pills.child(more_button(
                "pr-labels-more",
                cap.more,
                "labels",
                |this, _, cx| {
                    this.rail_expanded.labels = true;
                    cx.notify();
                },
                cx,
            ));
        }
        let mut col = v_flex().gap_1().child(pills);
        if cap.on_github > 0 {
            col = col.child(cap_row("pr-labels-cap", cap.on_github, &d.url, cx));
        }
        col.into_any_element()
    };
    Some(finish(&LABELS, state, body, page, cx))
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

    fn many(n: usize, total: usize) -> serde_json::Value {
        let nodes: Vec<_> = (0..n)
            .map(|i| json!({"id": format!("L{i}"), "name": format!("l{i}"), "color": "ff0000"}))
            .collect();
        json!({"labels": {"totalCount": total, "nodes": nodes}})
    }

    #[test]
    fn truncated_labels_hide_edit_and_say_how_many_load() {
        let d = detail(many(20, 34));
        assert_eq!(
            labels_edit(&d, &ctx()),
            EditState::Why(Reason::Truncated {
                noun: "labels",
                loaded: 20,
                total: 34
            })
        );
    }

    #[test]
    fn a_label_without_an_id_is_unreadable() {
        let d = detail(json!({"labels": {"totalCount": 1, "nodes": [{"name": "x"}]}}));
        assert_eq!(labels_edit(&d, &ctx()), EditState::Why(Reason::Unreadable));
    }

    #[test]
    fn a_viewer_who_cannot_label_sees_neither_edit_nor_reason() {
        let d = detail(json!({"viewerCanLabel": false, "labels": {"totalCount": 34, "nodes": []}}));
        assert_eq!(labels_edit(&d, &ctx()), EditState::Hidden);
    }

    #[test]
    fn twenty_of_twenty_labels_cap_at_eight_with_twelve_more() {
        let d = detail(many(20, 20));
        let cap = capped(d.labels.len(), d.labels_total, false);
        assert_eq!((cap.shown, cap.more, cap.on_github), (8, 12, 0));
    }

    #[test]
    fn labels_edit_shows_when_complete_and_idle() {
        let d = detail(many(3, 3));
        assert_eq!(labels_edit(&d, &ctx()), EditState::Show);
    }
}
