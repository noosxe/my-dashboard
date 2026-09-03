//! PR detail page (docs/0016 §6 stage 1) — the full-content takeover that
//! opens when a repo-view PR row is clicked: header (state badge, title,
//! number, author/branch meta, refresh + open-on-GitHub), an underline
//! `TabBar` with live counts, and the Conversation read path (description
//! card, merged comment/review timeline, comment-box chrome).
//!
//! Data: one GraphQL detail query on open + on header refresh (D2) — the
//! 200 body parses here, page-side; only non-200 results are forwarded to
//! `GithubState::apply` so the shared 401/403 handling keeps working.
//! Session-only state, no cache.
//!
//! Close paths: `Esc` (the `DismissPrPage` action, bound to this page's
//! `GithubPrPage` key context), the repo crumb (docs/0014 deferred that
//! `on_click` to the sub-pages doc), and sign-out (the page auto-clears in
//! `page.rs::sync_from_state_in` when auth drops).

use gpui::prelude::FluentBuilder as _;
use gpui::*;

use gpui_component::{
    ActiveTheme, Disableable, IconName, Sizable as _,
    avatar::Avatar,
    badge::Badge,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Textarea, TextareaState},
    searchable_list::SearchableVec,
    select::{Select, SelectState},
    skeleton::Skeleton,
    spinner::Spinner,
    tab::{Tab, TabBar},
    v_flex,
};

use crate::gql;
use crate::page::relative_age;
use crate::service::{FetchSpec, GithubState, Kind, run_spec_tracked};

actions!(github, [DismissPrPage]);

/// Ask the hosting `GithubPage` to close the PR sub-page (docs/0016 D1).
pub(crate) enum PrPageEvent {
    CloseRequested,
}

/// The four tabs (docs/0016 D3). Stage 1 renders Conversation; the other
/// three carry counts now and their content lands with stages 3–4 (#93/#94).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrTab {
    Conversation,
    Commits,
    Checks,
    Files,
}

impl PrTab {
    fn index(self) -> usize {
        match self {
            PrTab::Conversation => 0,
            PrTab::Commits => 1,
            PrTab::Checks => 2,
            PrTab::Files => 3,
        }
    }

    fn from_index(ix: usize) -> Option<Self> {
        Some(match ix {
            0 => PrTab::Conversation,
            1 => PrTab::Commits,
            2 => PrTab::Checks,
            3 => PrTab::Files,
            _ => return None,
        })
    }
}

pub(crate) struct PrDetailPage {
    state: Entity<GithubState>,
    /// `owner/repo` of the PR's repository.
    repo: String,
    pub(crate) number: i64,
    /// The snapshot row that opened the page — instant header paint while
    /// the detail query is in flight.
    summary: gql::PullRequest,
    detail: Option<gql::PrDetail>,
    loading: bool,
    load_error: Option<String>,
    tab: PrTab,
    focus_handle: FocusHandle,
    /// Comment-box chrome (stage 1 renders it read-only; the write path is
    /// docs/0016 §6 stage 2).
    comment: Entity<TextareaState>,
    action_select: Entity<SelectState<SearchableVec<SharedString>>>,
}

impl EventEmitter<PrPageEvent> for PrDetailPage {}

impl PrDetailPage {
    pub(crate) fn new(
        state: Entity<GithubState>,
        repo: String,
        summary: gql::PullRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let comment = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(4)
                .placeholder("Leave a comment")
        });
        let actions: Vec<SharedString> = ["Comment", "Approve", "Request changes"]
            .iter()
            .map(|s| (*s).into())
            .collect();
        let default_action: SharedString = "Comment".into();
        let action_select = cx.new(|cx| {
            let mut select = SelectState::new(SearchableVec::from(actions), None, window, cx);
            select.set_selected_value(&default_action, window, cx);
            select
        });
        let number = summary.number;
        let mut page = Self {
            state,
            repo,
            number,
            summary,
            detail: None,
            loading: false,
            load_error: None,
            tab: PrTab::Conversation,
            focus_handle: cx.focus_handle(),
            comment,
            action_select,
        };
        page.fetch_detail(cx);
        page
    }

    /// The detail spec: one query, `owner/repo#number` identity, no ETag
    /// (GraphQL has none upstream). Built here so the fetch and the test
    /// table share one shape (docs/0016 D2).
    fn detail_spec(repo: &str, number: i64) -> FetchSpec {
        let (owner, name) = split_repo(repo);
        FetchSpec {
            kind: Kind::PrDetail,
            url: gql::GRAPHQL_ENDPOINT.to_string(),
            etag: None,
            ident: Some(format!("{repo}#{number}")),
            max_pages: 1,
            variables: Some(serde_json::json!({ "owner": owner, "name": name, "number": number })),
        }
    }

    fn fetch_detail(&mut self, cx: &mut Context<Self>) {
        if !self.state.read(cx).auth.connected() {
            return; // the page is about to be torn down by the auth gate
        }
        let client = self.state.update(cx, |s, _| s.client.clone());
        let spec = Self::detail_spec(&self.repo, self.number);
        log::debug!(
            "pr-detail: fetching {}",
            spec.ident.as_deref().unwrap_or("?")
        );
        self.loading = true;
        self.load_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = run_spec_tracked(client, spec, cx).await;
            let _ = this.update(cx, |page, cx| page.apply_detail_result(result, cx));
        })
        .detach();
    }

    fn apply_detail_result(&mut self, result: crate::service::SpecResult, cx: &mut Context<Self>) {
        self.loading = false;
        if let Some(m) = &result.network {
            self.load_error = Some(format!("Network error: {m}"));
        } else if result.status != 200 {
            // Forward failures to the state entity so the shared 401
            // (reactive refresh / TokenRejected) and 403 (rate-limit pause)
            // handling runs exactly as for every other fetch — those paths
            // act on status before any kind match (docs/0016 D2).
            let status = result.status;
            self.state.update(cx, |s, cx| {
                s.apply(result);
                cx.notify();
            });
            self.load_error = Some(format!("GitHub answered HTTP {status}"));
        } else if let Some(body) = result.body {
            match serde_json::from_str::<gql::Envelope<gql::PrDetailData>>(body.as_str()) {
                Ok(env) => {
                    for e in &env.errors {
                        log::warn!("pr-detail: {}", e.message);
                    }
                    match env
                        .data
                        .and_then(|d| d.repository)
                        .as_ref()
                        .and_then(gql::pr_detail_from_value)
                    {
                        Some(detail) => {
                            log::info!(
                                "pr-detail applied: {}#{} — {} comments, {} reviews, {} commits",
                                self.repo,
                                detail.number,
                                detail.comments.nodes.len(),
                                detail.reviews.nodes.len(),
                                detail.commits.nodes.len()
                            );
                            self.detail = Some(detail);
                        }
                        None => {
                            log::warn!("pr-detail: repository/pullRequest null — keeping header");
                            self.load_error = Some("PR not found (removed, or access lost)".into());
                        }
                    }
                }
                Err(e) => {
                    log::warn!("pr-detail payload: {e}");
                    self.load_error = Some("Malformed payload".into());
                }
            }
        }
        cx.notify();
    }

    fn request_close(&mut self, cx: &mut Context<Self>) {
        cx.emit(PrPageEvent::CloseRequested);
    }

    // --- regions -----------------------------------------------------------

    fn render_header(&self, detail: &Option<gql::PrDetail>, cx: &Context<Self>) -> AnyElement {
        let (title, number, tone, author, base, head, age, url) = match detail {
            Some(d) => (
                d.title.clone(),
                d.number,
                pr_state_tone(d.state.as_deref(), d.draft),
                d.author.clone(),
                d.base_ref.clone(),
                d.head_ref.clone(),
                d.created_at.clone(),
                d.url.clone(),
            ),
            None => (
                self.summary.title.clone(),
                self.summary.number,
                // The snapshot only lists OPEN PRs; a draft stays a draft
                // until the detail payload says otherwise.
                if self.summary.draft {
                    PrStateTone::Draft
                } else {
                    PrStateTone::Open
                },
                self.summary.author.clone(),
                None,
                None,
                self.summary.updated_at.clone(),
                self.summary.url.clone(),
            ),
        };
        let commits = detail.as_ref().map(|d| d.commits.total_count);
        let branch_meta = match (&base, &head) {
            (Some(base), Some(head)) => Some(format!("{base} ← {head}")),
            _ => None,
        };
        h_flex()
            .id("pr-header")
            .justify_between()
            .gap_3()
            .items_start()
            .child(
                v_flex()
                    .gap_1()
                    .min_w_0()
                    .flex_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .flex_wrap()
                            .child(
                                Badge::new()
                                    .xsmall()
                                    .color(state_color(tone, cx))
                                    .child(state_label(tone)),
                            )
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("#{number}")),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .flex_wrap()
                            .when_some(author, |row, author| {
                                row.child(
                                    h_flex()
                                        .gap_1()
                                        .items_center()
                                        .child(
                                            Avatar::new().src(author.avatar_url.clone()).xsmall(),
                                        )
                                        .child(author.login.clone()),
                                )
                            })
                            .when_some(
                                commits
                                    .filter(|c| *c > 0)
                                    .map(|c| format!("wants to merge {c} commits")),
                                |row, text| row.child(text),
                            )
                            .when_some(branch_meta, |row, meta| {
                                row.child(
                                    div()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_xs()
                                        .child(meta),
                                )
                            })
                            .child(relative_age(age.as_deref())),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .when(self.loading, |row| row.child(Spinner::new().small()))
                    .child(
                        Button::new("pr-refresh")
                            .ghost()
                            .small()
                            .icon(IconName::RotateCw)
                            .tooltip("Refresh")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.fetch_detail(cx);
                            })),
                    )
                    .child(
                        Button::new("pr-open-external")
                            .ghost()
                            .small()
                            .icon(IconName::ExternalLink)
                            .tooltip("Open on GitHub")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
            )
            .into_any_element()
    }

    fn render_tabs(&self, detail: &Option<gql::PrDetail>, cx: &Context<Self>) -> AnyElement {
        // Counts render only with a detail payload — while loading the tabs
        // stay bare so nothing flashes a wrong "0" (docs/0016 D3).
        let labels = match detail {
            Some(d) => vec![
                tab_label(
                    "Conversation",
                    Some(d.comments.total_count + d.reviews.total_count),
                ),
                tab_label("Commits", Some(d.commits.total_count)),
                // Checks count needs the contexts connection — stage 3
                // (#93) adds it with the verified union shape.
                "Checks".into(),
                tab_label("Files changed", d.changed_files),
            ],
            None => vec![
                "Conversation".into(),
                "Commits".into(),
                "Checks".into(),
                "Files changed".into(),
            ],
        };
        TabBar::new("pr-tabs")
            .underline()
            .w_full()
            .selected_index(self.tab.index())
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                if let Some(tab) = PrTab::from_index(*ix) {
                    this.tab = tab;
                    cx.notify();
                }
            }))
            .child(Tab::new().label(labels[0].clone()))
            .child(Tab::new().label(labels[1].clone()))
            .child(Tab::new().label(labels[2].clone()))
            .child(Tab::new().label(labels[3].clone()))
            .into_any_element()
    }

    fn render_tab_content(&self, detail: &Option<gql::PrDetail>, cx: &Context<Self>) -> AnyElement {
        match self.tab {
            PrTab::Conversation => self.render_conversation(detail, cx),
            PrTab::Commits | PrTab::Checks | PrTab::Files => stage_placeholder(self.tab, cx),
        }
    }

    fn render_conversation(
        &self,
        detail: &Option<gql::PrDetail>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(d) = detail else {
            if let Some(err) = &self.load_error {
                return error_cell(err, cx);
            }
            return skeleton_list();
        };
        let author_login = d
            .author
            .as_ref()
            .map(|a| a.login.clone())
            .unwrap_or_else(|| "ghost".into());
        let author_avatar = d.author.as_ref().map(|a| a.avatar_url.clone());
        let mut children = vec![body_card(
            "pr-description",
            &author_login,
            author_avatar.as_deref(),
            relative_age(d.created_at.as_deref()),
            d.body.as_deref(),
            None,
            cx,
        )];
        for (kind, ix) in timeline_order(&d.comments.nodes, &d.reviews.nodes) {
            match kind {
                TimelineKind::Comment => {
                    let c = &d.comments.nodes[ix];
                    children.push(body_card(
                        &format!("pr-comment-{ix}"),
                        c.author
                            .as_ref()
                            .map(|a| a.login.as_str())
                            .unwrap_or("ghost"),
                        c.author.as_ref().map(|a| a.avatar_url.as_str()),
                        relative_age(c.created_at.as_deref()),
                        Some(c.body.as_str()),
                        None,
                        cx,
                    ));
                }
                TimelineKind::Review => {
                    let r = &d.reviews.nodes[ix];
                    let (label, tone) = review_state_label(r.state.as_deref());
                    children.push(body_card(
                        &format!("pr-review-{ix}"),
                        r.author
                            .as_ref()
                            .map(|a| a.login.as_str())
                            .unwrap_or("ghost"),
                        r.author.as_ref().map(|a| a.avatar_url.as_str()),
                        relative_age(r.submitted_at.as_deref()),
                        r.body.as_deref().filter(|b| !b.is_empty()),
                        Some((label, tone)),
                        cx,
                    ));
                }
            }
        }
        if d.comments.nodes.is_empty() && d.reviews.nodes.is_empty() {
            children.push(muted_centered("No conversation yet", cx));
        }
        children.push(self.render_comment_box(cx));
        div()
            .id("pr-conversation")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(v_flex().gap_3().py_1().children(children))
            .into_any_element()
    }

    /// The comment-box chrome, read-only: submit lands with the stage-2
    /// write path (docs/0016 §6.2), so the button ships disabled with a
    /// tooltip saying so rather than dead-looking.
    fn render_comment_box(&self, cx: &Context<Self>) -> AnyElement {
        v_flex()
            .id("pr-comment-box")
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .p_3()
            .child(Textarea::new(&self.comment).w_full())
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(Select::new(&self.action_select).small().w_40())
                    .child(
                        Button::new("pr-comment-submit")
                            .label("Comment")
                            .primary()
                            .small()
                            .disabled(true)
                            .tooltip("Posting comments arrives with an upcoming update"),
                    ),
            )
            .into_any_element()
    }
}

impl Focusable for PrDetailPage {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PrDetailPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let detail = self.detail.clone();
        v_flex()
            .id("pr-detail")
            .key_context("GithubPrPage")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &DismissPrPage, _, cx| {
                this.request_close(cx);
            }))
            .size_full()
            .min_w_0()
            .p_4()
            .gap_3()
            .child(self.render_header(&detail, cx))
            .child(self.render_tabs(&detail, cx))
            .child(self.render_tab_content(&detail, cx))
    }
}

// ---------------------------------------------------------------------------
// Pure helpers — table-tested.
// ---------------------------------------------------------------------------

/// Header state tone (docs/0016 D3): draft beats open, terminal states win.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrStateTone {
    Open,
    Draft,
    Merged,
    Closed,
    Unknown,
}

pub(crate) fn pr_state_tone(state: Option<&str>, draft: bool) -> PrStateTone {
    match state {
        Some("MERGED") => PrStateTone::Merged,
        Some("CLOSED") => PrStateTone::Closed,
        Some("OPEN") if draft => PrStateTone::Draft,
        Some("OPEN") => PrStateTone::Open,
        _ if draft => PrStateTone::Draft,
        _ => PrStateTone::Unknown,
    }
}

fn state_label(tone: PrStateTone) -> &'static str {
    match tone {
        PrStateTone::Open => "Open",
        PrStateTone::Draft => "Draft",
        PrStateTone::Merged => "Merged",
        PrStateTone::Closed => "Closed",
        PrStateTone::Unknown => "Unknown",
    }
}

fn state_color(tone: PrStateTone, cx: &Context<PrDetailPage>) -> Hsla {
    match tone {
        PrStateTone::Open => cx.theme().success,
        PrStateTone::Merged => cx.theme().accent,
        PrStateTone::Closed => cx.theme().danger,
        PrStateTone::Draft | PrStateTone::Unknown => cx.theme().muted_foreground,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReviewTone {
    Approved,
    ChangesRequested,
    Muted,
}

/// Review badge label + tone from `PullRequestReviewState`.
pub(crate) fn review_state_label(state: Option<&str>) -> (SharedString, ReviewTone) {
    match state {
        Some("APPROVED") => ("Approved".into(), ReviewTone::Approved),
        Some("CHANGES_REQUESTED") => ("Changes requested".into(), ReviewTone::ChangesRequested),
        Some("COMMENTED") => ("Commented".into(), ReviewTone::Muted),
        Some("DISMISSED") => ("Dismissed".into(), ReviewTone::Muted),
        other => (other.unwrap_or("Review").into(), ReviewTone::Muted),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TimelineKind {
    Comment,
    Review,
}

/// Stable ascending merge of comments and reviews by timestamp (docs/0016
/// D4): missing stamps count as 0 (oldest); Rust's stable sort keeps
/// comments before reviews on equal stamps. Pure — table-tested.
pub(crate) fn timeline_order(
    comments: &[gql::PrComment],
    reviews: &[gql::PrReview],
) -> Vec<(TimelineKind, usize)> {
    fn stamp(ts: Option<&str>) -> u64 {
        ts.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.timestamp().max(0) as u64)
            .unwrap_or(0)
    }
    let mut entries: Vec<(u64, TimelineKind, usize)> = comments
        .iter()
        .enumerate()
        .map(|(i, c)| (stamp(c.created_at.as_deref()), TimelineKind::Comment, i))
        .chain(
            reviews
                .iter()
                .enumerate()
                .map(|(i, r)| (stamp(r.submitted_at.as_deref()), TimelineKind::Review, i)),
        )
        .collect();
    entries.sort_by_key(|e| e.0);
    entries
        .into_iter()
        .map(|(_, kind, ix)| (kind, ix))
        .collect()
}

/// `owner/repo` → (`owner`, `name`), tolerating a missing slash (empty
/// owner — the query then fails server-side and lands in the error cell).
pub(crate) fn split_repo(full: &str) -> (&str, &str) {
    match full.split_once('/') {
        Some((owner, name)) => (owner, name),
        None => ("", full),
    }
}

fn tab_label(name: &str, count: Option<i64>) -> SharedString {
    match count {
        Some(c) => format!("{name} ({c})").into(),
        None => name.into(),
    }
}

// ---------------------------------------------------------------------------
// Small render helpers.
// ---------------------------------------------------------------------------

/// One timeline card: header row (avatar, login, age, optional review
/// badge) + body. Non-interactive — no id/role obligations (docs/0010 D10
/// covers interactive elements only), but ids ride along for tree greps.
fn body_card(
    id: &str,
    login: &str,
    avatar: Option<&str>,
    age: String,
    body: Option<&str>,
    review: Option<(SharedString, ReviewTone)>,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    let (badge_label, badge_color) = review
        .map(|(label, tone)| {
            let color = match tone {
                ReviewTone::Approved => cx.theme().success,
                ReviewTone::ChangesRequested => cx.theme().danger,
                ReviewTone::Muted => cx.theme().muted_foreground,
            };
            (label, color)
        })
        .unzip();
    v_flex()
        .id(SharedString::from(id))
        .w_full()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().secondary.opacity(0.4))
                .child(Avatar::new().src(avatar.unwrap_or_default()).xsmall())
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .child(login.to_string()),
                )
                .when_some(
                    badge_label.clone().zip(badge_color),
                    |row, (label, color)| {
                        row.child(Badge::new().xsmall().color(color).child(label))
                    },
                )
                .child(
                    div()
                        .ml_auto()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(age),
                ),
        )
        .when_some(body, |card, body| {
            card.child(
                div()
                    .px_3()
                    .py_3()
                    .text_sm()
                    .whitespace_normal()
                    .child(body.to_string()),
            )
        })
        .into_any_element()
}

fn skeleton_list() -> AnyElement {
    v_flex()
        .id("pr-skeleton")
        .flex_1()
        .min_h_0()
        .gap_3()
        .py_1()
        .child(Skeleton::new())
        .child(Skeleton::new().w_3_4())
        .child(Skeleton::new().w_1_2())
        .into_any_element()
}

fn error_cell(message: &str, cx: &Context<PrDetailPage>) -> AnyElement {
    v_flex()
        .id("pr-load-error")
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap_2()
        .text_sm()
        .text_color(cx.theme().danger)
        .child(message.to_string())
        .child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child("Use the refresh button in the header to retry"),
        )
        .into_any_element()
}

fn muted_centered(text: &str, cx: &Context<PrDetailPage>) -> AnyElement {
    div()
        .id("pr-muted")
        .w_full()
        .py_4()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.to_string())
        .into_any_element()
}

/// Stage boundary (docs/0016 §6): the commits/checks/files surfaces land
/// with #93/#94 — the tabs are present (counts live) and honest about it.
fn stage_placeholder(tab: PrTab, cx: &Context<PrDetailPage>) -> AnyElement {
    let text = match tab {
        PrTab::Commits => "The commits list arrives with an upcoming update",
        PrTab::Checks => "Checks arrive with an upcoming update",
        PrTab::Files => "Changed files arrive with an upcoming update",
        PrTab::Conversation => unreachable!("conversation renders its own content"),
    };
    div()
        .id("pr-stage-placeholder")
        .flex_1()
        .min_h_0()
        .flex()
        .items_center()
        .justify_center()
        .text_color(cx.theme().muted_foreground)
        .text_sm()
        .child(text)
        .into_any_element()
}
#[cfg(test)]
mod tests {
    // No `use super::*` — page.rs documents the gpui::* shadowing hazard.
    use crate::gql;
    use crate::pr_page::{
        PrStateTone, ReviewTone, TimelineKind, pr_state_tone, review_state_label, split_repo,
        timeline_order,
    };

    fn comment(login: &str, at: &str) -> gql::PrComment {
        gql::PrComment {
            author: Some(gql::Author {
                login: login.into(),
                avatar_url: String::new(),
            }),
            body: format!("by {login}"),
            created_at: Some(at.into()),
        }
    }

    fn review(login: &str, at: &str, state: &str) -> gql::PrReview {
        gql::PrReview {
            author: Some(gql::Author {
                login: login.into(),
                avatar_url: String::new(),
            }),
            state: Some(state.into()),
            body: Some(format!("by {login}")),
            submitted_at: Some(at.into()),
        }
    }

    #[test]
    fn timeline_orders_across_kinds_ascending() {
        let comments = vec![
            comment("a", "2026-08-03T00:00:00Z"),
            comment("b", "2026-08-01T00:00:00Z"),
        ];
        let reviews = vec![review("c", "2026-08-02T00:00:00Z", "APPROVED")];
        let order = timeline_order(&comments, &reviews);
        let logins: Vec<&str> = order
            .iter()
            .map(|(kind, ix)| match kind {
                TimelineKind::Comment => comments[*ix].author.as_ref().unwrap().login.as_str(),
                TimelineKind::Review => reviews[*ix].author.as_ref().unwrap().login.as_str(),
            })
            .collect();
        assert_eq!(logins, vec!["b", "c", "a"]);
    }

    #[test]
    fn timeline_ties_keep_comments_first_and_missing_stamps_are_oldest() {
        let comments = vec![comment("late", "2026-08-05T00:00:00Z")];
        let reviews = vec![
            review("nostamp", "", "COMMENTED"),
            review("tie", "2026-08-05T00:00:00Z", "APPROVED"),
        ];
        let order = timeline_order(&comments, &reviews);
        // Missing stamp (=0) first; on the equal 2026-08-05 stamps the
        // comment precedes the review (stable sort, comments chained first).
        assert_eq!(
            order,
            vec![
                (TimelineKind::Review, 0),
                (TimelineKind::Comment, 0),
                (TimelineKind::Review, 1),
            ]
        );
    }

    #[test]
    fn state_tone_table() {
        assert_eq!(pr_state_tone(Some("OPEN"), false), PrStateTone::Open);
        assert_eq!(pr_state_tone(Some("OPEN"), true), PrStateTone::Draft);
        assert_eq!(pr_state_tone(None, true), PrStateTone::Draft);
        assert_eq!(pr_state_tone(Some("MERGED"), false), PrStateTone::Merged);
        assert_eq!(pr_state_tone(Some("CLOSED"), false), PrStateTone::Closed);
        assert_eq!(pr_state_tone(Some("WEIRD"), false), PrStateTone::Unknown);
        assert_eq!(pr_state_tone(None, false), PrStateTone::Unknown);
    }

    #[test]
    fn review_label_table() {
        assert_eq!(
            review_state_label(Some("APPROVED")),
            ("Approved".into(), ReviewTone::Approved)
        );
        assert_eq!(
            review_state_label(Some("CHANGES_REQUESTED")),
            ("Changes requested".into(), ReviewTone::ChangesRequested)
        );
        assert_eq!(
            review_state_label(Some("COMMENTED")),
            ("Commented".into(), ReviewTone::Muted)
        );
        assert_eq!(
            review_state_label(None),
            ("Review".into(), ReviewTone::Muted)
        );
    }

    #[test]
    fn split_repo_table() {
        assert_eq!(split_repo("o/r"), ("o", "r"));
        assert_eq!(split_repo("a/b/c"), ("a", "b/c"));
        assert_eq!(split_repo("bare"), ("", "bare"));
    }
}
