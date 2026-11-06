//! The check log pane when the chain fails (docs/0039 D5, #815): what went
//! wrong and what to do, in plain words, over a row of normal buttons. The
//! raw status stays in the app's logs.

use gpui::{AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, div};
use gpui_component::button::Button;
use gpui_component::{ActiveTheme, Sizable as _, h_flex, v_flex};

use crate::gql;
use crate::pr_page::PrDetailPage;
use crate::pr_page::checks::identity::CheckTarget;

/// A non-200 from any step of the chain. 404 and 410 both mean the thing is
/// gone or not there yet; only the log step can tell which, and it doesn't.
pub(crate) fn http_copy(status: u16) -> String {
    match status {
        404 | 410 => "This log isn't available. It may have expired, the run may have been \
                      deleted, or the job may not have finished yet."
            .into(),
        401 => "GitHub didn't accept your sign-in. Sign in again, then retry.".into(),
        403 => "GitHub refused access to this log. Your token may not have Actions access, \
                or you may have hit a rate limit."
            .into(),
        429 => "GitHub is limiting requests right now. Retry in a few minutes.".into(),
        500.. => {
            format!("GitHub had a problem serving this log (HTTP {status}). Retry in a moment.")
        }
        _ => format!("GitHub couldn't return this log (HTTP {status})."),
    }
}

pub(crate) fn network_copy(detail: &str) -> String {
    log::warn!("check-log: network: {detail}");
    "Couldn't reach GitHub. Check your connection, then retry.".into()
}

pub(crate) fn malformed_copy(what: &str, detail: &str) -> String {
    log::warn!("check-log: malformed {what}: {detail}");
    "GitHub sent a response this app couldn't read. Retry, or open the check on GitHub.".into()
}

impl PrDetailPage {
    pub(in crate::pr_page) fn render_check_log_failure(
        &self,
        i: usize,
        message: &str,
        check: &gql::PrCheck,
        target: &CheckTarget,
        cx: &Context<Self>,
    ) -> AnyElement {
        let page = cx.entity();
        let target = target.clone();
        let open = check.details_url.clone().map(|url| {
            Button::new(SharedString::from(format!("check-log-full-{i}")))
                .outline()
                .xsmall()
                .label("Open on GitHub")
                .on_click(move |_, _, cx| cx.open_url(&url))
        });
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(message.to_string()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!("check-log-retry-{i}")))
                            .outline()
                            .xsmall()
                            .label("Retry")
                            .on_click(move |_, _, cx| {
                                page.update(cx, |this, cx| this.retry_check_log(&target, cx))
                            }),
                    )
                    .children(open),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_copy_reads_as_a_raw_status() {
        for status in [400, 401, 403, 404, 410, 422, 429, 500, 502] {
            let copy = http_copy(status);
            assert!(!copy.contains("answered"), "{copy}");
        }
    }

    #[test]
    fn a_missing_log_says_why_it_might_be() {
        for status in [404, 410] {
            let copy = http_copy(status);
            assert!(copy.contains("expired"), "{copy}");
            assert!(!copy.contains("HTTP"), "{copy}");
        }
    }

    #[test]
    fn auth_and_limits_name_the_next_step() {
        assert!(http_copy(401).contains("Sign in again"));
        assert!(http_copy(403).contains("Actions access"));
        assert!(http_copy(429).contains("few minutes"));
    }

    #[test]
    fn unexpected_statuses_keep_the_code_for_a_bug_report() {
        assert!(http_copy(503).contains("HTTP 503"));
        assert!(http_copy(418).contains("HTTP 418"));
    }

    #[test]
    fn transport_failures_never_show_their_raw_detail() {
        let net = network_copy("dns error: no such host");
        assert!(net.contains("connection") && !net.contains("dns"));
        let bad = malformed_copy("jobs", "expected value at line 1");
        assert!(bad.contains("couldn't read") && !bad.contains("line 1"));
    }
}
