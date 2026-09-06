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
    checkbox::Checkbox,
    group_box::GroupBox,
    h_flex,
    input::{Textarea, TextareaState},
    menu::{DropdownMenu as _, PopupMenuItem},
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
    /// Prefilled textarea while the title is being edited (0022 D4).
    title_edit: Option<Entity<TextareaState>>,
    /// The open rail picker (0022 D5) — at most one anywhere on the
    /// page (D7 widens the 0019 one-editor rule to five surfaces).
    rail_edit: Option<RailEditor>,
    /// Picker source pages (0022 D5) — lazy, session-only, the
    /// files-sidebar pattern.
    labels_source: PickerFetch<Vec<gql::IssueLabel>>,
    assignees_source: PickerFetch<Vec<gql::IssueAssignee>>,
    milestones_source: PickerFetch<Vec<gql::IssueMilestone>>,
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
    // 0022 D1/D4/D5 — the metadata writes. All four post the shared
    // updateIssue document; failures surface in the rail (D2).
    SetTitle,
    SetLabels,
    SetAssignees,
    SetMilestone,
    // 0023 D3/D5 — the reaction toggle (add and remove share one op
    // identity; the Mutation differs) and the conversation lock flip.
    ToggleReaction,
    ToggleLock,
}

/// docs/0022 D5 — the open picker and its draft choice. The draft is
/// plain page state. Rows are frozen at open (re-aligned by id when a
/// source page or detail payload lands) so checked flags can never
/// misalign mid-edit.
#[derive(Debug, PartialEq)]
enum RailEditor {
    Labels(PickerDraft),
    Assignees(PickerDraft),
    /// `chosen: None` = "No milestone" — the clearing choice (§2.1).
    Milestone {
        rows: Vec<PickerRow>,
        chosen: Option<String>,
    },
}

/// One checklist row: the write-target node id plus display data.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PickerRow {
    /// Empty for the milestone picker's "No milestone" row — never a
    /// write target (it maps to `milestoneId: null`).
    pub id: String,
    pub label: String,
    /// Assignee rows carry the avatar; label/milestone rows none.
    pub avatar_url: Option<String>,
}

/// Checked flags aligned 1:1 with [`PickerRow`] lists.
#[derive(Debug, PartialEq)]
struct PickerDraft {
    rows: Vec<PickerRow>,
    checked: Vec<bool>,
}

/// The picker source fetch lifecycle (0022 D5) — the files-sidebar
/// `FilesFetch` pattern, generic over the parsed page.
#[derive(Debug, Default)]
enum PickerFetch<T> {
    #[default]
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> PickerFetch<T> {
    fn failed_message(&self) -> Option<&str> {
        match self {
            PickerFetch::Failed(m) => Some(m),
            _ => None,
        }
    }
}

/// Which picker source page to (lazily) fetch (0022 D5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PickerSource {
    Labels,
    Assignees,
    Milestones,
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
            // 0022 D2: every metadata failure surfaces in the rail —
            // title included, its editor lives in the header but the
            // failure copy reads better beside the other metadata
            // errors (single rail error slot, 0020 D5).
            // 0023 D3/D5: reaction toggle failures read in the
            // conversation (the chips row), lock failures in the rail.
            IssueBusyOp::ToggleReaction => ErrorScope::Timeline,
            IssueBusyOp::ToggleLock => ErrorScope::Rail,
            IssueBusyOp::SetTitle
            | IssueBusyOp::SetLabels
            | IssueBusyOp::SetAssignees
            | IssueBusyOp::SetMilestone => ErrorScope::Rail,
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
            title_edit: None,
            rail_edit: None,
            labels_source: PickerFetch::Idle,
            assignees_source: PickerFetch::Idle,
            milestones_source: PickerFetch::Idle,
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
                            // 0022 D5: a fresh payload under an open
                            // picker re-aligns its frozen draft by id —
                            // checked flags can never silently misalign.
                            let open_picker = match self.rail_edit {
                                Some(RailEditor::Labels(_)) => Some(PickerSource::Labels),
                                Some(RailEditor::Assignees(_)) => Some(PickerSource::Assignees),
                                Some(RailEditor::Milestone { .. }) => {
                                    Some(PickerSource::Milestones)
                                }
                                None => None,
                            };
                            if let Some(source) = open_picker {
                                self.realign_open_picker(source, cx);
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
        // One editor at a time (0022 D7): every other surface yields.
        self.yield_all_editors();
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
        self.yield_all_editors();
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
        self.yield_all_editors();
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

    // --- metadata editing (docs/0022) ---------------------------------------

    /// D7: every editor surface yields — called before opening any of
    /// them, so at most one is ever open (0019 rule, five surfaces).
    fn yield_all_editors(&mut self) {
        self.description_edit = None;
        self.title_edit = None;
        self.timeline_edit = None;
        self.delete_confirm = None;
        self.rail_edit = None;
    }

    /// D7: true while any of the five editor surfaces is open — every
    /// affordance hides while this holds (hidden, not disabled).
    fn any_editor_open(&self) -> bool {
        self.description_edit.is_some()
            || self.title_edit.is_some()
            || self.timeline_edit.is_some()
            || self.delete_confirm.is_some()
            || self.rail_edit.is_some()
    }

    /// Esc consumes the topmost open editor (D4/D5 Cancel/Esc) and only
    /// closes the page when nothing is open. Priority: the delete
    /// confirm (most destructive to leave up) first.
    fn cancel_top_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.delete_confirm.take().is_some() {
            cx.notify();
            return true;
        }
        if self.timeline_edit.take().is_some() {
            self.refocus_page(window, cx);
            cx.notify();
            return true;
        }
        if self.description_edit.take().is_some() || self.title_edit.take().is_some() {
            self.mutation_error = None;
            self.refocus_page(window, cx);
            cx.notify();
            return true;
        }
        if self.rail_edit.take().is_some() {
            self.mutation_error = None;
            cx.notify();
            return true;
        }
        false
    }

    /// Open the title editor (D4), prefilled with the current title.
    fn start_edit_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.title_edit.is_some() {
            return;
        }
        self.yield_all_editors();
        let title = self
            .detail
            .as_ref()
            .map(|d| d.title.clone())
            .unwrap_or_default();
        let editor = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx).rows(2).placeholder("Title");
            state.set_value(title, window, cx);
            state
        });
        self.title_edit = Some(editor);
        cx.notify();
    }

    fn cancel_edit_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.title_edit = None;
        self.mutation_error = None;
        self.refocus_page(window, cx);
        cx.notify();
    }

    /// Save the edited title (`updateIssue { id title }` — D1). Empty
    /// titles never get here (Save renders disabled, the comment-editor
    /// submit gate).
    fn save_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(d), Some(editor)) = (self.detail.as_ref(), self.title_edit.as_ref()) else {
            return;
        };
        if self.busy.is_some() || d.id.is_empty() {
            return;
        }
        let title = editor.read(cx).value().trim().to_string();
        if title.is_empty() {
            return;
        }
        let variables = json!({ "input": { "id": d.id, "title": title } });
        self.mutation_error = None;
        self.busy = Some(IssueBusyOp::SetTitle);
        cx.notify();
        self.spawn_mutation(
            IssueBusyOp::SetTitle,
            Mutation::IssueSetTitle,
            variables,
            window,
            cx,
        );
    }

    // --- reactions + locking (docs/0023) ------------------------------------

    /// Chip click (D3): armed removes, otherwise adds. Single-flight —
    /// busy is checked by render and re-checked here as the honest guard.
    fn toggle_reaction(
        &mut self,
        subject_id: String,
        remove: bool,
        content: gql::ReactionContent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() || subject_id.is_empty() {
            return;
        }
        let variables = json!({ "input": { "subjectId": subject_id, "content": content.wire() } });
        self.mutation_error = None;
        self.busy = Some(IssueBusyOp::ToggleReaction);
        cx.notify();
        let mutation = if remove {
            Mutation::RemoveReaction
        } else {
            Mutation::AddReaction
        };
        self.spawn_mutation(IssueBusyOp::ToggleReaction, mutation, variables, window, cx);
    }

    /// Rail lock toggle (D5): direction derives from the fetched state —
    /// no optimistic flip; the refetch repaints tag and composer gate.
    fn toggle_lock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.clone() else { return };
        if self.busy.is_some() || d.id.is_empty() {
            return;
        }
        let variables = json!({ "input": { "lockableId": d.id } });
        self.mutation_error = None;
        self.busy = Some(IssueBusyOp::ToggleLock);
        cx.notify();
        let mutation = if d.locked {
            Mutation::UnlockIssue
        } else {
            Mutation::LockIssue
        };
        self.spawn_mutation(IssueBusyOp::ToggleLock, mutation, variables, window, cx);
    }

    // --- rail pickers (0022 D5) ---------------------------------------------

    fn fetch_source(&mut self, source: PickerSource, cx: &mut Context<Self>) {
        if !self.state.read(cx).auth.connected() {
            return; // the page is about to be torn down by the auth gate
        }
        let client = self.state.update(cx, |s, _| s.client.clone());
        let (owner, name) = split_repo(&self.repo);
        let (kind, query_name) = match source {
            PickerSource::Labels => (Kind::RepoLabels, "repo-labels"),
            PickerSource::Assignees => (Kind::RepoAssignableUsers, "repo-assignable-users"),
            PickerSource::Milestones => (Kind::RepoMilestones, "repo-milestones"),
        };
        let spec = FetchSpec {
            kind,
            url: gql::GRAPHQL_ENDPOINT.to_string(),
            etag: None,
            ident: Some(self.repo.clone()),
            max_pages: 1,
            variables: Some(json!({ "owner": owner, "name": name })),
        };
        log::debug!("{query_name}: fetching {}", self.repo);
        match source {
            PickerSource::Labels => self.labels_source = PickerFetch::Loading,
            PickerSource::Assignees => self.assignees_source = PickerFetch::Loading,
            PickerSource::Milestones => self.milestones_source = PickerFetch::Loading,
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = run_spec_tracked(client, spec, cx).await;
            let _ = this.update(cx, |page, cx| page.apply_source_result(source, result, cx));
        })
        .detach();
    }

    fn set_source_failed(&mut self, source: PickerSource, message: String) {
        match source {
            PickerSource::Labels => self.labels_source = PickerFetch::Failed(message),
            PickerSource::Assignees => self.assignees_source = PickerFetch::Failed(message),
            PickerSource::Milestones => self.milestones_source = PickerFetch::Failed(message),
        }
    }

    /// A picker source page came back (files-sidebar convention: the 200
    /// body parses page-side; non-200 forwards to the shared handling).
    fn apply_source_result(
        &mut self,
        source: PickerSource,
        result: crate::service::SpecResult,
        cx: &mut Context<Self>,
    ) {
        if let Some(m) = &result.network {
            let m = m.clone();
            self.set_source_failed(source, format!("Network error: {m}"));
        } else if result.status != 200 {
            let status = result.status;
            self.state.update(cx, |s, cx| {
                s.apply(result);
                cx.notify();
            });
            self.set_source_failed(source, format!("GitHub answered HTTP {status}"));
        } else if let Some(body) = result.body {
            match serde_json::from_str::<gql::Envelope<gql::RepoPickerData>>(body.as_str()) {
                Ok(env) => {
                    let repo = env
                        .data
                        .and_then(|d| d.repository)
                        .unwrap_or(serde_json::Value::Null);
                    match source {
                        PickerSource::Labels => {
                            self.labels_source =
                                PickerFetch::Ready(gql::repo_labels_from_value(&repo));
                        }
                        PickerSource::Assignees => {
                            self.assignees_source =
                                PickerFetch::Ready(gql::repo_assignable_users_from_value(&repo));
                        }
                        PickerSource::Milestones => {
                            self.milestones_source =
                                PickerFetch::Ready(gql::repo_milestones_from_value(&repo));
                        }
                    }
                }
                Err(e) => {
                    log::warn!("picker-source payload: {e}");
                    self.set_source_failed(source, "Malformed payload".into());
                }
            }
        }
        self.realign_open_picker(source, cx);
        cx.notify();
    }

    /// A source page just landed (or the detail refetched underneath an
    /// open picker): rebuild the frozen draft, preserving choices by id.
    fn realign_open_picker(&mut self, source: PickerSource, cx: &mut Context<Self>) {
        let Some(d) = self.detail.clone() else { return };
        let rebuilding = match (&self.rail_edit, source) {
            (Some(RailEditor::Labels(_)), PickerSource::Labels) => 0,
            (Some(RailEditor::Assignees(_)), PickerSource::Assignees) => 1,
            (Some(RailEditor::Milestone { .. }), PickerSource::Milestones) => 2,
            _ => return,
        };
        if rebuilding == 0 {
            let PickerFetch::Ready(src) = &self.labels_source else {
                return;
            };
            let rows = label_rows(&d.labels, src);
            let checked = rows
                .iter()
                .map(|r| {
                    d.labels
                        .iter()
                        .any(|l| l.id.as_deref() == Some(r.id.as_str()))
                })
                .collect();
            self.rail_edit = Some(RailEditor::Labels(PickerDraft { rows, checked }));
        } else if rebuilding == 1 {
            let PickerFetch::Ready(src) = &self.assignees_source else {
                return;
            };
            let rows = assignee_rows(&d.assignees, src);
            let checked = rows
                .iter()
                .map(|r| {
                    d.assignees
                        .iter()
                        .any(|a| a.id.as_deref() == Some(r.id.as_str()))
                })
                .collect();
            self.rail_edit = Some(RailEditor::Assignees(PickerDraft { rows, checked }));
        } else {
            let RailEditor::Milestone { chosen, .. } = self.rail_edit.as_ref().unwrap() else {
                unreachable!("matched above")
            };
            let chosen = chosen.clone();
            let PickerFetch::Ready(src) = &self.milestones_source else {
                return;
            };
            self.rail_edit = Some(RailEditor::Milestone {
                rows: milestone_rows(d.milestone.as_ref(), src),
                chosen,
            });
        }
        cx.notify();
    }

    fn start_labels_picker(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.detail.clone() else { return };
        if self.busy.is_some() || self.rail_edit.is_some() || !labels_edit_arming(&d) {
            return;
        }
        self.yield_all_editors();
        // Lazy source fetch (D5): fired when Idle or retrying a Failed
        // page; a Loading page just keeps its spinner.
        if matches!(
            self.labels_source,
            PickerFetch::Idle | PickerFetch::Failed(_)
        ) {
            self.fetch_source(PickerSource::Labels, cx);
        }
        // Open immediately: while the source page is Loading (or Failed),
        // the draft holds just the applied rows — realign_open_picker
        // rebuilds it by id when the source lands (D5).
        let source = match &self.labels_source {
            PickerFetch::Ready(src) => src.clone(),
            _ => Vec::new(),
        };
        let rows = label_rows(&d.labels, &source);
        let checked = rows
            .iter()
            .map(|r| {
                d.labels
                    .iter()
                    .any(|l| l.id.as_deref() == Some(r.id.as_str()))
            })
            .collect();
        self.rail_edit = Some(RailEditor::Labels(PickerDraft { rows, checked }));
        cx.notify();
    }

    fn start_assignees_picker(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.detail.clone() else { return };
        if self.busy.is_some() || self.rail_edit.is_some() || !assignees_edit_arming(&d) {
            return;
        }
        self.yield_all_editors();
        if matches!(
            self.assignees_source,
            PickerFetch::Idle | PickerFetch::Failed(_)
        ) {
            self.fetch_source(PickerSource::Assignees, cx);
        }
        let source = match &self.assignees_source {
            PickerFetch::Ready(src) => src.clone(),
            _ => Vec::new(),
        };
        let rows = assignee_rows(&d.assignees, &source);
        let checked = rows
            .iter()
            .map(|r| {
                d.assignees
                    .iter()
                    .any(|a| a.id.as_deref() == Some(r.id.as_str()))
            })
            .collect();
        self.rail_edit = Some(RailEditor::Assignees(PickerDraft { rows, checked }));
        cx.notify();
    }

    fn start_milestones_picker(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.detail.clone() else { return };
        if self.busy.is_some() || self.rail_edit.is_some() || !milestone_edit_arming(&d) {
            return;
        }
        self.yield_all_editors();
        if matches!(
            self.milestones_source,
            PickerFetch::Idle | PickerFetch::Failed(_)
        ) {
            self.fetch_source(PickerSource::Milestones, cx);
        }
        let source = match &self.milestones_source {
            PickerFetch::Ready(src) => src.clone(),
            _ => Vec::new(),
        };
        let chosen = d.milestone.as_ref().map(|m| m.id.clone());
        self.rail_edit = Some(RailEditor::Milestone {
            rows: milestone_rows(d.milestone.as_ref(), &source),
            chosen,
        });
        cx.notify();
    }

    fn toggle_label(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        if let Some(RailEditor::Labels(draft)) = &mut self.rail_edit {
            if let Some(checked) = draft.checked.get_mut(ix) {
                *checked = !*checked;
                cx.notify();
            }
        }
    }

    fn toggle_assignee(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        if let Some(RailEditor::Assignees(draft)) = &mut self.rail_edit {
            if let Some(checked) = draft.checked.get_mut(ix) {
                *checked = !*checked;
                cx.notify();
            }
        }
    }

    fn pick_milestone(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        if let Some(RailEditor::Milestone { chosen, .. }) = &mut self.rail_edit {
            *chosen = id;
            cx.notify();
        }
    }

    /// The milestone picker's row action: choose the clicked row (its
    /// id; the empty id of the "No milestone" row means clear).
    fn pick_milestone_ix(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(RailEditor::Milestone { rows, .. }) = &self.rail_edit else {
            return;
        };
        let id = rows
            .get(ix)
            .map(|r| r.id.clone())
            .filter(|id| !id.is_empty());
        self.pick_milestone(id, cx);
    }

    fn cancel_rail_editor(&mut self, cx: &mut Context<Self>) {
        self.rail_edit = None;
        self.mutation_error = None;
        cx.notify();
    }

    /// Save the labels draft (D1/D3): the input carries the complete
    /// target id list — replace semantics upstream.
    fn save_labels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.clone() else { return };
        let Some(RailEditor::Labels(draft)) = &self.rail_edit else {
            return;
        };
        if self.busy.is_some() || d.id.is_empty() {
            return;
        }
        let ids = compute_replace_ids(&draft.rows, &draft.checked);
        let variables = json!({ "input": { "id": d.id, "labelIds": ids } });
        self.mutation_error = None;
        self.busy = Some(IssueBusyOp::SetLabels);
        cx.notify();
        self.spawn_mutation(
            IssueBusyOp::SetLabels,
            Mutation::IssueSetLabels,
            variables,
            window,
            cx,
        );
    }

    fn save_assignees(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.clone() else { return };
        let Some(RailEditor::Assignees(draft)) = &self.rail_edit else {
            return;
        };
        if self.busy.is_some() || d.id.is_empty() {
            return;
        }
        let ids = compute_replace_ids(&draft.rows, &draft.checked);
        let variables = json!({ "input": { "id": d.id, "assigneeIds": ids } });
        self.mutation_error = None;
        self.busy = Some(IssueBusyOp::SetAssignees);
        cx.notify();
        self.spawn_mutation(
            IssueBusyOp::SetAssignees,
            Mutation::IssueSetAssignees,
            variables,
            window,
            cx,
        );
    }

    /// Save the milestone choice — `milestoneId: null` clears (§2.1).
    /// An unchanged choice closes without a pointless write.
    fn save_milestone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.clone() else { return };
        let Some(RailEditor::Milestone { chosen, .. }) = &self.rail_edit else {
            return;
        };
        if self.busy.is_some() || d.id.is_empty() {
            return;
        }
        if chosen.as_deref() == d.milestone.as_ref().map(|m| m.id.as_str()) {
            self.rail_edit = None;
            cx.notify();
            return;
        }
        let variables = json!({ "input": { "id": d.id, "milestoneId": chosen } });
        self.mutation_error = None;
        self.busy = Some(IssueBusyOp::SetMilestone);
        cx.notify();
        self.spawn_mutation(
            IssueBusyOp::SetMilestone,
            Mutation::IssueSetMilestone,
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
                // 0022 D4: success closes the title editor; the refetch
                // repaints the header.
                IssueBusyOp::SetTitle => {
                    self.title_edit = None;
                    self.refocus_page(window, cx);
                }
                // 0022 D5: success closes the picker; the refetch
                // repaints the rail from fresh data.
                IssueBusyOp::SetLabels | IssueBusyOp::SetAssignees | IssueBusyOp::SetMilestone => {
                    self.rail_edit = None;
                }
                // The refetch repaints the chips, the lock tag and the
                // composer gate (the + menu is component-owned — it
                // dismisses itself on click).
                IssueBusyOp::ToggleReaction | IssueBusyOp::ToggleLock => {}
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

    /// The header's first row (0022 D4): badge + title + number, the
    /// title editor card while editing, and the gated Edit affordance
    /// otherwise (hidden, not disabled).
    fn render_title_area(
        &self,
        detail: &Option<gql::IssueDetail>,
        badge_label: &'static str,
        badge_tone: IssueBadgeTone,
        title: String,
        number: i64,
        cx: &Context<Self>,
    ) -> AnyElement {
        if let Some(editor) = self.title_edit.as_ref() {
            return inline_edit_card(
                "issue-title",
                "Editing title",
                editor,
                self.busy.is_some(),
                cx.listener(|this, _, window, cx| this.save_title(window, cx)),
                cx.listener(|this, _, window, cx| this.cancel_edit_title(window, cx)),
                cx,
            );
        }
        let can_edit = detail
            .as_ref()
            .map(|d| {
                title_edit_visible(
                    d.viewer_can_update,
                    self.busy.is_some() || self.any_editor_open(),
                )
            })
            .unwrap_or(false);
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
            )
            .when(can_edit, |row| {
                row.child(
                    Button::new("issue-title-edit")
                        .debug_selector(|| "issue-title-edit".to_string())
                        .ghost()
                        .small()
                        .label("Edit")
                        .tooltip("Edit title")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.start_edit_title(window, cx)),
                        ),
                )
            })
            .into_any_element()
    }

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
                    .child(self.render_title_area(
                        detail,
                        badge_label,
                        badge_tone,
                        title,
                        number,
                        cx,
                    ))
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
                    self.render_reaction_row(
                        "issue-reaction-row",
                        &d.id,
                        &d.reaction_groups,
                        d.viewer_can_react,
                        cx,
                    ),
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
                self.render_reaction_row(
                    &format!("{key}-reaction-row"),
                    &c.id,
                    &c.reaction_groups,
                    c.viewer_can_react,
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
        // docs/0023 D6: the composer hides while the conversation is
        // locked and the viewer lacks lock rights; a muted notice takes
        // its place (keeping the Timeline failure slot visible).
        if composer_visible(d.locked, d.viewer_permission.as_deref()) {
            children.push(self.render_comment_box(cx));
        } else {
            children.push(self.render_locked_notice(cx));
        }
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
            // docs/0022 D8: the milestone panel only exists when there is
            // something to show or the picker can arm.
            .children(self.render_milestone_panel(d, cx))
            .child(self.render_state_panel(d, cx))
            // docs/0023 D5: the lock row only exists when locked or the
            // viewer can lock (nothing to show, nothing to do otherwise).
            .children(self.render_lock_panel(d, cx))
            .into_any_element()
    }

    /// A rail panel's title row with its trailing Edit affordance (0022
    /// D5). The button is hidden, not disabled, whenever anything blocks
    /// it — gate, busy, or another picker being open (D7).
    fn panel_title(
        &self,
        label: &'static str,
        edit_id: &'static str,
        tooltip: &'static str,
        show_edit: bool,
        on_edit: fn(&mut Self, &mut Context<Self>),
        cx: &Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .w_full()
            .justify_between()
            .items_center()
            .gap_1()
            .child(label)
            .when(show_edit, |row| {
                row.child(
                    Button::new(edit_id)
                        // 0019 §8 lesson: probes see selector keys only.
                        .debug_selector(move || edit_id.to_string())
                        .ghost()
                        .small()
                        .label("Edit")
                        .tooltip(tooltip)
                        .on_click(cx.listener(move |this, _, _, cx| on_edit(this, cx))),
                )
            })
    }

    /// Labels (0020 Q2, 0022 D5): muted name chips while displaying —
    /// the payload's hex color is user data and stays unused until the
    /// token-discipline exception exists — or the picker while editing.
    fn render_labels_panel(&self, d: &gql::IssueDetail, cx: &Context<Self>) -> AnyElement {
        if let Some(RailEditor::Labels(draft)) = &self.rail_edit {
            return GroupBox::new()
                .id("issue-labels-panel")
                .title("Labels")
                .child(self.render_picker_card(
                    "issue-labels",
                    draft,
                    PickerSource::Labels,
                    Self::toggle_label,
                    cx,
                ))
                .into_any_element();
        }
        let show_edit = labels_edit_arming(d) && !self.any_editor_open();
        GroupBox::new()
            .id("issue-labels-panel")
            .title(self.panel_title(
                "Labels",
                "issue-labels-edit",
                "Edit labels",
                show_edit,
                Self::start_labels_picker,
                cx,
            ))
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
            .children(
                d.labels
                    .iter()
                    .map(|l| Self::label_pill(l, cx.theme().tokens.group_box.into(), cx)),
            )
            .into_any_element()
    }

    /// One label chip (0022 Q2 / #119): the label's own color when the
    /// API carried one — the docs/0002-sanctioned data-color exception,
    /// all construction inside `dashboard_core::data_color` — else the
    /// muted token pill (missing/garbled hex degrades, never guesses).
    fn label_pill(label: &gql::IssueLabel, surface: gpui::Hsla, cx: &Context<Self>) -> Div {
        let chip = label
            .color
            .as_deref()
            .and_then(|hex| dashboard_core::data_color::data_chip(hex, surface.into()));
        let (bg, fg, border) = match chip {
            Some(c) => (c.bg, c.fg, c.border),
            None => (
                cx.theme().transparent,
                cx.theme().muted_foreground,
                cx.theme().border,
            ),
        };
        div()
            .px_2()
            .py_0p5()
            .rounded_full()
            .border_1()
            .border_color(border)
            .bg(bg)
            .text_xs()
            .text_color(fg)
            .child(label.name.clone())
    }

    fn render_assignees_panel(&self, d: &gql::IssueDetail, cx: &Context<Self>) -> AnyElement {
        if let Some(RailEditor::Assignees(draft)) = &self.rail_edit {
            return GroupBox::new()
                .id("issue-assignees-panel")
                .title("Assignees")
                .child(self.render_picker_card(
                    "issue-assignees",
                    draft,
                    PickerSource::Assignees,
                    Self::toggle_assignee,
                    cx,
                ))
                .into_any_element();
        }
        let show_edit = assignees_edit_arming(d) && !self.any_editor_open();
        GroupBox::new()
            .id("issue-assignees-panel")
            .title(self.panel_title(
                "Assignees",
                "issue-assignees-edit",
                "Edit assignees",
                show_edit,
                Self::start_assignees_picker,
                cx,
            ))
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

    /// Milestone (0022 D8): title + state tag; `None` when the issue has
    /// no milestone — unless the picker can arm (the affordance needs a
    /// home; a viewer who cannot set milestones sees no empty panel).
    fn render_milestone_panel(
        &self,
        d: &gql::IssueDetail,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let editing = matches!(self.rail_edit, Some(RailEditor::Milestone { .. }));
        if editing {
            let RailEditor::Milestone { rows, chosen } = self.rail_edit.as_ref().unwrap() else {
                unreachable!("matched above")
            };
            return Some(
                GroupBox::new()
                    .id("issue-milestone-panel")
                    .title("Milestone")
                    .child(self.render_milestone_card(rows, chosen, cx)),
            )
            .map(|g| g.into_any_element());
        }
        let show_edit = milestone_edit_arming(d) && !self.any_editor_open();
        if d.milestone.is_none() && !show_edit {
            return None;
        }
        let cell = h_flex()
            .id("issue-milestone")
            .debug_selector(|| "issue-milestone".to_string())
            .gap_2()
            .items_center()
            .py_1()
            .when_some(d.milestone.as_ref(), |row, m| {
                row.child(div().text_sm().child(m.title.clone())).child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(match m.state.as_deref() {
                            Some("CLOSED") => "CLOSED",
                            _ => "OPEN",
                        }),
                )
            })
            .when(d.milestone.is_none(), |row| {
                row.child(empty_text("None", cx))
            });
        Some(
            GroupBox::new()
                .id("issue-milestone-panel")
                .title(self.panel_title(
                    "Milestone",
                    "issue-milestones-edit",
                    "Edit milestone",
                    show_edit,
                    Self::start_milestones_picker,
                    cx,
                ))
                .child(cell),
        )
        .map(|g| g.into_any_element())
    }

    /// The shared checklist card (0022 D5): one clickable row per
    /// candidate — the row is the toggle target (its accesskit id is the
    /// probe key), the checkbox is display-only — plus Save/Cancel.
    fn render_picker_card(
        &self,
        id: &'static str,
        draft: &PickerDraft,
        source: PickerSource,
        toggle: fn(&mut Self, usize, &mut Context<Self>),
        cx: &Context<Self>,
    ) -> AnyElement {
        let busy = self.busy.is_some();
        let save: fn(&mut Self, &mut Window, &mut Context<Self>) = match source {
            PickerSource::Labels => Self::save_labels,
            PickerSource::Assignees => Self::save_assignees,
            PickerSource::Milestones => unreachable!("milestone uses render_milestone_card"),
        };
        let save_id = match source {
            PickerSource::Labels => "issue-labels-save",
            PickerSource::Assignees => "issue-assignees-save",
            PickerSource::Milestones => unreachable!("milestone uses render_milestone_card"),
        };
        let cancel = match source {
            PickerSource::Labels | PickerSource::Assignees => {
                cx.listener(|this, _, _, cx| this.cancel_rail_editor(cx))
            }
            PickerSource::Milestones => unreachable!("milestone uses render_milestone_card"),
        };
        let loading = match source {
            PickerSource::Labels => matches!(self.labels_source, PickerFetch::Loading),
            PickerSource::Assignees => matches!(self.assignees_source, PickerFetch::Loading),
            PickerSource::Milestones => false,
        };
        let failed = match source {
            PickerSource::Labels => self.labels_source.failed_message(),
            PickerSource::Assignees => self.assignees_source.failed_message(),
            PickerSource::Milestones => None,
        };
        let id_key = format!("{id}-card");
        v_flex()
            .id(SharedString::from(format!("{id}-card")))
            .debug_selector(move || id_key.clone())
            .gap_1()
            .py_1()
            .when(loading, |col| {
                col.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Spinner::new().small())
                        .child(empty_text("Loading…", cx)),
                )
            })
            .when_some(failed, |col, msg| {
                col.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(msg.to_string()),
                )
            })
            .children(draft.rows.iter().enumerate().map(|(ix, row)| {
                let checked = draft.checked.get(ix).copied().unwrap_or(false);
                self.render_picker_row(format!("{id}-row-{ix}"), ix, row, checked, toggle, cx)
            }))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .pt_1()
                    .child(
                        Button::new(save_id)
                            .debug_selector(move || save_id.to_string())
                            .label("Save")
                            .small()
                            .disabled(busy)
                            .on_click(
                                cx.listener(move |this, _, window, cx| save(this, window, cx)),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("{id}-cancel")))
                            .debug_selector(move || format!("{id}-cancel"))
                            .label("Cancel")
                            .small()
                            .on_click(cancel),
                    ),
            )
            .into_any_element()
    }

    fn render_picker_row(
        &self,
        key: String,
        ix: usize,
        row: &PickerRow,
        checked: bool,
        toggle: fn(&mut Self, usize, &mut Context<Self>),
        cx: &Context<Self>,
    ) -> AnyElement {
        let probe = key.clone();
        let check = format!("{key}-check");
        h_flex()
            .id(SharedString::from(key))
            .debug_selector(move || probe.clone())
            .cursor_pointer()
            .gap_2()
            .items_center()
            .px_2()
            .py_1()
            .rounded_sm()
            .hover(|s| s.bg(cx.theme().accent.opacity(0.08)))
            .on_click(cx.listener(move |this, _: &gpui::ClickEvent, _, cx| toggle(this, ix, cx)))
            .child(
                Checkbox::new(SharedString::from(check))
                    .checked(checked)
                    .small()
                    .tab_stop(false),
            )
            .when_some(row.avatar_url.clone(), |r, url| {
                r.child(Avatar::new().src(url).xsmall())
            })
            .child(div().text_sm().child(row.label.clone()))
            .into_any_element()
    }

    /// The milestone picker (0022 D5): single-select rows — clicking a
    /// row chooses it, "No milestone" clears. Save posts `milestoneId`
    /// (null clears); Cancel restores.
    fn render_milestone_card(
        &self,
        rows: &[PickerRow],
        chosen: &Option<String>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let busy = self.busy.is_some();
        let loading = matches!(self.milestones_source, PickerFetch::Loading);
        let id_key = "issue-milestones-card".to_string();
        v_flex()
            .id("issue-milestones-card")
            .debug_selector(move || id_key.clone())
            .gap_1()
            .py_1()
            .when(loading, |col| {
                col.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Spinner::new().small())
                        .child(empty_text("Loading…", cx)),
                )
            })
            .when_some(self.milestones_source.failed_message(), |col, msg| {
                col.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(msg.to_string()),
                )
            })
            .children(rows.iter().enumerate().map(|(ix, row)| {
                let selected = chosen.as_deref() == Some(row.id.as_str())
                    || (chosen.is_none() && row.id.is_empty());
                let key = format!("issue-milestones-row-{ix}");
                let check = format!("{key}-check");
                h_flex()
                    .id(SharedString::from(key.clone()))
                    .debug_selector(move || key.clone())
                    .cursor_pointer()
                    .gap_2()
                    .items_center()
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .hover(|s| s.bg(cx.theme().accent.opacity(0.08)))
                    .on_click(cx.listener(move |this, _: &gpui::ClickEvent, _, cx| {
                        this.pick_milestone_ix(ix, cx)
                    }))
                    .child(
                        Checkbox::new(SharedString::from(check))
                            .checked(selected)
                            .small()
                            .tab_stop(false),
                    )
                    .child(div().text_sm().child(row.label.clone()))
            }))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .pt_1()
                    .child(
                        Button::new("issue-milestones-save")
                            .debug_selector(|| "issue-milestones-save".to_string())
                            .label("Save")
                            .small()
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.save_milestone(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("issue-milestones-cancel")
                            .debug_selector(|| "issue-milestones-cancel".to_string())
                            .label("Cancel")
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_rail_editor(cx))),
                    ),
            )
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

    /// docs/0023 D3 — one chips row: filtered groups (`reaction_chips`) +
    /// the gated `+` affordance. `prefix` is the probe-key stem
    /// (`issue-reaction-row` for the body,
    /// `issue-comment-{ix}-reaction-row` for comments); chips extend it
    /// with the content's wire name. Returns `None` when there is nothing
    /// to show — no chips and the viewer cannot react.
    fn render_reaction_row(
        &self,
        prefix: &str,
        subject_id: &str,
        groups: &[gql::ReactionGroup],
        can_react: bool,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let chips = reaction_chips(groups);
        if chips.is_empty() && !can_react {
            return None;
        }
        let busy = self.busy.is_some();
        let mut row = h_flex().gap_1().flex_wrap();
        for g in &chips {
            let id = format!("{prefix}-{}", g.content.wire());
            let probe = id.clone();
            let armed = g.viewer_has_reacted;
            let subject_id = subject_id.to_string();
            let content = g.content;
            row = row.child(
                Button::new(id)
                    .debug_selector(move || probe.clone())
                    .when(armed, |b| b.outline())
                    .when(!armed, |b| b.ghost())
                    .small()
                    .label(format!("{} {}", g.content.glyph(), g.total))
                    .tooltip(g.content.label())
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle_reaction(subject_id.clone(), armed, content, window, cx)
                    })),
            );
        }
        if can_react {
            let subject_id = subject_id.to_string();
            let groups = groups.to_vec();
            let weak = cx.entity().downgrade();
            let picker_id = format!("{prefix}-add");
            let probe = picker_id.clone();
            row = row.child(
                Button::new(picker_id)
                    .debug_selector(move || probe.clone())
                    .ghost()
                    .small()
                    .label("+")
                    .tooltip("Add reaction")
                    .disabled(busy)
                    .dropdown_menu(move |menu, _, _| {
                        // docs/0023 D4 (§10 amendment): the static
                        // eight-row picker rides the + button as a
                        // PopupMenu. Rows freeze at open — built from
                        // this render's snapshot, so a refetch mid-pick
                        // repaints chips, never the open menu — and a
                        // row commits on click (the mutation is the
                        // commit; no Save ritual).
                        let mut next = menu;
                        for content in gql::ReactionContent::ALL {
                            let armed = reaction_direction(&groups, content);
                            let count = groups
                                .iter()
                                .find(|g| g.content == content)
                                .map(|g| g.total)
                                .unwrap_or(0);
                            let subject = subject_id.clone();
                            let item_weak = weak.clone();
                            let probe = format!("issue-reaction-pick-{}", content.wire());
                            next = next.item(
                                PopupMenuItem::element(move |_, _| {
                                    // Probe-visible row (0010 D10) — the
                                    // plain Item variant is not.
                                    let key = probe.clone();
                                    h_flex()
                                        .debug_selector(move || key.clone())
                                        .gap_2()
                                        .child(format!(
                                            "{} {} ({})",
                                            content.glyph(),
                                            content.label(),
                                            count
                                        ))
                                        .when(armed, |row| row.child("· you"))
                                })
                                .checked(armed)
                                .disabled(busy)
                                .on_click(move |_, window, cx| {
                                    // The click dispatches inside the window's
                                    // update — any re-entrant window update here
                                    // fails ("window not found"). Defer to the end
                                    // of the effect cycle, then post from the live
                                    // context (the page.rs spawn pattern, inline).
                                    let weak = item_weak.clone();
                                    let subject = subject.clone();
                                    window.defer(cx, move |window, cx| {
                                        let _ = weak.update(cx, |page, cx| {
                                            page.toggle_reaction(
                                                subject, armed, content, window, cx,
                                            )
                                        });
                                    });
                                }),
                            );
                        }
                        next
                    }),
            );
        }
        Some(
            row.debug_selector(move || format!("{prefix}-row"))
                .into_any_element(),
        )
    }

    /// docs/0023 D5 — the conversation lock row: state tag plus the
    /// gated toggle. No panel when unlocked AND the viewer lacks rights
    /// (nothing to show, nothing to do — 0022 D8 parity).
    fn render_lock_panel(&self, d: &gql::IssueDetail, cx: &Context<Self>) -> Option<AnyElement> {
        let gate = lock_gate(d.viewer_permission.as_deref());
        if !d.locked && !gate {
            return None;
        }
        let reason = lock_reason_label(d.active_lock_reason.as_deref());
        let tag = if d.locked {
            format!(
                "Locked{}",
                if reason.is_empty() {
                    String::new()
                } else {
                    format!(" · {reason}")
                }
            )
        } else {
            "Unlocked".to_string()
        };
        let busy = self.busy.is_some();
        let lock_busy = matches!(self.busy, Some(IssueBusyOp::ToggleLock));
        let mut col = v_flex()
            .debug_selector(|| "issue-lock-row-body".to_string())
            .gap_2()
            .py_1()
            .child(
                div()
                    .id("issue-lock-tag")
                    .debug_selector(|| "issue-lock-tag".to_string())
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tag),
            );
        if gate {
            let label = if d.locked { "Unlock" } else { "Lock" };
            col = col.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .when(lock_busy, |row| row.child(Spinner::new().small()))
                    .child(
                        Button::new("issue-lock-toggle")
                            .debug_selector(|| "issue-lock-toggle".to_string())
                            .ghost()
                            .small()
                            .label(label)
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.toggle_lock(window, cx)),
                            ),
                    ),
            );
        }
        Some(
            GroupBox::new()
                .id("issue-lock-row")
                .title("Lock")
                .child(col)
                .into_any_element(),
        )
    }

    /// docs/0023 D6 — the muted notice replacing the composer while the
    /// conversation is locked to this viewer. Reaction failures still
    /// read here (the Timeline error slot lives with the composer).
    fn render_locked_notice(&self, cx: &Context<Self>) -> AnyElement {
        v_flex()
            .id("issue-locked-notice")
            .debug_selector(|| "issue-locked-notice".to_string())
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .p_3()
            .children(self.timeline_error_row(cx))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Conversation locked"),
            )
            .into_any_element()
    }

    /// The Timeline-scoped failure copy (shared by the composer and the
    /// locked notice).
    fn timeline_error_row(&self, cx: &Context<Self>) -> Option<AnyElement> {
        self.mutation_error
            .as_ref()
            .filter(|e| e.scope == ErrorScope::Timeline)
            .map(|e| {
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(e.message.clone())
                    .into_any_element()
            })
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
            .debug_selector(|| "issue-comment-box".to_string())
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
            // 0022 D4/D5: an open editor or picker consumes Esc (its
            // Cancel); the page only closes when nothing is open.
            .on_action(cx.listener(|this, _: &DismissIssuePage, window, cx| {
                if !this.cancel_top_editor(window, cx) {
                    this.request_close(cx);
                }
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

// --- metadata-editing pure helpers (docs/0022) -----------------------------

/// D3, labels: the picker's Edit affordance arms only when the viewer
/// gate holds, every applied label parsed an `id`, and the connection is
/// complete — a save computed from partial state would silently drop
/// entries the page never saw. Pure — table-tested.
pub(crate) fn labels_edit_arming(d: &gql::IssueDetail) -> bool {
    d.viewer_can_label
        && d.labels.iter().all(|l| l.id.is_some())
        && d.labels.len() as i64 == d.labels_total
}

/// D3, assignees — same guard shape as [`labels_edit_arming`].
pub(crate) fn assignees_edit_arming(d: &gql::IssueDetail) -> bool {
    d.viewer_can_assign
        && d.assignees.iter().all(|a| a.id.is_some())
        && d.assignees.len() as i64 == d.assignees_total
}

/// D3, milestone: single-valued upstream, so there is no list to drop —
/// the gate and a write target are enough.
pub(crate) fn milestone_edit_arming(d: &gql::IssueDetail) -> bool {
    d.viewer_can_set_milestone && !d.id.is_empty()
}

/// D4: the title Edit affordance shows when the gate holds and nothing
/// is in flight (hidden, not disabled — 0020 D5).
pub(crate) fn title_edit_visible(viewer_can_update: bool, busy: bool) -> bool {
    viewer_can_update && !busy
}

// --- reactions + locking pure helpers (docs/0023) -------------------------

/// D3: the chips a row renders — groups with at least one reactor, in
/// the canonical [`gql::ReactionContent::ALL`] order (never count-sorted;
/// rows don't reshuffle on refetch). Zero-count groups are noise —
/// GitHub sends all eight even at zero (§2 of the doc). Pure —
/// table-tested.
pub(crate) fn reaction_chips(groups: &[gql::ReactionGroup]) -> Vec<gql::ReactionGroup> {
    let mut chips: Vec<gql::ReactionGroup> =
        groups.iter().filter(|g| g.total >= 1).cloned().collect();
    chips.sort_by_key(|g| {
        gql::ReactionContent::ALL
            .iter()
            .position(|c| *c == g.content)
            .unwrap_or(usize::MAX)
    });
    chips
}

/// docs/0023 D4 (§10 amendment): the frozen toggle direction for a
/// reaction row — armed iff the snapshot says the viewer has reacted.
/// Chips read the same value off their group; the + menu applies it per
/// row at open, so a refetch mid-pick repaints the chips, never the
/// open menu. Pure — table-tested.
pub(crate) fn reaction_direction(
    groups: &[gql::ReactionGroup],
    content: gql::ReactionContent,
) -> bool {
    groups
        .iter()
        .find(|g| g.content == content)
        .map(|g| g.viewer_has_reacted)
        .unwrap_or(false)
}

/// D5: the lock gate — no `viewerCanLock` flag exists upstream, so
/// triage-and-above is derived from the repository permission. `None`
/// (no explicit mapping) means no rights. Pure — table-tested.
pub(crate) fn lock_gate(permission: Option<&str>) -> bool {
    matches!(
        permission,
        Some("ADMIN") | Some("MAINTAIN") | Some("WRITE") | Some("TRIAGE")
    )
}

/// D6: the composer's visibility rule (no `viewerCanComment` flag
/// upstream — derived, honest): hidden only while the conversation is
/// locked AND the viewer lacks lock rights. Pure — table-tested.
pub(crate) fn composer_visible(locked: bool, permission: Option<&str>) -> bool {
    !locked || lock_gate(permission)
}

/// D5: the rail tag's copy for the raw `LockReason` wire value (empty
/// when unlocked or reason-less — the tag then reads just "Locked").
pub(crate) fn lock_reason_label(reason: Option<&str>) -> &'static str {
    match reason {
        Some("OFF_TOPIC") => "off-topic",
        Some("TOO_HEATED") => "too heated",
        Some("RESOLVED") => "resolved",
        Some("SPAM") => "spam",
        _ => "",
    }
}

/// The labels checklist: the source page in picker order, then any
/// applied label the source missed (repo page truncation, source still
/// loading) appended — dropping them would silently unlabel on save.
/// Pure — table-tested.
pub(crate) fn label_rows(
    applied: &[gql::IssueLabel],
    source: &[gql::IssueLabel],
) -> Vec<PickerRow> {
    let mut rows: Vec<PickerRow> = source
        .iter()
        .map(|l| PickerRow {
            id: l.id.clone().unwrap_or_default(),
            label: l.name.clone(),
            avatar_url: None,
        })
        .collect();
    for l in applied {
        let Some(id) = &l.id else { continue };
        if !rows.iter().any(|r| &r.id == id) {
            rows.push(PickerRow {
                id: id.clone(),
                label: l.name.clone(),
                avatar_url: None,
            });
        }
    }
    rows
}

/// The assignees checklist — same merge as [`label_rows`], logins +
/// avatars for display. Pure — table-tested.
pub(crate) fn assignee_rows(
    applied: &[gql::IssueAssignee],
    source: &[gql::IssueAssignee],
) -> Vec<PickerRow> {
    let mut rows: Vec<PickerRow> = source
        .iter()
        .map(|a| PickerRow {
            id: a.id.clone().unwrap_or_default(),
            label: a.login.clone(),
            avatar_url: Some(a.avatar_url.clone()),
        })
        .collect();
    for a in applied {
        let Some(id) = &a.id else { continue };
        if !rows.iter().any(|r| &r.id == id) {
            rows.push(PickerRow {
                id: id.clone(),
                label: a.login.clone(),
                avatar_url: Some(a.avatar_url.clone()),
            });
        }
    }
    rows
}

/// The milestone checklist: a leading "No milestone" row (id "" —
/// maps to `milestoneId: null`), then the open-milestone source page,
/// then an applied milestone the source missed (e.g. a closed one).
/// Pure — table-tested.
pub(crate) fn milestone_rows(
    applied: Option<&gql::IssueMilestone>,
    source: &[gql::IssueMilestone],
) -> Vec<PickerRow> {
    let mut rows = vec![PickerRow {
        id: String::new(),
        label: "No milestone".to_string(),
        avatar_url: None,
    }];
    rows.extend(source.iter().map(|m| PickerRow {
        id: m.id.clone(),
        label: m.title.clone(),
        avatar_url: None,
    }));
    if let Some(m) = applied {
        if !rows.iter().any(|r| r.id == m.id) {
            let label = if m.state.as_deref() == Some("CLOSED") {
                format!("{} · closed", m.title)
            } else {
                m.title.clone()
            };
            rows.push(PickerRow {
                id: m.id.clone(),
                label,
                avatar_url: None,
            });
        }
    }
    rows
}

/// D3's write: the checked rows' ids are the complete replacement list.
/// Zip-strict — a length mismatch is a render bug, not a partial write.
/// Pure — table-tested.
pub(crate) fn compute_replace_ids(rows: &[PickerRow], checked: &[bool]) -> Vec<String> {
    rows.iter()
        .zip(checked.iter())
        .filter(|(_, c)| **c)
        .map(|(r, _)| r.id.clone())
        .collect()
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
        CloseReason, ErrorScope, IssueBadgeTone, IssueBusyOp, IssueDetailPage, RailEditor,
        TimelineRow, assignee_rows, assignees_edit_arming, build_issue_md_cache, close_allowed,
        close_reason_from_label, comment_submit_allowed, composer_visible, compute_replace_ids,
        event_sentence, issue_badge, label_rows, labels_edit_arming, lock_gate, lock_reason_label,
        milestone_edit_arming, milestone_rows, reaction_chips, reaction_direction, reopen_allowed,
        row_is_edited, sorted_timeline, state_reason_word, title_edit_visible,
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
        let value = serde_json::json!({
            // 0023 D5: the permission rides the repository node beside issue.
            "viewerPermission": "ADMIN",
            "issue": {
            "id": "I_9", "number": 487, "title": "probe issue", "url": "u",
            "state": "OPEN", "stateReason": null,
            "createdAt": "2026-09-01T00:00:00Z",
            "viewerCanUpdate": true, "viewerCanClose": true, "viewerCanReopen": false,
            // 0022: picker gates on, ids present, milestone applied — the
            // metadata affordances arm in the fixture.
            "viewerCanLabel": true, "viewerCanAssign": true,
            "viewerCanSetMilestone": true,
            // 0023: reactions armed; one armed group on the body, one
            // zero-count (filtered), comment 0 reacts too.
            "viewerCanReact": true, "locked": false, "activeLockReason": null,
            "reactionGroups": [
                {"content": "THUMBS_UP", "viewerHasReacted": true, "reactors": {"totalCount": 3}},
                {"content": "EYES", "viewerHasReacted": false, "reactors": {"totalCount": 0}}
            ],
            "labels": {"totalCount": 1, "nodes": [{"id": "L_1", "name": "bug", "color": "ff0000"}]},
            "assignees": {"totalCount": 1, "nodes": [{"id": "U_1", "login": "bob", "avatarUrl": "b"}]},
            "milestone": {"id": "M_1", "title": "v1.0", "state": "OPEN"},
            "comments": {"totalCount": 2, "nodes": [
                {"id": "IC_1", "viewerCanUpdate": true, "viewerCanDelete": false,
                 "updatedAt": "2026-09-03T02:00:00Z",
                 "author": {"login": "dave"}, "body": "nice",
                 "createdAt": "2026-09-03T01:00:00Z",
                 "viewerCanReact": true,
                 "reactionGroups": [
                     {"content": "HEART", "viewerHasReacted": false, "reactors": {"totalCount": 1}}
                 ]},
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
        // Reaction/lock toggles really spawn (ProbeHttp fails them) — the
        // in-flight tracker global must exist, same contract as main() .
        cx.update(dashboard_core::status::init);
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

    // docs/0022 §7 — the D3 arming guards: gate ∧ full id coverage ∧ a
    // complete connection (totalCount). Any miss hides the Edit
    // affordance instead of offering a destructive partial save.
    #[test]
    fn metadata_arming_guard_table() {
        let mut d = probe_issue();
        assert!(labels_edit_arming(&d));
        assert!(assignees_edit_arming(&d));
        assert!(milestone_edit_arming(&d));

        // Gate off → disarmed (each surface gates independently).
        d.viewer_can_label = false;
        d.viewer_can_assign = false;
        d.viewer_can_set_milestone = false;
        assert!(!labels_edit_arming(&d));
        assert!(!assignees_edit_arming(&d));
        assert!(!milestone_edit_arming(&d));
        d.viewer_can_label = true;
        d.viewer_can_assign = true;
        d.viewer_can_set_milestone = true;

        // A degraded node (id absent) disarms the list pickers only.
        d.labels[0].id = None;
        d.assignees[0].id = None;
        assert!(!labels_edit_arming(&d));
        assert!(!assignees_edit_arming(&d));
        assert!(milestone_edit_arming(&d));
        d.labels[0].id = Some("L_1".into());
        d.assignees[0].id = Some("U_1".into());

        // A truncated connection (totalCount > fetched page) disarms:
        // the save would silently drop labels the page never saw (D3).
        d.labels_total = 30;
        assert!(!labels_edit_arming(&d));
        d.labels_total = 1;
        d.assignees_total = 30;
        assert!(!assignees_edit_arming(&d));

        // Milestone is single-valued upstream — no list to drop, so no
        // completeness check; only the gate and a write target matter.
        assert!(milestone_edit_arming(&d));
        d.id = String::new();
        assert!(!milestone_edit_arming(&d));

        // D4: the title affordance rides viewerCanUpdate alone.
        assert!(title_edit_visible(true, false));
        assert!(!title_edit_visible(true, true));
        assert!(!title_edit_visible(false, false));
    }

    // docs/0022 D5 — the merged checklist: source rows first, applied
    // rows the source missed appended (never silently dropped), and the
    // save computes the complete replacement list (add AND remove).
    #[test]
    fn picker_rows_merge_and_replace_compute_table() {
        let applied = vec![
            gql::IssueLabel {
                id: Some("L_1".into()),
                name: "bug".into(),
                color: None,
            },
            gql::IssueLabel {
                id: Some("L_9".into()),
                name: "stale".into(),
                color: None,
            },
        ];
        let source = vec![
            gql::IssueLabel {
                id: Some("L_1".into()),
                name: "bug".into(),
                color: None,
            },
            gql::IssueLabel {
                id: Some("L_2".into()),
                name: "p1".into(),
                color: None,
            },
        ];
        let rows = label_rows(&applied, &source);
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["L_1", "L_2", "L_9"]
        );
        // Checked flags: applied rows checked (including the merged one).
        let checked = rows
            .iter()
            .map(|r| {
                applied
                    .iter()
                    .any(|l| l.id.as_deref() == Some(r.id.as_str()))
            })
            .collect::<Vec<_>>();
        assert_eq!(checked, [true, false, true]);
        // Add L_2, drop L_1: the replacement list is the complete target.
        let ids = compute_replace_ids(&rows, &[false, true, true]);
        assert_eq!(ids, ["L_2", "L_9"]);

        // Assignees merge identically (avatars ride for display).
        let applied = vec![gql::IssueAssignee {
            id: Some("U_1".into()),
            login: "bob".into(),
            avatar_url: "b".into(),
        }];
        let rows = assignee_rows(&applied, &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].avatar_url.as_deref(), Some("b"));

        // Milestone rows: "No milestone" leads (empty id → null), the
        // applied milestone the source missed rides with its state tag.
        let applied = gql::IssueMilestone {
            id: "M_9".into(),
            title: "old".into(),
            state: Some("CLOSED".into()),
        };
        let source = vec![gql::IssueMilestone {
            id: "M_1".into(),
            title: "v1.0".into(),
            state: Some("OPEN".into()),
        }];
        let rows = milestone_rows(Some(&applied), &source);
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["", "M_1", "M_9"]
        );
        assert_eq!(rows[0].label, "No milestone");
        assert_eq!(rows[2].label, "old · closed");
    }

    // docs/0022 D4/D5/D7 — the five editor surfaces stay mutually
    // exclusive; every affordance is gate-hidden when the viewer lacks
    // it; the milestone cell renders only when it has something to show.
    #[gpui::test]
    fn metadata_editors_swap_and_follow_gates(cx: &mut gpui::TestAppContext) {
        let (page, vcx) = probe_page(cx);
        // Gates on in the fixture: all four affordances + the milestone
        // cell are visible.
        for sel in [
            "issue-title-edit",
            "issue-labels-edit",
            "issue-assignees-edit",
            "issue-milestones-edit",
            "issue-milestone",
        ] {
            assert!(vcx.debug_bounds(sel).is_some(), "{sel} must arm");
        }

        // Title editor swaps in; pickers stay closed.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| page.start_edit_title(window, cx));
        });
        assert!(vcx.debug_bounds("issue-title-card").is_some());
        assert!(vcx.debug_bounds("issue-labels-edit").is_none());

        // Opening a picker yields the title editor (D7).
        vcx.update(|_, cx| page.update(cx, |page, cx| page.start_labels_picker(cx)));
        assert!(vcx.debug_bounds("issue-title-card").is_none());
        assert!(vcx.debug_bounds("issue-labels-card").is_some());
        assert!(vcx.debug_bounds("issue-labels-row-0").is_some());

        // A second picker cannot open while one is up.
        vcx.update(|_, cx| page.update(cx, |page, cx| page.start_assignees_picker(cx)));
        assert!(vcx.debug_bounds("issue-labels-card").is_some());
        assert!(vcx.debug_bounds("issue-assignees-card").is_none());

        // Gates off → every affordance hides (hidden, not disabled).
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                page.cancel_rail_editor(cx);
                let d = page.detail.as_mut().unwrap();
                d.viewer_can_update = false;
                d.viewer_can_label = false;
                d.viewer_can_assign = false;
                d.viewer_can_set_milestone = false;
                d.milestone = None;
                cx.notify();
            });
        });
        for sel in [
            "issue-title-edit",
            "issue-labels-edit",
            "issue-assignees-edit",
            "issue-milestones-edit",
            "issue-milestone",
        ] {
            assert!(vcx.debug_bounds(sel).is_none(), "{sel} must hide");
        }
    }

    // docs/0022 D5 — picker drafts survive toggling and cancel restores;
    // busy blocks a save from arming (D2: one write at a time).
    #[gpui::test]
    fn label_picker_toggle_and_busy_gating(cx: &mut gpui::TestAppContext) {
        let (page, vcx) = probe_page(cx);
        vcx.update(|_, cx| page.update(cx, |page, cx| page.start_labels_picker(cx)));
        assert!(vcx.debug_bounds("issue-labels-card").is_some());

        // Toggle row 0 (currently applied) off; the draft flips.
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                page.toggle_label(0, cx);
                let Some(RailEditor::Labels(draft)) = &page.rail_edit else {
                    panic!("labels picker open");
                };
                assert_eq!(draft.checked, [false]);
            });
        });

        // Busy blocks the write and the toggles; the draft stays open.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.busy = Some(IssueBusyOp::Comment);
                page.save_labels(window, cx);
                assert!(page.busy == Some(IssueBusyOp::Comment), "no second write");
                page.toggle_label(0, cx);
                let Some(RailEditor::Labels(draft)) = &page.rail_edit else {
                    panic!("picker stays open");
                };
                assert_eq!(draft.checked, [false], "toggle ignored while busy");
                page.busy = None;
            });
        });

        // Cancel closes without writing and clears the rail error slot.
        vcx.update(|_, cx| page.update(cx, |page, cx| page.cancel_rail_editor(cx)));
        assert!(vcx.update(|_, cx| page.update(cx, |page, _| page.rail_edit.is_none())));
        assert!(vcx.debug_bounds("issue-labels-card").is_none());
    }

    // docs/0022 D5 — the milestone picker: rows choose single-select, an
    // unchanged choice closes without a write.
    #[gpui::test]
    fn milestone_picker_selects_and_skips_noop(cx: &mut gpui::TestAppContext) {
        let (page, vcx) = probe_page(cx);
        vcx.update(|_, cx| page.update(cx, |page, cx| page.start_milestones_picker(cx)));
        assert!(vcx.debug_bounds("issue-milestones-card").is_some());
        assert!(vcx.debug_bounds("issue-milestones-row-0").is_some());

        // Rows: "No milestone" leads (row 0, id ""), then the applied
        // milestone the empty source missed (row 1 = M_1, the current
        // choice). Picking "No milestone" chooses clearing (None).
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                page.pick_milestone_ix(0, cx);
                let Some(RailEditor::Milestone { chosen, .. }) = &page.rail_edit else {
                    panic!("milestone picker open");
                };
                assert_eq!(chosen.as_deref(), None);
            });
        });

        // Choosing the current milestone again → Save is a no-op close.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.pick_milestone_ix(1, cx);
                page.save_milestone(window, cx);
            });
        });
        assert!(vcx.update(|_, cx| page.update(cx, |page, _| page.rail_edit.is_none())));
        assert!(vcx.debug_bounds("issue-milestones-card").is_none());
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

    // --- reactions + locking (docs/0023) ------------------------------------

    #[test]
    fn reaction_chips_filter_and_order() {
        let groups = |wire: &str, armed: bool, total: i64| gql::ReactionGroup {
            content: gql::ReactionContent::parse(wire).unwrap(),
            viewer_has_reacted: armed,
            total,
        };
        // Zero-count groups drop; canonical ALL order wins over wire order
        // (D3: rows don't reshuffle, positions stable).
        let chips = reaction_chips(&[
            groups("EYES", false, 2),
            groups("THUMBS_DOWN", true, 0),
            groups("HEART", false, 5),
        ]);
        let wires: Vec<&str> = chips.iter().map(|g| g.content.wire()).collect();
        assert_eq!(wires, ["HEART", "EYES"]);
        // All-zero → no chips at all.
        assert!(reaction_chips(&[groups("HEART", false, 0)]).is_empty());
        assert!(reaction_chips(&[]).is_empty());
        // §10 amendment: the + menu's frozen direction rides the same
        // helper — armed per content, absent content stays unarmed.
        assert!(reaction_direction(
            &[groups("HEART", true, 5)],
            gql::ReactionContent::Heart
        ));
        assert!(!reaction_direction(
            &[groups("HEART", true, 5)],
            gql::ReactionContent::Eyes
        ));
        assert!(!reaction_direction(&[], gql::ReactionContent::Heart));
    }

    #[test]
    fn lock_gate_composer_and_reason_tables() {
        // D5: triage-and-above locks; READ and null don't.
        for (perm, gate) in [
            (Some("ADMIN"), true),
            (Some("MAINTAIN"), true),
            (Some("WRITE"), true),
            (Some("TRIAGE"), true),
            (Some("READ"), false),
            (None, false),
            (Some("HACKED"), false),
        ] {
            assert_eq!(lock_gate(perm), gate, "{perm:?}");
        }
        // D6: the composer hides only when locked AND unprivileged.
        for (locked, perm, visible) in [
            (false, Some("ADMIN"), true),
            (false, None, true),
            (true, Some("ADMIN"), true),
            (true, Some("TRIAGE"), true),
            (true, Some("READ"), false),
            (true, None, false),
        ] {
            assert_eq!(composer_visible(locked, perm), visible, "{locked} {perm:?}");
        }
        // D5: reason copy; unknown/absent degrade to the bare tag.
        assert_eq!(lock_reason_label(Some("OFF_TOPIC")), "off-topic");
        assert_eq!(lock_reason_label(Some("TOO_HEATED")), "too heated");
        assert_eq!(lock_reason_label(Some("RESOLVED")), "resolved");
        assert_eq!(lock_reason_label(Some("SPAM")), "spam");
        assert_eq!(lock_reason_label(Some("MYSTERY")), "");
        assert_eq!(lock_reason_label(None), "");
    }

    #[gpui::test]
    fn reaction_rows_toggle_and_single_flight(cx: &mut gpui::TestAppContext) {
        let (page, vcx) = probe_page(cx);
        // The body row renders: the armed chip plus the + affordance; the
        // zero-count EYES group renders no chip (D3).
        assert!(vcx.debug_bounds("issue-reaction-row-THUMBS_UP").is_some());
        assert!(vcx.debug_bounds("issue-reaction-row-EYES").is_none());
        assert!(vcx.debug_bounds("issue-reaction-row-add").is_some());
        // Comment 0 carries its own row; comment 1 (no fields) stays clean.
        assert!(
            vcx.debug_bounds("issue-comment-0-reaction-row-HEART")
                .is_some()
        );
        assert!(
            vcx.debug_bounds("issue-comment-1-reaction-row-add")
                .is_none()
        );

        // Chip click: armed → remove is the direction; single-flight holds.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.toggle_reaction(
                    "I_9".into(),
                    true,
                    gql::ReactionContent::ThumbsUp,
                    window,
                    cx,
                );
                assert_eq!(page.busy, Some(IssueBusyOp::ToggleReaction));
            });
        });
        // Busy blocks a second write (D7 single-flight).
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.toggle_reaction("I_9".into(), false, gql::ReactionContent::Heart, window, cx);
                assert_eq!(page.busy, Some(IssueBusyOp::ToggleReaction));
                page.busy = None;
            });
        });

        // §10 amendment: the picker is a component-owned PopupMenu on
        // the + button — no page state to drive, Esc/dismiss are the
        // menu's. The frozen per-row direction is the pure helper; the
        // armed direction posts removeReaction through the same toggle.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                let groups = &page.detail.as_ref().unwrap().reaction_groups;
                assert!(reaction_direction(groups, gql::ReactionContent::ThumbsUp));
                assert!(!reaction_direction(groups, gql::ReactionContent::Heart));
                page.toggle_reaction(
                    "I_9".into(),
                    reaction_direction(groups, gql::ReactionContent::ThumbsUp),
                    gql::ReactionContent::ThumbsUp,
                    window,
                    cx,
                );
                assert_eq!(page.busy, Some(IssueBusyOp::ToggleReaction));
                page.busy = None;
            });
        });
    }

    #[gpui::test]
    fn reaction_menu_item_click_commits(cx: &mut gpui::TestAppContext) {
        let (page, vcx) = probe_page(cx);
        // Real click on the + trigger opens the component-owned menu.
        let add = vcx
            .debug_bounds("issue-reaction-row-add")
            .expect("+ row visible");
        vcx.simulate_click(add.center(), gpui::Modifiers::default());
        let _ = vcx.debug_bounds("issue-reaction-row-THUMBS_UP"); // flush frames
        let row0 = vcx
            .debug_bounds("issue-reaction-pick-THUMBS_UP")
            .expect("menu row visible");
        vcx.simulate_click(row0.center(), gpui::Modifiers::default());
        // the handler defers to the end of the effect cycle — flush it
        let _ = vcx.debug_bounds("issue-reaction-pick-THUMBS_UP");
        vcx.update(|_, cx| {
            page.update(cx, |page, _| {
                // The full chain ran: deferred post → ProbeHttp failed it →
                // the scoped error landed (D3: reaction errors stay on the
                // Timeline slot). busy already cycled back to None.
                let err = page.mutation_error.as_ref().expect("deferred post applied");
                assert_eq!(err.scope, ErrorScope::Timeline);
                assert!(err.message.contains("Network error"), "{:?}", err.message);
            });
        });
    }

    #[gpui::test]
    fn lock_row_gates_and_composer_hides_while_locked(cx: &mut gpui::TestAppContext) {
        let (page, vcx) = probe_page(cx);
        // Fixture: unlocked + ADMIN → the panel shows with the Lock button;
        // the composer is up (D6).
        assert!(vcx.debug_bounds("issue-lock-tag").is_some());
        assert!(vcx.debug_bounds("issue-lock-toggle").is_some());
        assert!(vcx.debug_bounds("issue-comment-box").is_some());
        assert!(vcx.debug_bounds("issue-locked-notice").is_none());

        // Locked + privileged: tag flips, composer stays (maintainers
        // comment on locked threads).
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                let d = page.detail.as_mut().unwrap();
                d.locked = true;
                d.active_lock_reason = Some("TOO_HEATED".into());
                cx.notify();
            });
        });
        assert!(vcx.debug_bounds("issue-comment-box").is_some());

        // Locked + unprivileged: the toggle hides (no rights) and the
        // composer swaps to the locked notice (D6).
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                let d = page.detail.as_mut().unwrap();
                d.viewer_permission = None;
                cx.notify();
            });
        });
        assert!(vcx.debug_bounds("issue-lock-toggle").is_none());
        assert!(vcx.debug_bounds("issue-comment-box").is_none());
        assert!(vcx.debug_bounds("issue-locked-notice").is_some());

        // Unlocked + unprivileged: no panel at all (nothing to show, nothing
        // to do) and the composer returns.
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                let d = page.detail.as_mut().unwrap();
                d.locked = false;
                cx.notify();
            });
        });
        assert!(vcx.debug_bounds("issue-lock-row").is_none());
        assert!(vcx.debug_bounds("issue-comment-box").is_some());

        // Toggle direction derives from state: unlocked posts Lock, busy
        // guards a second flip.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                let d = page.detail.as_mut().unwrap();
                d.viewer_permission = Some("TRIAGE".into());
                cx.notify();
            });
            page.update(cx, |page, cx| page.toggle_lock(window, cx));
            page.update(cx, |page, _| {
                assert_eq!(page.busy, Some(IssueBusyOp::ToggleLock));
            });
            page.update(cx, |page, cx| page.toggle_lock(window, cx));
            page.update(cx, |page, _| {
                assert_eq!(page.busy, Some(IssueBusyOp::ToggleLock), "no second write");
                page.busy = None;
            });
        });
    }
}
