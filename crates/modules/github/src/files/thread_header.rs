//! #795: the review-thread card's header row. One line: where the thread
//! sits ("line 11", "lines 11–12"), its state chip, and on the trailing
//! edge Reply, Resolve or Unresolve, then Open on GitHub. The author and
//! age belong to each comment's own line below, so nothing repeats.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable, IconName, Sizable as _,
    button::{Button, ButtonVariants},
    h_flex,
};

use super::thread_chip;
use crate::gql::PrThread;
use crate::pr_page::PrDetailPage;

/// The card's accessible name (docs/0049 D1): author, span, comment count
/// and resolved state, as before the header lost its author.
pub(super) fn aria_label(author: &str, span: &str, replies: usize, resolved: bool) -> String {
    let header = match (replies > 0, span.is_empty()) {
        (true, false) => format!("{author} · {span} · {} replies", replies + 1),
        (true, true) => format!("{author} · {} replies", replies + 1),
        (false, false) => format!("{author} · {span}"),
        (false, true) => author.to_string(),
    };
    format!(
        "Review thread — {header}{}",
        if resolved { " · resolved" } else { "" }
    )
}

/// Which lines the thread sits on (#211): "line 11", "lines 11–12", or
/// empty for a file-level thread.
pub(super) fn span(t: &PrThread) -> String {
    let end = t
        .line
        .and_then(|l| u32::try_from(l).ok())
        .filter(|l| *l != 0);
    let start = t
        .start_line
        .and_then(|l| u32::try_from(l).ok())
        .filter(|l| *l != 0);
    match (start, end) {
        (Some(s), Some(e)) if s < e => format!("lines {s}\u{2013}{e}"),
        (_, Some(e)) => format!("line {e}"),
        _ => String::new(),
    }
}

/// The visible location text: the span, plus the comment count once the
/// thread has replies; a file-level thread has no span.
pub(super) fn location(span: &str, comments: usize) -> String {
    let place = if span.is_empty() {
        "File comment".to_string()
    } else {
        span.to_string()
    };
    if comments > 1 {
        format!("{place} · {comments} comments")
    } else {
        place
    }
}

pub(super) fn row(
    page: &PrDetailPage,
    page_h: &Entity<PrDetailPage>,
    window: &mut Window,
    ix: usize,
    t: &PrThread,
    cx: &App,
) -> AnyElement {
    let span = span(t);
    let permalink = t.comments.first().and_then(|c| c.url.clone());
    h_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .items_center()
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(location(&span, t.comments.len())),
        )
        .child(thread_chip(t))
        .child(div().flex_1())
        // 0030 D4/D5: reply and resolve/unresolve — schema-gated
        // (viewerCanReply / viewerCanResolve / viewerCanUnresolve).
        .when(t.viewer_can_reply, |row| {
            row.child(
                Button::new(SharedString::from(format!("pr-thread-{ix}-reply")))
                    .ghost()
                    .xsmall()
                    .label("Reply")
                    .disabled(page.busy.is_some())
                    .on_click({
                        let thread_id = t.id.clone();
                        window.listener_for(page_h, move |this, _, window, cx| {
                            this.open_reply(thread_id.clone(), window, cx)
                        })
                    }),
            )
        })
        .when(t.viewer_can_resolve && !t.is_resolved, |row| {
            row.child(
                Button::new(SharedString::from(format!("pr-thread-{ix}-resolve")))
                    .ghost()
                    .xsmall()
                    .label("Resolve")
                    .disabled(page.busy.is_some())
                    .on_click({
                        let thread_id = t.id.clone();
                        window.listener_for(page_h, move |this, _, window, cx| {
                            this.open_resolve_picker(thread_id.clone(), window, cx)
                        })
                    }),
            )
        })
        .when(t.viewer_can_unresolve && t.is_resolved, |row| {
            row.child(
                Button::new(SharedString::from(format!("pr-thread-{ix}-unresolve")))
                    .ghost()
                    .xsmall()
                    .label("Unresolve")
                    .disabled(page.busy.is_some())
                    .on_click({
                        let thread_id = t.id.clone();
                        window.listener_for(page_h, move |this, _, window, cx| {
                            this.set_thread_resolved(thread_id.clone(), false, None, window, cx)
                        })
                    }),
            )
        })
        // The link rides the trailing edge, after the actions.
        .when_some(permalink, |row, url| {
            row.child(
                Button::new(SharedString::from(format!("pr-thread-{ix}-open")))
                    .ghost()
                    .xsmall()
                    .icon(IconName::ExternalLink)
                    .tooltip("Open on GitHub")
                    .accessibility_label("Open thread on GitHub")
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{aria_label, location};

    #[test]
    fn the_label_keeps_the_author_the_header_drops() {
        assert_eq!(
            aria_label("a", "line 2", 0, false),
            "Review thread — a · line 2"
        );
        assert_eq!(
            aria_label("a", "lines 1\u{2013}2", 2, true),
            "Review thread — a · lines 1\u{2013}2 · 3 replies · resolved"
        );
    }

    #[test]
    fn the_location_never_repeats_the_author() {
        assert_eq!(location("line 11", 1), "line 11");
        assert_eq!(location("line 11", 3), "line 11 · 3 comments");
        assert_eq!(location("", 1), "File comment");
    }
}
