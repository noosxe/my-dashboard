//! The commit row shared by the repo Commits tab and the PR Commits tab
//! (docs/0081 D5/D6, #793): the two-line row, its accessible name, the
//! copy-SHA button and the day header, so both lists read the same.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable as _,
    avatar::Avatar,
    button::{Button, ButtonVariants},
    h_flex,
    list::ListItem,
    spinner::Spinner,
    tag::Tag,
    tooltip::Tooltip,
    v_flex,
};

use dashboard_core::freshness::relative_age;

use crate::commits::{day_key, day_label};
use crate::gql::{self, CommitRow};
use crate::page::ci_row_glyph;

/// The name a row shows for its author (D5): the linked login, else the
/// git author name for unlinked emails.
pub(crate) fn author_name(row: &CommitRow) -> String {
    row.author
        .as_ref()
        .and_then(|a| {
            a.user
                .as_ref()
                .map(|u| u.login.clone())
                .or_else(|| a.name.clone())
        })
        .unwrap_or_default()
}

/// The verification tag (D5): `Some(true)` = Verified, `Some(false)` =
/// Unverified (a signature exists but is invalid), `None` = unsigned —
/// GitHub's default without vigilant mode.
pub(crate) fn verification(row: &CommitRow) -> Option<bool> {
    row.signature.as_ref().map(|s| s.is_valid)
}

/// The row's accessible name (D6, docs/0049 D1), in reading order:
/// `"<headline> — <login>, committed <relative>, <Verified|Unverified>,
/// <CI text>, <short sha>"`. The unsigned and no-CI parts are omitted,
/// never left as empty commas.
pub(crate) fn commit_label(row: &CommitRow, relative: &str, ci_text: Option<&str>) -> String {
    let mut parts = Vec::new();
    let author = author_name(row);
    if !author.is_empty() {
        parts.push(author);
    }
    if !relative.is_empty() {
        parts.push(format!("committed {relative}"));
    }
    match verification(row) {
        Some(true) => parts.push("Verified".to_string()),
        Some(false) => parts.push("Unverified".to_string()),
        None => {}
    }
    if let Some(ci) = ci_text {
        parts.push(ci.to_string());
    }
    parts.push(row.abbreviated_oid.clone());
    format!("{} — {}", row.message_headline, parts.join(", "))
}

/// The first 12 hex digits of an oid — the row's debug-id stem (D6).
pub(crate) fn oid12(oid: &str) -> &str {
    &oid[..oid.len().min(12)]
}

/// One fixed two-line commit row (D5).
pub(crate) fn commit_row(row: &CommitRow, cx: &App) -> ListItem {
    let key = oid12(&row.oid).to_string();
    let author = author_name(row);
    let relative = relative_age(Some(&row.committed_date));
    let rollup = gql::rollup_state(
        row.status_check_rollup
            .as_ref()
            .and_then(|r| r.state.as_deref()),
    );
    let ci_text = rollup.map(|s| ci_row_glyph(s, cx).2);
    let label = commit_label(row, &relative, ci_text);
    let absolute = row
        .committed_date
        .parse::<jiff::Timestamp>()
        .map(|t| {
            jiff::tz::TimeZone::system()
                .to_datetime(t)
                .strftime("%b %-d, %Y, %H:%M")
                .to_string()
        })
        .unwrap_or_default();
    let muted = cx.theme().muted_foreground;

    ListItem::new(SharedString::from(format!("commit-{key}")))
        .debug_selector({
            let probe = format!("commit-{key}");
            move || probe.clone()
        })
        // The `ListState` wraps each row in its own unnamed `ListItem`
        // node (position, size, selected) and takes no label; a label on
        // a role-less element makes no node at all. A `Group` role gives
        // the D6 name a node of its own inside that item.
        .role(accesskit::Role::Group)
        .accessibility_label(label)
        .px_2()
        .py_1p5()
        .rounded_sm()
        .child(
            v_flex()
                .w_full()
                .min_w_0()
                .gap_0p5()
                // Line 1: headline, then the verification tag and CI glyph.
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .child(row.message_headline.clone()),
                        )
                        .when_some(verification(row), |line, valid| {
                            line.child(if valid {
                                Tag::success().outline().small().child("Verified")
                            } else {
                                Tag::secondary().outline().small().child("Unverified")
                            })
                        })
                        .when_some(rollup, |line, state| {
                            let (icon, color, tip) = ci_row_glyph(state, cx);
                            let id: SharedString = format!("commit-ci-{key}").into();
                            line.child(
                                div()
                                    .id(id.clone())
                                    .debug_selector(move || id.to_string())
                                    .flex_none()
                                    .child(if state == gql::CheckState::Pending {
                                        Spinner::new().icon(icon).color(color).into_any_element()
                                    } else {
                                        Icon::new(icon)
                                            .size_4()
                                            .text_color(color)
                                            .into_any_element()
                                    })
                                    .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx)),
                            )
                        }),
                )
                // Line 2: avatar, author, "committed 3h"; the short SHA
                // and its copy button right.
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .text_xs()
                        .text_color(muted)
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .items_center()
                                .child({
                                    let avatar = row.author.as_ref();
                                    Avatar::new()
                                        .when_some(
                                            avatar.and_then(|a| a.avatar_url.clone()),
                                            |a, url| a.src(url),
                                        )
                                        .name(author.clone())
                                        .xsmall()
                                })
                                .child(div().truncate().child(author))
                                .child(
                                    div()
                                        .id(SharedString::from(format!("commit-age-{key}")))
                                        .flex_none()
                                        .child(format!("committed {relative}"))
                                        .when(!absolute.is_empty(), |el| {
                                            el.tooltip(move |window, cx| {
                                                Tooltip::new(absolute.clone()).build(window, cx)
                                            })
                                        }),
                                ),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .gap_1()
                                .items_center()
                                .child(
                                    div()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .child(row.abbreviated_oid.clone()),
                                )
                                .child(CopyShaButton {
                                    id: format!("commit-copy-{key}").into(),
                                    oid: row.oid.clone(),
                                    short: row.abbreviated_oid.clone(),
                                }),
                        ),
                ),
        )
}

/// The row's copy-SHA button (D5). The pinned `Clipboard` renders a
/// tab-stop `Button` with no way to opt out, which would put one tab
/// stop per visible row inside the list — D6 makes the list a single
/// stop with secondary-confirm as the keyboard copy. This is the same
/// button with `tab_stop(false)` and the same two-second check mark.
#[derive(IntoElement)]
struct CopyShaButton {
    id: SharedString,
    oid: String,
    short: String,
}

#[derive(Default)]
struct CopiedState {
    copied: bool,
}

impl RenderOnce for CopyShaButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| CopiedState::default());
        let copied = state.read(cx).copied;
        let probe = self.id.to_string();
        Button::new(self.id.clone())
            .debug_selector(move || probe.clone())
            .icon(if copied {
                IconName::Check
            } else {
                IconName::Copy
            })
            .ghost()
            .xsmall()
            .tab_stop(false)
            .tooltip("Copy full SHA")
            .accessibility_label(format!("Copy SHA {}", self.short))
            .on_click(move |_, _, cx| {
                // The row's own click opens the commit — the copy must
                // not bubble into it.
                cx.stop_propagation();
                cx.write_to_clipboard(ClipboardItem::new_string(self.oid.clone()));
                state.update(cx, |s, cx| {
                    s.copied = true;
                    cx.notify();
                });
                let state = state.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(2))
                        .await;
                    state.update(cx, |s, cx| {
                        s.copied = false;
                        cx.notify();
                    });
                })
                .detach();
            })
    }
}

/// The "Commits on Oct 21, 2026" day header (docs/0081 D5): a level-3
/// heading, shared by both lists. `repeat` is `Some(first row index)`
/// where the same day can head two sections (the PR list keeps commit
/// order, and committer dates aren't monotonic), so ids stay unique.
pub(crate) fn day_header(
    day: Option<jiff::civil::Date>,
    repeat: Option<usize>,
    cx: &App,
) -> Stateful<Div> {
    let text = day_label(day);
    let id = match repeat {
        Some(ix) => format!("commits-day-{}-{ix}", day_key(day)),
        None => format!("commits-day-{}", day_key(day)),
    };
    div()
        .id(SharedString::from(id.clone()))
        .debug_selector(move || id.clone())
        .role(accesskit::Role::Heading)
        .aria_level(3)
        .aria_label(text.clone())
        .px_2()
        .pt_3()
        .pb_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}
