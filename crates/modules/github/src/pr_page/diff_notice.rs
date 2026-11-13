//! The Files pane's honest rows for a file whose diff can't be drawn
//! (#753). Three reasons, each with its own words and an Open on GitHub
//! link, so the pane is never blank:
//!
//! - **Malformed** — a patch is present but doesn't parse, or parses to
//!   nothing. The parse failure used to log and leave the pane empty
//!   (a11y: "Diff for 0 files").
//! - **Too large** — GitHub omits the patch of a file whose diff is
//!   too big, while the counts say lines changed.
//! - **No text diff** — no patch and no changed lines: a binary file,
//!   an empty file, or a pure rename or mode change. The REST file
//!   object can't tell these apart, so the row doesn't claim one.

use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, accesskit, div,
};
use gpui_component::{
    ActiveTheme, IconName, Sizable as _,
    button::{Button, ButtonVariants},
    diff::DiffFile,
    h_flex, v_flex,
};

use super::PrDetailPage;
use crate::api::PrFile;

/// Why a file's diff isn't drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffNotice {
    Malformed,
    TooLarge,
    NoTextDiff,
}

impl DiffNotice {
    /// The row's headline.
    pub(crate) fn headline(self) -> &'static str {
        match self {
            Self::Malformed => "Couldn't render this diff",
            Self::TooLarge => "This diff is too large to show here",
            Self::NoTextDiff => "No text changes to show",
        }
    }

    /// The row's explanation under the headline.
    pub(crate) fn detail(self) -> &'static str {
        match self {
            Self::Malformed => "The patch GitHub returned for this file could not be read.",
            Self::TooLarge => "GitHub does not send the patch for files with very large diffs.",
            Self::NoTextDiff => {
                "This file is binary or empty, or it was only renamed or had its mode changed."
            }
        }
    }
}

/// Classify a file against its parse result. `None` means the diff draws.
pub(crate) fn classify(
    file: &PrFile,
    parsed: &Result<Vec<DiffFile>, String>,
) -> Option<DiffNotice> {
    let has_patch = file.patch.as_deref().is_some_and(|p| !p.trim().is_empty());
    if !has_patch {
        return Some(if file.changes > 0 {
            DiffNotice::TooLarge
        } else {
            DiffNotice::NoTextDiff
        });
    }
    // Junk before the first hunk parses as an "extended header", so a
    // clean parse alone proves nothing: the patch must open with a hunk.
    let opens_with_hunk = file
        .patch
        .as_deref()
        .is_some_and(|p| p.trim_start().starts_with("@@"));
    match parsed {
        Ok(docs) if docs.len() == 1 && opens_with_hunk => None,
        _ => Some(DiffNotice::Malformed),
    }
}

/// The accessible name of the row (docs/0065 Appendix A: a state that
/// replaces content is announced, not just painted).
pub(crate) fn a11y_label(notice: DiffNotice, path: &str) -> String {
    format!("{} — {path}", notice.headline())
}

impl PrDetailPage {
    /// The PR's Files page on GitHub: the closest link that shows the
    /// file's diff without a hash of the path.
    fn files_url(&self, cx: &gpui::App) -> String {
        let base = self
            .detail(cx)
            .map(|d| d.url.clone())
            .unwrap_or_else(|| format!("https://github.com/{}/pull/{}", self.repo, self.number));
        format!("{}/files", base.trim_end_matches('/'))
    }
}

/// The pane body for a file with a notice.
pub(crate) fn render(
    page: &PrDetailPage,
    file: &PrFile,
    notice: DiffNotice,
    badge: AnyElement,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    let url = page.files_url(cx);
    let path = SharedString::from(file.filename.clone());
    v_flex()
        .id("pr-diff-notice")
        .debug_selector(|| "pr-diff-notice-key".into())
        .role(accesskit::Role::Group)
        .aria_label(a11y_label(notice, &path))
        .flex_1()
        .min_h_0()
        .min_w_0()
        .child(
            h_flex()
                .px_3()
                .py_2()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(div().text_sm().font_family("monospace").child(path))
                .child(badge),
        )
        .child(
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(cx.theme().muted_foreground)
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(cx.theme().foreground)
                        .child(notice.headline()),
                )
                .child(div().text_xs().child(notice.detail()))
                .child(
                    Button::new("pr-diff-notice-open")
                        .ghost()
                        .small()
                        .icon(IconName::ExternalLink)
                        .label("Open on GitHub")
                        .accessibility_label("Open this file's diff on GitHub")
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests;
