//! Issue detail page (docs/0020) — the PR page's sibling takeover: opened
//! from a repo-view issue row, it replaces the module content with a
//! header (state badge, title, number, author/age meta, refresh +
//! open-on-GitHub), a single conversation view (description card, comment
//! timeline, comment box — issues have no commits/checks/files, so no
//! tab bar), and a right rail with the metadata the PR page's merge panel
//! occupies: labels, assignees, and the close/reopen state panel.
//!
//! Data: one GraphQL detail query on open + on refresh and after every
//! mutation (D2) — the 200 body parses here, page-side; only non-200
//! results are forwarded to `GithubState::apply` so the shared 401/403
//! handling keeps working. Session-only state, no cache.
//!
//! Reused unchanged from the PR page: markdown bodies (0017), the
//! inline editor and delete-confirm cards, and the per-comment
//! `viewerCanUpdate`/`viewerCanDelete` gating — conversation comments on
//! issues are the same `IssueComment` nodes (0019), so the affordances,
//! one-editor rule, and node-id keying transfer wholesale.
//!
//! Close paths: `Esc` (the `DismissIssuePage` action, bound to this
//! page's `GithubIssuePage` key context), the repo crumb, and sign-out
//! (the page auto-clears in `page.rs` when auth drops).

use gpui::prelude::FluentBuilder as _;
use gpui::*;

use gpui_component::{
    ActiveTheme, Disableable, IconName, Sizable as _,
    avatar::Avatar,
    badge::Badge,
    button::{Button, ButtonVariants},
    group_box::GroupBox,
    h_flex,
    input::{Textarea, TextareaState},
    searchable_list::SearchableVec,
    select::{Select, SelectState},
    skeleton::Skeleton,
    spinner::Spinner,
    v_flex,
};

use serde_json::json;

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::gql;
use crate::markdown;
use crate::page::relative_age;
use crate::pr_page::{
    ErrorScope, MutationError, body_card, cap_row, delete_confirm_card, inline_edit_card,
    split_repo,
};
use crate::service::{FetchSpec, GithubState, Kind, Mutation, run_mutation, run_spec_tracked};

actions!(github, [DismissIssuePage]);

/// Ask the hosting `GithubPage` to close the issue sub-page (docs/0020 D1).
pub(crate) enum IssuePageEvent {
    CloseRequested,
}

pub(crate) struct IssueDetailPage {
    state: Entity<GithubState>,
    /// `owner/repo` of the issue's repository.
    repo: String,
    pub(crate) number: i64,
    /// The snapshot row that opened the page — instant header paint while
    /// the detail query is in flight.
    summary: gql::Issue,
    detail: Option<gql::IssueDetail>,
    loading: bool,
    load_error: Option<String>,
    focus_handle: FocusHandle,
    /// Comment box draft textarea (0020 D4).
    comment: Entity<TextareaState>,
    /// Close-reason choice (0020 Q3/D5) — built once in `new` (it needs
    /// the window), read at submit time.
    close_reason: Entity<SelectState<SearchableVec<SharedString>>>,
    /// The write in flight — one at a time; every write surface disables
    /// while set.
    busy: Option<IssueBusyOp>,
    /// The last mutation failure, scoped to the surface that shows it.
    mutation_error: Option<MutationError>,
    /// Prefilled textarea while the description is being edited (0020 D4).
    description_edit: Option<Entity<TextareaState>>,
    /// Row-level editor (0019 pattern): (node id, editor). At most one
    /// editor is open anywhere on the page.
    timeline_edit: Option<(String, Entity<TextareaState>)>,
    /// Node id awaiting delete confirmation (0019 pattern, inline
    /// two-step).
    delete_confirm: Option<String>,
    /// Parsed markdown bodies (0017) keyed by card id — rebuilt whenever
    /// a detail payload lands, never re-parsed per frame.
    md: BTreeMap<String, Arc<[markdown::Block]>>,
}

/// The issue page's write operations in flight (0020 D2/D4/D5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IssueBusyOp {
    Comment,
    Description,
    CommentEdit,
    CommentDelete,
    Close,
    Reopen,
}

impl IssueBusyOp {
    /// Where a failure of this op renders inline: the conversation column
    /// for everything on a row/card, the rail panel for state changes
    /// (0020 D5).
    fn error_scope(self) -> ErrorScope {
        match self {
            IssueBusyOp::Comment
            | IssueBusyOp::Description
            | IssueBusyOp::CommentEdit
            | IssueBusyOp::CommentDelete => ErrorScope::Timeline,
            IssueBusyOp::Close | IssueBusyOp::Reopen => ErrorScope::Rail,
        }
    }
}

impl EventEmitter<IssuePageEvent> for IssueDetailPage {}

impl IssueDetailPage {
    pub(crate) fn new(
        state: Entity<GithubState>,
        repo: String,
        summary: gql::Issue,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let comment = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(4)
                .placeholder("Leave a comment")
        });
        // Close-reason Select (0020 Q3): github.com parity — default
        // Completed, "Not planned" one click away. Read at submit time.
        let reasons: Vec<SharedString> = [
            CloseReason::Completed.label(),
            CloseReason::NotPlanned.label(),
        ]
        .iter()
        .map(|s| (*s).into())
        .collect();
        let default_reason: SharedString = CloseReason::Completed.label().into();
        let close_reason = cx.new(|cx| {
            let mut select = SelectState::new(SearchableVec::from(reasons), None, window, cx);
            select.set_selected_value(&default_reason, window, cx);
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
            focus_handle: cx.focus_handle(),
            comment,
            close_reason,
            busy: None,
            mutation_error: None,
            description_edit: None,
            timeline_edit: None,
            delete_confirm: None,
            md: BTreeMap::new(),
        };
        page.fetch_detail(cx);
        page
    }

    /// The detail spec: one query, `owner/repo#number` identity, no ETag
    /// (GraphQL has none upstream). Same shape as the PR page's (0020 D2).
    fn detail_spec(repo: &str, number: i64) -> FetchSpec {
        let (owner, name) = split_repo(repo);
        FetchSpec {
            kind: Kind::IssueDetail,
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
            "issue-detail: fetching {}",
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
            // Forward failures to the state entity so the shared 401/403
            // handling runs exactly as for every other fetch (0020 D2).
            let status = result.status;
            self.state.update(cx, |s, cx| {
                s.apply(result);
                cx.notify();
            });
            self.load_error = Some(format!("GitHub answered HTTP {status}"));
        } else if let Some(body) = result.body {
            match serde_json::from_str::<gql::Envelope<gql::IssueDetailData>>(body.as_str()) {
                Ok(env) => {
                    for e in &env.errors {
                        log::warn!("issue-detail: {}", e.message);
                    }
                    match env
                        .data
                        .and_then(|d| d.repository)
                        .as_ref()
                        .and_then(gql::issue_detail_from_value)
                    {
                        Some(detail) => {
                            log::info!(
                                "issue-detail applied: {}#{} — {} comments, {} events, {} labels",
                                self.repo,
                                detail.number,
                                detail.comments.nodes.len(),
                                detail.events.len(),
                                detail.labels.len()
                            );
                            self.md = build_issue_md_cache(&detail);
                            self.detail = Some(detail);
                            // 0019 D4 pattern: an editor/confirm whose node
                            // is gone with the new payload has nothing left
                            // to edit — drop it (deleted elsewhere,
                            // refetch race).
                            let d = self.detail.as_ref().unwrap();
                            let still_there =
                                |id: &str| d.comments.nodes.iter().any(|c| c.id == id);
                            if let Some((id, _)) = &self.timeline_edit {
                                if !still_there(id) {
                                    self.timeline_edit = None;
                                }
                            }
                            if let Some(id) = &self.delete_confirm {
                                if !still_there(id) {
                                    self.delete_confirm = None;
                                }
                            }
                        }
                        None => {
                            log::warn!("issue-detail: repository/issue null — keeping header");
                            self.load_error =
                                Some("Issue not found (removed, or access lost)".into());
                        }
                    }
                }
                Err(e) => {
                    log::warn!("issue-detail payload: {e}");
                    self.load_error = Some("Malformed payload".into());
                }
            }
        }
        cx.notify();
    }

    // --- write path (0020 D2/D4/D5) ----------------------------------------

    /// The close-reason the Select currently names — Completed when the
    /// Select has nothing selected or names an unknown label (0020 D5).
    fn selected_close_reason(&self, cx: &Context<Self>) -> Option<CloseReason> {
        let selected = self.close_reason.read(cx).selected_value()?;
        close_reason_from_label(selected.as_ref())
    }

    /// Post a comment (`addComment` — `subjectId` is a bare Node ID, so
    /// the PR page's document serves issues unchanged; 0020 D2). Gating
    /// is the same pure fn the button renders with — re-checked here as
    /// the honest guard.
    fn submit_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.clone() else { return };
        let body = self.comment.read(cx).value().trim().to_string();
        if !comment_submit_allowed(body.is_empty(), self.busy.is_some()) || d.id.is_empty() {
            return;
        }
        let variables = json!({ "input": { "subjectId": d.id, "body": body } });
        self.mutation_error = None;
        self.busy = Some(IssueBusyOp::Comment);
        cx.notify();
        self.spawn_mutation(
            IssueBusyOp::Comment,
            Mutation::Comment,
            variables,
            window,
            cx,
        );
    }

    /// Open the description editor (0020 D4), prefilled with the body.
    fn start_edit_description(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.description_edit.is_some() {
            return;
        }
        // One editor at a time: a row editor or delete confirm yields.
        self.timeline_edit = None;
        self.delete_confirm = None;
        let body = self
            .detail
            .as_ref()
            .and_then(|d| d.body.clone())
            .unwrap_or_default();
        let editor = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx)
                // Full content height (PR-page lesson): grow with the
                // body, scroll internally past the cap.
                .auto_grow(8, 40)
                .placeholder("Describe this issue");
            state.set_value(body, window, cx);
            state
        });
        self.description_edit = Some(editor);
        cx.notify();
    }

    fn cancel_edit_description(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.description_edit = None;
        self.mutation_error = None;
        self.refocus_page(_window, cx);
        cx.notify();
    }

    /// Save the edited description (`updateIssue { id body }` — the PR
    /// description edit's document; 0020 D2). Empty bodies never get here
    /// (Save renders disabled).
    fn save_description(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(d), Some(editor)) = (self.detail.as_ref(), self.description_edit.as_ref()) else {
            return;
        };
        if self.busy.is_some() || d.id.is_empty() {
            return;
        }
        let body = editor.read(cx).value().trim().to_string();
        if body.is_empty() {
            return;
        }
        let variables = json!({ "input": { "id": d.id, "body": body } });
        self.mutation_error = None;
        self.busy = Some(IssueBusyOp::Description);
        cx.notify();
        self.spawn_mutation(
            IssueBusyOp::Description,
            Mutation::UpdateDescription,
            variables,
            window,
            cx,
        );
    }

    // --- timeline-row edit/delete (0019 pattern, 0020 D4) -------------------

    /// Open the row editor for `node_id`, prefilled with `body`. The
    /// one-editor invariant: the description editor and any pending
    /// delete confirm yield to this editor.
    fn start_edit_row(
        &mut self,
        node_id: String,
        body: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() || node_id.is_empty() {
            return;
        }
        self.description_edit = None;
        self.delete_confirm = None;
        let editor = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx).auto_grow(4, 30);
            state.set_value(body, window, cx);
            state
        });
        self.timeline_edit = Some((node_id, editor));
        cx.notify();
    }

    fn cancel_edit_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.timeline_edit = None;
        self.refocus_page(window, cx);
        cx.notify();
    }

    /// Save the row editor (`updateIssueComment { id body }` — 0019 D5's
    /// document, unchanged for issues). Empty bodies never get here.
    fn save_edit_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        let Some((node_id, editor)) = self.timeline_edit.as_ref() else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        let body = editor.read(cx).value().trim().to_string();
        if body.is_empty() {
            return;
        }
        // The node vanished under the editor (refetch race) — close.
        if !d.comments.nodes.iter().any(|c| c.id == *node_id) {
            self.timeline_edit = None;
            cx.notify();
            return;
        }
        let variables = json!({ "input": { "id": node_id, "body": body } });
        self.mutation_error = None;
        self.busy = Some(IssueBusyOp::CommentEdit);
        cx.notify();
        self.spawn_mutation(
            IssueBusyOp::CommentEdit,
            Mutation::UpdateComment,
            variables,
            window,
            cx,
        );
    }

    /// Arm the inline delete confirm on a row. Editors yield.
    fn start_delete_confirm(&mut self, node_id: String, cx: &mut Context<Self>) {
        if self.busy.is_some() || node_id.is_empty() {
            return;
        }
        self.description_edit = None;
        self.timeline_edit = None;
        self.delete_confirm = Some(node_id);
        cx.notify();
    }

    fn cancel_delete_confirm(&mut self, cx: &mut Context<Self>) {
        self.delete_confirm = None;
        cx.notify();
    }

    /// Fire the confirmed delete (`deleteIssueComment { id }`). The
    /// confirm card stays up while the op is in flight; success clears it.
    fn confirm_delete_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        let Some(node_id) = self.delete_confirm.clone() else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        if !d.comments.nodes.iter().any(|c| c.id == node_id) {
            self.delete_confirm = None;
            cx.notify();
            return;
        }
        let variables = json!({ "input": { "id": node_id } });
        self.mutation_error = None;
        self.busy = Some(IssueBusyOp::CommentDelete);
        cx.notify();
        self.spawn_mutation(
            IssueBusyOp::CommentDelete,
            Mutation::DeleteComment,
            variables,
            window,
            cx,
        );
    }

    // --- state panel (0020 D5) ----------------------------------------------

    /// Close the issue (`closeIssue { issueId stateReason }`) — the
    /// panel's reason always rides along (0020 D2).
    fn close_issue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.clone() else { return };
        if d.id.is_empty() {
            return;
        }
        if !close_allowed(d.state.as_deref(), d.viewer_can_close, self.busy.is_some()) {
            return;
        }
        let reason = self
            .selected_close_reason(cx)
            .unwrap_or(CloseReason::Completed);
        let variables = json!({ "input": { "issueId": d.id, "stateReason": reason.as_str() } });
        self.mutation_error = None;
        self.busy = Some(IssueBusyOp::Close);
        cx.notify();
        self.spawn_mutation(
            IssueBusyOp::Close,
            Mutation::IssueClose,
            variables,
            window,
            cx,
        );
    }

    /// Reopen the issue (`reopenIssue { issueId }`).
    fn reopen_issue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.clone() else { return };
        if d.id.is_empty() {
            return;
        }
        if !reopen_allowed(d.state.as_deref(), d.viewer_can_reopen, self.busy.is_some()) {
            return;
        }
        let variables = json!({ "input": { "issueId": d.id } });
        self.mutation_error = None;
        self.busy = Some(IssueBusyOp::Reopen);
        cx.notify();
        self.spawn_mutation(
            IssueBusyOp::Reopen,
            Mutation::IssueReopen,
            variables,
            window,
            cx,
        );
    }

    /// The description/row editor holds keyboard focus while open;
    /// dropping its entity would leave the page with a dead focus handle
    /// — no key context, so `Esc` would stop working (PR-page lesson).
    fn refocus_page(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
    }

    fn spawn_mutation(
        &mut self,
        op: IssueBusyOp,
        mutation: Mutation,
        variables: serde_json::Value,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let client = self.state.update(cx, |s, _| s.client.clone());
        let ident = format!("{}#{} {:?}", self.repo, self.number, op);
        cx.spawn_in(window, async move |this, cx| {
            let result = run_mutation(client, mutation, variables, ident, cx).await;
            let _ = cx.update(|window, app| {
                this.update(app, |page, cx| {
                    page.apply_mutation_result(op, result, window, cx)
                })
            });
        })
        .detach();
    }

    /// A mutation came back: success clears the surface it came from, any
    /// failure lands as a scoped inline error — then the detail refetches
    /// in every case (0020 D2), so the page never disagrees with GitHub.
    /// A 200 with GraphQL `errors` (server-side rejections — closing an
    /// issue that was just locked, say) is a failure here.
    fn apply_mutation_result(
        &mut self,
        op: IssueBusyOp,
        result: crate::service::SpecResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = None;
        let scope = op.error_scope();
        let mut failed: Option<String> = None;
        if let Some(m) = &result.network {
            failed = Some(format!("Network error: {m}"));
        } else if result.status != 200 {
            let status = result.status;
            self.state.update(cx, |s, cx| {
                s.apply(result);
                cx.notify();
            });
            failed = Some(format!("GitHub answered HTTP {status}"));
        } else {
            let gql_error = result.body.as_deref().and_then(|body| {
                serde_json::from_str::<gql::Envelope<serde_json::Value>>(body)
                    .ok()
                    .and_then(|env| env.errors.first().map(|e| e.message.clone()))
            });
            if let Some(message) = gql_error {
                log::warn!(
                    "issue-mutation {}#{} {:?}: {}",
                    self.repo,
                    self.number,
                    op,
                    message
                );
                failed = Some(message);
            }
        }
        if let Some(message) = failed {
            self.mutation_error = Some(MutationError { scope, message });
        } else {
            self.mutation_error = None;
            match op {
                IssueBusyOp::Comment => {
                    // Clear the draft — `set_value` needs the window the
                    // spawn kept alive for exactly this.
                    self.comment
                        .update(cx, |state, cx| state.set_value("", window, cx));
                }
                IssueBusyOp::Description => {
                    self.description_edit = None;
                    self.refocus_page(window, cx);
                }
                // 0019 D4 pattern: success closes the row surface it came
                // from — the refetch below repaints from fresh data.
                IssueBusyOp::CommentEdit => {
                    self.timeline_edit = None;
                    self.refocus_page(window, cx);
                }
                IssueBusyOp::CommentDelete => {
                    self.delete_confirm = None;
                }
                // The refetch repaints the badge and swaps the panel.
                IssueBusyOp::Close | IssueBusyOp::Reopen => {}
            }
        }
        self.fetch_detail(cx);
        cx.notify();
    }

    fn request_close(&mut self, cx: &mut Context<Self>) {
        cx.emit(IssuePageEvent::CloseRequested);
    }

    // --- regions -------------------------------------------------------------

    fn render_header(&self, detail: &Option<gql::IssueDetail>, cx: &Context<Self>) -> AnyElement {
        let (title, number, author, age, url) = match detail {
            Some(d) => (
                d.title.clone(),
                d.number,
                d.author.clone(),
                d.created_at.clone(),
                d.url.clone(),
            ),
            None => (
                self.summary.title.clone(),
                self.summary.number,
                self.summary.author.clone(),
                self.summary.updated_at.clone(),
                self.summary.url.clone(),
            ),
        };
        let (badge_label, badge_tone) = match detail {
            Some(d) => issue_badge(d.state.as_deref(), d.state_reason.as_deref()),
            // The snapshot lists open issues only; the badge says Open
            // until the detail payload says otherwise (PR-page pattern).
            None => ("Open", IssueBadgeTone::Open),
        };
        let comment_count = detail
            .as_ref()
            .map(|d| d.comments.total_count)
            .filter(|n| *n > 0)
            .map(|n| format!("{n} comment{}", if n == 1 { "" } else { "s" }));
        h_flex()
            .id("issue-header")
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
                                    .color(badge_color(badge_tone, cx))
                                    .child(badge_label),
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
                            .child(relative_age(age.as_deref()))
                            .when_some(comment_count, |row, text| row.child(text)),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .when(self.loading, |row| row.child(Spinner::new().small()))
                    .child(
                        Button::new("issue-refresh")
                            .ghost()
                            .small()
                            .icon(IconName::RotateCw)
                            .tooltip("Refresh")
                            .on_click(cx.listener(|this, _, _, cx| this.fetch_detail(cx))),
                    )
                    .child(
                        Button::new("issue-open-github")
                            .ghost()
                            .small()
                            .icon(IconName::ExternalLink)
                            .tooltip("Open on GitHub")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
            )
            .into_any_element()
    }

    /// The conversation (description card → comments → comment box) beside
    /// the rail. No tab bar: issues carry no commits/checks/files (0020
    /// D3). The split uses the PR page's shipped mechanism (`items_start`
    /// + timeline `h_full()`) — the rail hugs its content, never the scroll.
    fn render_body(&self, detail: &Option<gql::IssueDetail>, cx: &Context<Self>) -> AnyElement {
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
        // Description card (0020 D4): the Edit affordance rides the header
        // row when the payload says the viewer may update; while editing,
        // a prefilled Textarea + Save/Cancel replaces the body.
        let description = match &self.description_edit {
            Some(editor) => inline_edit_card(
                "issue-description",
                "Editing description",
                editor,
                self.busy.is_some(),
                cx.listener(|this, _, window, cx| this.save_description(window, cx)),
                cx.listener(|this, _, window, cx| this.cancel_edit_description(window, cx)),
                cx,
            ),
            None => {
                let edit = d
                    .viewer_can_update
                    .then(|| {
                        Button::new("issue-description-edit")
                            .ghost()
                            .small()
                            .label("Edit")
                            .disabled(self.busy.is_some())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.start_edit_description(window, cx)
                            }))
                    })
                    .map(|b| b.into_any_element());
                body_card(
                    "issue-description",
                    &author_login,
                    author_avatar.as_deref(),
                    relative_age(d.created_at.as_deref()),
                    d.body.as_deref(),
                    self.md.get("issue-description").map(Arc::as_ref),
                    edit,
                    cx,
                )
            }
        };
        let mut children = vec![description];
        // docs/0021 D3: comments and events merge into one chronological
        // timeline (stable sort, comment-before-event on equal stamps).
        // Comment keys keep their comments-connection index — the md cache
        // and the 0019 editor swap key on it.
        let rows = sorted_timeline(d);
        let mut event_ix = 0usize;
        let mut comment_ix = 0usize;
        for row in &rows {
            let c = match row {
                TimelineRow::Comment(c) => c,
                TimelineRow::Event(e) => {
                    children.push(self.render_event_row(event_ix, e, cx));
                    event_ix += 1;
                    continue;
                }
            };
            let ix = comment_ix;
            comment_ix += 1;
            let key = format!("issue-comment-{ix}");
            // 0019 pattern: the row swaps to its editor or delete confirm
            // while one is active for this node.
            if let Some(editor) = self.timeline_edit.as_ref().filter(|e| e.0 == c.id) {
                children.push(inline_edit_card(
                    &key,
                    "Editing comment",
                    &editor.1,
                    self.busy.is_some(),
                    cx.listener(|this, _, window, cx| this.save_edit_row(window, cx)),
                    cx.listener(|this, _, window, cx| this.cancel_edit_row(window, cx)),
                    cx,
                ));
                continue;
            }
            if self.delete_confirm.as_deref() == Some(c.id.as_str()) {
                children.push(delete_confirm_card(
                    &key,
                    "Delete this comment?",
                    self.busy.is_some(),
                    cx.listener(|this, _, window, cx| this.confirm_delete_row(window, cx)),
                    cx.listener(|this, _, _, cx| this.cancel_delete_confirm(cx)),
                    cx,
                ));
                continue;
            }
            let mut age = relative_age(c.created_at.as_deref());
            if row_is_edited(c.created_at.as_deref(), c.updated_at.as_deref()) {
                age.push_str(" (edited)");
            }
            children.push(body_card(
                &key,
                c.author
                    .as_ref()
                    .map(|a| a.login.as_str())
                    .unwrap_or("ghost"),
                c.author.as_ref().map(|a| a.avatar_url.as_str()),
                age,
                Some(c.body.as_str()),
                self.md.get(&key).map(Arc::as_ref),
                self.row_actions(
                    &key,
                    c.id.as_str(),
                    &c.body,
                    c.viewer_can_update,
                    c.viewer_can_delete,
                    cx,
                ),
                cx,
            ));
        }
        // docs/0021 Q2: one cap row when either connection was truncated.
        if let Some(cap) = self.render_events_cap_row(d, cx) {
            children.push(cap);
        }
        if d.comments.nodes.is_empty() {
            children.push(muted_centered("No comments yet", cx));
        }
        children.push(self.render_comment_box(cx));
        // The timeline scrolls; the rail hugs its content at the top and
        // never rides along (PR-page, probe-verified mechanism).
        let timeline = div()
            .id("issue-conversation")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .overflow_y_scroll()
            .child(v_flex().gap_3().py_1().children(children));
        h_flex()
            .id("issue-conversation-split")
            .flex_1()
            .min_h_0()
            .items_start()
            .gap_3()
            .child(timeline)
            .child(self.render_rail(d, cx))
            .into_any_element()
    }

    /// Per-row Edit/Delete affordances, gated by the node's own viewer
    /// flags (0019 D3 — the schema's flags, nothing else). Probe key
    /// wraps the buttons; the buttons carry the bridge-visible ids.
    fn row_actions(
        &self,
        prefix: &str,
        node_id: &str,
        body: &str,
        can_update: bool,
        can_delete: bool,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if !can_update && !can_delete {
            return None;
        }
        let busy = self.busy.is_some();
        let node_id = node_id.to_string();
        let body = body.to_string();
        let mut bar = h_flex().gap_1();
        if can_update {
            let id = node_id.clone();
            let body = body.clone();
            bar = bar.child(
                Button::new(format!("{prefix}-edit"))
                    .ghost()
                    .small()
                    .label("Edit")
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.start_edit_row(id.clone(), body.clone(), window, cx)
                    })),
            );
        }
        if can_delete {
            let id = node_id.clone();
            bar =
                bar.child(
                    Button::new(format!("{prefix}-delete"))
                        .ghost()
                        .small()
                        .label("Delete")
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.start_delete_confirm(id.clone(), cx)
                        })),
                );
        }
        Some(
            bar.debug_selector(|| format!("{prefix}-actions"))
                .into_any_element(),
        )
    }

    /// The conversation right rail (0020 D5): labels, assignees, and the
    /// state panel (close with reason / reopen). Fixed ~288px like the PR
    /// page's rail.
    fn render_rail(&self, d: &gql::IssueDetail, cx: &Context<Self>) -> AnyElement {
        v_flex()
            .id("issue-rail")
            .w_72()
            .flex_none()
            .gap_3()
            .child(self.render_labels_panel(d, cx))
            .child(self.render_assignees_panel(d, cx))
            .child(self.render_state_panel(d, cx))
            .into_any_element()
    }

    /// Labels (0020 Q2): muted name chips — the payload's hex color is
    /// user data and stays unused until the token-discipline exception
    /// exists.
    fn render_labels_panel(&self, d: &gql::IssueDetail, cx: &Context<Self>) -> AnyElement {
        GroupBox::new()
            .id("issue-labels-panel")
            .title("Labels")
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_1()
                    .py_1()
                    .when(d.labels.is_empty(), |row| {
                        row.child(empty_text(
                            if d.labels_total > 0 {
                                "…more"
                            } else {
                                "No labels"
                            },
                            cx,
                        ))
                    }),
            )
            .children(d.labels.iter().map(|l| {
                div()
                    .px_2()
                    .py_0p5()
                    .rounded_full()
                    .border_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(l.name.clone())
            }))
            .into_any_element()
    }

    fn render_assignees_panel(&self, d: &gql::IssueDetail, cx: &Context<Self>) -> AnyElement {
        GroupBox::new()
            .id("issue-assignees-panel")
            .title("Assignees")
            .child(v_flex().gap_1().py_1().when(d.assignees.is_empty(), |col| {
                col.child(empty_text("No one assigned", cx))
            }))
            .children(d.assignees.iter().map(|a| {
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Avatar::new().src(a.avatar_url.clone()).xsmall())
                    .child(div().text_sm().child(a.login.clone()))
            }))
            .into_any_element()
    }

    /// The state panel (0020 D5): Close (danger) + close-reason Select
    /// while open, Reopen + the closed line while closed — each gated by
    /// the schema's `viewerCanClose`/`viewerCanReopen` (hidden, not
    /// disabled, when the viewer lacks it — affordance honesty).
    fn render_state_panel(&self, d: &gql::IssueDetail, cx: &Context<Self>) -> AnyElement {
        let busy = self.busy.is_some();
        let state_busy = matches!(self.busy, Some(IssueBusyOp::Close | IssueBusyOp::Reopen));
        let open = d.state.as_deref() == Some("OPEN");
        GroupBox::new()
            .id("issue-state-panel")
            .title("State")
            .child(
                v_flex()
                    .debug_selector(|| "issue-state-panel-body".to_string())
                    .gap_2()
                    .py_1()
                    .when(!open, |col| {
                        col.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "closed {}{}",
                                    relative_age(d.closed_at.as_deref()),
                                    state_reason_word(d.state_reason.as_deref())
                                        .map(|w| format!(" · {w}"))
                                        .unwrap_or_default(),
                                )),
                        )
                    })
                    .when_some(
                        self.mutation_error
                            .as_ref()
                            .filter(|e| e.scope == ErrorScope::Rail),
                        |col, e| {
                            col.child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().danger)
                                    .child(e.message.clone()),
                            )
                        },
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            // Affordance honesty (0020 D5): no gate, no
                            // button — hidden, not disabled.
                            .when(open && d.viewer_can_close, |col| {
                                col.child(
                                    Select::new(&self.close_reason)
                                        .small()
                                        .w_full()
                                        .disabled(busy),
                                )
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .when(state_busy, |row| row.child(Spinner::new().small()))
                                        .child(
                                            Button::new("issue-close")
                                                .label("Close issue")
                                                .danger()
                                                .small()
                                                .disabled(busy)
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.close_issue(window, cx)
                                                })),
                                        ),
                                )
                            })
                            .when(!open && d.viewer_can_reopen, |col| {
                                col.child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .when(state_busy, |row| row.child(Spinner::new().small()))
                                        .child(
                                            Button::new("issue-reopen")
                                                .label("Reopen issue")
                                                .small()
                                                .disabled(busy)
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.reopen_issue(window, cx)
                                                })),
                                        ),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }

    /// docs/0021 D3 — a muted, non-interactive event row: lead sentence,
    /// optional label chip, tail, age. Ids + a probe selector key (0019 §8
    /// lesson) so tests and the debug dump see it.
    fn render_event_row(
        &self,
        ix: usize,
        event: &gql::IssueEvent,
        cx: &Context<Self>,
    ) -> AnyElement {
        let key = format!("issue-event-{ix}");
        let s = event_sentence(event);
        let mut row = h_flex()
            .id(SharedString::from(key.clone()))
            .debug_selector(move || format!("issue-event-{ix}"))
            .flex_wrap()
            .items_baseline()
            .gap_1()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(s.lead);
        if let Some(label) = s.label {
            row = row.child(
                div()
                    .px_2()
                    .py_0p5()
                    .rounded_full()
                    .border_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .child(label),
            );
        }
        if !s.tail.is_empty() {
            row = row.child(s.tail);
        }
        let age = relative_age(event.created_at.as_deref());
        if !age.is_empty() {
            row = row.child(format!("· {age}"));
        }
        row.into_any_element()
    }

    /// docs/0021 Q2 — one cap row when either connection was truncated:
    /// "…+N more on github.com", opening the issue URL. The only interactive
    /// element the event work adds (0010 D10: id + role via Button).
    fn render_events_cap_row(
        &self,
        d: &gql::IssueDetail,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        // #123 / 0021 D3: one row for both capped connections — events
        // and comments are disjoint node sets, so the sum is the honest
        // hidden-row count. The row itself is the shared cap component
        // (0016 §11, also covers the PR conversation).
        let missing = (d.events_total - d.events.len() as i64).max(0)
            + (d.comments.total_count - d.comments.nodes.len() as i64).max(0);
        if missing <= 0 {
            return None;
        }
        Some(cap_row("issue-events-more", missing, &d.url, cx))
    }

    /// The comment box (0020 D4): draft Textarea + submit. A body is
    /// required (same pure fn gates button and handler). Failures keep
    /// the draft and surface an inline error row above the box.
    fn render_comment_box(&self, cx: &Context<Self>) -> AnyElement {
        let body_empty = self.comment.read(cx).value().trim().is_empty();
        let allowed = comment_submit_allowed(body_empty, self.busy.is_some());
        let busy = self.busy.is_some();
        let sending = matches!(self.busy, Some(IssueBusyOp::Comment));
        let tooltip = if !allowed {
            "Write a comment first"
        } else if busy {
            "Sending"
        } else {
            "Post to GitHub"
        };
        v_flex()
            .id("issue-comment-box")
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .p_3()
            .when_some(
                self.mutation_error
                    .as_ref()
                    .filter(|e| e.scope == ErrorScope::Timeline),
                |col, e| {
                    col.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().danger)
                            .child(e.message.clone()),
                    )
                },
            )
            .child(Textarea::new(&self.comment).w_full())
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .justify_end()
                    .when(sending, |row| row.child(Spinner::new().small()))
                    .child(
                        Button::new("issue-comment-submit")
                            .label("Comment")
                            .primary()
                            .small()
                            .disabled(busy || !allowed)
                            .tooltip(tooltip)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.submit_comment(window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }
}

impl Focusable for IssueDetailPage {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for IssueDetailPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let detail = self.detail.clone();
        v_flex()
            .id("issue-detail")
            .key_context("GithubIssuePage")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &DismissIssuePage, _, cx| {
                this.request_close(cx);
            }))
            .size_full()
            .min_w_0()
            .p_4()
            .gap_3()
            .child(self.render_header(&detail, cx))
            .child(self.render_body(&detail, cx))
    }
}

// ---------------------------------------------------------------------------
// Pure helpers — table-tested.
// ---------------------------------------------------------------------------

/// Header badge tone (0020 D6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IssueBadgeTone {
    Open,
    Closed,
    Unknown,
}

/// State × stateReason → badge copy + tone (0020 D6). The REOPENED
/// reason only appears while open — the badge stays bare "Open".
/// Pure — table-tested.
pub(crate) fn issue_badge(
    state: Option<&str>,
    reason: Option<&str>,
) -> (&'static str, IssueBadgeTone) {
    match state {
        Some("OPEN") => ("Open", IssueBadgeTone::Open),
        Some("CLOSED") => match reason {
            Some("NOT_PLANNED") => ("Closed · not planned", IssueBadgeTone::Closed),
            Some("DUPLICATE") => ("Closed · duplicate", IssueBadgeTone::Closed),
            _ => ("Closed", IssueBadgeTone::Closed),
        },
        _ => ("Unknown", IssueBadgeTone::Unknown),
    }
}

fn badge_color(tone: IssueBadgeTone, cx: &Context<IssueDetailPage>) -> Hsla {
    match tone {
        IssueBadgeTone::Open => cx.theme().success,
        IssueBadgeTone::Closed => cx.theme().danger,
        IssueBadgeTone::Unknown => cx.theme().muted_foreground,
    }
}

/// `stateReason` → the lowercase word the rail's closed line carries
/// (0020 D5); COMPLETED is GitHub's silent default — no word. Pure.
pub(crate) fn state_reason_word(reason: Option<&str>) -> Option<&'static str> {
    match reason {
        Some("NOT_PLANNED") => Some("not planned"),
        Some("DUPLICATE") => Some("duplicate"),
        _ => None,
    }
}

/// The two close reasons the panel offers (0020 Q3) — wire value and
/// GitHub's own label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseReason {
    Completed,
    NotPlanned,
}

impl CloseReason {
    fn as_str(self) -> &'static str {
        match self {
            CloseReason::Completed => "COMPLETED",
            CloseReason::NotPlanned => "NOT_PLANNED",
        }
    }

    fn label(self) -> &'static str {
        match self {
            CloseReason::Completed => "Completed",
            CloseReason::NotPlanned => "Not planned",
        }
    }
}

/// Select label → reason; unknown labels fall back to Completed at the
/// call site. Pure — table-tested.
pub(crate) fn close_reason_from_label(label: &str) -> Option<CloseReason> {
    match label {
        "Completed" => Some(CloseReason::Completed),
        "Not planned" => Some(CloseReason::NotPlanned),
        _ => None,
    }
}

/// The rail's Close button gate (0020 D5): open, the viewer may close,
/// nothing in flight. Pure — table-tested.
pub(crate) fn close_allowed(state: Option<&str>, can_close: bool, busy: bool) -> bool {
    state == Some("OPEN") && can_close && !busy
}

/// The rail's Reopen button gate (0020 D5). Pure — table-tested.
pub(crate) fn reopen_allowed(state: Option<&str>, can_reopen: bool, busy: bool) -> bool {
    state == Some("CLOSED") && can_reopen && !busy
}

/// The comment box's submit gate (0020 D4): a body is required and
/// nothing may be in flight (same pure fn gates button and handler).
/// Pure — table-tested.
pub(crate) fn comment_submit_allowed(body_empty: bool, busy: bool) -> bool {
    !body_empty && !busy
}

/// A row shows "(edited)" when the payload's `updatedAt` is strictly
/// newer than its created stamp (0019 D4). Pure — table-tested.
pub(crate) fn row_is_edited(created: Option<&str>, updated: Option<&str>) -> bool {
    fn stamp(ts: Option<&str>) -> Option<i64> {
        ts.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.timestamp())
    }
    match (stamp(created), stamp(updated)) {
        (Some(c), Some(u)) => u > c,
        _ => false,
    }
}

// --- docs/0021 — timeline events -------------------------------------------

/// docs/0021 D3 — one merged conversation row: a comment or an event.
pub(crate) enum TimelineRow<'a> {
    Comment(&'a gql::PrComment),
    Event(&'a gql::IssueEvent),
}

/// docs/0021 D3 — comments and events interleaved chronologically.
/// Stable sort; missing stamps sort first (0016 tolerance); equal stamps
/// keep comments before events.
pub(crate) fn sorted_timeline(d: &gql::IssueDetail) -> Vec<TimelineRow<'_>> {
    let mut rows: Vec<TimelineRow> = d
        .comments
        .nodes
        .iter()
        .map(TimelineRow::Comment)
        .chain(d.events.iter().map(TimelineRow::Event))
        .collect();
    rows.sort_by_key(row_key);
    rows
}

fn row_key(row: &TimelineRow<'_>) -> (Option<i64>, u8) {
    let (stamp, rank) = match row {
        TimelineRow::Comment(c) => (c.created_at.as_deref(), 0),
        TimelineRow::Event(e) => (e.created_at.as_deref(), 1),
    };
    (parse_stamp(stamp), rank)
}

fn parse_stamp(ts: Option<&str>) -> Option<i64> {
    ts.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.timestamp())
}

/// docs/0021 D3 — muted row copy. A label name renders as the rail's chip
/// (Q3), so it rides in its own slot between lead and tail.
pub(crate) struct EventSentence {
    pub lead: String,
    pub label: Option<String>,
    pub tail: String,
}

pub(crate) fn event_sentence(event: &gql::IssueEvent) -> EventSentence {
    let actor = event.actor_login.as_deref().unwrap_or("Someone");
    let plain = |verb: &str, tail: String| EventSentence {
        lead: format!("{actor} {verb}"),
        label: None,
        tail,
    };
    match &event.kind {
        gql::IssueEventKind::Assigned { login } => plain("assigned", login.clone()),
        gql::IssueEventKind::Unassigned { login } => plain("unassigned", login.clone()),
        gql::IssueEventKind::Labeled { label } => EventSentence {
            lead: format!("{actor} added the"),
            label: Some(label.clone()),
            tail: "label".into(),
        },
        gql::IssueEventKind::Unlabeled { label } => EventSentence {
            lead: format!("{actor} removed the"),
            label: Some(label.clone()),
            tail: "label".into(),
        },
        // One vocabulary with the header badge (0020 Q2 leans):
        // "closed as not planned" / "closed".
        gql::IssueEventKind::Closed { reason } => plain(
            "closed",
            state_reason_word(reason.as_deref())
                .map(|w| format!("as {w}"))
                .unwrap_or_default(),
        ),
        gql::IssueEventKind::Reopened => plain("reopened", String::new()),
        gql::IssueEventKind::Renamed { from, to } => {
            plain("changed the title", format!("from {from} to {to}"))
        }
        gql::IssueEventKind::Milestoned { title } => {
            plain("added this to the", format!("{title} milestone"))
        }
        gql::IssueEventKind::Demilestoned { title } => {
            plain("removed this from the", format!("{title} milestone"))
        }
    }
}

/// docs/0017 D6 for the issue page: parse every timeline body once per
/// detail payload — never per frame. Rebuilt (invalidating) each time a
/// detail lands.
fn build_issue_md_cache(d: &gql::IssueDetail) -> BTreeMap<String, Arc<[markdown::Block]>> {
    let mut map = BTreeMap::new();
    if let Some(body) = &d.body {
        map.insert(
            "issue-description".to_string(),
            markdown::parse_markdown(body),
        );
    }
    for (ix, comment) in d.comments.nodes.iter().enumerate() {
        map.insert(
            format!("issue-comment-{ix}"),
            markdown::parse_markdown(&comment.body),
        );
    }
    map
}

fn skeleton_list() -> AnyElement {
    v_flex()
        .id("issue-skeleton")
        .flex_1()
        .min_h_0()
        .gap_3()
        .py_1()
        .child(Skeleton::new())
        .child(Skeleton::new().w_3_4())
        .child(Skeleton::new().w_1_2())
        .into_any_element()
}

fn error_cell(message: &str, cx: &Context<IssueDetailPage>) -> AnyElement {
    v_flex()
        .id("issue-load-error")
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

fn muted_centered(text: &str, cx: &Context<IssueDetailPage>) -> AnyElement {
    div()
        .id("issue-muted")
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

/// Inline muted text for an empty rail group.
fn empty_text(text: &str, cx: &Context<IssueDetailPage>) -> AnyElement {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.to_string())
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Tests (docs/0020 §7) — pure-fn tables + real-page probes following the
// 0018/0019 pattern.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // No `use super::*` — page.rs documents the gpui::* shadowing hazard.
    use crate::gql;
    use crate::issue_page::{
        CloseReason, IssueBadgeTone, IssueBusyOp, IssueDetailPage, TimelineRow,
        build_issue_md_cache, close_allowed, close_reason_from_label, comment_submit_allowed,
        event_sentence, issue_badge, reopen_allowed, row_is_edited, sorted_timeline,
        state_reason_word,
    };
    use crate::service::{GithubCache, GithubState, Kind, Mutation, SpecResult};
    use gpui::AppContext as _;

    // --- pure fns ----------------------------------------------------------

    #[test]
    fn badge_copy_table() {
        // Open issues wear a bare Open badge — including the REOPENED
        // reason, which only appears while open.
        assert_eq!(
            issue_badge(Some("OPEN"), None),
            ("Open", IssueBadgeTone::Open)
        );
        assert_eq!(
            issue_badge(Some("OPEN"), Some("REOPENED")),
            ("Open", IssueBadgeTone::Open)
        );
        // Closed carries the reason where GitHub shows it.
        assert_eq!(
            issue_badge(Some("CLOSED"), Some("COMPLETED")),
            ("Closed", IssueBadgeTone::Closed)
        );
        assert_eq!(
            issue_badge(Some("CLOSED"), Some("NOT_PLANNED")),
            ("Closed · not planned", IssueBadgeTone::Closed)
        );
        assert_eq!(
            issue_badge(Some("CLOSED"), Some("DUPLICATE")),
            ("Closed · duplicate", IssueBadgeTone::Closed)
        );
        assert_eq!(
            issue_badge(Some("CLOSED"), None),
            ("Closed", IssueBadgeTone::Closed)
        );
        // Degraded payload → unknown, not a wrong state.
        assert_eq!(
            issue_badge(None, None),
            ("Unknown", IssueBadgeTone::Unknown)
        );
    }

    #[test]
    fn state_reason_word_table() {
        assert_eq!(state_reason_word(Some("NOT_PLANNED")), Some("not planned"));
        assert_eq!(state_reason_word(Some("DUPLICATE")), Some("duplicate"));
        // COMPLETED is GitHub's silent default — no word; REOPENED is an
        // open-state reason and never reaches the closed line.
        assert_eq!(state_reason_word(Some("COMPLETED")), None);
        assert_eq!(state_reason_word(Some("REOPENED")), None);
        assert_eq!(state_reason_word(None), None);
    }

    #[test]
    fn close_reopen_gating_table() {
        // Close: open + viewerCanClose + idle.
        assert!(close_allowed(Some("OPEN"), true, false));
        assert!(!close_allowed(Some("OPEN"), false, false));
        assert!(!close_allowed(Some("CLOSED"), true, false));
        assert!(!close_allowed(None, true, false));
        assert!(!close_allowed(Some("OPEN"), true, true));
        // Reopen: closed + viewerCanReopen + idle.
        assert!(reopen_allowed(Some("CLOSED"), true, false));
        assert!(!reopen_allowed(Some("CLOSED"), false, false));
        assert!(!reopen_allowed(Some("OPEN"), true, false));
        assert!(!reopen_allowed(Some("CLOSED"), true, true));
    }

    #[test]
    fn comment_submit_gating_table() {
        assert!(comment_submit_allowed(false, false));
        assert!(!comment_submit_allowed(true, false), "empty body");
        assert!(!comment_submit_allowed(false, true), "busy");
        assert!(!comment_submit_allowed(true, true));
    }

    #[test]
    fn close_reason_from_label_table() {
        assert_eq!(
            close_reason_from_label("Completed"),
            Some(CloseReason::Completed)
        );
        assert_eq!(
            close_reason_from_label("Not planned"),
            Some(CloseReason::NotPlanned)
        );
        assert_eq!(close_reason_from_label("nonsense"), None);
        // Wire values round-trip through the labels the Select shows.
        assert_eq!(CloseReason::Completed.as_str(), "COMPLETED");
        assert_eq!(CloseReason::NotPlanned.as_str(), "NOT_PLANNED");
    }

    #[test]
    fn row_is_edited_table() {
        assert!(row_is_edited(
            Some("2026-09-01T00:00:00Z"),
            Some("2026-09-02T00:00:00Z")
        ));
        // Same stamp or missing stamps are never "(edited)".
        assert!(!row_is_edited(
            Some("2026-09-01T00:00:00Z"),
            Some("2026-09-01T00:00:00Z")
        ));
        assert!(!row_is_edited(Some("2026-09-01T00:00:00Z"), None));
        assert!(!row_is_edited(None, Some("2026-09-01T00:00:00Z")));
    }

    // --- probes ------------------------------------------------------------

    struct ProbeHttp;
    impl crate::http::Http for ProbeHttp {
        fn request(
            &self,
            _: crate::http::Verb,
            _: &str,
            _: Option<&str>,
            _: Option<&str>,
        ) -> Result<crate::http::HttpResult, crate::http::HttpError> {
            Err(crate::http::HttpError("probe: no network".into()))
        }
        fn post_graphql(
            &self,
            _: &str,
            _: &serde_json::Value,
        ) -> Result<crate::http::HttpResult, crate::http::HttpError> {
            Err(crate::http::HttpError("probe: no network".into()))
        }
    }

    struct ProbeStore;
    impl crate::auth::TokenStore for ProbeStore {
        fn save(&self, _: &crate::auth::StoredCredential) -> Result<(), String> {
            Err("probe".into())
        }
        fn load(&self) -> Result<Option<crate::auth::StoredCredential>, String> {
            Ok(None)
        }
        fn delete(&self) -> Result<(), String> {
            Err("probe".into())
        }
    }

    fn probe_summary() -> gql::Issue {
        gql::Issue {
            key: "o/r#487".into(),
            database_id: None,
            number: 487,
            title: "probe issue".into(),
            author: None,
            updated_at: None,
            url: "https://probe".into(),
        }
    }

    /// One gated comment (update-only), one unflagged; the issue itself
    /// carries every viewer gate (0020 D3/D4/D5).
    fn probe_issue() -> gql::IssueDetail {
        let value = serde_json::json!({ "issue": {
            "id": "I_9", "number": 487, "title": "probe issue", "url": "u",
            "state": "OPEN", "stateReason": null,
            "createdAt": "2026-09-01T00:00:00Z",
            "viewerCanUpdate": true, "viewerCanClose": true, "viewerCanReopen": false,
            "labels": {"totalCount": 1, "nodes": [{"name": "bug", "color": "ff0000"}]},
            "assignees": {"totalCount": 1, "nodes": [{"login": "bob", "avatarUrl": "b"}]},
            "comments": {"totalCount": 2, "nodes": [
                {"id": "IC_1", "viewerCanUpdate": true, "viewerCanDelete": false,
                 "updatedAt": "2026-09-03T02:00:00Z",
                 "author": {"login": "dave"}, "body": "nice",
                 "createdAt": "2026-09-03T01:00:00Z"},
                {"author": {"login": "erin"}, "body": "plain",
                 "createdAt": "2026-09-04T01:00:00Z"}
            ]},
            "timelineItems": {"totalCount": 2, "filteredCount": 2, "nodes": [
                {"__typename": "LabeledEvent", "id": "EV_1",
                 "createdAt": "2026-09-02T01:00:00Z",
                 "actor": {"login": "alice"}, "label": {"name": "bug"}},
                {"__typename": "AssignedEvent", "id": "EV_2",
                 "createdAt": "2026-09-05T01:00:00Z",
                 "actor": {"login": "alice"},
                 "assignee": {"__typename": "User", "login": "bob"}}
            ]}
        }});
        gql::issue_detail_from_value(&value).expect("probe issue parses")
    }

    fn probe_page(
        cx: &mut gpui::TestAppContext,
    ) -> (gpui::Entity<IssueDetailPage>, &mut gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        let state = cx.update(|cx| {
            let client: std::sync::Arc<dyn crate::http::Http> = std::sync::Arc::new(ProbeHttp);
            let store: std::sync::Arc<dyn crate::auth::TokenStore> =
                std::sync::Arc::new(ProbeStore);
            cx.new(|_| {
                GithubState::new(
                    client,
                    store,
                    Default::default(),
                    Default::default(),
                    GithubCache::new(dashboard_core::paths::AppPaths::init()),
                )
            })
        });
        cx.add_window_view(|window, cx| {
            let mut page = IssueDetailPage::new(state, "o/r".into(), probe_summary(), window, cx);
            page.detail = Some(probe_issue());
            page.md = build_issue_md_cache(page.detail.as_ref().unwrap());
            page
        })
    }

    #[gpui::test]
    fn issue_affordances_follow_viewer_flags(cx: &mut gpui::TestAppContext) {
        let (page, vcx) = probe_page(cx);
        for sel in ["issue-comment-0-actions", "issue-comment-1-actions"] {
            eprintln!("[probe] {sel}={:?}", vcx.debug_bounds(sel));
        }
        // Gated rows carry their affordances; unflagged rows stay clean
        // (0019 D3 via 0020 D4).
        assert!(vcx.debug_bounds("issue-comment-0-actions").is_some());
        assert!(vcx.debug_bounds("issue-comment-1-actions").is_none());

        // Edit mode swaps the body for the editor card (0020 D4).
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.start_edit_row("IC_1".into(), "nice".into(), window, cx)
            });
        });
        assert!(vcx.debug_bounds("issue-comment-0-card").is_some());
        assert!(vcx.debug_bounds("issue-comment-0-actions").is_none());

        // One editor at a time: the description editor yields the row
        // editor (0020 D4).
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| page.start_edit_description(window, cx));
        });
        assert!(vcx.debug_bounds("issue-comment-0-card").is_none());
        assert!(vcx.debug_bounds("issue-description-card").is_some());

        // The delete confirm yields every editor and swaps the row.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.cancel_edit_description(window, cx);
                page.start_delete_confirm("IC_1".into(), cx);
            });
        });
        assert!(vcx.debug_bounds("issue-comment-0-confirm-card").is_some());
        assert!(vcx.debug_bounds("issue-comment-0-actions").is_none());
        assert!(vcx.debug_bounds("issue-description-card").is_none());

        // Guards: busy blocks arming; an empty node id opens no editor.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.cancel_delete_confirm(cx);
                page.busy = Some(IssueBusyOp::Comment);
                page.start_delete_confirm("IC_1".into(), cx);
                assert!(
                    page.delete_confirm.is_none(),
                    "busy must block arming the confirm"
                );
                page.busy = None;
                page.start_delete_confirm("IC_1".into(), cx);
                assert_eq!(page.delete_confirm.as_deref(), Some("IC_1"));
                page.start_edit_row("".into(), String::new(), window, cx);
                assert!(
                    page.timeline_edit.is_none(),
                    "empty node id must not open an editor"
                );
                page.timeline_edit = None;
            });
        });
    }

    #[gpui::test]
    fn detail_apply_drops_editors_of_vanished_nodes(cx: &mut gpui::TestAppContext) {
        let (page, vcx) = probe_page(cx);
        // Open an editor on IC_1 (the surface the refetch race acts on).
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.start_edit_row("IC_1".into(), "nice".into(), window, cx)
            });
        });
        // Editing IC_1; a refetch that still carries IC_1 keeps the editor
        // (0020 D4), one without it drops it. apply_detail_result parses
        // the full envelope — wrap the node.
        let keep = r#"{"data":{"repository":{"issue":{"id":"I_9",
            "number":487,"title":"t","url":"u","state":"OPEN",
            "comments":{"totalCount":1,"nodes":[{"id":"IC_1",
            "viewerCanUpdate":true,"viewerCanDelete":false,
            "author":{"login":"dave"},"body":"nice (edited)",
            "createdAt":"2026-09-03T01:00:00Z"}]}}}}}"#;
        let gone = r#"{"data":{"repository":{"issue":{"id":"I_9",
            "number":487,"title":"t","url":"u","state":"OPEN",
            "comments":{"totalCount":0,"nodes":[]}}}}}"#;
        let result_with = SpecResult {
            kind: Kind::IssueDetail,
            ident: Some("o/r".into()),
            status: 200,
            etag: None,
            rate: None,
            body: Some(keep.to_string()),
            network: None,
            cursor: None,
        };
        let result_gone = SpecResult {
            kind: Kind::IssueDetail,
            ident: Some("o/r".into()),
            status: 200,
            etag: None,
            rate: None,
            body: Some(gone.to_string()),
            network: None,
            cursor: None,
        };
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                page.apply_detail_result(result_with, cx);
                assert!(page.timeline_edit.is_some(), "node still present");
                page.apply_detail_result(result_gone, cx);
                assert!(page.timeline_edit.is_none(), "node gone — editor dropped");
            });
        });
    }

    #[gpui::test]
    fn mutation_results_scope_and_clear_their_surfaces(cx: &mut gpui::TestAppContext) {
        let (page, vcx) = probe_page(cx);
        let ok_body = r#"{"data":{"addComment":{"clientMutationId":"x"}}}"#;
        let err_body = r#"{"data":null,"errors":[{"message":"Issue is locked"}]}"#;
        let ok = |body: &str| SpecResult {
            kind: Kind::PrMutation(Mutation::Comment),
            ident: Some("o/r#487".into()),
            status: 200,
            etag: None,
            rate: None,
            body: Some(body.to_string()),
            network: None,
            cursor: None,
        };
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                // A posted comment clears the draft (0020 D4).
                page.comment
                    .update(cx, |state, cx| state.set_value("draft", window, cx));
                page.apply_mutation_result(IssueBusyOp::Comment, ok(ok_body), window, cx);
                assert!(page.mutation_error.is_none());
                assert!(
                    page.comment.read(cx).value().trim().is_empty(),
                    "success must clear the draft"
                );

                // A 200 with GraphQL errors keeps the draft and lands in
                // the timeline scope (0020 D4).
                page.comment
                    .update(cx, |state, cx| state.set_value("kept", window, cx));
                page.apply_mutation_result(IssueBusyOp::Comment, ok(err_body), window, cx);
                let err = page.mutation_error.as_ref().expect("scoped failure");
                assert_eq!(err.scope, crate::pr_page::ErrorScope::Timeline);
                assert_eq!(err.message, "Issue is locked");
                assert_eq!(
                    page.comment.read(cx).value(),
                    "kept",
                    "failures keep the draft"
                );
                assert!(page.busy.is_none(), "the write is no longer in flight");

                // A state-write failure lands in the rail scope instead
                // (0020 D5) — same result plumbing, different surface.
                page.apply_mutation_result(IssueBusyOp::Close, ok(err_body), window, cx);
                assert_eq!(
                    page.mutation_error.as_ref().expect("rail failure").scope,
                    crate::pr_page::ErrorScope::Rail
                );
            });
        });
    }

    // --- docs/0021: merged timeline ---------------------------------------

    fn timeline_fixture() -> gql::IssueDetail {
        let value = serde_json::json!({ "issue": {
            "id": "I_8", "number": 5, "title": "t", "url": "u", "state": "OPEN",
            "comments": {"totalCount": 2, "nodes": [
                {"id": "C_LATE", "author": {"login": "erin"}, "body": "late",
                 "createdAt": "2026-09-03T00:00:00Z"},
                {"id": "C_FIRST", "author": {"login": "dave"}, "body": "first",
                 "createdAt": "2026-09-01T00:00:00Z"}
            ]},
            "timelineItems": {"totalCount": 4, "filteredCount": 4, "nodes": [
                {"__typename": "AssignedEvent", "id": "E_TIE",
                 "createdAt": "2026-09-01T00:00:00Z",
                 "actor": {"login": "alice"},
                 "assignee": {"__typename": "User", "login": "bob"}},
                {"__typename": "LabeledEvent", "id": "E_MID",
                 "createdAt": "2026-09-02T00:00:00Z",
                 "actor": {"login": "alice"}, "label": {"name": "bug"}},
                {"__typename": "ReopenedEvent", "id": "E_NOSTAMP", "actor": null}
            ]}
        }});
        gql::issue_detail_from_value(&value).expect("timeline fixture parses")
    }

    fn row_ids(d: &gql::IssueDetail) -> Vec<String> {
        sorted_timeline(d)
            .iter()
            .map(|row| match row {
                TimelineRow::Comment(c) => format!("c:{}", c.id),
                TimelineRow::Event(e) => format!("e:{}", e.id),
            })
            .collect()
    }

    #[test]
    fn timeline_sort_interleave_table() {
        // Missing stamps sort first; equal stamps keep the comment before
        // the event; the rest interleaves chronologically.
        assert_eq!(
            row_ids(&timeline_fixture()),
            [
                "e:E_NOSTAMP",
                "c:C_FIRST", // 09-01T00:00 comment wins the tie over E_TIE
                "e:E_TIE",
                "e:E_MID",  // 09-02
                "c:C_LATE", // 09-03
            ]
        );
    }

    #[test]
    fn event_sentence_table() {
        let d = timeline_fixture();
        let events: Vec<&gql::IssueEvent> = d.events.iter().collect();
        // Ghost actor degrades to Someone; assigned copies the login.
        let ghost = &events[2];
        let s = event_sentence(ghost);
        assert_eq!(s.lead, "Someone reopened");
        assert_eq!(s.tail, "");
        let assigned = &events[0];
        let s = event_sentence(assigned);
        assert_eq!(s.lead, "alice assigned");
        assert_eq!(s.label, None);
        assert_eq!(s.tail, "bob");
        let labeled = &events[1];
        let s = event_sentence(labeled);
        assert_eq!(s.lead, "alice added the");
        assert_eq!(s.label.as_deref(), Some("bug"));
        assert_eq!(s.tail, "label");

        // Closed copy shares the badge vocabulary (0020 Q2 leans).
        let closed = |reason: Option<&str>| {
            let value = serde_json::json!({ "issue": {
                "id": "I", "number": 1, "title": "t", "url": "u", "state": "OPEN",
                "timelineItems": {"totalCount": 1, "filteredCount": 1, "nodes": [
                    {"__typename": "ClosedEvent", "id": "E", "createdAt": "2026-09-01T00:00:00Z",
                     "actor": {"login": "alice"}, "stateReason": reason}
                ]}
            }});
            let d = gql::issue_detail_from_value(&value).unwrap();
            event_sentence(&d.events[0])
        };
        assert_eq!(closed(Some("NOT_PLANNED")).tail, "as not planned");
        assert_eq!(closed(None).tail, "");
        assert_eq!(closed(None).lead, "alice closed");
    }

    #[gpui::test]
    fn events_render_in_order_with_cap_row(cx: &mut gpui::TestAppContext) {
        let (page, vcx) = probe_page(cx);
        // probe_issue(): EV_1 (09-02) < IC_1 (09-03) < IC_2 (09-04) < EV_2
        // (09-05). Rows are measurable through their probe selector keys;
        // comment bodies are measured via the actions bar (0019 §8).
        assert!(vcx.debug_bounds("issue-event-0").is_some());
        assert!(vcx.debug_bounds("issue-event-1").is_some());
        let ev0 = vcx.debug_bounds("issue-event-0").unwrap();
        let ic0 = vcx.debug_bounds("issue-comment-0-actions").unwrap();
        let ev1 = vcx.debug_bounds("issue-event-1").unwrap();
        assert!(ev0.origin.y < ic0.origin.y, "first event above IC_1");
        assert!(ic0.origin.y < ev1.origin.y, "second event below IC_1");
        // Uncapped connections → no cap row.
        assert!(vcx.debug_bounds("issue-events-more").is_none());

        // A refetch with filteredCount beyond the page arms the cap row,
        // and events never disturb the 0019 comment editor swap.
        let capped = r#"{"data":{"repository":{"issue":{"id":"I_9",
            "number":487,"title":"t","url":"u","state":"OPEN",
            "comments":{"totalCount":1,"nodes":[{"id":"IC_1",
            "viewerCanUpdate":true,"viewerCanDelete":false,
            "author":{"login":"dave"},"body":"nice",
            "createdAt":"2026-09-03T01:00:00Z"}]},
            "timelineItems":{"totalCount":210,"filteredCount":105,
            "nodes":[]}}}}}"#;
        let result = SpecResult {
            kind: Kind::IssueDetail,
            ident: Some("o/r".into()),
            status: 200,
            etag: None,
            rate: None,
            body: Some(capped.to_string()),
            network: None,
            cursor: None,
        };
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| page.apply_detail_result(result, cx));
        });
        assert!(vcx.debug_bounds("issue-events-more").is_some());
        // #123: comments-only truncation arms the same row — the events
        // page is empty (filteredCount 0) and the count is the comment
        // shortfall alone (105 - 1 visible).
        let comments_capped = r#"{"data":{"repository":{"issue":{"id":"I_9",
            "number":487,"title":"t","url":"u","state":"OPEN",
            "comments":{"totalCount":105,"nodes":[{"id":"IC_1",
            "viewerCanUpdate":true,"viewerCanDelete":false,
            "author":{"login":"dave"},"body":"nice",
            "createdAt":"2026-09-03T01:00:00Z"}]},
            "timelineItems":{"totalCount":0,"filteredCount":0,
            "nodes":[]}}}}}"#;
        let result = SpecResult {
            kind: Kind::IssueDetail,
            ident: Some("o/r".into()),
            status: 200,
            etag: None,
            rate: None,
            body: Some(comments_capped.to_string()),
            network: None,
            cursor: None,
        };
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| page.apply_detail_result(result, cx));
        });
        assert!(vcx.debug_bounds("issue-events-more").is_some());
        // Events gone with the empty page; the editor still swaps (0021 D4).
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.start_edit_row("IC_1".into(), "nice".into(), window, cx)
            });
        });
        assert!(vcx.debug_bounds("issue-comment-0-card").is_some());
        assert!(vcx.debug_bounds("issue-event-0").is_none());
    }
}
