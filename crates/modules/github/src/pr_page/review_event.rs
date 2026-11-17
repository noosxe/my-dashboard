//! #786: a review with no text is a timeline event, not a card — one
//! line with the verdict's glyph, "reviewer-a approved" and the age. Only
//! a review with a body (or reactions to carry) keeps the full card.

use gpui::prelude::*;
use gpui::{AnyElement, App, FontWeight, SharedString, div};
use gpui_component::{ActiveTheme as _, Sizable as _, h_flex};

use super::review_state_label;
use super::state_pill::review_look;
use crate::gql::PrReview;
use crate::state_pill::{Tone, icon};

/// True when the review has nothing a card would show: no text and no
/// reactions.
pub(super) fn is_event(r: &PrReview) -> bool {
    r.body.as_deref().is_none_or(|b| b.trim().is_empty())
        && r.reaction_groups.iter().all(|g| g.total == 0)
}

/// The verb after the reviewer's login.
pub(super) fn verb(state: Option<&str>) -> &'static str {
    match state {
        Some("APPROVED") => "approved",
        Some("CHANGES_REQUESTED") => "requested changes",
        Some("COMMENTED") => "reviewed",
        Some("DISMISSED") => "review dismissed",
        _ => "reviewed",
    }
}

pub(super) fn event_line(
    id: &str,
    login: &str,
    state: Option<&str>,
    age: String,
    trailing: Option<AnyElement>,
    anchored: bool,
    cx: &App,
) -> AnyElement {
    let (tone, glyph) = review_look(review_state_label(state).1);
    let color = match tone {
        Tone::Success => cx.theme().success,
        Tone::Danger => cx.theme().danger,
        _ => cx.theme().muted_foreground,
    };
    h_flex()
        .id(SharedString::from(id.to_string()))
        .w_full()
        .gap_2()
        .items_center()
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(if anchored {
            cx.theme().accent
        } else {
            gpui::transparent_black()
        })
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(icon(glyph).xsmall().text_color(color))
        .child(
            h_flex()
                .gap_1()
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(cx.theme().foreground)
                        .child(login.to_string()),
                )
                .child(verb(state)),
        )
        .children(trailing)
        .child(div().ml_auto().text_xs().child(age))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review(body: Option<&str>) -> PrReview {
        PrReview {
            author: None,
            state: Some("APPROVED".into()),
            body: body.map(str::to_string),
            submitted_at: None,
            id: "R".into(),
            viewer_can_update: false,
            viewer_can_delete: false,
            updated_at: None,
            viewer_can_react: false,
            reaction_groups: vec![],
        }
    }

    #[test]
    fn only_text_or_reactions_keep_the_card() {
        assert!(is_event(&review(None)));
        assert!(is_event(&review(Some(""))));
        assert!(is_event(&review(Some("  \n"))));
        assert!(!is_event(&review(Some("Looks good"))));
    }

    #[test]
    fn verbs_read_after_the_login() {
        assert_eq!(verb(Some("APPROVED")), "approved");
        assert_eq!(verb(Some("CHANGES_REQUESTED")), "requested changes");
        assert_eq!(verb(Some("COMMENTED")), "reviewed");
        assert_eq!(verb(None), "reviewed");
    }
}
