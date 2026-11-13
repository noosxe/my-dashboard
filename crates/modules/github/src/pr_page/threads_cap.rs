//! Cap rows for review threads (#745). The threads query reads the first
//! 100 threads and the first 100 comments of each; what it cut used to
//! vanish without a word. Both rows count the shortfall against the
//! server's `totalCount` and send the reader to github.com.

use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, Styled as _,
};
use gpui_component::{
    ActiveTheme, Sizable as _,
    button::{Button, ButtonVariants},
    h_flex,
};

use super::PrDetailPage;
use crate::gql;

/// Threads the query cut; 0 when everything is loaded.
pub(crate) fn hidden_threads(t: &gql::PrThreads) -> i64 {
    (t.total_count - t.nodes.len() as i64).max(0)
}

/// Comments of one thread the query cut; 0 when everything is loaded.
pub(crate) fn hidden_thread_comments(t: &gql::PrThread) -> i64 {
    (t.comments_total - t.comments.len() as i64).max(0)
}

/// One muted row: "+N more … on github.com", opening `url`.
pub(crate) fn threads_cap_row(
    id: impl Into<SharedString>,
    label: String,
    url: &str,
    cx: &App,
) -> AnyElement {
    let id: SharedString = id.into();
    h_flex()
        .debug_selector({
            let id = id.to_string();
            move || id
        })
        .px_3()
        .justify_center()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(Button::new(id).ghost().small().label(label).on_click({
            let url = url.to_string();
            move |_, _, cx| cx.open_url(&url)
        }))
        .into_any_element()
}

pub(crate) fn threads_row_label(hidden: i64) -> String {
    let noun = if hidden == 1 { "thread" } else { "threads" };
    format!("+{hidden} more review {noun} on github.com")
}

pub(crate) fn comments_row_label(hidden: i64) -> String {
    let noun = if hidden == 1 { "comment" } else { "comments" };
    format!("+{hidden} more {noun} on github.com")
}

/// The pane-level row under the diff when the thread list is capped.
pub(crate) fn pane_cap_row(page: &PrDetailPage, cx: &Context<PrDetailPage>) -> Option<AnyElement> {
    let hidden = hidden_threads(page.threads.data()?);
    let url = page.detail(cx)?.url.clone();
    (hidden > 0).then(|| threads_cap_row("pr-threads-more", threads_row_label(hidden), &url, cx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_threads_counts_the_shortfall() {
        // Before the fix `total_count` was never read.
        let t = gql::PrThreads {
            total_count: 130,
            nodes: Vec::new(),
        };
        assert_eq!(hidden_threads(&t), 130);
        assert_eq!(hidden_threads(&gql::PrThreads::default()), 0);
    }

    #[test]
    fn labels_agree_with_the_count() {
        assert_eq!(threads_row_label(1), "+1 more review thread on github.com");
        assert_eq!(comments_row_label(30), "+30 more comments on github.com");
    }

    #[test]
    fn the_thread_comment_total_is_parsed() {
        let repo = serde_json::json!({"pullRequest": {"reviewThreads": {
            "totalCount": 1,
            "nodes": [{"id": "T", "path": "a.rs", "comments": {
                "totalCount": 130,
                "nodes": [{"id": "C", "body": "b"}]}}]}}});
        let t = gql::pr_threads_from_value(&repo).unwrap();
        assert_eq!(hidden_thread_comments(&t.nodes[0]), 129);
    }
}
