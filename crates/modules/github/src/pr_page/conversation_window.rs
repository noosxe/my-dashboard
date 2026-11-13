//! Which part of the conversation the timeline shows (#742, #746).
//!
//! The detail query reads the newest 100 comments and the newest 100
//! reviews (`last: 100`, still ascending), so a long PR hides its
//! *oldest* items: the cap row sits at the top and reads as older. The
//! viewer's own PENDING review (one started on github.com, never by this
//! page, whose review flow submits in one batch) is not a timeline entry:
//! it has no submit time, no age, and Edit/Delete would act on a draft
//! this page does not own. A quiet notice at the composer points to
//! github.com instead.

use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, accesskit, div,
};
use gpui_component::{
    ActiveTheme, Sizable as _,
    button::{Button, ButtonVariants},
    h_flex,
};

use super::{PrDetailPage, TimelineKind, timeline_order};
use crate::gql;

/// `PullRequestReviewState` of a review its author has not submitted.
const PENDING: &str = "PENDING";

pub(super) fn is_pending_review(r: &gql::PrReview) -> bool {
    r.state.as_deref() == Some(PENDING)
}

/// The timeline's entries in order, without pending reviews. Pure.
pub(super) fn visible_timeline(
    comments: &[gql::PrComment],
    reviews: &[gql::PrReview],
) -> Vec<(TimelineKind, usize)> {
    timeline_order(comments, reviews)
        .into_iter()
        .filter(|(kind, ix)| *kind != TimelineKind::Review || !is_pending_review(&reviews[*ix]))
        .collect()
}

/// Whether the loaded reviews include the viewer's pending one.
pub(super) fn has_pending_review(reviews: &[gql::PrReview]) -> bool {
    reviews.iter().any(is_pending_review)
}

/// Comments and reviews the query cut. The two sets are disjoint, so the
/// sum is the honest count (docs/0021 D3).
pub(super) fn hidden_older(d: &gql::PrDetail) -> i64 {
    (d.comments.total_count - d.comments.nodes.len() as i64).max(0)
        + (d.reviews.total_count - d.reviews.nodes.len() as i64).max(0)
}

/// The Conversation tab's count: every comment and review the server
/// counts, minus the pending review the timeline leaves out (#746).
pub(super) fn conversation_count(d: &gql::PrDetail) -> i64 {
    d.comments.total_count + d.reviews.total_count
        - d.reviews
            .nodes
            .iter()
            .filter(|r| is_pending_review(r))
            .count() as i64
}

/// The cap row at the top of the timeline: what the query cut is older
/// than everything shown below it.
pub(super) fn older_cap_row(missing: i64, url: &str, cx: &App) -> AnyElement {
    h_flex()
        .debug_selector(|| "pr-comments-more".to_string())
        .justify_center()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(
            Button::new("pr-comments-more")
                .ghost()
                .small()
                .label(format!("{missing} older on github.com"))
                .on_click({
                    let url = url.to_string();
                    move |_, _, cx| cx.open_url(&url)
                }),
        )
        .into_any_element()
}

impl PrDetailPage {
    /// The pending-review notice above the composer; `None` without one.
    pub(super) fn render_pending_review_notice(
        &self,
        d: &gql::PrDetail,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if !has_pending_review(&d.reviews.nodes) {
            return None;
        }
        let url = d.url.clone();
        Some(
            h_flex()
                .id("pr-pending-review")
                .debug_selector(|| "pr-pending-review".to_string())
                .role(accesskit::Role::Group)
                .aria_label("You have a pending review on GitHub")
                .gap_2()
                .items_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(div().child("You have a pending review on GitHub"))
                .child(
                    Button::new("pr-pending-review-open")
                        .ghost()
                        .small()
                        .label("Open on GitHub")
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review(id: &str, state: &str, at: Option<&str>) -> gql::PrReview {
        gql::PrReview {
            author: None,
            state: Some(state.into()),
            body: None,
            submitted_at: at.map(str::to_string),
            id: id.into(),
            viewer_can_update: true,
            viewer_can_delete: true,
            updated_at: None,
            viewer_can_react: false,
            reaction_groups: Vec::new(),
        }
    }

    #[test]
    fn a_pending_review_is_not_a_timeline_entry() {
        // Before the fix a null `submittedAt` sorted to the very top.
        let reviews = vec![
            review("done", "APPROVED", Some("2026-08-02T00:00:00Z")),
            review("draft", "PENDING", None),
        ];
        let order = visible_timeline(&[], &reviews);
        assert_eq!(order, vec![(TimelineKind::Review, 0)]);
        assert!(has_pending_review(&reviews));
    }

    #[test]
    fn without_a_pending_review_nothing_is_filtered() {
        let reviews = vec![review("done", "COMMENTED", Some("2026-08-02T00:00:00Z"))];
        assert_eq!(visible_timeline(&[], &reviews).len(), 1);
        assert!(!has_pending_review(&reviews));
    }

    #[test]
    fn the_detail_query_reads_the_newest_hundred() {
        // `first: 100` counts from the oldest and cut the newest items.
        assert!(gql::PR_DETAIL_QUERY.contains("comments(last: 100)"));
        assert!(gql::PR_DETAIL_QUERY.contains("reviews(last: 100)"));
    }

    #[test]
    fn the_cap_counts_both_shortfalls() {
        let repo = serde_json::json!({"pullRequest": {"id": "PR", "number": 7,
            "title": "t", "url": "u",
            "comments": {"totalCount": 130, "nodes": []},
            "reviews": {"totalCount": 3, "nodes": []}}});
        let d = gql::pr_detail_from_value(&repo).unwrap();
        assert_eq!(hidden_older(&d), 133);
        assert_eq!(conversation_count(&d), 133);
    }
}
