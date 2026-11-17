//! The cross-reference row (docs/0085): "octo-dev mentioned this pull
//! request in Fix loader #42", shared by the PR page and the issue page. One
//! muted line in the #919 event-row style — glyph, sentence, age — that is
//! also one button: it opens the source in the app (a repository the module
//! lists) or in the browser. The wire lives in `gql::xref_events`.

use gpui::prelude::*;
use gpui::{AnyElement, App, FontWeight, SharedString, Window, accesskit::Role, div};
use gpui_component::{ActiveTheme as _, Sizable as _, h_flex};

mod look;
mod sentence;
mod target;

pub(crate) use self::target::{Target, listed, target as resolve};
use self::{look::look, look::state_words, sentence::sentence};
use crate::gql::XrefEvent;
use crate::state_pill::{Tone, icon};
use dashboard_core::freshness::relative_age;

/// The page the row sits on; it picks the noun in the sentence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PageKind {
    Issue,
    PullRequest,
}

impl PageKind {
    fn noun(self) -> &'static str {
        match self {
            PageKind::Issue => "issue",
            PageKind::PullRequest => "pull request",
        }
    }
}

/// The accessible name: the sentence, the source's state, and what
/// activating does.
fn accessible_name(page: PageKind, e: &XrefEvent, page_repo: &str, target: &Target) -> String {
    let s = sentence(page, e, page_repo);
    let opens = match target {
        Target::InApp { is_pr: true, .. } => "Opens the pull request.",
        Target::InApp { is_pr: false, .. } => "Opens the issue.",
        Target::Browser(_) => "Opens in the browser.",
    };
    format!(
        "{}, {}. {opens}",
        s.text(),
        state_words(e.source.is_pr, e.source.state)
    )
}

/// Where a row sits: the page, its repository, and the row's key.
pub(crate) struct Place<'a> {
    /// `pr-event` or `issue-event`.
    pub id_prefix: &'a str,
    /// The entry index, as for the other event rows.
    pub ix: usize,
    pub page: PageKind,
    pub page_repo: &'a str,
}

/// One row. `target` is `resolve(&e.source, tracked)`; `on_open` receives it
/// on click, Enter or Space.
pub(crate) fn row(
    at: Place<'_>,
    e: &XrefEvent,
    target: Target,
    on_open: impl Fn(Target, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let Place {
        id_prefix,
        ix,
        page,
        page_repo,
    } = at;
    let (tone, glyph) = look(e.source.is_pr, e.source.state);
    let color = match tone {
        Tone::Merged => cx.theme().info,
        Tone::Success => cx.theme().success,
        Tone::Danger => cx.theme().danger,
        _ => cx.theme().muted_foreground,
    };
    let name = accessible_name(page, e, page_repo, &target);
    let s = sentence(page, e, page_repo);
    let key = format!("{id_prefix}-{ix}");
    let probe = key.clone();
    let mono = cx.theme().mono_font_family.clone();
    h_flex()
        .id(SharedString::from(key))
        .debug_selector(move || probe.clone())
        .role(Role::Button)
        .aria_label(SharedString::from(name))
        .focusable()
        .tab_stop(true)
        .w_full()
        .gap_2()
        .items_center()
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(gpui::transparent_black())
        .cursor_pointer()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .hover({
            let hover = cx.theme().accent.opacity(0.5);
            move |style| style.bg(hover)
        })
        .focus({
            let ring = cx.theme().ring;
            move |style| style.border_color(ring)
        })
        .on_click(move |_, window, cx| on_open(target.clone(), window, cx))
        .child(icon(glyph).xsmall().text_color(color))
        .child(
            // A long title wraps word by word beside the sentence; the
            // reference and the age keep their edge.
            h_flex()
                .flex_1()
                .min_w_0()
                .flex_wrap()
                .gap_1()
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(cx.theme().foreground)
                        .child(s.actor.clone()),
                )
                .child(div().child(s.verb.clone()))
                .child(
                    div()
                        .min_w_0()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(cx.theme().foreground)
                        .child(s.title),
                )
                .child(div().flex_shrink_0().font_family(mono).child(s.reference)),
        )
        .child(
            div()
                .ml_auto()
                .flex_shrink_0()
                .text_xs()
                .child(relative_age(e.created_at.as_deref())),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gql::{XrefSource, XrefState};

    #[test]
    fn the_accessible_name_carries_sentence_state_and_action() {
        let e = XrefEvent {
            id: "E".into(),
            actor_login: Some("octo-dev".into()),
            created_at: None,
            will_close: false,
            source: XrefSource {
                is_pr: true,
                number: 42,
                title: "Fix loader".into(),
                repo: "fixture-org/beacon".into(),
                url: "u".into(),
                state: XrefState::Open,
            },
        };
        let t = Target::InApp {
            repo: "fixture-org/beacon".into(),
            number: 42,
            is_pr: true,
        };
        assert_eq!(
            accessible_name(PageKind::Issue, &e, "fixture-org/beacon", &t),
            "octo-dev mentioned this issue in Fix loader #42, open pull request. \
             Opens the pull request."
        );
        let b = Target::Browser("u".into());
        assert!(accessible_name(PageKind::Issue, &e, "x/y", &b).ends_with("Opens in the browser."));
    }
}
