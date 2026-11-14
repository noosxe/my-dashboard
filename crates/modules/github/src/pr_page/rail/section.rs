//! The shape every metadata section shares (docs/0083 D7): a quiet title
//! row with an icon-only Edit, a body, an optional reason line, and the
//! pure rules behind them — when Edit shows, why it is missing, and how
//! many rows a collapsed section keeps.

use gpui::{
    AnyElement, App, Context, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_component::{
    ActiveTheme, Icon, Sizable as _,
    button::{Button, ButtonVariants},
    h_flex, v_flex,
};

use super::metrics::EMPTY_ROW_H;
use crate::pr_page::PrDetailPage;

/// The vendored pencil glyph (octicon `pencil-16`), served by the app's
/// asset source like `git-branch.svg`.
pub(super) const PENCIL: &str = "icons/pencil.svg";

/// Rows (or pills) a collapsed section shows before "+N more" (D8, D9).
pub(super) const COLLAPSED_CAP: usize = 8;

/// The four sections, in rail order (D7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Section {
    Reviewers,
    Assignees,
    Labels,
    Milestone,
}

impl Section {
    pub(super) fn title(self) -> &'static str {
        match self {
            Section::Reviewers => "Reviewers",
            Section::Assignees => "Assignees",
            Section::Labels => "Labels",
            Section::Milestone => "Milestone",
        }
    }

    /// The scroll container's child index — the order of D7, stable
    /// because an editor only opens on an open PR, where all four render.
    pub(super) fn index(self) -> usize {
        match self {
            Section::Reviewers => 0,
            Section::Assignees => 1,
            Section::Labels => 2,
            Section::Milestone => 3,
        }
    }
}

/// Why a section's Edit is missing although the viewer could use it (D7,
/// #754): a data guard of docs/0043 D3 fired, so the way out is GitHub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Reason {
    /// The query loaded `loaded` of `total` entries.
    Truncated {
        noun: &'static str,
        loaded: usize,
        total: i64,
    },
    /// A team, bot or other non-user review request is pending.
    TeamRequested,
    /// An entry came back without the id a write needs.
    Unreadable,
}

impl Reason {
    pub(super) fn text(&self) -> String {
        match self {
            Reason::Truncated {
                noun,
                loaded,
                total,
            } => format!("Edit on GitHub: this page loads {loaded} of {total} {noun}"),
            Reason::TeamRequested => "Edit on GitHub: a team review is requested".to_string(),
            Reason::Unreadable => "Edit on GitHub: an entry couldn't be read".to_string(),
        }
    }
}

/// What a section's title row shows in place of an Edit button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum EditState {
    Show,
    /// Hidden silently: permission, terminal PR, or a transient block.
    Hidden,
    /// Hidden with a reason line under the body.
    Why(Reason),
}

/// D7's visibility rule, one place. The reason line is only for a data
/// guard — and only when the viewer could otherwise edit an open PR; a
/// permission, a terminal state or a busy write stays silent.
pub(super) fn edit_state(
    editable: bool,
    viewer_gate: bool,
    guard: Option<Reason>,
    blocked: bool,
) -> EditState {
    if !editable || !viewer_gate {
        return EditState::Hidden;
    }
    if let Some(reason) = guard {
        return EditState::Why(reason);
    }
    if blocked {
        EditState::Hidden
    } else {
        EditState::Show
    }
}

/// The truncation guard shared by labels, assignees and reviewers.
pub(super) fn truncated(noun: &'static str, loaded: usize, total: i64) -> Option<Reason> {
    (loaded as i64 != total).then_some(Reason::Truncated {
        noun,
        loaded,
        total,
    })
}

/// How a capped list renders: how many entries show, what "+N more"
/// reveals, and how many live only on GitHub.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Capped {
    pub shown: usize,
    /// Loaded entries hidden by the collapsed cap; 0 once expanded.
    pub more: usize,
    /// `totalCount` beyond what the query loaded; shown as the shared
    /// `cap_row` once nothing else is hidden (D8).
    pub on_github: i64,
}

pub(super) fn capped(loaded: usize, total: i64, expanded: bool) -> Capped {
    let more = if expanded {
        0
    } else {
        loaded.saturating_sub(COLLAPSED_CAP)
    };
    let missing = (total - loaded as i64).max(0);
    Capped {
        shown: loaded - more,
        more,
        on_github: if more == 0 { missing } else { 0 },
    }
}

/// The "+N more" button's visible label and its a11y name (D14).
pub(super) fn more_label(hidden: usize) -> String {
    format!("+{hidden} more")
}

pub(super) fn more_a11y(hidden: usize, noun: &str) -> String {
    format!("Show {hidden} more {noun}")
}

/// The icon-only Edit button (D7): its tooltip and a11y name both name
/// the target (#768).
pub(super) fn edit_button(
    id: &'static str,
    label: &'static str,
    on_edit: OnEdit,
    focus: &FocusHandle,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    Button::new(id)
        .track_focus(focus)
        // 0019 §8 lesson: probes see selector keys only.
        .debug_selector(move || id.to_string())
        .ghost()
        .xsmall()
        .icon(Icon::empty().path(PENCIL))
        .tooltip(label)
        .accessibility_label(label)
        .on_click(cx.listener(move |this, _, window, cx| on_edit(this, window, cx)))
        .into_any_element()
}

/// The ghost "+N more" button (D8, D9), muted and `text_xs`.
pub(super) fn more_button(
    id: &'static str,
    hidden: usize,
    noun: &'static str,
    on_click: impl Fn(&mut PrDetailPage, &mut Window, &mut Context<PrDetailPage>) + 'static,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(
            Button::new(id)
                .debug_selector(move || id.to_string())
                .ghost()
                .xsmall()
                .label(more_label(hidden))
                .accessibility_label(more_a11y(hidden, noun))
                .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx))),
        )
        .into_any_element()
}

/// One muted `text_sm` line, left-aligned, 24 px high (D11).
pub(super) fn empty_line(id: &'static str, text: &'static str, cx: &App) -> AnyElement {
    h_flex()
        .id(id)
        .role(gpui::accesskit::Role::Label)
        .aria_label(text)
        .h(px(EMPTY_ROW_H))
        .items_center()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}

/// The reason line under a section whose Edit a data guard hid (D7).
pub(super) fn reason_line(id: &'static str, reason: &Reason, cx: &App) -> AnyElement {
    let text = reason.text();
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        // D14: a content node named by its text.
        .role(gpui::accesskit::Role::Label)
        .aria_label(text.clone())
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}

/// The signature of the callback an Edit button runs.
pub(super) type OnEdit = fn(&mut PrDetailPage, &mut Window, &mut Context<PrDetailPage>);

/// What one section is called on the page: panel id, reason-line id and
/// its Edit button (D14).
pub(super) struct Spec {
    pub section: Section,
    pub id: &'static str,
    pub why_id: &'static str,
    pub edit_id: &'static str,
    pub edit_label: &'static str,
    pub on_edit: OnEdit,
}

/// The section while its picker is open: the card replaces the body, the
/// title row stays without its Edit (D12).
pub(super) fn open_picker(spec: &Spec, card: AnyElement, cx: &App) -> AnyElement {
    rail_section(spec.id, spec.section, None, card, None, cx)
}

/// Turn an [`EditState`] into the section: the Edit button, or the
/// reason line, or neither.
pub(super) fn finish(
    spec: &Spec,
    state: EditState,
    body: AnyElement,
    page: &PrDetailPage,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    let (button, why) = match state {
        EditState::Show => (
            Some(edit_button(
                spec.edit_id,
                spec.edit_label,
                spec.on_edit,
                page.rail_focus.handle(spec.section),
                cx,
            )),
            None,
        ),
        EditState::Hidden => (None, None),
        EditState::Why(reason) => (None, Some(reason_line(spec.why_id, &reason, cx))),
    };
    rail_section(spec.id, spec.section, button, body, why, cx)
}

/// `rail_section(title, edit, body)` (D7): flat, no box, no hairline —
/// space separates sections. A 20 px title row (muted, `text_xs`,
/// medium), 2 px to the body, 4 px between the body's rows.
pub(super) fn rail_section(
    id: &'static str,
    section: Section,
    edit: Option<AnyElement>,
    body: AnyElement,
    reason: Option<AnyElement>,
    cx: &App,
) -> AnyElement {
    v_flex()
        .id(id)
        .debug_selector(move || id.to_string())
        .gap_0p5()
        .child(
            h_flex()
                .h_5()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(cx.theme().muted_foreground)
                        .child(section.title()),
                )
                .children(edit),
        )
        .child(body)
        .children(reason)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trunc() -> Option<Reason> {
        truncated("labels", 20, 34)
    }

    #[test]
    fn edit_shows_only_when_everything_allows() {
        assert_eq!(edit_state(true, true, None, false), EditState::Show);
    }

    #[test]
    fn edit_is_hidden_on_terminal_prs_even_with_a_data_guard() {
        // Merged/closed (§2): silent, no reason line either.
        assert_eq!(edit_state(false, true, trunc(), false), EditState::Hidden);
        assert_eq!(edit_state(false, true, None, false), EditState::Hidden);
    }

    #[test]
    fn edit_is_hidden_without_the_viewer_gate_and_says_nothing() {
        assert_eq!(edit_state(true, false, trunc(), false), EditState::Hidden);
    }

    #[test]
    fn busy_or_open_editor_hides_edit_silently() {
        assert_eq!(edit_state(true, true, None, true), EditState::Hidden);
    }

    #[test]
    fn a_data_guard_hides_edit_with_a_reason_even_while_busy() {
        // The reason is stable; busy is transient and must not flicker it.
        assert_eq!(
            edit_state(true, true, trunc(), true),
            EditState::Why(Reason::Truncated {
                noun: "labels",
                loaded: 20,
                total: 34
            })
        );
    }

    #[test]
    fn reason_texts_name_the_way_out() {
        assert_eq!(
            trunc().unwrap().text(),
            "Edit on GitHub: this page loads 20 of 34 labels"
        );
        assert_eq!(
            Reason::TeamRequested.text(),
            "Edit on GitHub: a team review is requested"
        );
        assert_eq!(
            Reason::Unreadable.text(),
            "Edit on GitHub: an entry couldn't be read"
        );
    }

    #[test]
    fn a_complete_connection_has_no_truncation_reason() {
        assert_eq!(truncated("labels", 3, 3), None);
    }

    #[test]
    fn cap_keeps_eight_and_counts_the_rest() {
        assert_eq!(
            capped(8, 8, false),
            Capped {
                shown: 8,
                more: 0,
                on_github: 0
            }
        );
        assert_eq!(
            capped(11, 11, false),
            Capped {
                shown: 8,
                more: 3,
                on_github: 0
            }
        );
    }

    #[test]
    fn expanding_shows_everything_loaded_and_ends_with_the_github_row() {
        assert_eq!(
            capped(20, 34, true),
            Capped {
                shown: 20,
                more: 0,
                on_github: 14
            }
        );
    }

    #[test]
    fn collapsed_with_hidden_rows_defers_the_github_row() {
        // "+N more" comes first; the shortfall shows after expanding.
        assert_eq!(
            capped(20, 34, false),
            Capped {
                shown: 8,
                more: 12,
                on_github: 0
            }
        );
    }

    #[test]
    fn a_short_list_with_a_shortfall_shows_the_github_row_at_once() {
        assert_eq!(
            capped(5, 9, false),
            Capped {
                shown: 5,
                more: 0,
                on_github: 4
            }
        );
    }

    #[test]
    fn more_labels() {
        assert_eq!(more_label(12), "+12 more");
        assert_eq!(more_a11y(12, "labels"), "Show 12 more labels");
    }

    #[test]
    fn sections_keep_the_d7_order() {
        let order = [
            Section::Reviewers,
            Section::Assignees,
            Section::Labels,
            Section::Milestone,
        ];
        assert_eq!(
            order.map(Section::index),
            [0, 1, 2, 3],
            "scroll child indices follow D7's order"
        );
        assert_eq!(
            order.map(Section::title),
            ["Reviewers", "Assignees", "Labels", "Milestone"]
        );
    }
}
