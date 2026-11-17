//! #791: a failed read of the PR page's data (detail, changed files) says
//! what went wrong in plain words and offers Retry in place. The raw
//! status stays in the app's logs, as for the check log (#815).

use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement, SharedString, Styled,
    Window, div,
};
use gpui_component::button::Button;
use gpui_component::{ActiveTheme as _, Sizable as _, v_flex};

use crate::service::{GithubState, Kind};

/// The copy for a non-200 read. `what` names the thing, e.g. "this pull
/// request". A paused lane keeps its own note (rate limit with a clock).
pub(crate) fn http_copy(state: &GithubState, kind: Kind, status: u16, what: &str) -> String {
    if let Some(note) = state.paused_note(kind.lane()) {
        return note;
    }
    log::warn!("{}: HTTP {status}", kind.kind_name());
    plain_http_copy(status, what)
}

fn plain_http_copy(status: u16, what: &str) -> String {
    match status {
        404 | 410 => {
            format!(
                "Couldn't find {what}. It may have been removed, or you may no longer have access."
            )
        }
        401 => "GitHub didn't accept your sign-in. Sign in again, then retry.".into(),
        403 => format!(
            "GitHub refused access to {what}. Your token may lack permission, or you may have hit a rate limit."
        ),
        429 => "GitHub is limiting requests right now. Retry in a few minutes.".into(),
        500.. => format!("GitHub had a problem loading {what}. Retry in a moment."),
        _ => format!("GitHub couldn't load {what}. Retry, or open it on GitHub."),
    }
}

pub(crate) fn network_copy(detail: &str) -> String {
    log::warn!("pr-page: network: {detail}");
    "Couldn't reach GitHub. Check your connection, then retry.".into()
}

/// An empty or unparseable body.
pub(crate) fn unreadable_copy(detail: &str) -> String {
    log::warn!("pr-page: unreadable response: {detail}");
    "GitHub sent a response this app couldn't read. Retry, or open it on GitHub.".into()
}

/// The pane's error state: the message over a Retry button. `id` is the
/// pane's element id; the button is `{id}-retry`.
pub(super) fn error_cell(
    id: &'static str,
    message: &str,
    on_retry: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    v_flex()
        .id(id)
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap_3()
        .text_sm()
        .text_color(cx.theme().danger)
        .child(
            div()
                .max_w(gpui::px(480.))
                .text_center()
                .child(message.to_string()),
        )
        .child(
            Button::new(SharedString::from(format!("{id}-retry")))
                .outline()
                .small()
                .label("Retry")
                .on_click(move |_, window, cx| on_retry(window, cx)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_never_shows_a_raw_status() {
        for status in [400, 401, 403, 404, 410, 418, 429, 500, 502] {
            let copy = plain_http_copy(status, "this pull request");
            assert!(
                !copy.contains("HTTP") && !copy.contains("answered"),
                "{copy}"
            );
        }
    }

    #[test]
    fn a_missing_pr_says_why_it_might_be() {
        let copy = plain_http_copy(404, "this pull request");
        assert!(
            copy.contains("removed") && copy.contains("access"),
            "{copy}"
        );
    }

    #[test]
    fn transport_failures_hide_their_detail() {
        assert!(!network_copy("dns error").contains("dns"));
        assert!(!unreadable_copy("line 1").contains("line 1"));
    }
}
