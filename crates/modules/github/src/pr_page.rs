//! PR detail page (docs/0016 §6 stages 1–3) — the full-content takeover that
//! opens when a repo-view PR row is clicked: header (state badge, title,
//! number, author/branch meta, refresh + open-on-GitHub), an underline
//! `TabBar` with live counts, the Conversation read path (D3/D4), the
//! stage-2 write path (comment/review submission, description editing,
//! the merge/close rail — every mutation runs through `run_mutation` on
//! the shared tracked transport and ends with a detail refetch), and the
//! stage-3 read tabs: the Commits list (D6) and the Checks list (D7) from
//! the live-verified `statusCheckRollup.contexts` union.
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
    ActiveTheme, Disableable, Icon, IconName, Sizable as _,
    avatar::Avatar,
    badge::Badge,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    group_box::GroupBox,
    h_flex,
    input::{InputEvent, InputState, Textarea, TextareaState},
    list::{ListEvent, ListState},
    menu::{DropdownMenu as _, PopupMenuItem},
    scroll::ScrollableElement as _,
    searchable_list::SearchableVec,
    select::{Select, SelectEvent, SelectState},
    skeleton::Skeleton,
    spinner::Spinner,
    tab::{Tab, TabBar},
    tree::TreeState,
    v_flex,
};

use serde_json::json;

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// docs/0026 D9: how long the jumped comment keeps its accent ring before
/// the fade clears it (GitHub-like flash).
pub(crate) const ANCHOR_FADE: Duration = Duration::from_secs(3);

use crate::api::{self, PrFile};
use crate::files::{self, FileEntry, FileListDelegate, FilesView, build_file_forest, path_matches};
use crate::gql;
use crate::issue_page::{reaction_chips, reaction_direction};
use crate::markdown;
use crate::page::{LaneClass, lane_min_age};
use crate::pickers::{
    PickerDraft, PickerFetch, PickerRow, PickerSource, assignee_rows, compute_replace_ids,
    label_rows, milestone_rows, reviewer_rows,
};
use crate::service::{
    Fetch, FetchSpec, GithubState, Kind, Mutation, run_check_log_chain, run_mutation,
    run_spec_tracked,
};
use crate::syntax;
use dashboard_core::freshness::{Freshness, freshness_row, relative_age};

actions!(github, [DismissPrPage]);

/// Ask the hosting `GithubPage` to close the PR sub-page (docs/0016 D1).
pub(crate) enum PrPageEvent {
    CloseRequested,
}

/// One check row's log-pane lifecycle (docs/0039 D4/D5). Content is the
/// pane-ready tail (already escape-stripped and \r-folded), so a render
/// clone is an Arc bump, not a 512 KiB copy.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CheckLogState {
    /// The resolution chain is running.
    Running,
    Ready {
        text: SharedString,
        truncated: bool,
    },
    Failed(String),
}

/// The pane's tail cap (docs/0039 D4): CI conclusions — the failure —
/// live at the end of the log; 512 KiB is ~10k lines of terminal-width
/// text, far past any reading need.
pub(crate) const LOG_TAIL_CAP: usize = 512 * 1024;

/// Pane-ready text from a raw job log (docs/0039 D4): fold `\r` progress
/// overwrites to the segment a terminal would show, strip terminal
/// escape sequences (CI logs carry CSI commands and OSC hyperlinks —
/// gpui renders neither), then keep the tail at the cap on a line
/// boundary without splitting a UTF-8 character. Returns
/// `(text, truncated)`.
pub(crate) fn prepare_log_body(raw: &str, cap: usize) -> (String, bool) {
    // A `\r\n` is a plain newline — only a bare `\r` overwrites.
    let normalized = raw.replace("\r\n", "\n");
    let cleaned: String = normalized
        .split('\n')
        .map(|line| strip_escapes(line.split('\r').next_back().unwrap_or(line)))
        .collect::<Vec<_>>()
        .join("\n");
    if cleaned.len() <= cap {
        return (cleaned, false);
    }
    let mut start = cleaned.len() - cap;
    while start < cleaned.len() && !cleaned.is_char_boundary(start) {
        start += 1;
    }
    // Drop the partial line at the cut — the pane opens at a line start.
    let cut = cleaned[start..]
        .find('\n')
        .map_or(start, |i| (start + i + 1).min(cleaned.len()));
    (cleaned[cut..].to_string(), true)
}

/// Strip ANSI/ECMA-48 escape sequences from one `\r`-folded segment:
/// CSI (`ESC [ … final-byte`) and OSC (`ESC ] … BEL | ESC \`) vanish;
/// a lone ESC is dropped. Bytes outside sequences pass through.
fn strip_escapes(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    let mut chars = segment.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                // CSI parameter/intermediate bytes, then a final @..~.
                for n in chars.by_ref() {
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC: terminated by BEL or ESC \.
                for n in chars.by_ref() {
                    if n == '\u{7}' {
                        break;
                    }
                    if n == '\u{1b}' {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {} // a lone or truncated escape — drop it
        }
    }
    out
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
    detail: Fetch<gql::PrDetail>,
    tab: PrTab,
    focus_handle: FocusHandle,
    /// Comment box (D4): draft textarea + action select.
    comment: Entity<TextareaState>,
    action_select: Entity<SelectState<SearchableVec<SharedString>>>,
    /// The write path in flight (D4/D5) — one at a time; every write
    /// surface disables while set.
    pub(crate) busy: Option<BusyOp>,
    /// The last mutation failure, scoped to the surface that shows it.
    pub(crate) mutation_error: Option<MutationError>,
    /// Prefilled textarea while the description is being edited (D4).
    description_edit: Option<Entity<TextareaState>>,
    /// The open rail picker (docs/0043 D5/D7) — one at a time, mutually
    /// exclusive with the description editor and timeline surfaces.
    meta_edit: Option<PrMetaEditor>,
    /// Picker source pages (0022 D5 lifecycle): lazy on first open,
    /// session-cached, re-fetched on refresh while open. The
    /// `assignableUsers` page is shared by the assignee and reviewer
    /// pickers — one fetch, two consumers (docs/0043 D5).
    labels_source: PickerFetch<Vec<gql::IssueLabel>>,
    users_source: PickerFetch<Vec<gql::IssueAssignee>>,
    milestones_source: PickerFetch<Vec<gql::IssueMilestone>>,
    /// Row-level editor (0019 D4): (node id, editor). At most one editor
    /// is open anywhere on the page — opening one closes the description
    /// editor, other row editors, and any pending delete confirm.
    timeline_edit: Option<(String, Entity<TextareaState>)>,
    /// Node id awaiting delete confirmation (0019 D4, inline two-step).
    delete_confirm: Option<String>,
    /// 0030 D4: the armed row composer on the Files tab — one at a time;
    /// opening another row's composer discards the previous unsubmitted
    /// text (drafts are explicit via "Start a review").
    pub(crate) line_composer: Option<LineComposer>,
    /// 0030 D4: the open reply box — (thread node id, editor); one at a
    /// time, gated by the thread's `viewerCanReply`.
    pub(crate) reply_composer: Option<(String, Entity<TextareaState>)>,
    /// 0030 #213: the open resolve-reason picker — (thread node id); one
    /// at a time, gated by the thread's `viewerCanResolve`.
    pub(crate) resolve_picker: Option<String>,
    /// #212: the review-thread comment editor — (comment node id,
    /// editor). One editor anywhere on the page (0019 D4); timeline
    /// surfaces yield to it and vice versa.
    pub(crate) thread_edit: Option<(String, Entity<TextareaState>)>,
    /// #212: the review-thread comment id awaiting delete confirmation
    /// (0019 D4 inline two-step).
    pub(crate) thread_delete_confirm: Option<String>,
    /// 0030 D4: queued draft comments — page-local, submitted as ONE
    /// batch by the Finish-review bar. Session-only (closing the page
    /// discards), like every other draft surface here.
    pub(crate) drafts: Vec<LineDraft>,
    /// The Finish bar's summary editor — built with the first draft.
    pub(crate) finish_summary: Option<Entity<TextareaState>>,
    /// The event the Finish bar will submit (docs/0030 D4; Comment default).
    pub(crate) finish_event: FinishEvent,
    /// Methods the repo allows (D5), from the detail payload — drives the
    /// method Select and the Merge button verb.
    methods: Vec<MergeMethod>,
    /// Method Select state — built once, on the first render that has a
    /// detail payload (SelectState needs a window; render has one).
    method_select: Option<Entity<SelectState<SearchableVec<SharedString>>>>,
    /// Changed-files fetch lifecycle (D8): lazy — `Idle` until the
    /// Files tab is first opened, then one REST walk; `Ready` keeps the
    /// parsed list (a refresh refetches it).
    /// Changed-files fetch lifecycle (D8): lazy — `Idle` until the
    /// Files tab is first opened, then one REST walk; `Ready` keeps the
    /// parsed list (a refresh refetches it).
    pub(crate) files: Fetch<Vec<PrFile>>,
    /// Diff-row highlight memo (docs/0040 D5) — per-file span lists,
    /// invalidated by patch text or palette change. Pure memo: filled
    /// from render, never notifies.
    pub(crate) highlight_cache: crate::syntax::HighlightCache,
    /// Check log panes (docs/0039 D4/D5), keyed by check name — unique
    /// across the head ref's checks. `Ready` content is immutable (a
    /// completed job's log never changes) and cached per job fetch;
    /// `Failed` keeps the message until a Retry re-runs the chain.
    check_logs: BTreeMap<String, CheckLogState>,
    /// The one open log pane's scroll (D4 accordion — at most one pane);
    /// driven to the bottom on every expand/land so the failure is on
    /// screen first.
    check_log_scroll: ScrollHandle,
    /// The row id under the pointer (docs/0040 §4 D6 amendment): the
    /// per-row comment button renders only for this row — 460 always-on
    /// Buttons+tooltips were the diff-pane scroll jank.
    pub(crate) hovered_row: Option<SharedString>,
    /// Virtualized diff body (docs/0041 D1/D2): the list state lives on
    /// the page (intrusive — heights cache across frames), the flattened
    /// item list is rebuilt by `sync_diff_list`, and `diff_key` is the
    /// (file, patch) the current list was built for.
    pub(crate) diff_list: gpui::ListState,
    pub(crate) diff_items: std::sync::Arc<Vec<crate::files::DiffItem>>,
    diff_key: Option<(SharedString, u64)>,
    /// The diff wrapper's horizontal scroll (docs/0041 D5) — persistent
    /// so x-position survives frames.
    pub(crate) diff_x_scroll: ScrollHandle,
    /// One `anchor_patch` parse per file version (docs/0041 D4), shared
    /// by placement, body, and the highlight pump.
    pub(crate) anchor_cache: crate::files::AnchorCache,
    /// Height-resync observers (docs/0041 D3): typing grows the composer
    /// / reply textareas; the subscriptions live while the editor does.
    composer_sub: Option<gpui::Subscription>,
    reply_sub: Option<gpui::Subscription>,
    /// The accordion: at most one expanded check at a time (D4).
    expanded_check: Option<String>,
    /// Review-threads fetch lifecycle (docs/0030 D1): lazy like the
    /// files walk — one GraphQL one-shot when the Files tab first opens,
    /// refetched on refresh (once loaded) and after thread mutations.
    pub(crate) threads: Fetch<gql::PrThreads>,
    /// Per-file expand state for the outdated/stray thread bucket
    /// (docs/0030 D3), keyed by path so a refetch never collapses it
    /// behind the user's back.
    pub(crate) outdated_open: BTreeMap<String, bool>,
    /// Files sidebar (docs/0018 D3/D6/D7): view mode + the path-keyed
    /// List selection (the Tree's selection lives in `file_tree`). The
    /// sidebar entities are built once in `new` — which has the window
    /// `SelectState`/`InputState` need — and stay subscribed there.
    pub(crate) files_view: FilesView,
    pub(crate) selected_file: Option<SharedString>,
    pub(crate) view_select: Entity<SelectState<SearchableVec<SharedString>>>,
    pub(crate) file_filter: Entity<InputState>,
    pub(crate) file_list: Entity<ListState<FileListDelegate>>,
    pub(crate) file_tree: Entity<TreeState>,
    /// Parsed markdown bodies (docs/0017 D6) keyed by card id — rebuilt
    /// whenever a detail payload lands, never re-parsed per frame.
    pub(crate) md: BTreeMap<String, Arc<[markdown::Block]>>,
    /// docs/0026 D9 (#154): the notification hand-off's comment anchor —
    /// see [`CommentAnchor`]. `Idle` unless opened from the jump.
    pub(crate) anchor: CommentAnchor,
    /// The conversation scroll — tracked so the jump can land the timeline
    /// on the anchored comment (docs/0026 D9).
    timeline_scroll: ScrollHandle,
    /// One-shot guard for the jump (docs/0026 D9): the builder arms the
    /// scroll exactly once, the frame the jumped card first renders.
    jump_done: Cell<bool>,
}

/// The notification hand-off's comment anchor (docs/0026 D9, #154).
/// `Pending` carries the REST comment id from `latest_comment_url` until
/// the detail payload lands; a comment with that `databaseId` arms the
/// jump (`Jumping`). The jump fires the frame the row actually mounts and
/// the state becomes `Highlighted` (accent ring, GitHub-like); a short
/// fade returns it to `Idle`. A target outside the loaded conversation
/// (deleted, beyond the first 100, a review comment) gives up quietly —
/// no jump, no error. Shared with the issue page (same conversation shape).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CommentAnchor {
    /// REST id awaiting the detail payload.
    Pending(i64),
    /// Matched node id — fires on the row's next mounted frame.
    Jumping(String),
    /// Jumped — accent ring until the fade timer clears it.
    Highlighted(String),
    /// No anchor, given up, or faded.
    Idle,
}

impl EventEmitter<PrPageEvent> for PrDetailPage {}

impl PrDetailPage {
    pub(crate) fn new(
        state: Entity<GithubState>,
        repo: String,
        summary: gql::PullRequest,
        anchor: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let timeline_scroll = ScrollHandle::new();
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
        // Files sidebar (docs/0018): the entities are built up front —
        // `new` has the window `SelectState`/`InputState` need — and the
        // subscriptions below wire them to the page for its lifetime.
        let default_view: SharedString = files::VIEW_LIST_LABEL.into();
        let view_select = cx.new(|cx| {
            let mut select = SelectState::new(
                SearchableVec::from(vec![
                    SharedString::from(files::VIEW_LIST_LABEL),
                    SharedString::from(files::VIEW_TREE_LABEL),
                ]),
                None,
                window,
                cx,
            );
            select.set_selected_value(&default_view, window, cx);
            select
        });
        let file_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter files…"));
        let file_list =
            cx.new(|cx| ListState::new(FileListDelegate::default(), window, cx).searchable(false));
        let file_tree = cx.new(|cx| TreeState::new(cx));
        let number = summary.number;
        let mut page = Self {
            state,
            repo,
            number,
            summary,
            detail: Fetch::new(),
            tab: PrTab::Conversation,
            focus_handle: cx.focus_handle(),
            comment,
            action_select,
            busy: None,
            mutation_error: None,
            description_edit: None,
            meta_edit: None,
            labels_source: PickerFetch::Idle,
            users_source: PickerFetch::Idle,
            milestones_source: PickerFetch::Idle,
            timeline_edit: None,
            delete_confirm: None,
            methods: Vec::new(),
            method_select: None,
            files: Fetch::new(),
            highlight_cache: crate::syntax::HighlightCache::default(),
            check_logs: BTreeMap::new(),
            expanded_check: None,
            check_log_scroll: ScrollHandle::new(),
            hovered_row: None,
            diff_list: gpui::ListState::new(0, gpui::ListAlignment::Top, px(400.)),
            diff_items: std::sync::Arc::default(),
            diff_key: None,
            diff_x_scroll: ScrollHandle::new(),
            anchor_cache: crate::files::AnchorCache::default(),
            composer_sub: None,
            reply_sub: None,
            threads: Fetch::new(),
            line_composer: None,
            reply_composer: None,
            drafts: Vec::new(),
            resolve_picker: None,
            thread_edit: None,
            thread_delete_confirm: None,
            finish_summary: None,
            finish_event: FinishEvent::default(),
            outdated_open: BTreeMap::new(),
            files_view: FilesView::default(),
            selected_file: None,
            view_select: view_select.clone(),
            file_filter: file_filter.clone(),
            file_list: file_list.clone(),
            file_tree: file_tree.clone(),
            md: BTreeMap::new(),
            anchor: anchor.map_or(CommentAnchor::Idle, CommentAnchor::Pending),
            timeline_scroll,
            jump_done: Cell::new(false),
        };
        // View-mode switch (docs/0018 D3): swaps the sidebar view. Selection
        // is per view (D7), so the diff pane follows the newly active view.
        cx.subscribe_in(
            &view_select,
            window,
            |this, _, event: &SelectEvent<SearchableVec<SharedString>>, _, cx| {
                let SelectEvent::Confirm(Some(label)) = event else {
                    return;
                };
                let next = if label.as_ref() == files::VIEW_TREE_LABEL {
                    FilesView::Tree
                } else {
                    FilesView::List
                };
                if this.files_view != next {
                    this.files_view = next;
                    cx.notify();
                }
            },
        )
        .detach();

        // Shared filter input (docs/0018 D6): one query drives both views.
        cx.subscribe_in(
            &file_filter,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let q = this.file_filter.read(cx).value().trim().to_string();
                    this.apply_file_filter(&q, window, cx);
                }
            },
        )
        .detach();

        // List selection (docs/0018 D4/D7): Confirm = click or Enter.
        cx.subscribe_in(&file_list, window, |this, _, event: &ListEvent, _, cx| {
            if let ListEvent::Confirm(ix) = event {
                let path = this.file_list.read(cx).delegate().path_at(*ix);
                if let Some(path) = path {
                    this.select_file(Some(path), cx);
                }
            }
        })
        .detach();

        // Tree selection (docs/0018 D5): upstream emits no selection event —
        // clicks notify the TreeState; observing the entity catches every
        // change (selection, expand, collapse) and the diff pane reads
        // `selected_entry()` at render time.
        cx.observe(&file_tree, |_, _, cx| cx.notify()).detach();
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
        let state = self.state.clone();
        let spec = Self::detail_spec(&self.repo, self.number);
        log::debug!(
            "pr-detail: fetching {}",
            spec.ident.as_deref().unwrap_or("?")
        );
        // Arm the shared guard synchronously (docs/0031 D1/D2): a
        // duplicate dispatch (a second refresh click while one is in
        // flight) skips before the record's `begin` can orphan it.
        // Record-owning fetches dispatch on the raw lane and clear their
        // own guard when the run lands — the wrapper's skip decision is
        // for the stateless lanes only (docs/0031 §8).
        let key = (spec.kind, spec.ident.clone());
        let armed = self.state.update(cx, |s, _| s.in_flight_arm(key.clone()));
        if !armed {
            log::debug!("pr-detail: skipped — an identical read is in flight");
            return;
        }
        self.detail.begin();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = run_spec_tracked(state.clone(), spec, cx).await;
            cx.update(|cx| state.update(cx, |s, _| s.in_flight_clear(&key)));
            let _ = this.update(cx, |page, cx| page.apply_detail_result(result, cx));
        })
        .detach();
    }

    fn apply_detail_result(&mut self, result: crate::service::SpecResult, cx: &mut Context<Self>) {
        if let Some(m) = &result.network {
            self.detail.fail(format!("Network error: {m}"));
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
            self.detail.fail(format!("GitHub answered HTTP {status}"));
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
                            self.md = build_md_cache(&self.repo, &detail);
                            // Thread keys live in the same cache — a detail
                            // refetch wipes it, so re-fold them (0030 D1).
                            self.extend_threads_md();
                            // 0019 D4: an editor/confirm whose node is gone
                            // with the new payload has nothing left to edit —
                            // drop it (deleted elsewhere, refetch race).
                            let still_there = |id: &str| {
                                node_kind(&detail.comments.nodes, &detail.reviews.nodes, id)
                                    .is_some()
                            };
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
                            self.detail.land(detail);
                            // docs/0043 D5: a picker open across the
                            // refetch realigns to the new payload
                            // (0022 §11 — choices preserved by id).
                            if self.meta_edit.is_some() {
                                let source = match self.meta_edit.as_ref().unwrap() {
                                    PrMetaEditor::Labels(_) => PickerSource::Labels,
                                    PrMetaEditor::Assignees(_) => PickerSource::Assignees,
                                    PrMetaEditor::Reviewers(_) => PickerSource::Reviewers,
                                    PrMetaEditor::Milestone { .. } => PickerSource::Milestones,
                                };
                                self.realign_open_picker(source, cx);
                            }
                            // docs/0026 D9: the payload is in — try to arm
                            // the notification jump against it.
                            self.resolve_comment_anchor();
                        }
                        None => {
                            log::warn!("pr-detail: repository/pullRequest null — keeping header");
                            self.detail
                                .fail("PR not found (removed, or access lost)".into());
                        }
                    }
                }
                Err(e) => {
                    log::warn!("pr-detail payload: {e}");
                    self.detail.fail("Malformed payload".into());
                }
            }
        }
        cx.notify();
    }

    /// docs/0026 D9: resolve the pending REST-id anchor against the landed
    /// detail — the matching comment node arms the jump; a miss gives up.
    fn resolve_comment_anchor(&mut self) {
        let CommentAnchor::Pending(target) = self.anchor else {
            return;
        };
        self.anchor = self
            .detail
            .data()
            .and_then(|d| {
                d.comments
                    .nodes
                    .iter()
                    .find(|c| c.database_id == Some(target))
            })
            .map(|c| {
                log::info!("pr-page anchor: jumping to comment {}", c.id);
                self.jump_done.set(false);
                CommentAnchor::Jumping(c.id.clone())
            })
            .unwrap_or_else(|| {
                log::info!(
                    "pr-page anchor: comment {target} not in the loaded conversation — no jump",
                );
                CommentAnchor::Idle
            });
    }

    /// Whether the anchored comment renders as a plain card this frame —
    /// an editor/delete-confirm swap on it, or a non-conversation tab,
    /// defers the jump until the card is really mounted (docs/0026 D9).
    fn anchor_row_mounts(&self, node_id: &str) -> bool {
        self.tab == PrTab::Conversation
            && self.timeline_edit.as_ref().map(|(id, _)| id.as_str()) != Some(node_id)
            && self.delete_confirm.as_deref() != Some(node_id)
    }

    /// The highlight's fade (docs/0026 D9): GitHub-like — the accent ring
    /// clears shortly after the jump. A page close tears the entity down
    /// first; a later jump re-arms (Highlighted guard).
    fn start_anchor_fade(&mut self, cx: &mut Context<Self>) {
        let fade = ANCHOR_FADE;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(fade).await;
            let _ = this.update(cx, |page, cx| {
                if matches!(page.anchor, CommentAnchor::Highlighted(_)) {
                    page.anchor = CommentAnchor::Idle;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    // --- files tab (docs/0016 §6 stage 4, D8) -------------------------------

    /// The changed-files spec: one REST walk over
    /// `GET /repos/{o}/{r}/pulls/{n}/files`, page-stitched by the
    /// transport (`run_rest`, 100/page). No ETag — page-owned like
    /// [`Self::detail_spec`]. 30 pages = GitHub's 3000-file cap; the
    /// walk stops at the first short page long before it.
    fn files_spec(repo: &str, number: i64, etag: Option<String>) -> FetchSpec {
        let (owner, name) = split_repo(repo);
        FetchSpec {
            kind: Kind::PrFiles,
            url: api::pr_files_url(owner, name, number, 1),
            etag,
            ident: Some(format!("{repo}#{number}")),
            max_pages: 30,
            variables: None,
        }
    }

    // --- check log panes (docs/0039) ----------------------------------------

    /// Expander toggle (D4): the accordion is one-open-at-a-time;
    /// re-toggling the open row collapses it. Cached `Ready` content
    /// re-expands instantly (immutable, D5); everything else chains.
    fn toggle_check_log(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.expanded_check.as_deref() == Some(name) {
            self.expanded_check = None;
            cx.notify();
            return;
        }
        self.expanded_check = Some(name.to_string());
        if !matches!(self.check_logs.get(name), Some(CheckLogState::Ready { .. })) {
            self.start_check_log_chain(name, cx);
        }
        // The failure lives at the tail (D4) — every expansion opens at
        // the bottom; the flag is consumed by the pane's next paint.
        self.check_log_scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Retry (D5): drop the failed state and re-run the chain — the
    /// resolution steps re-resolve, so a re-run's new job ids are picked
    /// up naturally.
    fn retry_check_log(&mut self, name: &str, cx: &mut Context<Self>) {
        self.check_logs.remove(name);
        self.start_check_log_chain(name, cx);
    }

    fn start_check_log_chain(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.state.read(cx).auth.connected() {
            return; // the page is about to be torn down by the auth gate
        }
        let Some(head_oid) = self.detail.data().and_then(|d| d.head_oid.clone()) else {
            self.check_logs.insert(
                name.to_string(),
                CheckLogState::Failed("No head commit on this pull request".into()),
            );
            cx.notify();
            return;
        };
        let ident = format!("{}#{}#{}", self.repo, self.number, name);
        let key = (Kind::JobLog, Some(ident.clone()));
        let armed = self.state.update(cx, |s, _| s.in_flight_arm(key.clone()));
        if !armed {
            log::debug!("check-log: skipped — an identical chain is in flight");
            return;
        }
        self.check_logs
            .insert(name.to_string(), CheckLogState::Running);
        cx.notify();
        let state = self.state.clone();
        let repo = self.repo.clone();
        let number = self.number;
        let name = name.to_string();
        cx.spawn(async move |this, cx| {
            let outcome =
                run_check_log_chain(state.clone(), &repo, number, &head_oid, &name, cx).await;
            cx.update(|cx| state.update(cx, |s, _| s.in_flight_clear(&key)));
            let _ = this.update(cx, |page, cx| {
                page.check_logs.insert(
                    name.clone(),
                    match outcome {
                        Ok((text, truncated)) => CheckLogState::Ready {
                            text: SharedString::from(text),
                            truncated,
                        },
                        Err(e) => CheckLogState::Failed(e),
                    },
                );
                page.check_log_scroll.scroll_to_bottom();
                cx.notify();
            });
        })
        .detach();
    }

    /// Lazy fetch (D8): fired once, when the Files tab is first opened,
    /// and again on refresh once a list is showing.
    fn fetch_files(&mut self, cx: &mut Context<Self>) {
        if !self.state.read(cx).auth.connected() {
            return; // the page is about to be torn down by the auth gate
        }
        let state = self.state.clone();
        // docs/0038 D4: conditional revalidation when the lane holds
        // data — a cold session with a disk etag must full-fetch (a 304
        // against an empty record would strand it with nothing to keep).
        let ident = format!("{}#{}", self.repo, self.number);
        let etag = self
            .state
            .read(cx)
            .etag_for(Kind::PrFiles, Some(&ident))
            .filter(|_| self.files.is_ready());
        let spec = Self::files_spec(&self.repo, self.number, etag);
        log::debug!(
            "pr-files: fetching {}",
            spec.ident.as_deref().unwrap_or("?")
        );
        let key = (spec.kind, spec.ident.clone());
        let armed = self.state.update(cx, |s, _| s.in_flight_arm(key.clone()));
        if !armed {
            log::debug!("pr-files: skipped — an identical read is in flight");
            return;
        }
        self.files.begin();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = run_spec_tracked(state.clone(), spec, cx).await;
            cx.update(|cx| state.update(cx, |s, _| s.in_flight_clear(&key)));
            let _ = this.update(cx, |page, cx| page.apply_files_result(result, cx));
        })
        .detach();
    }

    fn apply_files_result(&mut self, result: crate::service::SpecResult, cx: &mut Context<Self>) {
        if let Some(m) = &result.network {
            self.files.fail(format!("Network error: {m}"));
        } else if result.status == 304 {
            // docs/0038 D4: unchanged — forward so a refreshed etag
            // persists, then land without re-parsing.
            self.state.update(cx, |s, _| s.apply(result));
            self.files.land_unchanged();
        } else if result.status != 200 {
            // Shared 401/403 handling, same as the detail fetch (D2).
            let status = result.status;
            self.state.update(cx, |s, cx| {
                s.apply(result);
                cx.notify();
            });
            self.files.fail(format!("GitHub answered HTTP {status}"));
        } else {
            match serde_json::from_str::<Vec<PrFile>>(result.body.as_deref().unwrap_or("[]")) {
                Ok(files) => {
                    log::info!(
                        "pr-files applied: {}#{} — {} files",
                        self.repo,
                        self.number,
                        files.len()
                    );
                    self.files.land(files);
                    // No bulk memo clear here (docs/0040 D5, amended):
                    // entries self-invalidate per file by patch hash, so
                    // an unchanged diff survives the refresh without a
                    // render-time re-tokenize spike.
                    self.apply_files_payload(cx);
                }
                Err(e) => {
                    log::warn!("pr-files payload: {e}");
                    self.files.fail("Malformed payload".into());
                }
            }
        }
        cx.notify();
    }

    /// Fetch-ready apply (docs/0018 D6/D7): snapshot the payload into the
    /// sidebar entities, re-applying the active filter (a refresh must not
    /// clear it), then auto-select the first visible file so the diff pane
    /// is never mysteriously empty.
    fn apply_files_payload(&mut self, cx: &mut Context<Self>) {
        let Some(files) = self.files.data() else {
            return;
        };
        let entries: Vec<FileEntry> = files.iter().map(FileEntry::from_api).collect();
        let query = self.file_filter.read(cx).value().trim().to_string();
        // A refresh must not yank the reader to the first file (0018
        // D6 amendment): keep the current selection while it still
        // exists and matches the filter; fall back to the first match
        // only when there is none (first open, deleted file).
        let selected = match self.selected_file.as_ref() {
            Some(current)
                if entries
                    .iter()
                    .any(|f| &f.path == current && path_matches(f.path.as_ref(), &query)) =>
            {
                Some(current.clone())
            }
            _ => entries
                .iter()
                .find(|f| path_matches(f.path.as_ref(), &query))
                .map(|f| f.path.clone()),
        };
        self.selected_file = selected.clone();
        self.file_list.update(cx, |st, cx| {
            st.delegate_mut().set_files(entries.clone());
            st.delegate_mut().set_selected(selected.clone());
            cx.notify();
        });
        self.file_tree.update(cx, |st, cx| {
            st.set_items(build_file_forest(&entries), cx);
            // The List's auto-selection does not carry to the Tree (D7,
            // per-view selection); selecting entry 0 would pick a folder.
        });
        cx.notify();
    }

    /// Schedule background tokenization for the selected file when its
    /// memo entry is missing (0040 D5 amended). The diff renders plain
    /// until the task lands; `finish` notifies and the next frame is
    /// colored. Measured: ~0.6 ms/row (fancy-regex) — 440 ms cold for a
    /// 460-row Rust diff, never acceptable inside render.
    /// Rebuild the virtualized diff item list (docs/0041 D2/D3). Runs at
    /// the top of `render`, next to `pump_highlight`: pure state work —
    /// reset when the (file, patch) changed, a minimal splice when the
    /// flattened items moved, nothing when they didn't.
    fn sync_diff_list(&mut self) {
        use crate::files::{flatten_diff_items, thread_placement};
        if self.tab != PrTab::Files {
            return;
        }
        let selected = self.selected_file.clone();
        let file = self.files.data().and_then(|files| {
            files
                .iter()
                .find(|f| Some(f.filename.as_ref()) == selected.as_deref())
        });
        let Some(file) = file else {
            self.diff_key = None;
            if !self.diff_items.is_empty() {
                self.diff_items = std::sync::Arc::default();
                self.diff_list.reset(0);
            }
            return;
        };
        let path = file.filename.clone();
        let Some(patch) = file.patch.as_deref() else {
            self.diff_key = None;
            if !self.diff_items.is_empty() {
                self.diff_items = std::sync::Arc::default();
                self.diff_list.reset(0);
            }
            return;
        };
        let rows = self.anchor_cache.rows_for(&path, patch);
        let key = (
            SharedString::from(path.as_str()),
            crate::syntax::hash_patch(patch),
        );
        if self.diff_key.as_ref() != Some(&key) {
            let placed = thread_placement(self, file, &rows);
            let items = flatten_diff_items(self, &rows, &placed);
            self.diff_list.reset(items.len());
            self.diff_items = std::sync::Arc::new(items);
            self.diff_key = Some(key);
            return;
        }
        let placed = thread_placement(self, file, &rows);
        let items = flatten_diff_items(self, &rows, &placed);
        if items != *self.diff_items {
            let old_len = self.diff_items.len();
            let first = items
                .iter()
                .zip(self.diff_items.iter())
                .position(|(a, b)| a != b)
                .unwrap_or(items.len().min(old_len));
            self.diff_list.splice(first..old_len, items.len() - first);
            self.diff_items = std::sync::Arc::new(items);
        }
    }

    /// Resync the list from `from_ix` after a height-only change
    /// (docs/0041 D3): composer open/close, drafts, reply box, resolve
    /// picker, stray bucket. The splice re-measures the tail; the scroll
    /// anchor above `from_ix` survives.
    pub(crate) fn splice_diff_from(&mut self, from_ix: usize) {
        let len = self.diff_items.len();
        if from_ix < len {
            self.diff_list.splice(from_ix..len, len - from_ix);
        }
    }

    /// Re-measure from a thread's card (docs/0041 D3): the reply box and
    /// the resolve picker live inside the card, so opening/closing either
    /// changes its height without changing the flattened items.
    fn splice_diff_thread(&mut self, thread_id: &str) {
        let nodes = self.threads_nodes();
        let ix = self
            .diff_items
            .iter()
            .position(|it| {
                matches!(it, crate::files::DiffItem::ThreadCard(tix)
                    if nodes.get(*tix).is_some_and(|t| t.id == thread_id))
            })
            .unwrap_or(0);
        self.splice_diff_from(ix);
    }

    /// Re-measure from the composer item (typing grows the textarea).
    fn splice_diff_composer(&mut self) {
        let ix = self
            .diff_items
            .iter()
            .position(|it| matches!(it, crate::files::DiffItem::Composer(_)))
            .unwrap_or(0);
        self.splice_diff_from(ix);
    }

    /// Re-measure from the first draft chip.
    pub(crate) fn splice_diff_drafts(&mut self) {
        let ix = self
            .diff_items
            .iter()
            .position(|it| matches!(it, crate::files::DiffItem::Draft(_)))
            .unwrap_or(0);
        self.splice_diff_from(ix);
    }

    /// Re-measure from the stray bucket.
    pub(crate) fn splice_diff_stray(&mut self) {
        let ix = self
            .diff_items
            .iter()
            .position(|it| matches!(it, crate::files::DiffItem::StrayBucket))
            .unwrap_or(0);
        self.splice_diff_from(ix);
    }

    fn pump_highlight(&mut self, cx: &mut Context<Self>) {
        let Some(file) = self
            .files
            .data()
            .and_then(|files| {
                files
                    .iter()
                    .find(|f| Some(f.filename.as_str()) == self.selected_file.as_deref())
            })
            .cloned()
        else {
            return;
        };
        let Some(patch) = file.patch.clone() else {
            return;
        };
        let path = file.filename;
        let patch_hash = syntax::hash_patch(&patch);
        let theme_key = syntax::theme_key(cx.theme());
        // Already settled or already running for this exact (patch,
        // theme)? Nothing to do — without this, every render would
        // re-queue a task whose completion notifies, forever.
        if !matches!(
            self.highlight_cache.lookup(&path, patch_hash, &theme_key),
            syntax::Lookup::Miss
        ) {
            return;
        }
        // Cheap settled answers stay synchronous; only real tokenization
        // goes to a task.
        let rows = self.anchor_cache.rows_for(&path, &patch);
        if syntax::resolve_syntax(&path).is_none()
            || rows.len() > syntax::SYNTAX_HIGHLIGHT_MAX_LINES
        {
            self.highlight_cache
                .store_plain(&path, patch_hash, theme_key);
            return;
        }
        if !self
            .highlight_cache
            .begin(&path, patch_hash, theme_key.clone())
        {
            return; // a task for this exact (patch, theme) is running
        }
        let task_path = path.clone();
        let task_rows = (*rows).clone();
        cx.spawn(async move |this, cx| {
            let computed = cx
                .background_executor()
                .spawn(async move { syntax::compute_rows(&task_path, &task_rows) })
                .await;
            let _ = this.update(cx, |page, cx| {
                page.highlight_cache
                    .finish(&path, patch_hash, &theme_key, computed);
                cx.notify();
            });
        })
        .detach();
    }

    // --- review threads (docs/0030 D1) --------------------------------------

    /// The thread nodes behind [`Self::threads`] — empty unless `Ready`.
    /// files.rs reads this for placement; keep it cheap, it runs per render.
    pub(crate) fn threads_nodes(&self) -> &[gql::PrThread] {
        match self.threads.data() {
            Some(t) => &t.nodes,
            None => &[],
        }
    }

    /// The threads spec: one GraphQL one-shot (docs/0030 D1) — same
    /// variables and endpoint as the detail query, page-owned like it.
    fn threads_spec(repo: &str, number: i64) -> FetchSpec {
        let (owner, name) = split_repo(repo);
        FetchSpec {
            kind: Kind::PrThreads,
            url: gql::GRAPHQL_ENDPOINT.to_string(),
            etag: None,
            ident: Some(format!("{repo}#{number}")),
            max_pages: 1,
            variables: Some(serde_json::json!({ "owner": owner, "name": name, "number": number })),
        }
    }

    /// Lazy fetch (docs/0030 D1): fired when the Files tab first opens,
    /// on refresh once a thread list is showing, and after thread
    /// mutations (stage 2).
    pub(crate) fn fetch_threads(&mut self, cx: &mut Context<Self>) {
        if !self.state.read(cx).auth.connected() {
            return; // the page is about to be torn down by the auth gate
        }
        let state = self.state.clone();
        let spec = Self::threads_spec(&self.repo, self.number);
        log::debug!(
            "pr-threads: fetching {}",
            spec.ident.as_deref().unwrap_or("?")
        );
        let key = (spec.kind, spec.ident.clone());
        let armed = self.state.update(cx, |s, _| s.in_flight_arm(key.clone()));
        if !armed {
            log::debug!("pr-threads: skipped — an identical read is in flight");
            return;
        }
        self.threads.begin();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = run_spec_tracked(state.clone(), spec, cx).await;
            cx.update(|cx| state.update(cx, |s, _| s.in_flight_clear(&key)));
            let _ = this.update(cx, |page, cx| page.apply_threads_result(result, cx));
        })
        .detach();
    }

    /// docs/0032 D2: the host heartbeat and focus trigger land here —
    /// each lane re-dispatches by its own record (stale or failed);
    /// unopened (Idle) lanes stay untouched.
    pub(crate) fn auto_refresh(
        &mut self,
        engaged: bool,
        force_min_age: Option<Duration>,
        cx: &mut Context<Self>,
    ) {
        // One composed gate (docs/0037 D4): stale-or-failed (0032 D2)
        // AND past backoff (0036 D2) — the policy lives in core, the
        // call-site is one line per lane. docs/0038 D3: the engaged
        // tier shortens the threshold per transport class — detail and
        // threads are GraphQL (paid, half pace), files is REST (free
        // 304s, full pace).
        let gql_min = force_min_age.unwrap_or_else(|| lane_min_age(LaneClass::Gql, engaged));
        let rest_min = force_min_age.unwrap_or_else(|| lane_min_age(LaneClass::Rest, engaged));
        if self.detail.wants_refresh(gql_min) {
            self.fetch_detail(cx);
        }
        if self.files.wants_refresh(rest_min) {
            self.fetch_files(cx);
        }
        if self.threads.wants_refresh(gql_min) {
            self.fetch_threads(cx);
        }
        // The #192 label's clock: even a fully-skipped beat re-renders the
        // header so the relative age advances (docs/0033 D4).
        cx.notify();
    }

    fn apply_threads_result(&mut self, result: crate::service::SpecResult, cx: &mut Context<Self>) {
        if let Some(m) = &result.network {
            self.threads.fail(format!("Network error: {m}"));
        } else if result.status != 200 {
            // Shared 401/403 handling, same as the detail fetch (0030 D1).
            let status = result.status;
            self.state.update(cx, |s, cx| {
                s.apply(result);
                cx.notify();
            });
            self.threads.fail(format!("GitHub answered HTTP {status}"));
        } else {
            let parsed = result.body.as_deref().and_then(|body| {
                serde_json::from_str::<gql::Envelope<gql::PrThreadsData>>(body)
                    .ok()
                    .and_then(|env| {
                        for e in &env.errors {
                            log::warn!("pr-threads: {}", e.message);
                        }
                        env.data
                            .and_then(|d| d.repository)
                            .as_ref()
                            .and_then(gql::pr_threads_from_value)
                    })
            });
            match parsed {
                Some(t) => {
                    log::info!(
                        "pr-threads applied: {}#{} — {} threads",
                        self.repo,
                        self.number,
                        t.nodes.len()
                    );
                    self.threads.land(t);
                    self.extend_threads_md();
                }
                None => {
                    // A null repository/pullRequest node on an open PR is
                    // bizarre — degrade to an empty set so the diff pane
                    // keeps working (docs/0030 D1).
                    log::warn!("pr-threads: repository/pullRequest null — empty set");
                    self.threads.land(gql::PrThreads::default());
                }
            }
        }
        cx.notify();
    }

    /// Parse every thread comment body once per threads payload (the
    /// 0017 D6 rule) and fold the blocks into the page cache under
    /// `thread-{n}-comment-{m}` keys.
    fn extend_threads_md(&mut self) {
        let Some(t) = self.threads.data() else {
            return;
        };
        // Ambient repo for bare `#123` refs (docs/0017 D1 extension).
        for (ix, thread) in t.nodes.iter().enumerate() {
            for (m, comment) in thread.comments.iter().enumerate() {
                self.md.insert(
                    format!("thread-{ix}-comment-{m}"),
                    markdown::parse_markdown_in_repo(&comment.body, Some(&self.repo)),
                );
            }
        }
    }

    /// The shared filter (docs/0018 D6): the List through its delegate
    /// search; the Tree through a forest rebuild (ancestors of matches
    /// survive, everything rebuilt expanded). The List selection survives
    /// only if still visible; upstream `set_items` drops a Tree selection
    /// whose id vanished on its own.
    fn apply_file_filter(&mut self, q: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.file_list
            .update(cx, |st, cx| st.set_query(q, window, cx));
        let list = self.file_list.clone();
        let matched = list.read(cx).delegate().matched().to_vec();
        if let Some(sel) = &self.selected_file {
            if !matched.iter().any(|f| &f.path == sel) {
                self.selected_file = None;
            }
        }
        let selected = self.selected_file.clone();
        self.file_list.update(cx, |st, cx| {
            st.delegate_mut().set_selected(selected);
            cx.notify();
        });
        let items = build_file_forest(&matched);
        self.file_tree.update(cx, |st, cx| st.set_items(items, cx));
        cx.notify();
    }

    /// Select a file from the List view (docs/0018 D7): page selection
    /// plus the delegate's row-highlight mirror.
    fn select_file(&mut self, path: Option<SharedString>, cx: &mut Context<Self>) {
        self.selected_file = path.clone();
        self.file_list.update(cx, |st, cx| {
            st.delegate_mut().set_selected(path);
            cx.notify();
        });
        cx.notify();
    }
    // --- write path (docs/0016 §6 stage 2) ---------------------------------

    /// The selected comment-box action label, cloned out of the Select.
    fn current_action(&self, cx: &Context<Self>) -> SharedString {
        self.action_select
            .read(cx)
            .selected_value()
            .cloned()
            .unwrap_or_else(|| "Comment".into())
    }

    /// Comment/Approve/Request-changes submit (D4): `addComment` for a
    /// plain comment, `addPullRequestReview` for the two review events
    /// (body optional). Gating is the same pure fn the button renders
    /// with — re-checked here as the honest guard.
    fn submit_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.data().cloned() else {
            return;
        };
        if self.busy.is_some() || d.id.is_empty() {
            return;
        }
        let action = self.current_action(cx);
        let body = self.comment.read(cx).value().trim().to_string();
        if !comment_action_allowed(action.as_ref(), body.is_empty()) {
            return;
        }
        let op = if action.as_ref() == "Comment" {
            BusyOp::Comment
        } else {
            BusyOp::Review
        };
        let (mutation, variables) = if action.as_ref() == "Comment" {
            (
                Mutation::Comment,
                json!({ "input": { "subjectId": d.id, "body": body } }),
            )
        } else {
            let event = review_event(action.as_ref())
                .expect("comment_action_allowed guarantees a review event");
            let mut input = json!({ "input": { "pullRequestId": d.id, "event": event } });
            if !body.is_empty() {
                input["input"]["body"] = json!(body);
            }
            (Mutation::Review, input)
        };
        self.mutation_error = None;
        self.busy = Some(op);
        cx.notify();
        self.spawn_mutation(op, mutation, variables, window, cx);
    }

    /// Open the description editor (D4), prefilled with the current body.
    fn start_edit_description(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.description_edit.is_some() {
            return;
        }
        // One editor at a time (0019 D4; docs/0043 D7): a row editor,
        // delete confirm or rail picker yields to the description editor.
        self.timeline_edit = None;
        self.delete_confirm = None;
        self.meta_edit = None;
        let body = self
            .detail
            .data()
            .and_then(|d| d.body.clone())
            .unwrap_or_default();
        let editor = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx)
                // Full content height: plain multi-line ignores `rows` for
                // layout (upstream honors it only in auto-grow mode), so
                // grow with the body — 8 rows minimum, capped at 40 (the
                // editor scrolls internally past that, like GitHub's).
                .auto_grow(8, 40)
                .placeholder("Describe this pull request");
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

    // --- rail pickers (docs/0043, 0022 D5 lifecycle) ------------------------

    /// The viewer's login (`GithubState.user`, global snapshot) — the
    /// reviewer picker's author branch of the gate reads it (docs/0043
    /// D4).
    fn viewer_login(&self, cx: &Context<Self>) -> Option<String> {
        self.state.read(cx).user.as_ref().map(|u| u.login.clone())
    }

    fn fetch_source(&mut self, source: PickerSource, cx: &mut Context<Self>) {
        if !self.state.read(cx).auth.connected() {
            return; // the page is about to be torn down by the auth gate
        }
        let state = self.state.clone();
        let (owner, name) = split_repo(&self.repo);
        let (kind, query_name) = source.fetch_kind();
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
            PickerSource::Assignees | PickerSource::Reviewers => {
                self.users_source = PickerFetch::Loading
            }
            PickerSource::Milestones => self.milestones_source = PickerFetch::Loading,
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = run_spec_tracked(state, spec, cx).await;
            let _ = this.update(cx, |page, cx| page.apply_source_result(source, result, cx));
        })
        .detach();
    }

    fn set_source_failed(&mut self, source: PickerSource, message: String) {
        match source {
            PickerSource::Labels => self.labels_source = PickerFetch::Failed(message),
            PickerSource::Assignees | PickerSource::Reviewers => {
                self.users_source = PickerFetch::Failed(message)
            }
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
                        PickerSource::Assignees | PickerSource::Reviewers => {
                            self.users_source =
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
    /// open picker): rebuild the frozen draft, preserving choices by id
    /// (0022 §11 open-immediately).
    fn realign_open_picker(&mut self, source: PickerSource, cx: &mut Context<Self>) {
        let Some(d) = self.detail.data().cloned() else {
            return;
        };
        let rebuilding = match (&self.meta_edit, source) {
            (Some(PrMetaEditor::Labels(_)), PickerSource::Labels) => 0,
            (Some(PrMetaEditor::Assignees(_)), PickerSource::Assignees) => 1,
            (Some(PrMetaEditor::Reviewers(_)), PickerSource::Reviewers) => 2,
            (Some(PrMetaEditor::Milestone { .. }), PickerSource::Milestones) => 3,
            _ => return,
        };
        match rebuilding {
            0 => {
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
                self.meta_edit = Some(PrMetaEditor::Labels(PickerDraft { rows, checked }));
            }
            1 => {
                let PickerFetch::Ready(src) = &self.users_source else {
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
                self.meta_edit = Some(PrMetaEditor::Assignees(PickerDraft { rows, checked }));
            }
            2 => {
                let PickerFetch::Ready(src) = &self.users_source else {
                    return;
                };
                let author = d.author.as_ref().map(|a| a.login.as_str());
                let rows = reviewer_rows(&d.review_requests, src, author);
                let checked = rows
                    .iter()
                    .map(|r| {
                        d.review_requests
                            .iter()
                            .any(|req| req.id.as_deref() == Some(r.id.as_str()))
                    })
                    .collect();
                self.meta_edit = Some(PrMetaEditor::Reviewers(PickerDraft { rows, checked }));
            }
            _ => {
                let PrMetaEditor::Milestone { chosen, .. } = self.meta_edit.as_ref().unwrap()
                else {
                    unreachable!("matched above")
                };
                let chosen = chosen.clone();
                let PickerFetch::Ready(src) = &self.milestones_source else {
                    return;
                };
                self.meta_edit = Some(PrMetaEditor::Milestone {
                    rows: milestone_rows(d.milestone.as_ref(), src),
                    chosen,
                });
            }
        }
        cx.notify();
    }

    fn start_reviewers_picker(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.detail.data().cloned() else {
            return;
        };
        let viewer = self.viewer_login(cx);
        if self.busy.is_some()
            || self.meta_edit.is_some()
            || !pr_reviewers_arming(&d, viewer.as_deref())
        {
            return;
        }
        if matches!(
            self.users_source,
            PickerFetch::Idle | PickerFetch::Failed(_)
        ) {
            self.fetch_source(PickerSource::Reviewers, cx);
        }
        // Open immediately (0022 §11); realign rebuilds by id when the
        // source lands.
        let source = match &self.users_source {
            PickerFetch::Ready(src) => src.clone(),
            _ => Vec::new(),
        };
        let author = d.author.as_ref().map(|a| a.login.as_str());
        let rows = reviewer_rows(&d.review_requests, &source, author);
        let checked = rows
            .iter()
            .map(|r| {
                d.review_requests
                    .iter()
                    .any(|req| req.id.as_deref() == Some(r.id.as_str()))
            })
            .collect();
        self.meta_edit = Some(PrMetaEditor::Reviewers(PickerDraft { rows, checked }));
        cx.notify();
    }

    fn start_labels_picker(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.detail.data().cloned() else {
            return;
        };
        if self.busy.is_some() || self.meta_edit.is_some() || !pr_labels_arming(&d) {
            return;
        }
        if matches!(
            self.labels_source,
            PickerFetch::Idle | PickerFetch::Failed(_)
        ) {
            self.fetch_source(PickerSource::Labels, cx);
        }
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
        self.meta_edit = Some(PrMetaEditor::Labels(PickerDraft { rows, checked }));
        cx.notify();
    }

    fn start_assignees_picker(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.detail.data().cloned() else {
            return;
        };
        if self.busy.is_some() || self.meta_edit.is_some() || !pr_assignees_arming(&d) {
            return;
        }
        if matches!(
            self.users_source,
            PickerFetch::Idle | PickerFetch::Failed(_)
        ) {
            self.fetch_source(PickerSource::Assignees, cx);
        }
        let source = match &self.users_source {
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
        self.meta_edit = Some(PrMetaEditor::Assignees(PickerDraft { rows, checked }));
        cx.notify();
    }

    fn start_milestone_picker(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.detail.data().cloned() else {
            return;
        };
        if self.busy.is_some() || self.meta_edit.is_some() || !pr_milestone_arming(&d) {
            return;
        }
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
        self.meta_edit = Some(PrMetaEditor::Milestone {
            rows: milestone_rows(d.milestone.as_ref(), &source),
            chosen,
        });
        cx.notify();
    }

    fn toggle_meta_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        if let Some(draft) = self.meta_edit.as_mut() {
            let slots = match draft {
                PrMetaEditor::Reviewers(draft) | PrMetaEditor::Labels(draft) => &mut draft.checked,
                PrMetaEditor::Assignees(draft) => &mut draft.checked,
                PrMetaEditor::Milestone { .. } => return,
            };
            if let Some(checked) = slots.get_mut(ix) {
                *checked = !*checked;
                cx.notify();
            }
        }
    }

    fn pick_milestone_ix(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let Some(PrMetaEditor::Milestone { rows, chosen }) = self.meta_edit.as_mut() else {
            return;
        };
        if let Some(row) = rows.get(ix) {
            // The empty id is the "No milestone" row — clearing.
            *chosen = if row.id.is_empty() {
                None
            } else {
                Some(row.id.clone())
            };
            cx.notify();
        }
    }

    fn cancel_meta_editor(&mut self, cx: &mut Context<Self>) {
        self.meta_edit = None;
        self.mutation_error = None;
        cx.notify();
    }

    /// The pure payload for a metadata save — the payload tests assert
    /// the one-field-group rule here (docs/0043 §7): labels/assignees/
    /// milestone ride `updatePullRequest` with exactly one field-group,
    /// reviewers ride `requestReviews` with `union: true` and the full
    /// replacement set (empty list clears).
    pub(crate) fn pr_meta_mutation(
        op: BusyOp,
        node_id: &str,
        ids: &[String],
        current_milestone: Option<&str>,
    ) -> (Mutation, serde_json::Value) {
        let input = match op {
            BusyOp::PrSetLabels => json!({ "pullRequestId": node_id, "labelIds": ids }),
            BusyOp::PrSetAssignees => json!({ "pullRequestId": node_id, "assigneeIds": ids }),
            // `milestoneId: null` clears; `union: true` replaces the
            // request set (docs/0043 §2.2).
            BusyOp::PrSetMilestone => {
                json!({ "pullRequestId": node_id, "milestoneId": current_milestone })
            }
            BusyOp::PrSetReviewers => json!({
                "pullRequestId": node_id,
                "userIds": ids,
                "union": true
            }),
            _ => unreachable!("meta builder only"),
        };
        let mutation = match op {
            BusyOp::PrSetLabels => Mutation::PrSetLabels,
            BusyOp::PrSetAssignees => Mutation::PrSetAssignees,
            BusyOp::PrSetMilestone => Mutation::PrSetMilestone,
            BusyOp::PrSetReviewers => Mutation::PrSetReviewers,
            _ => unreachable!("meta builder only"),
        };
        (mutation, json!({ "input": input }))
    }

    /// Save the edited description (`updateIssue`, D4) — the PR node id is
    /// the issue id; empty bodies never get here (Save renders disabled).
    fn save_description(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(d), Some(editor)) = (self.detail.data(), self.description_edit.as_ref()) else {
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
        self.busy = Some(BusyOp::Description);
        cx.notify();
        self.spawn_mutation(
            BusyOp::Description,
            Mutation::UpdateDescription,
            variables,
            window,
            cx,
        );
    }

    // --- timeline-row edit/delete (docs/0019 D4/D5) ---------------------

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
        // #212: thread surfaces yield to this editor too.
        self.thread_edit = None;
        self.thread_delete_confirm = None;
        let editor = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx)
                // Grows with the body; scrolls internally past 30 rows.
                .auto_grow(4, 30);
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

    /// Save the row editor (0019 D5): the node's kind picks the mutation
    /// and its input shape. Empty bodies never get here (Save renders
    /// disabled and the guard re-checks).
    fn save_edit_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.data() else {
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
        let Some(kind) = node_kind(&d.comments.nodes, &d.reviews.nodes, node_id) else {
            // The node vanished under the editor (refetch race) — close.
            self.timeline_edit = None;
            cx.notify();
            return;
        };
        let (op, mutation, variables) = edit_row(kind, node_id, &body);
        self.mutation_error = None;
        self.busy = Some(op);
        cx.notify();
        self.spawn_mutation(op, mutation, variables, window, cx);
    }

    /// Arm the inline delete confirm on a row (0019 D4). Editors yield.
    fn start_delete_confirm(&mut self, node_id: String, cx: &mut Context<Self>) {
        if self.busy.is_some() || node_id.is_empty() {
            return;
        }
        self.description_edit = None;
        self.timeline_edit = None;
        self.delete_confirm = Some(node_id);
        // #212: thread surfaces yield to this confirm too.
        self.thread_edit = None;
        self.thread_delete_confirm = None;
        cx.notify();
    }

    fn cancel_delete_confirm(&mut self, cx: &mut Context<Self>) {
        self.delete_confirm = None;
        cx.notify();
    }

    /// Fire the confirmed delete (0019 D5). The confirm card stays up
    /// while the op is in flight — it shows which row is being deleted —
    /// and the success arm clears it.
    fn confirm_delete_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.data() else {
            return;
        };
        let Some(node_id) = self.delete_confirm.clone() else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        let Some(kind) = node_kind(&d.comments.nodes, &d.reviews.nodes, &node_id) else {
            self.delete_confirm = None;
            cx.notify();
            return;
        };
        let (op, mutation, variables) = delete_row(kind, &node_id);
        self.mutation_error = None;
        self.busy = Some(op);
        cx.notify();
        self.spawn_mutation(op, mutation, variables, window, cx);
    }
    /// Merge the PR (D5) with the selected (or only) method.
    fn merge_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.data().cloned() else {
            return;
        };
        if !merge_enabled(
            d.state.as_deref(),
            d.draft,
            d.mergeable.as_deref(),
            self.busy.is_some(),
            self.methods.is_empty(),
        ) {
            return;
        }
        let method = self.selected_method(cx).unwrap_or(MergeMethod::Merge);
        let variables =
            json!({ "input": { "pullRequestId": d.id, "mergeMethod": method.as_str() } });
        self.mutation_error = None;
        self.busy = Some(BusyOp::Merge);
        cx.notify();
        self.spawn_mutation(BusyOp::Merge, Mutation::Merge, variables, window, cx);
    }

    /// Close the PR (D5).
    fn close_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.data().cloned() else {
            return;
        };
        if self.busy.is_some() || d.id.is_empty() || d.state.as_deref() != Some("OPEN") {
            return;
        }
        let variables = json!({ "input": { "pullRequestId": d.id } });
        self.mutation_error = None;
        self.busy = Some(BusyOp::Close);
        cx.notify();
        self.spawn_mutation(BusyOp::Close, Mutation::Close, variables, window, cx);
    }

    // --- Files-tab review writes (docs/0030 D4/D5/D7) -------------------

    /// Arm the row composer on a diff line (D4). One composer at a time:
    /// opening another row's discards the previous unsubmitted text.
    pub(crate) fn open_line_composer(
        &mut self,
        path: String,
        side: gql::DiffSide,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() {
            return;
        }
        // #211: with the composer already open on this file+side, a
        // click on another row extends the range to it — the range is
        // always between the last two clicked rows; clicking the end
        // row again collapses back to a single line. Same side only in
        // v1; a different side or file starts a fresh composer.
        if let Some(c) = self.line_composer.as_mut() {
            if c.path == path && c.side == side {
                if let Some((s, e)) = normalize_range((side, c.line), (side, line)) {
                    c.line = e.1;
                    c.start = if s.1 == e.1 { None } else { Some(s) };
                    cx.notify();
                    return;
                }
            }
        }
        let editor = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 10)
                .placeholder("Leave a review comment")
        });
        self.reply_composer = None;
        self.reply_sub = None;
        // Typing grows the composer — keep the list heights honest
        // (0041 D3).
        self.composer_sub = Some(cx.observe(&editor, |this, _, cx| {
            this.splice_diff_composer();
            cx.notify();
        }));
        self.line_composer = Some(LineComposer {
            path,
            side,
            line,
            start: None,
            editor,
        });
        cx.notify();
    }

    pub(crate) fn cancel_line_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.line_composer = None;
        self.composer_sub = None;
        self.refocus_page(window, cx);
        cx.notify();
    }

    /// Add single comment (D4): `addPullRequestReviewThread` publishes
    /// immediately as its own COMMENT review. The composer stays open
    /// with its text until success closes it (draft retained on failure,
    /// the 0019 pattern).
    pub(crate) fn add_single_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.data().cloned() else {
            return;
        };
        let Some(composer) = self.line_composer.as_ref() else {
            return;
        };
        if self.busy.is_some() || d.id.is_empty() {
            return;
        }
        let body = composer.editor.read(cx).value().trim().to_string();
        if !composer_body_allowed(&body) {
            return;
        }
        let (path, side, line) = (composer.path.clone(), composer.side, composer.line);
        let start = composer.start;
        let mut input = json!({
            "pullRequestId": d.id,
            "path": path,
            "line": line,
            "side": side.as_str(),
            "body": body,
        });
        // #211: `startLine`/`startSide` ride along for ranges; omitted
        // (byte-identical variables) for single-line comments.
        for (k, v) in start_fields(start) {
            input[k] = v;
        }
        let variables = json!({ "input": input });
        self.mutation_error = None;
        self.busy = Some(BusyOp::LineComment);
        cx.notify();
        self.spawn_mutation(
            BusyOp::LineComment,
            Mutation::ReviewThread,
            variables,
            window,
            cx,
        );
    }

    /// Queue the composer's text as a local draft (D4) — published later
    /// by the Finish-review bar as one batch. The draft list is
    /// session-only page state (D4).
    pub(crate) fn start_review_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(composer) = self.line_composer.as_ref() else {
            return;
        };
        let body = composer.editor.read(cx).value().trim().to_string();
        if !composer_body_allowed(&body) {
            return;
        }
        let draft = LineDraft {
            path: composer.path.clone(),
            side: composer.side,
            line: composer.line,
            start: composer.start,
            body,
        };
        if self.finish_summary.is_none() {
            self.finish_summary = Some(cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(2)
                    .placeholder("Summarize your review (optional)")
            }));
        }
        self.line_composer = None;
        self.composer_sub = None;
        self.drafts.push(draft);
        self.refocus_page(window, cx);
        cx.notify();
    }

    /// Open the reply box on a thread (D4) — one at a time; the caller
    /// gates on the thread's `viewerCanReply`.
    pub(crate) fn open_reply(
        &mut self,
        thread_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() || thread_id.is_empty() {
            return;
        }
        self.line_composer = None;
        self.composer_sub = None;
        let editor = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .placeholder("Reply…")
        });
        // Typing grows the box — keep the list heights honest (0041 D3).
        self.composer_sub = None;
        self.reply_sub = Some(cx.observe(&editor, {
            let thread_id = thread_id.clone();
            move |this, _, cx| {
                this.splice_diff_thread(&thread_id);
                cx.notify();
            }
        }));
        self.reply_composer = Some((thread_id.clone(), editor));
        self.splice_diff_thread(&thread_id);
        cx.notify();
    }

    pub(crate) fn cancel_reply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((thread_id, _)) = self.reply_composer.take() {
            self.reply_sub = None;
            self.splice_diff_thread(&thread_id);
        }
        self.refocus_page(window, cx);
        cx.notify();
    }

    /// Send the reply (D4): `addPullRequestReviewComment { body,
    /// inReplyTo }` — immediate; replies cannot ride a draft batch (§3).
    /// The box stays open with its text until success closes it.
    pub(crate) fn send_reply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((thread_id, editor)) = self.reply_composer.as_ref() else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        let body = editor.read(cx).value().trim().to_string();
        if !composer_body_allowed(&body) {
            return;
        }
        // Reply to the thread's latest comment (the GitHub-web
        // convention); the API shows the reply in the whole thread
        // either way. The thread vanishing under the box (refetch race)
        // closes it.
        let Some(in_reply_to) = self
            .threads_nodes()
            .iter()
            .find(|t| t.id == *thread_id)
            .and_then(|t| t.comments.last())
            .map(|c| c.id.clone())
        else {
            self.reply_composer = None;
            cx.notify();
            return;
        };
        if in_reply_to.is_empty() {
            return; // degraded node — nothing to anchor the reply to
        }
        let variables = json!({ "input": { "body": body, "inReplyTo": in_reply_to } });
        self.mutation_error = None;
        self.busy = Some(BusyOp::ReviewReply);
        cx.notify();
        self.spawn_mutation(
            BusyOp::ReviewReply,
            Mutation::ReviewThreadReply,
            variables,
            window,
            cx,
        );
    }

    /// Open the resolve-reason picker (#213) — the Resolve button no
    /// longer fires immediately; the picker row offers the enum reasons
    /// plus a plain resolve (field omitted).
    pub(crate) fn open_resolve_picker(
        &mut self,
        thread_id: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() || thread_id.is_empty() {
            return;
        }
        self.resolve_picker = Some(thread_id.clone());
        self.splice_diff_thread(&thread_id);
        cx.notify();
    }

    pub(crate) fn cancel_resolve_picker(&mut self, cx: &mut Context<Self>) {
        let thread_id = self.resolve_picker.clone();
        self.resolve_picker = None;
        if let Some(id) = thread_id {
            self.splice_diff_thread(&id);
        }
        self.resolve_picker = None;
        cx.notify();
    }

    // --- review-thread comment edit/delete (#212) ---------------------
    // The 0019 D4/D5 row pattern applied to thread comments: one editor
    // anywhere on the page, inline two-step delete confirm, Save gated
    // on a non-blank body, failures land in the Files scope with the
    // surface retained.

    pub(crate) fn start_thread_edit(
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
        self.timeline_edit = None;
        self.delete_confirm = None;
        self.thread_delete_confirm = None;
        self.resolve_picker = None;
        let editor = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx).auto_grow(4, 30);
            state.set_value(body, window, cx);
            state
        });
        self.thread_edit = Some((node_id, editor));
        cx.notify();
    }

    pub(crate) fn cancel_thread_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.thread_edit = None;
        self.refocus_page(window, cx);
        cx.notify();
    }

    /// Save the thread-comment editor (#212). The id field is
    /// `pullRequestReviewCommentId` (verified live 2026-09-16), unlike
    /// the timeline edits' `id` / `pullRequestReviewId`.
    pub(crate) fn save_thread_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((node_id, editor)) = self.thread_edit.as_ref() else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        let body = editor.read(cx).value().trim().to_string();
        if body.is_empty() {
            return;
        }
        let op = BusyOp::ReviewCommentEdit;
        let mutation = Mutation::UpdateReviewComment;
        let variables = json!({
            "input": { "pullRequestReviewCommentId": node_id, "body": body }
        });
        self.mutation_error = None;
        self.busy = Some(op);
        cx.notify();
        self.spawn_mutation(op, mutation, variables, window, cx);
    }

    pub(crate) fn start_thread_delete_confirm(&mut self, node_id: String, cx: &mut Context<Self>) {
        if self.busy.is_some() || node_id.is_empty() {
            return;
        }
        self.description_edit = None;
        self.timeline_edit = None;
        self.delete_confirm = None;
        self.thread_edit = None;
        self.resolve_picker = None;
        self.thread_delete_confirm = Some(node_id);
        cx.notify();
    }

    pub(crate) fn cancel_thread_delete_confirm(&mut self, cx: &mut Context<Self>) {
        self.thread_delete_confirm = None;
        cx.notify();
    }

    /// Fire the confirmed thread-comment delete (#212,
    /// `DeletePullRequestReviewCommentInput { id }`). The confirm stays
    /// up while the op is in flight; the success arm clears it.
    pub(crate) fn confirm_thread_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node_id) = self.thread_delete_confirm.clone() else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        let op = BusyOp::ReviewCommentDelete;
        let mutation = Mutation::DeleteReviewComment;
        let variables = json!({ "input": { "id": node_id } });
        self.mutation_error = None;
        self.busy = Some(op);
        cx.notify();
        self.spawn_mutation(op, mutation, variables, window, cx);
    }
    /// Resolve / unresolve (D5) — schema-gated buttons upstream. Since
    /// #213 the resolve path carries an optional `resolutionReason`
    /// (`PullRequestReviewThreadResolutionReason`, verified live
    /// 2026-09-16); the field is omitted for a plain resolve.
    pub(crate) fn set_thread_resolved(
        &mut self,
        thread_id: String,
        resolved: bool,
        reason: Option<ResolutionReason>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() || thread_id.is_empty() {
            return;
        }
        let (op, mutation) = if resolved {
            (BusyOp::ThreadResolve, Mutation::ThreadResolve)
        } else {
            (BusyOp::ThreadUnresolve, Mutation::ThreadUnresolve)
        };
        let mut input = serde_json::Map::new();
        input.insert("threadId".into(), json!(thread_id));
        if let Some(reason) = reason {
            input.insert("resolutionReason".into(), json!(reason.as_str()));
        }
        let variables = json!({ "input": input });
        self.mutation_error = None;
        self.resolve_picker = None;
        self.busy = Some(op);
        cx.notify();
        self.spawn_mutation(op, mutation, variables, window, cx);
    }

    pub(crate) fn set_finish_event(&mut self, event: FinishEvent, cx: &mut Context<Self>) {
        self.finish_event = event;
        cx.notify();
    }

    /// Submit the whole draft batch (D4): ONE `addPullRequestReview`
    /// carrying the `threads` array and the chosen event — no
    /// server-PENDING review ever exists. The summary body is omitted
    /// when empty.
    pub(crate) fn submit_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.data().cloned() else {
            return;
        };
        if !review_batch_allowed(self.drafts.len(), self.busy.is_some()) || d.id.is_empty() {
            return;
        }
        let threads: Vec<serde_json::Value> = self
            .drafts
            .iter()
            .map(|draft| {
                let mut thread = json!({
                    "body": draft.body,
                    "path": draft.path,
                    "line": draft.line,
                    "side": draft.side.as_str(),
                });
                for (k, v) in start_fields(draft.start) {
                    thread[k] = v;
                }
                thread
            })
            .collect();
        let mut input = json!({
            "pullRequestId": d.id,
            "event": self.finish_event.as_str(),
            "threads": threads,
        });
        if let Some(summary) = self.finish_summary.as_ref() {
            let body = summary.read(cx).value().trim().to_string();
            if !body.is_empty() {
                input["body"] = json!(body);
            }
        }
        let variables = json!({ "input": input });
        self.mutation_error = None;
        self.busy = Some(BusyOp::ReviewBatch);
        cx.notify();
        self.spawn_mutation(
            BusyOp::ReviewBatch,
            Mutation::ReviewBatch,
            variables,
            window,
            cx,
        );
    }

    /// Discard every draft (D4) — the bar disappears with them.
    pub(crate) fn discard_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.drafts.is_empty() {
            return;
        }
        self.drafts.clear();
        self.finish_summary = None;
        self.finish_event = FinishEvent::default();
        self.refocus_page(window, cx);
        cx.notify();
    }

    /// The description editor holds keyboard focus while open; dropping
    /// its entity (cancel, or save success) would leave the page with a
    /// dead focus handle — no key context, so `Esc` would stop working
    /// (found via the bridge during stage-2 verification). Hand focus
    /// back to the page root.
    /// Esc cancels the topmost surface (docs/0043 D7, 0022 §11
    /// ordering): delete confirms first, then row editors, the
    /// description editor, the rail pickers — and only closes the page
    /// when nothing is open.
    fn cancel_top_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.delete_confirm.take().is_some() || self.thread_delete_confirm.take().is_some() {
            cx.notify();
            return true;
        }
        if self.timeline_edit.take().is_some() || self.thread_edit.take().is_some() {
            self.refocus_page(window, cx);
            cx.notify();
            return true;
        }
        if self.line_composer.take().is_some() || self.reply_composer.take().is_some() {
            self.refocus_page(window, cx);
            cx.notify();
            return true;
        }
        if self.description_edit.take().is_some() {
            self.mutation_error = None;
            self.refocus_page(window, cx);
            cx.notify();
            return true;
        }
        if self.meta_edit.take().is_some() {
            self.mutation_error = None;
            cx.notify();
            return true;
        }
        false
    }

    fn refocus_page(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
    }

    /// The method the Select currently names — `None` when the Select has
    /// nothing selected or names a label this build doesn't know.
    fn selected_method(&self, cx: &Context<Self>) -> Option<MergeMethod> {
        let selected = self.method_select.as_ref()?.read(cx).selected_value()?;
        self.methods
            .iter()
            .copied()
            .find(|m| m.label() == selected.as_ref())
    }

    fn spawn_mutation(
        &mut self,
        op: BusyOp,
        mutation: Mutation,
        variables: serde_json::Value,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.clone();
        let ident = format!("{}#{} {:?}", self.repo, self.number, op);
        // `spawn_in` keeps the window reachable across the await so the
        // result handler can clear the draft (set_value needs a Window).
        cx.spawn_in(window, async move |this, cx| {
            let result = run_mutation(state, mutation, variables, ident, cx).await;
            let _ = cx.update(|window, app| {
                this.update(app, |page, cx| {
                    page.apply_mutation_result(op, result, window, cx)
                })
            });
        })
        .detach();
    }

    /// A mutation came back (D4/D5): success clears the draft/edits, any
    /// failure lands as a scoped inline error — then the detail refetches
    /// in every case (docs/0016 §6.2), so the page never disagrees with
    /// GitHub about what just happened. A 200 with GraphQL `errors`
    /// (server-side rejections — approving your own PR, merging a draft)
    /// is a failure here.
    fn apply_mutation_result(
        &mut self,
        op: BusyOp,
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
                    "pr-mutation {}#{} {:?}: {}",
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
                BusyOp::Comment | BusyOp::Review => {
                    // Clear the draft — `set_value` needs the window the
                    // spawn kept alive for exactly this.
                    self.comment
                        .update(cx, |state, cx| state.set_value("", window, cx));
                }
                BusyOp::Description => {
                    self.description_edit = None;
                    self.refocus_page(window, cx);
                }
                // 0019 D4: success closes the row surface it came from —
                // the refetch below repaints the row from fresh data.
                BusyOp::CommentEdit | BusyOp::ReviewEdit => {
                    self.timeline_edit = None;
                    self.refocus_page(window, cx);
                }
                BusyOp::CommentDelete | BusyOp::ReviewDelete => {
                    self.delete_confirm = None;
                }
                BusyOp::Merge | BusyOp::Close => {}
                // 0030 D7: Files-tab successes close the surface they
                // came from; the threads refetch below repaints the pane.
                BusyOp::LineComment => {
                    self.line_composer = None;
                    self.refocus_page(window, cx);
                }
                BusyOp::ReviewReply => {
                    self.reply_composer = None;
                    self.refocus_page(window, cx);
                }
                BusyOp::ReviewBatch => {
                    self.drafts.clear();
                    self.finish_summary = None;
                    self.finish_event = FinishEvent::default();
                }
                BusyOp::ThreadResolve | BusyOp::ThreadUnresolve => {}
                // #212: success closes the surface it came from; the
                // threads refetch below repaints the pane (a deleted
                // thread root takes its whole card with it).
                BusyOp::ReviewCommentEdit => {
                    self.thread_edit = None;
                    self.refocus_page(window, cx);
                }
                BusyOp::ReviewCommentDelete => {
                    self.thread_delete_confirm = None;
                }
                // #196: no surface to close — the refetch below repaints
                // the chips row from the fresh groups.
                BusyOp::Reaction => {}
                // docs/0043 D2/D5: success closes the picker; the detail
                // refetch below repaints the rail from fresh data.
                BusyOp::PrSetLabels
                | BusyOp::PrSetAssignees
                | BusyOp::PrSetMilestone
                | BusyOp::PrSetReviewers => {
                    self.meta_edit = None;
                }
            }
        }
        if op.error_scope() == ErrorScope::Files {
            // 0030 D7: the threads refetch re-renders the diff pane;
            // patches are not refetched — only merge/close change them,
            // and those end the page's useful life anyway.
            self.fetch_threads(cx);
        } else {
            self.fetch_detail(cx);
        }
        cx.notify();
    }
    // --- reactions (docs/0023 D3 ported to the PR surfaces, #196) ----------

    /// Chip click (docs/0023 D3): armed removes, otherwise adds.
    /// Single-flight — busy is checked by render and re-checked here as
    /// the honest guard. Same mutations the issue page posts; the
    /// subject id is the PR, review, or comment node id.
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
        self.busy = Some(BusyOp::Reaction);
        cx.notify();
        let mutation = if remove {
            Mutation::RemoveReaction
        } else {
            Mutation::AddReaction
        };
        self.spawn_mutation(BusyOp::Reaction, mutation, variables, window, cx);
    }

    /// docs/0023 D3 — one chips row: filtered groups (`reaction_chips`) +
    /// the gated `+` affordance. `prefix` is the probe-key stem
    /// (`pr-reaction-row` for the body,
    /// `pr-comment-{ix}-reaction-row` / `pr-review-{ix}-reaction-row`
    /// for timeline rows); chips extend it with the content's wire name.
    /// Returns `None` when there is nothing to show — no chips and the
    /// viewer cannot react.
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
                            let probe = format!("pr-reaction-pick-{}", content.wire());
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

    fn request_close(&mut self, cx: &mut Context<Self>) {
        cx.emit(PrPageEvent::CloseRequested);
    }

    // --- regions -----------------------------------------------------------

    fn render_header(&self, detail: Option<&gql::PrDetail>, cx: &Context<Self>) -> AnyElement {
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
            .child(freshness_row("pr", &self.detail, cx, |this, _, cx| {
                this.fetch_detail(cx);
                // A refresh with files/threads already loaded refetches
                // those lists too (0030 D8) — collapse states reset with
                // the new payload.
                if this.files.is_ready() {
                    this.fetch_files(cx);
                }
                if this.threads.is_ready() {
                    this.fetch_threads(cx);
                }
            }))
            .child(
                Button::new("pr-open-external")
                    .ghost()
                    .small()
                    .icon(IconName::ExternalLink)
                    .tooltip("Open on GitHub")
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            )
            .into_any_element()
    }

    fn render_tabs(&self, detail: Option<&gql::PrDetail>, cx: &Context<Self>) -> AnyElement {
        // Counts render only with a detail payload — while loading the tabs
        // stay bare so nothing flashes a wrong "0" (docs/0016 D3).
        let labels = match detail {
            Some(d) => vec![
                tab_label(
                    "Conversation",
                    Some(d.comments.total_count + d.reviews.total_count),
                ),
                tab_label("Commits", Some(d.commits.total_count)),
                // The contexts connection's server-side count (D7) —
                // 0 for a PR without checks, same as GitHub shows.
                tab_label("Checks", Some(d.checks.total_count)),
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
                    // Lazy fetches (0016 D8 / 0030 D1): the files REST walk and
                    // the threads one-shot both start the first time the Files
                    // tab opens, not on page open.
                    if tab == PrTab::Files {
                        // Lazy fetches (0016 D8 / 0030 D1): the files REST
                        // walk and the threads one-shot both start the first
                        // time the Files tab opens, not on page open.
                        if this.files.is_idle() {
                            this.fetch_files(cx);
                        }
                        if this.threads.is_idle() {
                            this.fetch_threads(cx);
                        }
                    }
                    cx.notify();
                }
            }))
            .child(Tab::new().label(labels[0].clone()))
            .child(Tab::new().label(labels[1].clone()))
            .child(Tab::new().label(labels[2].clone()))
            .child(Tab::new().label(labels[3].clone()))
            .into_any_element()
    }

    fn render_tab_content(&self, detail: Option<&gql::PrDetail>, cx: &Context<Self>) -> AnyElement {
        match self.tab {
            PrTab::Conversation => self.render_conversation(detail, cx),
            PrTab::Commits => self.render_commits(detail, cx),
            PrTab::Checks => self.render_checks(detail, cx),
            PrTab::Files => render_files(&cx.entity(), self, cx),
        }
    }

    /// Commits tab (D6): scrollable rows — avatar/name (login, falling
    /// back to `author.name`), `messageHeadline` (`text_sm`), 7-char SHA
    /// (mono, muted), relative age; muted truncation caption when the
    /// connection holds more than the `first: 100` page.
    fn render_commits(&self, detail: Option<&gql::PrDetail>, cx: &Context<Self>) -> AnyElement {
        let Some(d) = detail else {
            if let Some(err) = self.detail.load_error() {
                return error_cell(err, cx);
            }
            return skeleton_list();
        };
        if d.commits.nodes.is_empty() {
            return empty_cell("No commits yet", cx);
        }
        let mut rows: Vec<AnyElement> = d
            .commits
            .nodes
            .iter()
            .enumerate()
            .map(|(i, commit)| {
                let name = commit
                    .author_login
                    .clone()
                    .or_else(|| commit.author_name.clone())
                    .unwrap_or_else(|| "ghost".into());
                h_flex()
                    .id(SharedString::from(format!("pr-commit-{i}")))
                    .gap_2()
                    .items_center()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Avatar::new()
                            .src(commit.author_avatar.clone().unwrap_or_default())
                            .xsmall(),
                    )
                    .child(
                        div()
                            .text_sm()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .child(commit.headline.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(name),
                    )
                    .child(
                        div()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(short_sha(&commit.oid)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(relative_age(commit.committed_at.as_deref())),
                    )
                    .into_any_element()
            })
            .collect();
        if d.commits.total_count > d.commits.nodes.len() as i64 {
            rows.push(
                div()
                    .id("pr-commits-truncated")
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "Showing first {} of {} commits",
                        d.commits.nodes.len(),
                        d.commits.total_count
                    ))
                    .into_any_element(),
            );
        }
        div()
            .id("pr-commits")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(v_flex().children(rows))
            .into_any_element()
    }

    /// Checks tab (D7): summary header outside the scroll (derived
    /// counts, pure fn), then one row per `statusCheckRollup.contexts`
    /// member — icon + tone from the unified state, trailing
    /// details-link button when the union member carries a URL.
    fn render_checks(&self, detail: Option<&gql::PrDetail>, cx: &Context<Self>) -> AnyElement {
        let Some(d) = detail else {
            if let Some(err) = self.detail.load_error() {
                return error_cell(err, cx);
            }
            return skeleton_list();
        };
        if d.checks.nodes.is_empty() {
            return empty_cell("No checks reported for this pull request", cx);
        }
        let counts = checks_counts(&d.checks.nodes);
        let header = div()
            .id("pr-checks-header")
            .flex_none()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(checks_header_line(&counts)),
            );
        let rows: Vec<AnyElement> = d
            .checks
            .nodes
            .iter()
            .enumerate()
            .map(|(i, check)| {
                let (icon, color) = check_visual(check.state, cx);
                let expanded = self.expanded_check.as_deref() == Some(check.name.as_str());
                // docs/0039 D3: only completed Actions-origin check runs
                // have job logs — statuses, third-party checks and
                // in-progress rows keep the external-link affordance.
                let loggable = check.origin == gql::CheckOrigin::Actions
                    && check.state != gql::CheckState::Pending;
                let row = h_flex()
                    .id(SharedString::from(format!("pr-check-{i}")))
                    .gap_2()
                    .items_center()
                    .px_3()
                    .py_2()
                    .child(Icon::new(icon).size_4().text_color(color))
                    .child(
                        div()
                            .text_sm()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .child(check.name.clone()),
                    );
                let row = row.when(loggable, |row| {
                    let page = cx.entity();
                    let name = check.name.clone();
                    row.child(
                        // The wrapper carries the probe selector (docs/0010
                        // D10 id discipline lives on the Button itself).
                        div()
                            .debug_selector(move || format!("check-log-toggle-{i}"))
                            .child(
                                Button::new(SharedString::from(format!("check-log-toggle-{i}")))
                                    .ghost()
                                    .xsmall()
                                    .icon(if expanded {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .tooltip(if expanded { "Hide log" } else { "View log" })
                                    .on_click(move |_, _, cx| {
                                        page.update(cx, |this, cx| this.toggle_check_log(&name, cx))
                                    }),
                            ),
                    )
                });
                let row = row.when_some(check.details_url.clone(), |row, url| {
                    row.child(
                        Button::new(SharedString::from(format!("pr-check-open-{i}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::ExternalLink)
                            .tooltip("View details")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                });
                v_flex()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(row)
                    .when(expanded && loggable, |col| {
                        col.child(self.render_check_log_pane(i, check, cx))
                    })
                    .into_any_element()
            })
            .collect();
        v_flex()
            .id("pr-checks")
            .flex_1()
            .min_h_0()
            .child(header)
            .child(
                div()
                    .id("pr-checks-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(v_flex().children(rows)),
            )
            .into_any_element()
    }

    /// The expanded check's log pane (docs/0039 D4): an inset panel
    /// under the row — running indicator, tail-truncated monospace body
    /// (markdown code-block styling), or an inline failure with Retry
    /// plus the external hand-off. Never holds the uncapped body: the
    /// page keeps only the pane-ready tail.
    fn render_check_log_pane(
        &self,
        i: usize,
        check: &gql::PrCheck,
        cx: &Context<Self>,
    ) -> AnyElement {
        let body: AnyElement = match self.check_logs.get(check.name.as_str()) {
            None | Some(CheckLogState::Running) => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Loading log…")
                .into_any_element(),
            Some(CheckLogState::Ready { text, truncated }) => {
                let open_full = check.details_url.clone().map(|url| {
                    Button::new(SharedString::from(format!("check-log-full-{i}")))
                        .ghost()
                        .xsmall()
                        .label("Open full log")
                        .on_click(move |_, _, cx| cx.open_url(&url))
                });
                v_flex()
                    .gap_1()
                    .children(truncated.then(|| {
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("Showing the last {} KiB", LOG_TAIL_CAP / 1024))
                    }))
                    .child(if text.is_empty() {
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("(no log output)")
                            .into_any_element()
                    } else {
                        div()
                            .id(SharedString::from(format!("check-log-pane-{i}")))
                            .track_scroll(&self.check_log_scroll)
                            .overflow_y_scroll()
                            .vertical_scrollbar(&self.check_log_scroll)
                            .max_h(px(384.))
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_xs()
                            .child(text.clone())
                            .into_any_element()
                    })
                    .children(open_full)
                    .into_any_element()
            }
            Some(CheckLogState::Failed(message)) => {
                let page = cx.entity();
                let name = check.name.clone();
                let open_full = check.details_url.clone().map(|url| {
                    Button::new(SharedString::from(format!("check-log-full-{i}")))
                        .ghost()
                        .xsmall()
                        .label("Open on github.com")
                        .on_click(move |_, _, cx| cx.open_url(&url))
                });
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().danger)
                            .child(message.clone()),
                    )
                    .child(
                        Button::new(SharedString::from(format!("check-log-retry-{i}")))
                            .ghost()
                            .xsmall()
                            .label("Retry")
                            .on_click(move |_, _, cx| {
                                page.update(cx, |this, cx| this.retry_check_log(&name, cx))
                            }),
                    )
                    .children(open_full)
                    .into_any_element()
            }
        };
        div()
            .debug_selector(move || format!("check-log-pane-{i}"))
            .mx_3()
            .mb_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted.opacity(0.4))
            .p_2()
            .child(body)
            .into_any_element()
    }

    /// The header Edit/Delete affordances for one timeline row (0019
    /// D3/D4): present only when the payload's viewer flags allow. The
    /// `prefix` keys the bridge ids (`{prefix}-edit` / `{prefix}-delete`).
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
        // Test-only probe key wrapping the buttons; the buttons themselves
        // carry the bridge-visible accesskit ids (0019 D8).
        Some(
            bar.debug_selector(|| format!("{prefix}-actions"))
                .into_any_element(),
        )
    }
    fn render_conversation(
        &self,
        detail: Option<&gql::PrDetail>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(d) = detail else {
            if let Some(err) = self.detail.load_error() {
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
        // Description card (D4): the Edit affordance rides the header row
        // when the payload says the viewer may update; while editing, a
        // prefilled Textarea + Save/Cancel replaces the body.
        let description = match &self.description_edit {
            Some(editor) => inline_edit_card(
                "pr-description",
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
                        Button::new("pr-description-edit")
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
                    "pr-description",
                    &author_login,
                    author_avatar.as_deref(),
                    relative_age(d.created_at.as_deref()),
                    d.body.as_deref(),
                    self.md.get("pr-description").map(Arc::as_ref),
                    edit,
                    // docs/0023 D3 (#196): the body's chips row.
                    self.render_reaction_row(
                        "pr-reaction-row",
                        &d.id,
                        &d.reaction_groups,
                        d.viewer_can_react,
                        cx,
                    ),
                    false,
                    cx,
                )
            }
        };
        let mut children = vec![description];
        for (kind, ix) in timeline_order(&d.comments.nodes, &d.reviews.nodes) {
            match kind {
                TimelineKind::Comment => {
                    let c = &d.comments.nodes[ix];
                    let key = format!("pr-comment-{ix}");
                    // 0019 D4: the row swaps to its editor or delete
                    // confirm while one is active for this node.
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
                    // docs/0026 D9: the ring marks the jumped target; the
                    // frame it first renders arms the one-shot scroll to it.
                    let anchored = matches!(
                        &self.anchor,
                        CommentAnchor::Highlighted(id) | CommentAnchor::Jumping(id) if *id == c.id
                    );
                    let mut jump_ix = None;
                    if anchored && !self.jump_done.get() {
                        self.jump_done.set(true);
                        jump_ix = Some(children.len());
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
                        // docs/0023 D3 (#196): the comment's chips row.
                        self.render_reaction_row(
                            &format!("{key}-reaction-row"),
                            &c.id,
                            &c.reaction_groups,
                            c.viewer_can_react,
                            cx,
                        ),
                        anchored,
                        cx,
                    ));
                    if let Some(ix) = jump_ix {
                        self.timeline_scroll.scroll_to_top_of_item(ix);
                    }
                }
                TimelineKind::Review => {
                    let r = &d.reviews.nodes[ix];
                    let key = format!("pr-review-{ix}");
                    if let Some(editor) = self.timeline_edit.as_ref().filter(|e| e.0 == r.id) {
                        children.push(inline_edit_card(
                            &key,
                            "Editing review",
                            &editor.1,
                            self.busy.is_some(),
                            cx.listener(|this, _, window, cx| this.save_edit_row(window, cx)),
                            cx.listener(|this, _, window, cx| this.cancel_edit_row(window, cx)),
                            cx,
                        ));
                        continue;
                    }
                    if self.delete_confirm.as_deref() == Some(r.id.as_str()) {
                        children.push(delete_confirm_card(
                            &key,
                            // 0019 D1: the review goes wholesale — say so.
                            "Delete this review? Its approval or request-changes state goes with it.",
                            self.busy.is_some(),
                            cx.listener(|this, _, window, cx| this.confirm_delete_row(window, cx)),
                            cx.listener(|this, _, _, cx| this.cancel_delete_confirm(cx)),
                            cx,
                        ));
                        continue;
                    }
                    let mut age = relative_age(r.submitted_at.as_deref());
                    if row_is_edited(r.submitted_at.as_deref(), r.updated_at.as_deref()) {
                        age.push_str(" (edited)");
                    }
                    let badge = review_badge(r.state.as_deref(), cx);
                    let actions = self.row_actions(
                        &key,
                        r.id.as_str(),
                        r.body.as_deref().unwrap_or_default(),
                        r.viewer_can_update,
                        r.viewer_can_delete,
                        cx,
                    );
                    // 0019 D4: badge and actions share the header's
                    // trailing slot.
                    let trailing = match (badge, actions) {
                        (Some(b), Some(a)) => {
                            Some(h_flex().gap_2().child(b).child(a).into_any_element())
                        }
                        (Some(b), None) => Some(b),
                        (None, a) => a,
                    };
                    children.push(body_card(
                        &key,
                        r.author
                            .as_ref()
                            .map(|a| a.login.as_str())
                            .unwrap_or("ghost"),
                        r.author.as_ref().map(|a| a.avatar_url.as_str()),
                        age,
                        r.body.as_deref().filter(|b| !b.is_empty()),
                        self.md.get(&key).map(Arc::as_ref),
                        trailing,
                        // docs/0023 D3 (#196): the review's chips row.
                        self.render_reaction_row(
                            &format!("{key}-reaction-row"),
                            &r.id,
                            &r.reaction_groups,
                            r.viewer_can_react,
                            cx,
                        ),
                        false,
                        cx,
                    ));
                }
            }
        }
        // #123: both connections cap at first: 100 — hidden rows surface
        // as the shared cap row (disjoint sets, so the sum is the honest
        // hidden count; 0021 D3).
        let missing = (d.comments.total_count - d.comments.nodes.len() as i64).max(0)
            + (d.reviews.total_count - d.reviews.nodes.len() as i64).max(0);
        if missing > 0 {
            children.push(cap_row("pr-comments-more", missing, &d.url, cx));
        }
        if d.comments.nodes.is_empty() && d.reviews.nodes.is_empty() {
            children.push(muted_centered("No conversation yet", cx));
        }
        children.push(self.render_comment_box(cx));
        // D4/D5: the timeline scrolls; the rail (merge/close panel + checks
        // mini-summary) stands beside it.
        let timeline = v_flex()
            .id("pr-conversation")
            .flex_1()
            .min_w_0()
            .min_h_0()
            // `h_full` + `items_start`: the timeline owns the scroll
            // (bounded to the split's height); the rail hugs its content
            // at the top and never rides along. Plain stretch (no
            // items_start) bounds the timeline too, but also stretches
            // the rail's v_flex and pushes its panels down the column —
            // probe-verified on the live build.
            .h_full()
            .overflow_y_scroll()
            // docs/0026 D9: the handle lets the notification jump land the
            // timeline on the anchored comment.
            .track_scroll(&self.timeline_scroll)
            // docs/0026 D9: cards are direct scroll children — the jump
            // resolves against their measured bounds in prepaint.
            .gap_3()
            .py_1()
            .children(children);
        h_flex()
            .id("pr-conversation-split")
            .flex_1()
            .min_h_0()
            .items_start()
            .gap_3()
            .child(timeline)
            .child(self.render_rail(d, cx))
            .into_any_element()
    }

    /// The conversation right rail (D5): merge/close panel + checks
    /// mini-summary. Fixed ~288px; terminal PR states collapse it to a
    /// note.
    fn render_rail(&self, d: &gql::PrDetail, cx: &Context<Self>) -> AnyElement {
        v_flex()
            .id("pr-rail")
            .w_72()
            .flex_none()
            .gap_3()
            .child(self.render_merge_panel(d, cx))
            .children(self.render_rail_error(cx))
            // docs/0043 D5 rail order; the milestone panel only exists
            // when there is something to show or the picker can arm
            // (0022 D8).
            .child(self.render_reviewers_panel(d, cx))
            .child(self.render_labels_panel(d, cx))
            .child(self.render_assignees_panel(d, cx))
            .children(self.render_milestone_panel(d, cx))
            .child(self.render_checks_summary(d, cx))
            .into_any_element()
    }

    /// A rail panel's title row with its trailing Edit affordance (0022
    /// D5, docs/0043 D5). Hidden, not disabled, whenever anything
    /// blocks it — gate, busy, or another editor being open (D7).
    fn meta_panel_title(
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

    /// The one rail error slot (docs/0043 D2): any Rail-scoped
    /// mutation failure reads here, above the metadata cluster.
    fn render_rail_error(&self, cx: &Context<Self>) -> Option<AnyElement> {
        self.mutation_error
            .as_ref()
            .filter(|e| e.scope == ErrorScope::Rail)
            .map(|e| {
                div()
                    .id("pr-rail-error")
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(e.message.clone())
                    .into_any_element()
            })
    }

    /// Reviewers (docs/0043 D5): login rows while displaying, the
    /// checklist picker while editing. A team/bot request (or any
    /// degraded ref) keeps the panel display-only — D3's reviewer
    /// clause hides the Edit affordance.
    fn render_reviewers_panel(&self, d: &gql::PrDetail, cx: &Context<Self>) -> AnyElement {
        if let Some(PrMetaEditor::Reviewers(draft)) = &self.meta_edit {
            return GroupBox::new()
                .id("pr-reviewers-panel")
                .title("Reviewers")
                .child(self.render_meta_picker_card(
                    "pr-reviewers",
                    draft,
                    PickerSource::Reviewers,
                    cx,
                ))
                .into_any_element();
        }
        let viewer = self.viewer_login(cx);
        let show_edit = pr_reviewers_arming(d, viewer.as_deref()) && !self.any_meta_editor_open();
        GroupBox::new()
            .id("pr-reviewers-panel")
            .title(self.meta_panel_title(
                "Reviewers",
                "pr-reviewers-edit",
                "Request reviews",
                show_edit,
                Self::start_reviewers_picker,
                cx,
            ))
            .child(
                v_flex()
                    .gap_1()
                    .py_1()
                    .when(d.review_requests.is_empty(), |col| {
                        col.child(empty_cell("No reviews requested", cx))
                    }),
            )
            .children(d.review_requests.iter().map(|r| {
                if r.login.is_empty() {
                    // team / bot / mannequin member (D3): display-only
                    return div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("team or bot")
                        .into_any_element();
                }
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_sm().child(r.login.clone()))
                    .into_any_element()
            }))
            .into_any_element()
    }

    /// Labels (docs/0043 D5): the shared data-color pill builder
    /// (docs/0002 D5 exception) — the reuse docs/0022 §12 D5
    /// anticipated.
    fn render_labels_panel(&self, d: &gql::PrDetail, cx: &Context<Self>) -> AnyElement {
        if let Some(PrMetaEditor::Labels(draft)) = &self.meta_edit {
            return GroupBox::new()
                .id("pr-labels-panel")
                .title("Labels")
                .child(self.render_meta_picker_card("pr-labels", draft, PickerSource::Labels, cx))
                .into_any_element();
        }
        let show_edit = pr_labels_arming(d) && !self.any_meta_editor_open();
        let surface = cx.theme().tokens.group_box.into();
        GroupBox::new()
            .id("pr-labels-panel")
            .title(self.meta_panel_title(
                "Labels",
                "pr-labels-edit",
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
                        row.child(empty_cell(
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
                    .map(|l| crate::page::label_pill(&l.name, l.color.as_deref(), surface, cx)),
            )
            .into_any_element()
    }

    fn render_assignees_panel(&self, d: &gql::PrDetail, cx: &Context<Self>) -> AnyElement {
        if let Some(PrMetaEditor::Assignees(draft)) = &self.meta_edit {
            return GroupBox::new()
                .id("pr-assignees-panel")
                .title("Assignees")
                .child(self.render_meta_picker_card(
                    "pr-assignees",
                    draft,
                    PickerSource::Assignees,
                    cx,
                ))
                .into_any_element();
        }
        let show_edit = pr_assignees_arming(d) && !self.any_meta_editor_open();
        GroupBox::new()
            .id("pr-assignees-panel")
            .title(self.meta_panel_title(
                "Assignees",
                "pr-assignees-edit",
                "Edit assignees",
                show_edit,
                Self::start_assignees_picker,
                cx,
            ))
            .child(v_flex().gap_1().py_1().when(d.assignees.is_empty(), |col| {
                col.child(empty_cell("No one assigned", cx))
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

    /// Milestone (docs/0043 D5, 0022 D8): title + state tag; `None`
    /// when the PR has no milestone — unless the picker can arm (the
    /// affordance needs a home; a triage-less viewer sees no empty
    /// panel).
    fn render_milestone_panel(&self, d: &gql::PrDetail, cx: &Context<Self>) -> Option<AnyElement> {
        if let Some(PrMetaEditor::Milestone { rows, chosen }) = &self.meta_edit {
            return Some(
                GroupBox::new()
                    .id("pr-milestone-panel")
                    .title("Milestone")
                    .child(self.render_milestone_card(rows, chosen, cx)),
            )
            .map(|g| g.into_any_element());
        }
        let show_edit = pr_milestone_arming(d) && !self.any_meta_editor_open();
        if d.milestone.is_none() && !show_edit {
            return None;
        }
        let cell = h_flex()
            .id("pr-milestone")
            .debug_selector(|| "pr-milestone".to_string())
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
                row.child(empty_cell("None", cx))
            });
        Some(
            GroupBox::new()
                .id("pr-milestone-panel")
                .title(self.meta_panel_title(
                    "Milestone",
                    "pr-milestones-edit",
                    "Edit milestone",
                    show_edit,
                    Self::start_milestone_picker,
                    cx,
                ))
                .child(cell),
        )
        .map(|g| g.into_any_element())
    }

    /// The shared checklist card (docs/0043 D5, 0022 D5 shape): one
    /// clickable row per candidate — the row is the toggle target (its
    /// accesskit id is the probe key), the checkbox is display-only —
    /// plus Save/Cancel.
    fn render_meta_picker_card(
        &self,
        id: &'static str,
        draft: &PickerDraft,
        source: PickerSource,
        cx: &Context<Self>,
    ) -> AnyElement {
        let busy = self.busy.is_some();
        let loading = match source {
            PickerSource::Labels => matches!(self.labels_source, PickerFetch::Loading),
            PickerSource::Assignees | PickerSource::Reviewers => {
                matches!(self.users_source, PickerFetch::Loading)
            }
            PickerSource::Milestones => false,
        };
        let failed = match source {
            PickerSource::Labels => self.labels_source.failed_message(),
            PickerSource::Assignees | PickerSource::Reviewers => self.users_source.failed_message(),
            PickerSource::Milestones => None,
        };
        let (op, save_id) = match source {
            PickerSource::Reviewers => (BusyOp::PrSetReviewers, "pr-reviewers-save"),
            PickerSource::Labels => (BusyOp::PrSetLabels, "pr-labels-save"),
            PickerSource::Assignees => (BusyOp::PrSetAssignees, "pr-assignees-save"),
            PickerSource::Milestones => unreachable!("milestone uses render_milestone_card"),
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
                        .child(empty_cell("Loading…", cx)),
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
                let key = format!("{id}-row-{ix}");
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
                    .on_click(cx.listener(move |this, _: &gpui::ClickEvent, _, cx| {
                        this.toggle_meta_row(ix, cx)
                    }))
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
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.save_meta(op, window, cx)
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("{id}-cancel")))
                            .debug_selector(move || format!("{id}-cancel"))
                            .label("Cancel")
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_meta_editor(cx))),
                    ),
            )
            .into_any_element()
    }

    /// The milestone picker (0022 D5 shape): single-select rows —
    /// clicking a row chooses it, "No milestone" clears. Save posts
    /// `milestoneId` (null clears); Cancel restores.
    fn render_milestone_card(
        &self,
        rows: &[PickerRow],
        chosen: &Option<String>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let busy = self.busy.is_some();
        let loading = matches!(self.milestones_source, PickerFetch::Loading);
        let id_key = "pr-milestones-card".to_string();
        v_flex()
            .id("pr-milestones-card")
            .debug_selector(move || id_key.clone())
            .gap_1()
            .py_1()
            .when(loading, |col| {
                col.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Spinner::new().small())
                        .child(empty_cell("Loading…", cx)),
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
                let key = format!("pr-milestones-row-{ix}");
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
                        Button::new("pr-milestones-save")
                            .debug_selector(|| "pr-milestones-save".to_string())
                            .label("Save")
                            .small()
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save_meta(BusyOp::PrSetMilestone, window, cx)
                            })),
                    )
                    .child(
                        Button::new("pr-milestones-cancel")
                            .debug_selector(|| "pr-milestones-cancel".to_string())
                            .label("Cancel")
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_meta_editor(cx))),
                    ),
            )
            .into_any_element()
    }

    /// Any editor or picker open (docs/0043 D7) — hides the Edit
    /// affordances while one is up.
    fn any_meta_editor_open(&self) -> bool {
        self.meta_edit.is_some()
            || self.description_edit.is_some()
            || self.timeline_edit.is_some()
            || self.thread_edit.is_some()
    }

    /// Save the open picker's draft (docs/0043 D1/D3): the input
    /// carries the complete target id list — replace semantics
    /// upstream. The milestone's unchanged choice closes without a
    /// pointless write (0022 §11).
    fn save_meta(&mut self, op: BusyOp, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.data().cloned() else {
            return;
        };
        if self.busy.is_some() || d.id.is_empty() {
            return;
        }
        let payload = match op {
            BusyOp::PrSetMilestone => {
                let Some(PrMetaEditor::Milestone { chosen, .. }) = &self.meta_edit else {
                    return;
                };
                if chosen.as_deref() == d.milestone.as_ref().map(|m| m.id.as_str()) {
                    self.meta_edit = None;
                    cx.notify();
                    return;
                }
                Self::pr_meta_mutation(op, &d.id, &[], chosen.as_deref())
            }
            BusyOp::PrSetReviewers => {
                let Some(PrMetaEditor::Reviewers(draft)) = &self.meta_edit else {
                    return;
                };
                let ids = compute_replace_ids(&draft.rows, &draft.checked);
                Self::pr_meta_mutation(op, &d.id, &ids, None)
            }
            BusyOp::PrSetLabels => {
                let Some(PrMetaEditor::Labels(draft)) = &self.meta_edit else {
                    return;
                };
                let ids = compute_replace_ids(&draft.rows, &draft.checked);
                Self::pr_meta_mutation(op, &d.id, &ids, None)
            }
            BusyOp::PrSetAssignees => {
                let Some(PrMetaEditor::Assignees(draft)) = &self.meta_edit else {
                    return;
                };
                let ids = compute_replace_ids(&draft.rows, &draft.checked);
                Self::pr_meta_mutation(op, &d.id, &ids, None)
            }
            _ => return,
        };
        self.mutation_error = None;
        self.busy = Some(op);
        cx.notify();
        self.spawn_mutation(op, payload.0, payload.1, window, cx);
    }

    fn render_merge_panel(&self, d: &gql::PrDetail, cx: &Context<Self>) -> AnyElement {
        let tone = pr_state_tone(d.state.as_deref(), d.draft);
        let busy = self.busy.is_some();
        // Terminal states (D5): the refetch after merge/close repaints the
        // panel as a note — no actions on a PR that is already done.
        if tone == PrStateTone::Merged || tone == PrStateTone::Closed {
            let (label, color) = match tone {
                PrStateTone::Merged => ("Merged", cx.theme().success),
                _ => ("Closed", cx.theme().muted_foreground),
            };
            return GroupBox::new()
                .id("pr-merge-panel")
                .title("Merge")
                .child(
                    v_flex()
                        .gap_1()
                        .py_1()
                        .child(Badge::new().color(color).child(label))
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("This pull request is no longer open."),
                        ),
                )
                .into_any_element();
        }
        // The state line (D5): mergeStateStatus wording; a draft overrides
        // whatever GitHub still reports.
        let (line, line_tone) = if d.draft {
            (
                "Draft pull requests can't be merged until marked ready",
                MergeLineTone::Muted,
            )
        } else {
            merge_state_line(d.merge_state_status.as_deref())
        };
        let line_color = match line_tone {
            MergeLineTone::Success => cx.theme().success,
            MergeLineTone::Warning => cx.theme().warning,
            MergeLineTone::Danger => cx.theme().danger,
            MergeLineTone::Muted => cx.theme().muted_foreground,
        };
        let can_merge = merge_enabled(
            d.state.as_deref(),
            d.draft,
            d.mergeable.as_deref(),
            busy,
            self.methods.is_empty(),
        );
        // Method selector (D5): shown only when the repo allows a real
        // choice; a single method makes the button carry the verb.
        let method_row = match (&self.method_select, self.methods.len()) {
            (Some(select), n) if n > 1 => Select::new(select).small().w_full().into_any_element(),
            _ => div().into_any_element(),
        };
        let merge_label: SharedString = if self.methods.len() == 1 {
            self.methods[0].verb().into()
        } else {
            "Merge pull request".into()
        };
        GroupBox::new()
            .id("pr-merge-panel")
            .title("Merge")
            .child(
                v_flex()
                    .gap_2()
                    .py_1()
                    .child(div().text_sm().text_color(line_color).child(line))
                    .child(method_row)
                    .when_some(
                        self.mutation_error
                            .as_ref()
                            .filter(|e| e.scope == ErrorScope::Panel),
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
                        h_flex()
                            .gap_2()
                            .items_center()
                            .when(
                                matches!(self.busy, Some(BusyOp::Merge | BusyOp::Close)),
                                |row| row.child(Spinner::new().small()),
                            )
                            .child(
                                Button::new("pr-merge")
                                    .label(merge_label)
                                    .primary()
                                    .small()
                                    .disabled(!can_merge)
                                    .tooltip(merge_disabled_reason(can_merge, line))
                                    .on_click(
                                        cx.listener(|this, _, window, cx| {
                                            this.merge_pr(window, cx)
                                        }),
                                    ),
                            )
                            .child(
                                Button::new("pr-close")
                                    .label("Close pull request")
                                    .danger()
                                    .ghost()
                                    .small()
                                    .disabled(busy || d.state.as_deref() != Some("OPEN"))
                                    .on_click(
                                        cx.listener(|this, _, window, cx| {
                                            this.close_pr(window, cx)
                                        }),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_checks_summary(&self, d: &gql::PrDetail, cx: &Context<Self>) -> AnyElement {
        let (label, tone) = rollup_summary(d.rollup_state.as_deref());
        let color = match tone {
            ChecksTone::Success => cx.theme().success,
            ChecksTone::Danger => cx.theme().danger,
            ChecksTone::Warning => cx.theme().warning,
            ChecksTone::Muted => cx.theme().muted_foreground,
        };
        v_flex()
            .id("pr-checks-summary")
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .p_3()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color)
                    .child(label),
            )
            .child(
                Button::new("pr-view-checks")
                    .ghost()
                    .small()
                    .label("View checks")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.tab = PrTab::Checks;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// The comment box (D4): draft Textarea + action Select + submit.
    /// Comment requires a body; so does Request-changes (server-side
    /// rule, #171) — only Approve may go empty. Same pure fn gates
    /// button and handler. Failures keep the draft
    /// and surface an inline error row above the box.
    fn render_comment_box(&self, cx: &Context<Self>) -> AnyElement {
        let action = self.current_action(cx);
        let body_empty = self.comment.read(cx).value().trim().is_empty();
        let allowed = comment_action_allowed(action.as_ref(), body_empty);
        let busy = self.busy.is_some();
        let sending = matches!(self.busy, Some(BusyOp::Comment) | Some(BusyOp::Review));
        let tooltip = if !allowed {
            "Write a comment first"
        } else if busy {
            "Sending"
        } else {
            "Post to GitHub"
        };
        v_flex()
            .id("pr-comment-box")
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
                    .justify_between()
                    .when(sending, |row| row.child(Spinner::new().small()))
                    .child(Select::new(&self.action_select).small().w_40())
                    .child(
                        Button::new("pr-comment-submit")
                            .label(action.clone())
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

impl Focusable for PrDetailPage {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PrDetailPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_diff_list();
        // docs/0040 D5 (amended): tokenization is a background task —
        // pump the selected file's queue from the one &mut Context in
        // the render path; diff_body only reads.
        self.pump_highlight(cx);
        // docs/0026 D9: fire the armed jump the frame the target card
        // mounts — the anchor records its origin on paint, the offset lands
        // next frame. A card swapped for an editor/confirm (or a
        // non-conversation tab) keeps the jump armed until it really shows.
        if let CommentAnchor::Jumping(id) = self.anchor.clone() {
            if self.anchor_row_mounts(&id) {
                self.anchor = CommentAnchor::Highlighted(id);
                self.start_anchor_fade(cx);
            }
        }
        // Method Select (D5): `SelectState` needs a window, so it is built
        // on the first render that has a detail payload — once per page
        // (a repo's allowed methods can't change while the page is open).
        if self.method_select.is_none() {
            if let Some(d) = self.detail.data() {
                let methods = allowed_methods(&d.merge_methods);
                if !methods.is_empty() {
                    let labels: Vec<SharedString> = methods
                        .iter()
                        .map(|m| SharedString::from(m.label()))
                        .collect();
                    let default = default_method(&methods).unwrap_or(methods[0]);
                    let default_label: SharedString = default.label().into();
                    let select = cx.new(|cx| {
                        let mut select =
                            SelectState::new(SearchableVec::from(labels), None, window, cx);
                        select.set_selected_value(&default_label, window, cx);
                        select
                    });
                    self.methods = methods;
                    self.method_select = Some(select);
                }
            }
        }
        let detail = self.detail.data();
        v_flex()
            .id("pr-detail")
            .key_context("GithubPrPage")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &DismissPrPage, window, cx| {
                // docs/0043 D7 (0022 §11): Esc cancels the topmost
                // surface and only closes the page when nothing is open.
                if !this.cancel_top_editor(window, cx) {
                    this.request_close(cx);
                }
            }))
            .size_full()
            .min_w_0()
            .p_4()
            .gap_3()
            .child(self.render_header(detail, cx))
            .child(self.render_tabs(detail, cx))
            .child(self.render_tab_content(detail, cx))
    }
}

// ---------------------------------------------------------------------------
// Pure helpers — table-tested.
// ---------------------------------------------------------------------------

// --- rail metadata gates (docs/0043 D3/D4) — table-tested ------------------

/// D3 labels arming: the shared core over the detail fields.
pub(crate) fn pr_labels_arming(d: &gql::PrDetail) -> bool {
    crate::pickers::connection_arming(
        d.viewer_can_label,
        d.labels_total,
        d.labels.len(),
        d.labels.iter().all(|l| l.id.is_some()),
    )
}

/// D3 assignees arming — same guard shape.
pub(crate) fn pr_assignees_arming(d: &gql::PrDetail) -> bool {
    crate::pickers::connection_arming(
        d.viewer_can_assign,
        d.assignees_total,
        d.assignees.len(),
        d.assignees.iter().all(|a| a.id.is_some()),
    )
}

/// D4 milestone arming: **no** `viewerCanSetMilestone` exists on
/// `PullRequest` — triage-and-above via `repository.viewerPermission`
/// substitutes; a write target must exist.
pub(crate) fn pr_milestone_arming(d: &gql::PrDetail) -> bool {
    crate::pickers::triage_gate(d.viewer_permission.as_deref()) && !d.id.is_empty()
}

/// D4 reviewers arming: **no** upstream gate exists — triage-and-above
/// or the PR author; plus D3's reviewer clause: every current request
/// must parse as a user-with-id (a team/bot/mannequin forces
/// display-only) and the connection must be complete.
pub(crate) fn pr_reviewers_arming(d: &gql::PrDetail, viewer_login: Option<&str>) -> bool {
    let gated = crate::pickers::triage_gate(d.viewer_permission.as_deref())
        || d.author
            .as_ref()
            .map(|a| Some(a.login.as_str()) == viewer_login)
            .unwrap_or(false);
    gated
        && d.review_requests.iter().all(|r| r.id.is_some())
        && d.review_requests.len() as i64 == d.review_requests_total
        && !d.id.is_empty()
}

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

/// Which timeline kind owns this node id (docs/0019 D4). Pure —
/// table-tested; `None` when no node carries the id (deleted elsewhere
/// between refetches, or a stale editor).
pub(crate) fn node_kind(
    comments: &[gql::PrComment],
    reviews: &[gql::PrReview],
    node_id: &str,
) -> Option<TimelineKind> {
    if comments.iter().any(|c| c.id == node_id) {
        Some(TimelineKind::Comment)
    } else if reviews.iter().any(|r| r.id == node_id) {
        Some(TimelineKind::Review)
    } else {
        None
    }
}

/// The (op, mutation, variables) for saving an edited row (docs/0019 D5).
/// Pure — the handler stays a thin guard around it.
pub(crate) fn edit_row(
    kind: TimelineKind,
    node_id: &str,
    body: &str,
) -> (BusyOp, Mutation, serde_json::Value) {
    match kind {
        TimelineKind::Comment => (
            BusyOp::CommentEdit,
            Mutation::UpdateComment,
            json!({ "input": { "id": node_id, "body": body } }),
        ),
        TimelineKind::Review => (
            BusyOp::ReviewEdit,
            Mutation::UpdateReview,
            json!({ "input": { "pullRequestReviewId": node_id, "body": body } }),
        ),
    }
}

/// The (op, mutation, variables) for a confirmed delete (docs/0019 D5).
pub(crate) fn delete_row(
    kind: TimelineKind,
    node_id: &str,
) -> (BusyOp, Mutation, serde_json::Value) {
    match kind {
        TimelineKind::Comment => (
            BusyOp::CommentDelete,
            Mutation::DeleteComment,
            json!({ "input": { "id": node_id } }),
        ),
        TimelineKind::Review => (
            BusyOp::ReviewDelete,
            Mutation::DeleteReview,
            json!({ "input": { "pullRequestReviewId": node_id } }),
        ),
    }
}

/// A row shows "(edited)" when the payload's `updatedAt` is strictly
/// newer than its created/submitted stamp; missing stamps are never
/// edited (same stamp rule as `timeline_order`). Pure — table-tested.
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
/// `owner/repo` → (`owner`, `name`), tolerating a missing slash (empty
/// owner — the query then fails server-side and lands in the error cell).
pub(crate) fn split_repo(full: &str) -> (&str, &str) {
    match full.split_once('/') {
        Some((owner, name)) => (owner, name),
        None => ("", full),
    }
}

/// The open rail picker (docs/0043 D5): checklist drafts for the three
/// connection pickers, chosen-id for the single-select milestone. One
/// at a time (D7).
#[derive(Debug)]
enum PrMetaEditor {
    Reviewers(PickerDraft),
    Labels(PickerDraft),
    Assignees(PickerDraft),
    /// `chosen: None` = "No milestone" — the clearing choice (0022 §2.1).
    Milestone {
        rows: Vec<PickerRow>,
        chosen: Option<String>,
    },
}

/// The write operation in flight (D4/D5) — one at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BusyOp {
    Comment,
    Review,
    Description,
    Merge,
    Close,
    // 0019 D5: timeline-row writes — the op names the surface (and the
    // log line) honestly; all of them error into the timeline scope.
    CommentEdit,
    CommentDelete,
    ReviewEdit,
    ReviewDelete,
    // 0030 D7: the Files-tab review writes — errors land in the diff
    // pane (ErrorScope::Files), and success refetches threads (not the
    // detail — patches don't change).
    LineComment,
    ReviewReply,
    ReviewBatch,
    ThreadResolve,
    ThreadUnresolve,
    // #212: review-thread comment writes — Files scope like the rest of
    // the review surfaces.
    ReviewCommentEdit,
    ReviewCommentDelete,
    // #196: reaction toggles on the conversation tab (body, review
    // summaries, issue comments) — Timeline scope like the other
    // conversation writes; the refetch repaints the chips.
    Reaction,
    // docs/0043 D1/D2: the rail metadata writes — Rail scope (one slot,
    // 0022's precedent), success refetches the detail so the rail
    // repaints from fresh data.
    PrSetLabels,
    PrSetAssignees,
    PrSetMilestone,
    PrSetReviewers,
}

impl BusyOp {
    /// Where a failure of this op renders inline (D4/D5).
    fn error_scope(self) -> ErrorScope {
        match self {
            BusyOp::Comment
            | BusyOp::Review
            | BusyOp::Description
            | BusyOp::CommentEdit
            | BusyOp::CommentDelete
            | BusyOp::ReviewEdit
            | BusyOp::ReviewDelete
            | BusyOp::Reaction => ErrorScope::Timeline,
            BusyOp::Merge | BusyOp::Close => ErrorScope::Panel,
            // 0030 D7: Files-tab failures render in the diff pane —
            // inline, never modal (the composer stays open with its draft).
            BusyOp::LineComment
            | BusyOp::ReviewReply
            | BusyOp::ReviewBatch
            | BusyOp::ThreadResolve
            | BusyOp::ThreadUnresolve
            | BusyOp::ReviewCommentEdit
            | BusyOp::ReviewCommentDelete => ErrorScope::Files,
            // docs/0043 D2: metadata failures read in the rail.
            BusyOp::PrSetLabels
            | BusyOp::PrSetAssignees
            | BusyOp::PrSetMilestone
            | BusyOp::PrSetReviewers => ErrorScope::Rail,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ErrorScope {
    /// The conversation column — above the comment box.
    Timeline,
    /// The merge/close rail panel.
    Panel,
    /// The issue page's state panel (docs/0020 D5) — the PR page never
    /// produces it, the issue page never produces Panel.
    Rail,
    /// The diff pane (docs/0030 D7) — above the diff body; only the
    /// Files-tab review writes produce it.
    Files,
}

/// The last mutation failure — kept until the next attempt or success, so
/// it survives the post-mutation refetch that repaints everything else.
#[derive(Clone, Debug)]
pub(crate) struct MutationError {
    pub(crate) scope: ErrorScope,
    pub(crate) message: String,
}

/// One armed diff row's composer (docs/0030 D4): the row it anchors to
/// plus its editor.
pub(crate) struct LineComposer {
    pub(crate) path: String,
    /// The range's END line (GitHub's `line`/`side` fields).
    pub(crate) side: gql::DiffSide,
    pub(crate) line: u32,
    /// #211: the range's START line when a second row was picked — same
    /// side only in v1 (cross-side spans need diff-order rules). None =
    /// single-line comment.
    pub(crate) start: Option<(gql::DiffSide, u32)>,
    pub(crate) editor: Entity<TextareaState>,
}

/// One queued draft comment (docs/0030 D4) — page-local until the
/// Finish-review bar submits the whole batch as one mutation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LineDraft {
    pub(crate) path: String,
    pub(crate) side: gql::DiffSide,
    pub(crate) line: u32,
    /// #211: the range's START line, when the draft spans lines.
    pub(crate) start: Option<(gql::DiffSide, u32)>,
    pub(crate) body: String,
}

/// The event the Finish-review bar submits with (docs/0030 D4).
/// DISMISS is never offered — it is what Discard means locally.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum FinishEvent {
    #[default]
    Comment,
    Approve,
    RequestChanges,
}

impl FinishEvent {
    /// `PullRequestReviewEvent` wire value (docs/0030 §3).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            FinishEvent::Comment => "COMMENT",
            FinishEvent::Approve => "APPROVE",
            FinishEvent::RequestChanges => "REQUEST_CHANGES",
        }
    }

    /// The bridge-id token (docs/0030 D8).
    pub(crate) fn id_token(self) -> &'static str {
        match self {
            FinishEvent::Comment => "comment",
            FinishEvent::Approve => "approve",
            FinishEvent::RequestChanges => "request-changes",
        }
    }

    /// The bar's event buttons, (label, variant) in display order.
    pub(crate) const CHOICES: [(&'static str, FinishEvent); 3] = [
        ("Comment", FinishEvent::Comment),
        ("Approve", FinishEvent::Approve),
        ("Request changes", FinishEvent::RequestChanges),
    ];
}

/// Why a thread is being resolved (#213) — `PullRequestReviewThread
/// ResolutionReason`, verified live (2026-09-16 introspection): the
/// enum is `ADDRESSED | WONT_FIX | INVALID`; the input field is optional
/// and omitted for a plain resolve. The *read* schema has no
/// `resolutionReason` field, so resolved chips stay generic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResolutionReason {
    Addressed,
    WontFix,
    Invalid,
}

impl ResolutionReason {
    /// Wire value (`PullRequestReviewThreadResolutionReason`).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ResolutionReason::Addressed => "ADDRESSED",
            ResolutionReason::WontFix => "WONT_FIX",
            ResolutionReason::Invalid => "INVALID",
        }
    }

    /// The bridge-id token (docs/0030 D8).
    pub(crate) fn id_token(self) -> &'static str {
        match self {
            ResolutionReason::Addressed => "addressed",
            ResolutionReason::WontFix => "wont-fix",
            ResolutionReason::Invalid => "invalid",
        }
    }

    /// The picker's buttons, (label, reason) in display order — the
    /// plain "Resolve" (no reason) and Cancel buttons are separate.
    pub(crate) const CHOICES: [(&'static str, ResolutionReason); 3] = [
        ("Addressed", ResolutionReason::Addressed),
        ("Won't fix", ResolutionReason::WontFix),
        ("Invalid", ResolutionReason::Invalid),
    ];
}

/// #211: order two row anchors into a `(start, end)` range — same side
/// only in v1 (cross-side spans need diff-order rules); None when the
/// sides differ. Equal lines collapse (start == end).
pub(crate) fn normalize_range(
    a: (gql::DiffSide, u32),
    b: (gql::DiffSide, u32),
) -> Option<((gql::DiffSide, u32), (gql::DiffSide, u32))> {
    if a.0 != b.0 {
        return None;
    }
    if a.1 <= b.1 {
        Some((a, b))
    } else {
        Some((b, a))
    }
}

/// #211: the optional `startLine`/`startSide` input fields of
/// `DraftPullRequestReviewThread` (verified live 2026-09-16) — an empty
/// map for single-line comments, so the variables stay byte-identical
/// to the pre-#211 shape.
pub(crate) fn start_fields(
    start: Option<(gql::DiffSide, u32)>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut fields = serde_json::Map::new();
    if let Some((side, line)) = start {
        fields.insert("startLine".into(), json!(line));
        fields.insert("startSide".into(), json!(side.as_str()));
    }
    fields
}

/// The composer's send gating (docs/0030 §5): a non-blank body.
pub(crate) fn composer_body_allowed(body: &str) -> bool {
    !body.trim().is_empty()
}

/// The Finish-review bar's Submit gating (docs/0030 D4/§5): at least one
/// draft, nothing else in flight.
pub(crate) fn review_batch_allowed(drafts: usize, busy: bool) -> bool {
    drafts > 0 && !busy
}

/// A merge method the repo allows (D5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    /// `PullRequestMergeMethod` wire value.
    fn as_str(self) -> &'static str {
        match self {
            MergeMethod::Merge => "MERGE",
            MergeMethod::Squash => "SQUASH",
            MergeMethod::Rebase => "REBASE",
        }
    }

    /// Select label (GitHub's own wording).
    fn label(self) -> &'static str {
        match self {
            MergeMethod::Merge => "Create a merge commit",
            MergeMethod::Squash => "Squash and merge",
            MergeMethod::Rebase => "Rebase and merge",
        }
    }

    /// Merge-button verb — what the button says when this is the only
    /// allowed method.
    fn verb(self) -> &'static str {
        match self {
            MergeMethod::Merge => "Merge pull request",
            MergeMethod::Squash => "Squash and merge",
            MergeMethod::Rebase => "Rebase and merge",
        }
    }
}

/// Exactly the methods the repo allows, in GitHub order (D5). Empty when
/// the repo disables all in-app merging.
pub(crate) fn allowed_methods(m: &gql::MergeMethods) -> Vec<MergeMethod> {
    let mut out = Vec::new();
    if m.merge {
        out.push(MergeMethod::Merge);
    }
    if m.squash {
        out.push(MergeMethod::Squash);
    }
    if m.rebase {
        out.push(MergeMethod::Rebase);
    }
    out
}

/// MERGE when allowed, else the first allowed (D5).
pub(crate) fn default_method(methods: &[MergeMethod]) -> Option<MergeMethod> {
    methods
        .iter()
        .copied()
        .find(|m| *m == MergeMethod::Merge)
        .or_else(|| methods.first().copied())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MergeLineTone {
    Success,
    Warning,
    Danger,
    Muted,
}

/// Rail state-line wording from `MergeStateStatus` (D5; enum values
/// verified against the GraphQL reference).
pub(crate) fn merge_state_line(status: Option<&str>) -> (&'static str, MergeLineTone) {
    match status {
        Some("CLEAN") | Some("HAS_HOOKS") => (
            "This branch has no conflicts with the base branch",
            MergeLineTone::Success,
        ),
        Some("BEHIND") => (
            "This branch is out-of-date with the base branch",
            MergeLineTone::Warning,
        ),
        Some("BLOCKED") => (
            "Merging is blocked by required reviews or status checks",
            MergeLineTone::Warning,
        ),
        Some("DIRTY") => (
            "This branch has conflicts that must be resolved",
            MergeLineTone::Danger,
        ),
        Some("DRAFT") => (
            "Draft pull requests can't be merged until marked ready",
            MergeLineTone::Muted,
        ),
        Some("UNSTABLE") => (
            "Mergeable, but some checks are not passing",
            MergeLineTone::Muted,
        ),
        _ => (
            "GitHub is still calculating merge status",
            MergeLineTone::Muted,
        ),
    }
}

/// Merge-button gating (D5): open, not draft, server-says-MERGEABLE, idle,
/// and at least one allowed method.
pub(crate) fn merge_enabled(
    state: Option<&str>,
    draft: bool,
    mergeable: Option<&str>,
    busy: bool,
    no_methods: bool,
) -> bool {
    state == Some("OPEN") && !draft && mergeable == Some("MERGEABLE") && !busy && !no_methods
}

/// Tooltip for the Merge button — the state line doubles as the reason.
fn merge_disabled_reason(can_merge: bool, line: &str) -> SharedString {
    if can_merge {
        "Merge this pull request".into()
    } else {
        line.into()
    }
}

/// Comment-box gating (D4): a plain comment and a Request-changes
/// review need a body — GitHub rejects both empty server-side (REST
/// docs: `body` is required for `REQUEST_CHANGES`/`COMMENT`; only
/// `APPROVE` may go empty, #171). Unknown actions never fire.
pub(crate) fn comment_action_allowed(action: &str, body_empty: bool) -> bool {
    match action {
        "Comment" | "Request changes" => !body_empty,
        "Approve" => true,
        _ => false,
    }
}

/// Comment-box action → `PullRequestReviewEvent` wire value.
pub(crate) fn review_event(action: &str) -> Option<&'static str> {
    match action {
        "Approve" => Some("APPROVE"),
        "Request changes" => Some("REQUEST_CHANGES"),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChecksTone {
    Success,
    Danger,
    Warning,
    Muted,
}

/// Checks mini-summary wording from `statusCheckRollup.state` (D5) —
/// unknown/absent states degrade to a muted line, never an error.
pub(crate) fn rollup_summary(state: Option<&str>) -> (&'static str, ChecksTone) {
    match state {
        Some("SUCCESS") => ("All checks passed", ChecksTone::Success),
        Some("FAILURE") => ("Some checks failed", ChecksTone::Danger),
        Some("ERROR") => ("Some checks errored", ChecksTone::Danger),
        Some("PENDING") => ("Checks are running", ChecksTone::Warning),
        Some("EXPECTED") => ("Checks are queued", ChecksTone::Muted),
        _ => ("Checks status unknown", ChecksTone::Muted),
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
/// The review-state badge for a timeline card header (D4) — tone colors
/// from `review_state_label`.
fn review_badge(state: Option<&str>, cx: &Context<PrDetailPage>) -> Option<AnyElement> {
    let (label, tone) = review_state_label(state);
    let color = match tone {
        ReviewTone::Approved => cx.theme().success,
        ReviewTone::ChangesRequested => cx.theme().danger,
        ReviewTone::Muted => cx.theme().muted_foreground,
    };
    Some(
        Badge::new()
            .xsmall()
            .color(color)
            .child(label)
            .into_any_element(),
    )
}

// Markdown bodies add one parameter to an already wide card seam; the
// arguments stay flat because every call site has them at hand.
#[allow(clippy::too_many_arguments)]
pub(crate) fn body_card(
    id: &str,
    login: &str,
    avatar: Option<&str>,
    age: String,
    body: Option<&str>,
    md: Option<&[markdown::Block]>,
    trailing: Option<AnyElement>,
    // docs/0023 D3 — the chips row under the body (reactions). Cards
    // without reactions pass `None`.
    footer: Option<AnyElement>,
    // docs/0026 D9 — accent ring while this card is the jumped target
    // (the notification hand-off's latest-comment jump). Everything else
    // passes `false`.
    anchored: bool,
    cx: &App,
) -> AnyElement {
    v_flex()
        .id(SharedString::from(id))
        .w_full()
        .rounded_md()
        .border_1()
        .border_color(if anchored {
            cx.theme().accent
        } else {
            cx.theme().border
        })
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
                .children(trailing)
                .child(
                    div()
                        .ml_auto()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(age),
                ),
        )
        .when_some(body, |card, body| {
            // Markdown when the detail payload was parsed (0017); the raw
            // text fallback only fires if the cache somehow missed.
            let body_child = match md {
                Some(blocks) => markdown::render_blocks(blocks, id, cx),
                None => div().child(body.to_string()).into_any_element(),
            };
            card.child(
                div()
                    .px_3()
                    .py_3()
                    .text_sm()
                    .whitespace_normal()
                    .child(body_child),
            )
        })
        // docs/0023 D3 — the chips row sits under the body, inside the
        // card (the 0019 action bar stays in the header row).
        .when_some(footer, |card, footer| {
            card.child(div().px_3().pb_2().text_sm().child(footer))
        })
        .into_any_element()
}

/// docs/0017 D6: parse every timeline body once per detail payload —
/// never per frame. Rebuilt (invalidating) each time a detail lands.
fn build_md_cache(repo: &str, d: &gql::PrDetail) -> BTreeMap<String, Arc<[markdown::Block]>> {
    let mut map = BTreeMap::new();
    // Ambient repo for bare `#123` cross-references (docs/0017 D1
    // extension).
    let md = |body: &str| markdown::parse_markdown_in_repo(body, Some(repo));
    if let Some(body) = &d.body {
        map.insert("pr-description".to_string(), md(body));
    }
    for (ix, comment) in d.comments.nodes.iter().enumerate() {
        map.insert(format!("pr-comment-{ix}"), md(&comment.body));
    }
    for (ix, review) in d.reviews.nodes.iter().enumerate() {
        if let Some(body) = review.body.as_deref().filter(|b| !b.is_empty()) {
            map.insert(format!("pr-review-{ix}"), md(body));
        }
    }
    map
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

/// Centered muted empty-state cell for a tab with nothing to show
/// (D6/D7): not a skeleton, not an error — just no content.
fn empty_cell(text: &str, cx: &App) -> AnyElement {
    div()
        .id("pr-empty")
        .flex_1()
        .min_h_0()
        .flex()
        .items_center()
        .justify_center()
        .text_color(cx.theme().muted_foreground)
        .text_sm()
        .child(text.to_string())
        .into_any_element()
}

/// First 7 SHA characters (D6) — `oid` is hex ASCII; `chars().take`
/// keeps this panic-free on any payload.
fn short_sha(oid: &str) -> String {
    oid.chars().take(7).collect()
}

/// Counts derived from the parsed contexts list (D7) — the summary
/// header and nothing else; the rollup `state` stays on the rail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ChecksCounts {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub pending: usize,
    pub muted: usize,
}

pub(crate) fn checks_counts(nodes: &[gql::PrCheck]) -> ChecksCounts {
    let mut counts = ChecksCounts::default();
    for check in nodes {
        counts.total += 1;
        match check.state {
            gql::CheckState::Success => counts.passed += 1,
            gql::CheckState::Failure => counts.failed += 1,
            gql::CheckState::Pending => counts.pending += 1,
            gql::CheckState::Neutral => counts.muted += 1,
        }
    }
    counts
}

/// The D7 summary line: `"8 checks · 7 passed · 1 failed"` — the total
/// always renders (singular/plural aware); buckets only when non-zero.
pub(crate) fn checks_header_line(counts: &ChecksCounts) -> String {
    let mut line = format!(
        "{} check{}",
        counts.total,
        if counts.total == 1 { "" } else { "s" }
    );
    for (n, word) in [
        (counts.passed, "passed"),
        (counts.failed, "failed"),
        (counts.pending, "running"),
        (counts.muted, "skipped"),
    ] {
        if n > 0 {
            line.push_str(" · ");
            line.push_str(&n.to_string());
            line.push(' ');
            line.push_str(word);
        }
    }
    line
}

/// Unified check state → (icon, theme color) for the D7 row mapping:
/// SUCCESS success / FAILURE danger / running warning / NEUTRAL+SKIPPED
/// muted.
fn check_visual(state: gql::CheckState, cx: &Context<PrDetailPage>) -> (IconName, Hsla) {
    match state {
        gql::CheckState::Success => (IconName::CircleCheck, cx.theme().success),
        gql::CheckState::Failure => (IconName::CircleX, cx.theme().danger),
        gql::CheckState::Pending => (IconName::LoaderCircle, cx.theme().warning),
        gql::CheckState::Neutral => (IconName::Dash, cx.theme().muted_foreground),
    }
}
fn error_cell(message: &str, cx: &App) -> AnyElement {
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

/// A card in edit mode (D4; generalized for the timeline rows by 0019
/// D4): same card chrome, body swapped for the prefilled Textarea +
/// Save (disabled while empty/busy) / Cancel. `id` keys the bridge ids
/// (`{id}-save` / `{id}-cancel`) and doubles as the card id.
pub(crate) fn inline_edit_card(
    id: &str,
    title: &str,
    editor: &Entity<TextareaState>,
    busy: bool,
    on_save: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    on_cancel: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    // 0019 D7: derived here so the call sites can't drift apart from the
    // Save gating.
    let empty = editor.read(cx).value().trim().is_empty();
    v_flex()
        // Test-only probe key (0019 §5); the buttons keep their accesskit
        // ids `{id}-save` / `{id}-cancel`.
        .debug_selector(|| format!("{id}-card"))
        .id(SharedString::from(id.to_string()))
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
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .child(title.to_string()),
                ),
        )
        .child(
            v_flex()
                .gap_2()
                .p_3()
                .child(Textarea::new(editor).w_full())
                .child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new(format!("{id}-cancel"))
                                .ghost()
                                .small()
                                .label("Cancel")
                                .on_click(on_cancel),
                        )
                        .child(
                            Button::new(format!("{id}-save"))
                                .primary()
                                .small()
                                .label("Save")
                                .disabled(busy || empty)
                                .tooltip(if empty {
                                    "The body can't be empty"
                                } else {
                                    "Save"
                                })
                                .on_click(on_save),
                        ),
                ),
        )
        .into_any_element()
}

/// The inline two-step delete confirmation (0019 D4): the question reads
/// as text (the `tree` dump shows what is being confirmed) above
/// Cancel / Delete. `id` keys `{id}-confirm` / `{id}-cancel` and doubles
/// as the card id.
pub(crate) fn delete_confirm_card(
    id: &str,
    question: &str,
    busy: bool,
    on_confirm: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    on_cancel: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    v_flex()
        // Test-only probe key (0019 §5); the buttons keep their accesskit
        // ids `{id}-confirm` / `{id}-cancel`.
        .debug_selector(|| format!("{id}-confirm-card"))
        .id(SharedString::from(id.to_string()))
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
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .child(question.to_string()),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .justify_end()
                .p_3()
                .child(
                    Button::new(format!("{id}-cancel"))
                        .ghost()
                        .small()
                        .label("Cancel")
                        .on_click(on_cancel),
                )
                .child(
                    Button::new(format!("{id}-confirm"))
                        .danger()
                        .small()
                        .label("Delete")
                        .disabled(busy)
                        .tooltip("This cannot be undone")
                        .on_click(on_confirm),
                ),
        )
        .into_any_element()
}

fn muted_centered(text: &str, cx: &App) -> AnyElement {
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

/// #123 / docs/0021 D3 — the shared "+N more on github.com" truncation
/// row. Every capped connection surfaces as this one muted ghost row;
/// `missing` is the caller's honest hidden-row count (disjoint node
/// sets sum their shortfalls). The only affordance is opening the
/// upstream page — in-app pagination stays out of scope (#87 is the
/// same pattern on the repo view).
pub(crate) fn cap_row(id: &'static str, missing: i64, url: &str, cx: &App) -> AnyElement {
    h_flex()
        .debug_selector(move || id.to_string())
        .justify_center()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(
            Button::new(id)
                .ghost()
                .small()
                .label(format!("+{missing} more on github.com"))
                .on_click({
                    let url = url.to_string();
                    move |_, _, cx| cx.open_url(&url)
                }),
        )
        .into_any_element()
}

/// Files tab (D8): summary line + per-file collapsible cards with the
/// parsed diff body. The REST fetch is lazy — `fetch_files` fires on the
/// first Files-tab selection, so opening the page never costs a walk.
fn render_files(
    page_h: &Entity<PrDetailPage>,
    page: &PrDetailPage,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    // Zero UX drift from the pre-0031 shape: an error row (with retry)
    // replaces the pane, including after a failed refresh — D3 keeps the
    // payload in the record, so the next successful land repaints it.
    if let Some(msg) = page.files.load_error() {
        return v_flex()
            .id("pr-files-error")
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_2()
            .text_sm()
            .text_color(cx.theme().danger)
            .child(msg.to_string())
            .child(
                Button::new("pr-files-retry")
                    .ghost()
                    .small()
                    .icon(IconName::RotateCw)
                    .tooltip("Retry")
                    .on_click(cx.listener(|this, _, _, cx| this.fetch_files(cx))),
            )
            .into_any_element();
    }
    if page.files.is_loading() {
        return skeleton_list();
    }
    match page.files.data() {
        Some(files) if files.is_empty() => empty_cell("No file changes", cx),
        Some(files) => files::render_split(page_h, page, files, cx),
        None => skeleton_list(),
    }
}

#[cfg(test)]
mod tests {
    // No `use super::*` — page.rs documents the gpui::* shadowing hazard.
    use super::{ANCHOR_FADE, CommentAnchor};
    use crate::api::PrFile;
    use crate::files::{FilesSummary, files_summary, split_path};
    use crate::gql;
    use crate::gql::MergeMethods;
    use crate::issue_page::reaction_direction;
    use crate::pr_page::{
        BusyOp, ChecksCounts, ChecksTone, ErrorScope, FinishEvent, MergeLineTone, MergeMethod,
        PrDetailPage, PrStateTone, PrTab, ReviewTone, TimelineKind, allowed_methods, checks_counts,
        checks_header_line, comment_action_allowed, composer_body_allowed, default_method,
        merge_enabled, merge_state_line, pr_assignees_arming, pr_labels_arming,
        pr_milestone_arming, pr_reviewers_arming, pr_state_tone, review_batch_allowed,
        review_event, review_state_label, rollup_summary, short_sha, split_repo, timeline_order,
    };
    use crate::service::{GithubCache, GithubState};
    use gpui::AppContext as _;
    use gpui::{Size, px};

    // --- Files sidebar layout probe (0018 §8 follow-up) ---------------
    // The views collapsed to zero height on the live build; this renders
    // the real page in a test window and asserts measured bounds via
    // gpui `debug_bounds` (test-only selectors in `files.rs`).

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

    fn probe_file(name: &str) -> PrFile {
        PrFile {
            filename: name.into(),
            status: "modified".into(),
            additions: 2,
            deletions: 1,
            changes: 3,
            patch: Some("@@ -1 +1 @@\n-a\n+b".into()),
        }
    }

    // --- fetch_detail through the guard (docs/0031 §5, #222 regression) -

    // --- auto_refresh policy (docs/0032 D2) -------------------------------

    /// One page, the whole policy table over real dispatches: stale and
    /// failed lanes re-dispatch, a fresh lane never does, Idle lanes
    /// (threads here) stay untouched, and the in-flight guard swallows a
    /// beat that races a manual refresh. MockHttp records every request;
    /// its script panics on unexpected ones.
    #[gpui::test]
    fn auto_refresh_policy_over_real_dispatches(cx: &mut gpui::TestAppContext) {
        use std::sync::Arc;
        use std::time::Duration;

        use crate::auth::MockTokenStore;
        use crate::http::{Http, SharedToken, mock::MockHttp};
        use crate::service::{GithubCache, GithubState};

        cx.update(dashboard_core::status::init);
        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
        // 1: the eager detail fetch at `new` (fails -> streak 1); 2: the
        // beat's files dispatch (also fails -> streak 1); 3+4: the
        // backdated beat retries both failed lanes (docs/0036 D2).
        let mock = Arc::new(MockHttp::new(vec![
            Err(crate::http::HttpError("probe".into())),
            Err(crate::http::HttpError("probe".into())),
            Err(crate::http::HttpError("probe".into())),
            Err(crate::http::HttpError("probe".into())),
        ]));
        let state: gpui::Entity<GithubState> = cx.update(|cx| {
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            cx.new(|_| {
                GithubState::new(
                    mock.clone() as Arc<dyn Http>,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::new(dashboard_core::paths::AppPaths::init()),
                )
            })
        });
        cx.update(|cx| state.update(cx, |s, _| s.auth = crate::service::AuthStatus::Connected));

        let (page, _vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "policy probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            PrDetailPage::new(state.clone(), "o/r".into(), summary, None, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(mock.recorded().len(), 1, "the eager detail fetch only");
        cx.update(|cx| {
            page.update(cx, |p, _| {
                p.files = Fetch::ready(vec![probe_file("src/a.rs")])
            })
        });

        // Beat at zero threshold: the detail lane's last attempt is
        // fresh — its first retry waits out the backoff (docs/0036 D2);
        // files (stale enough, never failed) dispatch; threads (Idle —
        // never opened) must not.
        cx.update(|cx| page.update(cx, |p, cx| p.auto_refresh(false, Some(Duration::ZERO), cx)));
        cx.run_until_parked();
        assert_eq!(
            mock.recorded().len(),
            2,
            "failed lane backoffs, stale dispatch, Idle skips"
        );

        // Beat at an hour threshold with both lanes freshly failed
        // (the files dispatch just failed too): both are in backoff —
        // the beat is quiet.
        cx.update(|cx| {
            page.update(cx, |p, cx| {
                p.auto_refresh(false, Some(Duration::from_secs(3600)), cx)
            })
        });
        cx.run_until_parked();
        assert_eq!(mock.recorded().len(), 2, "freshly failed lanes stay quiet");

        // The clocks pass the streak-1 delay: both failed lanes retry —
        // failed lanes still self-heal at any age (docs/0032 D2), now
        // paced by the backoff (docs/0036 D2).
        cx.update(|cx| {
            page.update(cx, |p, _| {
                let back = Some(std::time::Instant::now() - Duration::from_secs(61));
                p.detail.attempted_at = back;
                p.files.attempted_at = back;
            });
            page.update(cx, |p, cx| {
                p.auto_refresh(false, Some(Duration::from_secs(3600)), cx)
            });
        });
        cx.run_until_parked();
        assert_eq!(mock.recorded().len(), 4, "backoff elapsed: lanes rejoin");

        // A beat racing a manual refresh: with every other lane fresh,
        // arm the detail key as if a dispatch were in flight — the beat
        // must swallow it and dispatch nothing.
        cx.update(|cx| {
            page.update(cx, |p, _| {
                p.files = Fetch::ready(vec![probe_file("src/a.rs")])
            });
            let detail = (crate::service::Kind::PrDetail, Some("o/r#7".to_string()));
            state.update(cx, |s, _| {
                s.in_flight_arm(detail);
            });
            page.update(cx, |p, cx| {
                p.auto_refresh(false, Some(Duration::from_secs(3600)), cx)
            });
        });
        cx.run_until_parked();
        assert_eq!(mock.recorded().len(), 4, "the guard ate the racing beat");
    }

    /// A page-owned fetch arms the guard synchronously, dispatches on the
    /// raw lane, and clears the guard when the run lands. The regression
    /// this pins: dispatching through `run_read_deduped` made its re-arm
    /// see the holder's key (`insert` → false), skip, and orphan the
    /// record in-flight forever. ProbeHttp always errors, so a LANDED
    /// error proves the apply path ran end-to-end.
    /// docs/0033 D2: no confirmed sync, no label; a landed record shows
    /// one. ProbeHttp never lands, so the label is driven by the record
    /// alone — absence first, then presence once a Ready record is set.
    #[gpui::test]
    fn synced_indicator_follows_the_record(cx: &mut gpui::TestAppContext) {
        cx.update(dashboard_core::status::init);
        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
        let state: gpui::Entity<GithubState> = cx.update(|cx| {
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
        cx.update(|cx| state.update(cx, |s, _| s.auth = crate::service::AuthStatus::Connected));

        let (page, vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "synced probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            PrDetailPage::new(state, "o/r".into(), summary, None, window, cx)
        });
        vcx.run_until_parked();
        assert!(
            vcx.debug_bounds("pr-synced").is_none(),
            "a never-landed record renders no label"
        );
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.detail = Fetch::ready(probe_detail());
                cx.notify();
            });
        });
        let _ = vcx.debug_bounds("pr-synced"); // flush frames
        assert!(
            vcx.debug_bounds("pr-synced").is_some(),
            "a landed record renders the label"
        );
    }

    #[gpui::test]
    fn fetch_detail_lands_through_the_guard(cx: &mut gpui::TestAppContext) {
        cx.update(dashboard_core::status::init);
        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
        let state: gpui::Entity<GithubState> = cx.update(|cx| {
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
        // fetch_detail refuses to dispatch while disconnected — connect
        // the probe state (the real flow connects before pages exist).
        cx.update(|cx| state.update(cx, |s, _| s.auth = crate::service::AuthStatus::Connected));

        let (page, _vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "guard probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            // `new` dispatches the first fetch — the transport errors.
            PrDetailPage::new(state.clone(), "o/r".into(), summary, None, window, cx)
        });

        cx.run_until_parked();
        let (landed, clear) = cx.update(|cx| {
            (
                page.read(cx).detail.load_error().is_some(),
                state.read(cx).in_flight.is_empty(),
            )
        });
        assert!(
            landed,
            "the fetch must land (here: as the scripted transport error) — \
             a stuck in-flight record means the guard swallowed the dispatch"
        );
        assert!(clear, "the guard clears when the run lands");

        // A second dispatch (the refresh click) runs the same way.
        cx.update(|cx| page.update(cx, |page, cx| page.fetch_detail(cx)));
        cx.run_until_parked();
        let (landed, clear) = cx.update(|cx| {
            (
                page.read(cx).detail.load_error().is_some(),
                state.read(cx).in_flight.is_empty(),
            )
        });
        assert!(landed);
        assert!(clear);
    }

    #[gpui::test]
    fn files_split_has_real_layout(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
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
        let (_page, vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "layout probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.tab = PrTab::Files;
            page.files = Fetch::ready(vec![
                probe_file("src/main.rs"),
                probe_file("src/lib.rs"),
                probe_file("docs/readme.md"),
            ]);
            page.apply_files_payload(cx);
            page
        });
        let sidebar = vcx.debug_bounds("files-sidebar");
        let view = vcx.debug_bounds("files-view");
        for sel in [
            "files-split",
            "files-sidebar",
            "files-view",
            "files-summary-wrap",
            "files-select-wrap",
            "files-filter-wrap",
        ] {
            eprintln!("[probe] {sel}={:?}", vcx.debug_bounds(sel));
        }
        let small = |b: Option<gpui::Bounds<gpui::Pixels>>| {
            b.map(|b| b.size.height < gpui::px(100.)).unwrap_or(true)
        };
        assert!(
            !small(sidebar),
            "sidebar has no height — layout collapsed: {sidebar:?}"
        );
        assert!(
            !small(view),
            "view area has no height — layout collapsed: {view:?}"
        );
        let (s, v) = (sidebar.unwrap(), view.unwrap());
        assert!(
            v.size.height < s.size.height,
            "view must sit inside the sidebar, not equal it"
        );
    }

    // --- review threads on the diff (docs/0030 §5) ----------------------

    /// The threads probe's fixture thread.
    fn probe_thread(ix: usize, line: Option<i64>, outdated: bool, body: &str) -> gql::PrThread {
        gql::PrThread {
            id: format!("T_{ix}"),
            path: "src/main.rs".into(),
            line,
            start_line: None,
            side: gql::DiffSide::Right,
            subject: gql::ThreadSubject::Line,
            is_outdated: outdated,
            is_resolved: false,
            resolved_by: None,
            viewer_can_reply: false,
            viewer_can_resolve: false,
            viewer_can_unresolve: false,
            comments: vec![gql::PrThreadComment {
                id: format!("T_{ix}_C_0"),
                author: Some(gql::Author {
                    login: "reviewer".into(),
                    avatar_url: String::new(),
                }),
                body: body.into(),
                created_at: Some("2026-09-10T00:00:00Z".into()),
                updated_at: None,
                last_edited_at: None,
                url: Some(format!("https://github.com/o/r/pull/7#_{ix}")),
                viewer_can_update: false,
                viewer_can_delete: false,
                viewer_did_author: false,
            }],
        }
    }

    #[gpui::test]
    fn virtualized_diff_builds_viewport_rows_per_frame(cx: &mut gpui::TestAppContext) {
        // docs/0041 \u{a7}6 perf probe: scrolling a large diff must build
        // ~viewport rows per frame, independent of file size (the old
        // div stack built every row every frame).
        use crate::files::DIFF_ROWS_BUILT;
        use std::sync::atomic::Ordering;

        cx.update(gpui_component::init);
        cx.update(|cx| cx.background_executor().allow_parking());
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
        let (page, vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "perf probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.tab = PrTab::Files;
            let body: String = (0..1000)
                .map(|i| format!("+    let value_{i} = compute_thing({i}, axis, stride);\n"))
                .collect();
            page.files = Fetch::ready(vec![PrFile {
                filename: "src/generated.rs".into(),
                status: "modified".into(),
                additions: 1000,
                deletions: 0,
                changes: 1000,
                patch: Some(format!("@@ -1,2 +1,1001 @@\n{body}")),
            }]);
            page.apply_files_payload(cx);
            page
        });
        assert_eq!(
            vcx.update(|_, cx| page.read_with(cx, |p, _| p.diff_items.len())),
            1001
        );
        DIFF_ROWS_BUILT.store(0, Ordering::Relaxed);
        // Ten mid-file scroll positions; each triggers a full re-render.
        for ix in [100usize, 200, 300, 400, 500, 600, 700, 800, 900, 500] {
            vcx.update(|_, cx| {
                page.update(cx, |p, cx| {
                    p.diff_list.scroll_to(gpui::ListOffset {
                        item_ix: ix,
                        offset_in_item: px(0.),
                    });
                    cx.notify();
                });
            });
            vcx.run_until_parked();
        }
        let built = DIFF_ROWS_BUILT.load(Ordering::Relaxed);
        assert!(
            built < 10 * 200,
            "expected ~viewport rows per frame, built {built} over 10 frames"
        );
    }

    #[gpui::test]
    fn diff_items_flatten_in_0030_order(cx: &mut gpui::TestAppContext) {
        // docs/0041 §6: the flattened virtualized items follow the exact
        // order the pre-list pane rendered — file-level threads, then per
        // row [row, its thread cards, composer, drafts], then the fetch
        // row and the stray bucket.
        use crate::files::DiffItem;
        cx.update(gpui_component::init);
        cx.update(|cx| cx.background_executor().allow_parking());
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
        let (page, vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "flatten probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.tab = PrTab::Files;
            // The hunk header plus two added rows (+a at new line 1, +b
            // at new line 2); one thread anchored at line 1, one
            // file-level (no line), one stray (beyond the file).
            page.files = Fetch::ready(vec![PrFile {
                filename: "src/main.rs".into(),
                status: "modified".into(),
                additions: 2,
                deletions: 0,
                changes: 2,
                patch: Some("@@ -1,2 +1,2 @@\n+a\n+b".into()),
            }]);
            let mut file_level = probe_thread(1, None, false, "file-level");
            file_level.subject = gql::ThreadSubject::File;
            page.threads = Fetch::ready(gql::PrThreads {
                total_count: 3,
                nodes: vec![
                    probe_thread(0, Some(1), false, "anchored"),
                    file_level,
                    probe_thread(2, Some(999), true, "stray"),
                ],
            });
            page.extend_threads_md();
            page.apply_files_payload(cx);
            page
        });
        vcx.update(|_, cx| {
            page.update(cx, |p, _| {
                p.sync_diff_list();
                assert_eq!(
                    p.diff_items.as_ref(),
                    &vec![
                        DiffItem::FileThread(1),
                        DiffItem::Row(0),
                        DiffItem::Row(1),
                        DiffItem::ThreadCard(0),
                        DiffItem::Row(2),
                        DiffItem::StrayBucket,
                    ],
                    "flatten must mirror the 0030 order"
                );
                assert_eq!(p.diff_list.item_count(), 6, "list must track the items");
            });
        });
    }

    #[gpui::test]
    fn composer_and_drafts_splice_the_item_list(cx: &mut gpui::TestAppContext) {
        // docs/0041 §6: opening the composer adds an item, a queued draft
        // adds one and removes the composer, cancel closes both — and the
        // list's item count tracks through the page's own methods.
        use crate::files::DiffItem;
        use gql::DiffSide;
        cx.update(gpui_component::init);
        cx.update(|cx| cx.background_executor().allow_parking());
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
        let (page, vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "splice probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.tab = PrTab::Files;
            page.files = Fetch::ready(vec![probe_file("src/main.rs")]);
            page.apply_files_payload(cx);
            page
        });
        // Open the composer on the +b row (new-side line 1).
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| {
                p.open_line_composer("src/main.rs".into(), DiffSide::Right, 1, window, cx);
                p.sync_diff_list();
            });
        });
        vcx.update(|_, cx| {
            page.update(cx, |p, _| {
                assert!(p.diff_items.contains(&DiffItem::Composer(2)));
                assert_eq!(
                    p.diff_list.item_count(),
                    p.diff_items.len(),
                    "list count must track the items after the splice"
                );
            });
        });
        // Cancel: the composer item disappears on the next sync.
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| {
                p.cancel_line_composer(window, cx);
                p.sync_diff_list();
                assert!(
                    !p.diff_items
                        .iter()
                        .any(|it| matches!(it, DiffItem::Composer(_)))
                );
            });
        });
    }

    #[gpui::test]
    fn threads_render_in_the_files_tab(cx: &mut gpui::TestAppContext) {
        // docs/0030 §5: the Files tab renders anchored thread cards under
        // their diff rows, suggestion bodies as cards, and keeps outdated
        // threads in the collapsed end-of-file bucket until expanded.
        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
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
        let (page, vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "threads probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.tab = PrTab::Files;
            // probe_file's patch anchors +b at new-side line 1.
            page.files = Fetch::ready(vec![probe_file("src/main.rs")]);
            page.threads = Fetch::ready(gql::PrThreads {
                total_count: 2,
                nodes: vec![
                    probe_thread(0, Some(1), false, "looks fine"),
                    // Outdated → the end-of-file bucket, collapsed.
                    probe_thread(1, Some(999), true, "stale comment"),
                ],
            });
            page.extend_threads_md();
            page.apply_files_payload(cx);
            page
        });
        // button; the suggestion body parses into the md cache.
        assert!(
            vcx.debug_bounds("pr-thread-0-card").is_some(),
            "anchored thread card missing"
        );
        let card = vcx.debug_bounds("pr-thread-0-card").unwrap();
        let diff = vcx.debug_bounds("pr-diff-scroll-key").unwrap();
        assert!(
            diff.contains(&card.origin),
            "thread card must sit inside the diff pane"
        );
        // The outdated thread stays collapsed until expanded.
        assert!(
            vcx.debug_bounds("pr-outdated-toggle-key").is_some(),
            "bucket toggle missing"
        );
        assert_eq!(
            vcx.debug_bounds("pr-thread-1-card"),
            None,
            "stray card must not render while collapsed"
        );
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                // The expand path the toggle drives, applied directly.
                p.outdated_open.insert("src/main.rs".into(), true);
                cx.notify();
            });
        });
        assert!(
            vcx.debug_bounds("pr-thread-1-card").is_some(),
            "stray card missing after expand"
        );
    }

    #[gpui::test]
    fn suggestion_bodies_render_as_cards(cx: &mut gpui::TestAppContext) {
        // docs/0030 D6: a ```suggestion fence in a thread comment renders
        // as the suggestion card, not a plain code block.
        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
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
        let (_page, vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "suggestion probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.tab = PrTab::Files;
            page.files = Fetch::ready(vec![probe_file("src/main.rs")]);
            page.threads = Fetch::ready(gql::PrThreads {
                total_count: 1,
                nodes: vec![probe_thread(
                    0,
                    Some(1),
                    false,
                    // The fence is block 0 — the assert keys ride that.
                    "```suggestion\nfn fixed() {}\n```",
                )],
            });
            page.extend_threads_md();
            page.apply_files_payload(cx);
            page
        });
        assert!(
            vcx.debug_bounds("pr-suggestion-0-0-0-card").is_some(),
            "suggestion card missing"
        );
    }

    // docs/0043 §7 — the rail metadata panels: display states render,
    // Edit swaps the panel to the checklist card, the one-editor rule
    // holds across pickers, busy blocks arming, Esc cancels topmost,
    // and a team request keeps the reviewers panel display-only (D3).
    #[gpui::test]
    fn pr_metadata_panels_swap_and_follow_one_editor(cx: &mut gpui::TestAppContext) {
        cx.update(dashboard_core::status::init);
        cx.update(gpui_component::init);
        cx.update(|cx| cx.background_executor().allow_parking());
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
        let (page, vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "layout probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            let mut d = probe_detail();
            // Arm the label picker: the fixture's second label is the
            // degraded node — give it an id so the tables below toggle
            // one condition at a time.
            d.labels[1].id = Some("L2".into());
            page.detail = Fetch::ready(d);
            page.md = build_md_cache(&page.repo, page.detail.data().unwrap());
            page
        });
        // Display states: the milestone cell renders (armed + applied);
        // the reviewers Edit renders (fixture permission + clean refs).
        assert!(
            vcx.debug_bounds("pr-milestone").is_some(),
            "milestone display cell missing"
        );
        assert!(
            vcx.debug_bounds("pr-reviewers-edit").is_some(),
            "reviewers Edit affordance missing"
        );

        // Edit swaps the panel to the checklist card (open-immediately
        // draft, 0022 §11 — the source fetch is still loading).
        vcx.update(|_, cx| page.update(cx, |p, cx| p.start_labels_picker(cx)));
        assert!(
            vcx.debug_bounds("pr-labels-card").is_some(),
            "labels card missing after Edit"
        );
        assert!(
            vcx.debug_bounds("pr-labels-row-0").is_some(),
            "checklist row missing"
        );

        // One editor at a time (D7, the 0022 model): a second picker
        // cannot open while one is up — its Edit is hidden and the
        // start refuses.
        vcx.update(|_, cx| page.update(cx, |p, cx| p.start_assignees_picker(cx)));
        assert!(
            vcx.debug_bounds("pr-assignees-card").is_none(),
            "a second picker opened over the first"
        );
        assert!(vcx.debug_bounds("pr-labels-card").is_some());
        vcx.update(|_, cx| page.update(cx, |p, cx| p.cancel_meta_editor(cx)));
        vcx.update(|_, cx| page.update(cx, |p, cx| p.start_assignees_picker(cx)));
        assert!(
            vcx.debug_bounds("pr-assignees-card").is_some(),
            "assignees card missing after cancel + Edit"
        );

        // Busy blocks arming (D7).
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.cancel_meta_editor(cx);
                p.busy = Some(BusyOp::Merge);
                p.start_labels_picker(cx);
            });
        });
        assert!(
            vcx.debug_bounds("pr-labels-card").is_none(),
            "busy opened a picker"
        );
        vcx.update(|_, cx| {
            page.update(cx, |p, _cx| {
                p.busy = None;
            });
        });

        // Esc cancels the topmost surface and only then would close the
        // page (D7) — drive the same handler the action binds.
        vcx.update(|_, cx| page.update(cx, |p, cx| p.start_milestone_picker(cx)));
        assert!(vcx.debug_bounds("pr-milestones-card").is_some());
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| {
                assert!(
                    p.cancel_top_editor(window, cx),
                    "Esc consumed by the picker"
                );
            });
        });
        assert!(vcx.debug_bounds("pr-milestones-card").is_none());

        // D3's reviewer clause: a team request forces display-only —
        // the Edit affordance disappears and the picker cannot open.
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                let mut d = p.detail.data().unwrap().clone();
                d.review_requests.push(gql::PrReviewRequest {
                    id: None,
                    login: String::new(),
                });
                p.detail = Fetch::ready(d);
                cx.notify();
            });
        });
        assert!(
            vcx.debug_bounds("pr-reviewers-edit").is_none(),
            "team request must hide the reviewers Edit"
        );
        vcx.update(|_, cx| page.update(cx, |p, cx| p.start_reviewers_picker(cx)));
        assert!(
            vcx.debug_bounds("pr-reviewers-card").is_none(),
            "display-only panel must not open"
        );
    }

    // --- timeline-row edit/delete (docs/0019 §5) ------------------------

    use crate::pr_page::{
        Fetch, ResolutionReason, build_md_cache, delete_row, edit_row, node_kind, normalize_range,
        row_is_edited, start_fields,
    };
    use crate::service::{Kind, Mutation, SpecResult};

    /// docs/0026 D9: the anchor variant — the notification hand-off's jump
    /// opens the page with `Some(rest_comment_id)`; the detail fixture rides
    /// in directly, as the real fetch would.
    fn probe_pr_anchored(
        cx: &mut gpui::TestAppContext,
        anchor: Option<i64>,
    ) -> (gpui::Entity<PrDetailPage>, &mut gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
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
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "layout probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, anchor, window, cx);
            page.detail = Fetch::ready(probe_detail());
            page.md = build_md_cache(&page.repo, page.detail.data().unwrap());
            page
        })
    }

    #[gpui::test]
    fn anchor_jump_lands_on_and_highlights_the_comment(cx: &mut gpui::TestAppContext) {
        // docs/0026 D9, PR-page half: the jump scrolls the conversation to
        // the matched comment (987654 = IC_1), highlights it, and fades.
        let (page, vcx) = probe_pr_anchored(cx, Some(987_654));
        vcx.simulate_resize(Size {
            width: px(1024.),
            height: px(300.),
        });
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.resolve_comment_anchor();
                cx.notify();
            });
        });
        vcx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        assert_eq!(
            vcx.read(|cx| page.read(cx).anchor.clone()),
            CommentAnchor::Highlighted("IC_1".into()),
        );
        vcx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let offset = vcx.read(|cx| page.read(cx).timeline_scroll.offset());
        assert!(
            offset.y < px(0.),
            "conversation must scroll to the comment, offset = {offset:?}"
        );
        cx.executor().advance_clock(ANCHOR_FADE);
        cx.run_until_parked();
        assert_eq!(
            cx.read(|cx| page.read(cx).anchor.clone()),
            CommentAnchor::Idle,
        );
    }

    #[gpui::test]
    fn anchor_miss_gives_up_quietly(cx: &mut gpui::TestAppContext) {
        // docs/0026 D9, PR-page half: a comment id outside the loaded
        // conversation — no jump, no error, the page opens unanchored.
        let (page, vcx) = probe_pr_anchored(cx, Some(111_222));
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.resolve_comment_anchor();
                cx.notify();
            });
        });
        assert_eq!(
            vcx.read(|cx| page.read(cx).anchor.clone()),
            CommentAnchor::Idle,
            "a miss gives up without arming anything"
        );
        assert!(vcx.debug_bounds("pr-comment-0-actions").is_some());
    }

    fn probe_detail() -> gql::PrDetail {
        // Two gated rows (one per kind), two unflagged rows — the probe
        // asserts the affordances follow the flags exactly (0019 D3).
        // pr_detail_from_value takes the repository wrapper node.
        let value = serde_json::json!({
            // docs/0043 D4: the repository wrapper node carries the
            // viewer permission the substituted gates read.
            "viewerPermission": "ADMIN",
            "pullRequest": {
            "id": "PR_2", "number": 7, "title": "layout probe", "url": "u",
            "state": "OPEN", "isDraft": false,
            "createdAt": "2026-09-01T00:00:00Z",
            "viewerCanReact": true,
            "viewerCanLabel": true,
            "viewerCanAssign": true,
            "labels": {"totalCount": 2, "nodes": [
                {"id": "L1", "name": "bug", "color": "ff0000"},
                {"name": "no-id"}
            ]},
            "assignees": {"totalCount": 1, "nodes": [
                {"id": "U1", "login": "bob", "avatarUrl": "a:bob"}
            ]},
            "milestone": {"id": "M1", "title": "v1", "state": "OPEN"},
            "reviewRequests": {"totalCount": 1, "nodes": [
                {"requestedReviewer": {"__typename": "User", "id": "U2", "login": "carol"}}
            ]},
            "reactionGroups": [
                {"content": "THUMBS_UP", "viewerHasReacted": true,
                 "reactors": {"totalCount": 3}},
                {"content": "EYES", "viewerHasReacted": false,
                 "reactors": {"totalCount": 0}}
            ],
            "reviews": {"totalCount": 2, "nodes": [
                {"id": "PRR_1", "viewerCanUpdate": true, "viewerCanDelete": true,
                 "author": {"login": "bob"}, "state": "APPROVED", "body": "lgtm",
                 "submittedAt": "2026-09-01T01:00:00Z",
                 "viewerCanReact": true,
                 "reactionGroups": [
                     {"content": "HEART", "viewerHasReacted": false,
                      "reactors": {"totalCount": 2}}
                 ]},
                {"author": {"login": "carol"}, "state": "COMMENTED", "body": "note",
                 "submittedAt": "2026-09-02T01:00:00Z"}
            ]},
            "comments": {"totalCount": 2, "nodes": [
                {"id": "IC_1", "databaseId": 987654,
                 "viewerCanUpdate": true, "viewerCanDelete": false,
                 "updatedAt": "2026-09-03T02:00:00Z",
                 "author": {"login": "dave"}, "body": "nice",
                 "createdAt": "2026-09-03T01:00:00Z",
                 "viewerCanReact": true,
                 "reactionGroups": [
                     {"content": "HEART", "viewerHasReacted": true,
                      "reactors": {"totalCount": 1}}
                 ]},
                {"author": {"login": "erin"}, "body": "plain",
                 "createdAt": "2026-09-04T01:00:00Z"}
            ]}
        }});
        gql::pr_detail_from_value(&value).expect("probe detail parses")
    }

    #[test]
    fn row_write_helpers_table() {
        let mut c = comment("dave", "2026-09-03T01:00:00Z");
        c.id = "IC_1".into();
        let mut r = review("bob", "2026-09-01T01:00:00Z", "APPROVED");
        r.id = "PRR_1".into();
        let comments = vec![c];
        let reviews = vec![r];
        assert_eq!(
            node_kind(&comments, &reviews, "IC_1"),
            Some(TimelineKind::Comment)
        );
        assert_eq!(
            node_kind(&comments, &reviews, "PRR_1"),
            Some(TimelineKind::Review)
        );
        assert_eq!(node_kind(&comments, &reviews, "gone"), None);

        let (op, m, vars) = edit_row(TimelineKind::Comment, "IC_1", "edited");
        assert_eq!(op, BusyOp::CommentEdit);
        assert_eq!(m, Mutation::UpdateComment);
        assert_eq!(vars["input"]["id"], "IC_1");
        assert_eq!(vars["input"]["body"], "edited");
        let (op, m, vars) = edit_row(TimelineKind::Review, "PRR_1", "revised");
        assert_eq!(op, BusyOp::ReviewEdit);
        assert_eq!(m, Mutation::UpdateReview);
        assert_eq!(vars["input"]["pullRequestReviewId"], "PRR_1");

        let (op, m, vars) = delete_row(TimelineKind::Comment, "IC_1");
        assert_eq!(op, BusyOp::CommentDelete);
        assert_eq!(m, Mutation::DeleteComment);
        assert_eq!(vars["input"]["id"], "IC_1");
        assert!(
            vars["input"].get("body").is_none(),
            "delete carries no body"
        );
        let (op, m, vars) = delete_row(TimelineKind::Review, "PRR_1");
        assert_eq!(op, BusyOp::ReviewDelete);
        assert_eq!(m, Mutation::DeleteReview);
        assert_eq!(vars["input"]["pullRequestReviewId"], "PRR_1");
    }

    // docs/0043 D3/D4 — the arming tables: flags gate labels/assignees;
    // milestone substitutes the permission; reviewers take permission
    // or author, and any degraded request (team/bot/id-less) or a
    // truncated connection forces display-only.
    fn gate_detail() -> gql::PrDetail {
        let mut d = probe_detail();
        // The fixture's second label is the degraded node — arm it so
        // the tables can toggle one condition at a time.
        d.labels[1].id = Some("L2".into());
        d.viewer_can_label = true;
        d.viewer_can_assign = true;
        d.viewer_permission = Some("WRITE".into());
        d.id = "PR_1".into();
        d
    }

    #[test]
    fn pr_meta_gates_table() {
        let mut d = gate_detail();
        assert!(pr_labels_arming(&d));
        d.viewer_can_label = false;
        assert!(!pr_labels_arming(&d), "flag off hides the label Edit");
        d.viewer_can_label = true;
        d.labels[0].id = None;
        assert!(!pr_labels_arming(&d), "degraded node hides the Edit");
        d.labels[0].id = Some("L1".into());
        d.labels_total = 5;
        assert!(!pr_labels_arming(&d), "truncated connection hides the Edit");

        let mut d = gate_detail();
        assert!(pr_assignees_arming(&d));
        d.viewer_can_assign = false;
        assert!(!pr_assignees_arming(&d));

        let mut d = gate_detail();
        assert!(pr_milestone_arming(&d), "triage+ arms milestone");
        d.viewer_permission = Some("READ".into());
        assert!(!pr_milestone_arming(&d), "read-only sees no Edit");
        d.viewer_permission = None;
        assert!(!pr_milestone_arming(&d), "no mapping means no rights");
        d.viewer_permission = Some("TRIAGE".into());
        d.id = String::new();
        assert!(!pr_milestone_arming(&d), "no write target, no Edit");

        let mut d = gate_detail();
        assert!(pr_reviewers_arming(&d, None), "triage+ arms reviewers");
        d.viewer_permission = Some("READ".into());
        assert!(!pr_reviewers_arming(&d, None), "read-only, not author");
        d.author = Some(gql::Author {
            login: "pat".into(),
            avatar_url: String::new(),
        });
        assert!(
            pr_reviewers_arming(&d, Some("pat")),
            "author arms reviewers without permission"
        );
        assert!(
            !pr_reviewers_arming(&d, Some("carol")),
            "another user's login does not"
        );
        d.author = None;
        assert!(!pr_reviewers_arming(&d, Some("carol")));
        let mut d = gate_detail();
        d.review_requests.push(gql::PrReviewRequest {
            id: None,
            login: String::new(),
        });
        assert!(
            !pr_reviewers_arming(&d, None),
            "a team/bot request forces display-only (D3)"
        );
        let mut d = gate_detail();
        d.review_requests_total = 9;
        assert!(
            !pr_reviewers_arming(&d, None),
            "truncated requests hide Edit"
        );
    }

    // docs/0043 §7 — each op posts exactly its one field-group on
    // `updatePullRequest`; reviewers ride `requestReviews` with
    // `union: true` and the complete replacement set (empty clears).
    #[test]
    fn pr_meta_payloads_post_one_field_group() {
        use BusyOp::*;
        let (m, v) = PrDetailPage::pr_meta_mutation(PrSetLabels, "PR_1", &["L1".into()], None);
        assert_eq!(m, Mutation::PrSetLabels);
        assert_eq!(v["input"]["pullRequestId"], "PR_1");
        assert_eq!(v["input"]["labelIds"], serde_json::json!(["L1"]));
        assert!(v["input"].get("assigneeIds").is_none());
        assert!(v["input"].get("milestoneId").is_none());

        let (m, v) = PrDetailPage::pr_meta_mutation(
            PrSetAssignees,
            "PR_1",
            &["U1".into(), "U2".into()],
            None,
        );
        assert_eq!(m, Mutation::PrSetAssignees);
        assert_eq!(v["input"]["assigneeIds"], serde_json::json!(["U1", "U2"]));
        assert!(v["input"].get("labelIds").is_none());

        let (m, v) = PrDetailPage::pr_meta_mutation(PrSetMilestone, "PR_1", &[], Some("M1"));
        assert_eq!(m, Mutation::PrSetMilestone);
        assert_eq!(v["input"]["milestoneId"], "M1");
        let (m, v) = PrDetailPage::pr_meta_mutation(PrSetMilestone, "PR_1", &[], None);
        assert_eq!(m, Mutation::PrSetMilestone);
        assert!(v["input"]["milestoneId"].is_null(), "null clears");

        let (m, v) = PrDetailPage::pr_meta_mutation(PrSetReviewers, "PR_1", &["U2".into()], None);
        assert_eq!(m, Mutation::PrSetReviewers);
        assert_eq!(v["input"]["userIds"], serde_json::json!(["U2"]));
        assert_eq!(v["input"]["union"], true, "replace, not add");
        let (_, v) = PrDetailPage::pr_meta_mutation(PrSetReviewers, "PR_1", &[], None);
        assert_eq!(
            v["input"]["userIds"],
            serde_json::json!([]),
            "empty set clears"
        );
        assert_eq!(v["input"]["union"], true);
    }

    #[test]
    fn row_is_edited_table() {
        assert!(row_is_edited(
            Some("2026-09-01T00:00:00Z"),
            Some("2026-09-02T00:00:00Z")
        ));
        assert!(!row_is_edited(
            Some("2026-09-02T00:00:00Z"),
            Some("2026-09-01T00:00:00Z")
        ));
        assert!(
            !row_is_edited(Some("2026-09-01T00:00:00Z"), Some("2026-09-01T00:00:00Z")),
            "equal stamps are not an edit"
        );
        assert!(!row_is_edited(None, Some("2026-09-02T00:00:00Z")));
        assert!(!row_is_edited(Some("2026-09-01T00:00:00Z"), None));
    }

    #[gpui::test]
    fn row_affordances_follow_viewer_flags(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
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
        let (page, vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "layout probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.detail = Fetch::ready(probe_detail());
            page.md = build_md_cache(&page.repo, page.detail.data().unwrap());
            page
        });
        for sel in [
            "pr-conversation",
            "pr-review-0-actions",
            "pr-review-1-actions",
            "pr-comment-0-actions",
            "pr-comment-1-actions",
        ] {
            eprintln!("[probe] {sel}={:?}", vcx.debug_bounds(sel));
        }
        // Gated rows carry their affordances; unflagged rows and the
        // can-delete=false comment stay clean (0019 D3).
        assert!(vcx.debug_bounds("pr-review-0-actions").is_some());
        assert!(vcx.debug_bounds("pr-review-1-actions").is_none());
        assert!(vcx.debug_bounds("pr-comment-0-actions").is_some());
        assert!(vcx.debug_bounds("pr-comment-1-actions").is_none());

        // Edit mode swaps the body for the editor card (0019 D4).
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.start_edit_row("IC_1".into(), "nice".into(), window, cx)
            });
        });
        assert!(vcx.debug_bounds("pr-comment-0-card").is_some());
        assert!(vcx.debug_bounds("pr-comment-0-actions").is_none());

        // One editor at a time: the description editor yields the row
        // editor (0019 D4).
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| page.start_edit_description(window, cx));
        });
        assert!(vcx.debug_bounds("pr-comment-0-card").is_none());
        assert!(vcx.debug_bounds("pr-description-card").is_some());

        // The delete confirm yields every editor and swaps the row
        // (0019 D4); the confirm copy question rides the card header.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.cancel_edit_description(window, cx);
                page.start_delete_confirm("PRR_1".into(), cx);
            });
        });
        assert!(vcx.debug_bounds("pr-review-0-confirm-card").is_some());
        assert!(vcx.debug_bounds("pr-review-0-actions").is_none());
        assert!(vcx.debug_bounds("pr-description-card").is_none());

        // Guards (0019 D7): busy blocks arming; an empty node id opens
        // no editor.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.cancel_delete_confirm(cx);
                page.busy = Some(BusyOp::Comment);
                page.start_delete_confirm("PRR_1".into(), cx);
                assert!(
                    page.delete_confirm.is_none(),
                    "busy must block arming the confirm"
                );
                page.busy = None;
                page.start_delete_confirm("PRR_1".into(), cx);
                assert_eq!(page.delete_confirm.as_deref(), Some("PRR_1"));
                page.start_edit_row("".into(), String::new(), window, cx);
                assert!(
                    page.timeline_edit.is_none(),
                    "empty node id must not open an editor"
                );
                page.timeline_edit = None;
            });
        });
    }

    /// docs/0023 D3 ported to the PR surfaces (#196): the body, review
    /// summaries and issue comments render chips rows; chip clicks are
    /// single-flight; the picker direction rides the pure helper.
    #[gpui::test]
    fn reaction_rows_toggle_and_single_flight(cx: &mut gpui::TestAppContext) {
        cx.update(dashboard_core::status::init);
        // The toggle really spawns (ProbeHttp fails it) — the in-flight
        // tracker global must exist, same contract as main().
        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
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
        let (page, vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "layout probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.detail = Fetch::ready(probe_detail());
            page.md = build_md_cache(&page.repo, page.detail.data().unwrap());
            page
        });
        // The body row renders: the armed chip plus the + affordance; the
        // zero-count EYES group renders no chip (D3).
        assert!(vcx.debug_bounds("pr-reaction-row-THUMBS_UP").is_some());
        assert!(vcx.debug_bounds("pr-reaction-row-EYES").is_none());
        assert!(vcx.debug_bounds("pr-reaction-row-add").is_some());
        // Comment 0 and review 0 carry their own rows; comment 1 (no
        // fields) and review 1 stay clean.
        assert!(
            vcx.debug_bounds("pr-comment-0-reaction-row-HEART")
                .is_some()
        );
        assert!(vcx.debug_bounds("pr-comment-1-reaction-row-add").is_none());
        assert!(vcx.debug_bounds("pr-review-0-reaction-row-HEART").is_some());
        assert!(vcx.debug_bounds("pr-review-1-reaction-row-add").is_none());

        // Chip click: armed → remove is the direction; single-flight holds.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.toggle_reaction(
                    "PR_2".into(),
                    true,
                    gql::ReactionContent::ThumbsUp,
                    window,
                    cx,
                );
                assert_eq!(page.busy, Some(BusyOp::Reaction));
            });
        });
        // Busy blocks a second write (docs/0023 D7 single-flight).
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.toggle_reaction(
                    "PR_2".into(),
                    false,
                    gql::ReactionContent::Heart,
                    window,
                    cx,
                );
                assert_eq!(page.busy, Some(BusyOp::Reaction), "no second write");
                page.busy = None;
            });
        });

        // The picker is a component-owned PopupMenu on the + button — no
        // page state to drive. The frozen per-row direction is the pure
        // helper; the armed direction posts removeReaction through the
        // same toggle.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                let groups = &page.detail.data().unwrap().reaction_groups;
                assert!(reaction_direction(groups, gql::ReactionContent::ThumbsUp));
                assert!(!reaction_direction(groups, gql::ReactionContent::Heart));
                page.toggle_reaction(
                    "PR_2".into(),
                    reaction_direction(groups, gql::ReactionContent::ThumbsUp),
                    gql::ReactionContent::ThumbsUp,
                    window,
                    cx,
                );
                assert_eq!(page.busy, Some(BusyOp::Reaction));
                page.busy = None;
            });
        });
    }

    /// #123: the shared cap row appears only when a connection was
    /// truncated, and its id/role survive on the PR page.
    #[gpui::test]
    fn cap_row_covers_truncated_connections(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
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
        let (page, vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "cap probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.detail = Fetch::ready(probe_detail());
            page.md = build_md_cache(&page.repo, page.detail.data().unwrap());
            page
        });
        // probe_detail() carries every node it counts — nothing hidden.
        assert!(vcx.debug_bounds("pr-comments-more").is_none());

        // A refetch that truncates both connections arms the row; the
        // count sums both shortfalls (210-0 comments, 105-0 reviews).
        let capped = r#"{"data":{"repository":{"pullRequest":{"id":"PR_2",
            "number":7,"title":"t","url":"u",
            "reviews":{"totalCount":105,"nodes":[]},
            "comments":{"totalCount":210,"nodes":[]}}}}}"#;
        let result = SpecResult {
            kind: Kind::PrDetail,
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
        assert!(vcx.debug_bounds("pr-comments-more").is_some());
    }

    #[gpui::test]
    fn detail_apply_drops_editors_of_vanished_nodes(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
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
        let (page, vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "layout probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.detail = Fetch::ready(probe_detail());
            page.timeline_edit = Some((
                "IC_1".into(),
                cx.new(|cx| gpui_component::input::TextareaState::new(window, cx)),
            ));
            page
        });
        // Editing IC_1; a refetch that still carries IC_1 keeps the
        // editor (0019 D4), one without it drops it.
        // apply_detail_result parses the full envelope — wrap the node.
        let keep = r#"{"data":{"repository":{"pullRequest":{"id":"PR_2",
            "number":7,"title":"t","url":"u",
            "comments":{"totalCount":1,"nodes":[{"id":"IC_1",
            "viewerCanUpdate":true,"viewerCanDelete":false,
            "author":{"login":"dave"},"body":"nice (edited)",
            "createdAt":"2026-09-03T01:00:00Z"}]}}}}}"#;
        let gone = r#"{"data":{"repository":{"pullRequest":{"id":"PR_2",
            "number":7,"title":"t","url":"u",
            "comments":{"totalCount":0,"nodes":[]}}}}}"#;
        let result_with = SpecResult {
            kind: Kind::PrDetail,
            ident: Some("o/r".into()),
            status: 200,
            etag: None,
            rate: None,
            body: Some(keep.to_string()),
            network: None,
            cursor: None,
        };
        let result_gone = SpecResult {
            kind: Kind::PrDetail,
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
    fn check(name: &str, state: gql::CheckState) -> gql::PrCheck {
        gql::PrCheck {
            name: name.into(),
            state,
            details_url: None,
            origin: gql::CheckOrigin::StatusContext,
        }
    }

    #[test]
    fn checks_counts_bucket_every_state() {
        let nodes = vec![
            check("build", gql::CheckState::Success),
            check("test", gql::CheckState::Success),
            check("lint", gql::CheckState::Failure),
            check("e2e", gql::CheckState::Pending),
            check("cla", gql::CheckState::Neutral),
        ];
        assert_eq!(
            checks_counts(&nodes),
            ChecksCounts {
                total: 5,
                passed: 2,
                failed: 1,
                pending: 1,
                muted: 1
            }
        );
        assert_eq!(checks_counts(&[]), ChecksCounts::default());
    }

    // --- check log panes (docs/0039) ------------------------------------

    use super::{CheckLogState, prepare_log_body};

    /// The summary row literal shared by the log-pane probes (the real
    /// page builds it from the snapshot row that opened the page).
    fn probe_summary(key: &str, number: i64) -> gql::PullRequest {
        gql::PullRequest {
            key: key.into(),
            database_id: None,
            number,
            title: "check-log probe".into(),
            draft: false,
            state: None,
            author: None,
            updated_at: None,
            url: "https://probe".into(),
            status: None,
            labels: None,
            review_requests: None,
        }
    }

    #[test]
    fn prepare_log_body_cases() {
        // \r progress overwrites fold to what a terminal shows (D4).
        let (text, truncated) = prepare_log_body("a\rb\nc", 512);
        assert_eq!(text, "b\nc");
        assert!(!truncated);
        // CSI color sequences vanish; payload text stays.
        let (text, _) = prepare_log_body("ok\u{1b}[31mred\u{1b}[0m", 512);
        assert_eq!(text, "okred");
        // OSC hyperlink sequences (BEL- and ST-terminated) vanish.
        let (text, _) = prepare_log_body("\u{1b}]8;;https://x\u{7}link\u{1b}]0;;\u{1b}\\", 512);
        assert_eq!(text, "link");
        // An empty log stays empty and untruncated.
        let (text, truncated) = prepare_log_body("", 512);
        assert_eq!(text, "");
        assert!(!truncated);
    }

    #[test]
    fn prepare_log_body_keeps_the_tail_on_line_boundaries() {
        let long = ["aaaa", "bbbbbbbb", "cccccccc"].repeat(10).join("\n");
        let (text, truncated) = prepare_log_body(&long, 24);
        assert!(truncated);
        // The cut lands on a line boundary: every kept line is whole.
        for line in text.split('\n') {
            assert!(!line.is_empty(), "cut produced a partial line: {text:?}");
            assert!(
                ["aaaa", "bbbbbbbb", "cccccccc"].contains(&line),
                "unexpected partial line {line:?}"
            );
        }
        // The tail is honest: the last line of the input ends the output.
        assert!(text.ends_with("cccccccc"));
    }

    #[test]
    fn prepare_log_body_respects_char_boundaries() {
        // A cap that lands mid-codepoint must not panic (multibyte é = 2B).
        let long = "é".repeat(64);
        let (text, truncated) = prepare_log_body(&long, 10);
        assert!(truncated);
        assert!(text.chars().all(|c| c == 'é'));
    }

    #[gpui::test]
    fn check_log_chain_resolves_and_lands(cx: &mut gpui::TestAppContext) {
        use crate::http::HttpResult;
        use crate::http::mock::MockHttp;
        use crate::service::{AuthStatus, GithubCache};
        use std::sync::Arc;
        use std::sync::RwLock;

        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
        cx.update(dashboard_core::status::init);
        let mock = Arc::new(MockHttp::new(vec![
            // The page's own detail fetch fires first — fail it benignly.
            Err(crate::http::HttpError("probe: detail fetch ignored".into())),
            Ok(HttpResult {
                status: 200,
                etag: None,
                rate: None,
                body: Some(
                    r#"{"check_runs":[{"name":"build","status":"COMPLETED","conclusion":"FAILURE","check_suite":{"id":5},"app":{"slug":"github-actions"}}]}"#.into(),
                ),
            }),
            Ok(HttpResult {
                status: 200,
                etag: None,
                rate: None,
                body: Some(r#"{"total_count":1,"workflow_runs":[{"id":7}]}"#.into()),
            }),
            Ok(HttpResult {
                status: 200,
                etag: None,
                rate: None,
                body: Some(
                    r#"{"total_count":2,"jobs":[{"id":1,"name":"use-existing-src"},{"id":9,"name":"build"}]}"#.into(),
                ),
            }),
            Ok(HttpResult {
                status: 200,
                etag: None,
                rate: None,
                // Tall enough to overflow the 384px pane so the
                // auto-scroll-to-end behavior is observable.
                body: Some(
                    ["step1\r\nstep2 \u{1b}[31mfailed\u{1b}[0m".to_string()]
                        .into_iter()
                        .chain(std::iter::repeat_n("filler line".to_string(), 400))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
            }),
        ]));
        let state = cx.update(|cx| {
            let shared: crate::http::SharedToken = Arc::new(RwLock::new(None));
            let refresh: crate::http::SharedToken = Arc::new(RwLock::new(None));
            let state = cx.new(|_| {
                GithubState::new(
                    mock.clone() as Arc<dyn crate::http::Http>,
                    Arc::new(crate::auth::MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::at_tmp("check-log-chain"),
                )
            });
            state.update(cx, |s, _| s.auth = AuthStatus::Connected);
            state
        });
        let detail_body = r#"{"data":{"repository":{"pullRequest":{
            "id":"PR_1","number":7,"title":"t","url":"u","state":"OPEN","isDraft":false,
            "headRefOid":"abc123",
            "statusCheckRollup":{"state":"FAILURE","contexts":{"totalCount":1,"nodes":[
                {"__typename":"CheckRun","name":"build","status":"COMPLETED",
                 "conclusion":"FAILURE","checkSuite":{"app":{"slug":"github-actions"}}}
            ]}}}}}}"#;
        let (page, vcx) = cx.add_window_view(|window, cx| {
            PrDetailPage::new(
                state,
                "o/r".into(),
                probe_summary("o/r#7", 7),
                None,
                window,
                cx,
            )
        });
        vcx.simulate_resize(gpui::Size {
            width: px(1024.),
            height: px(768.),
        });
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                page.tab = PrTab::Checks;
                page.apply_detail_result(
                    SpecResult {
                        kind: Kind::PrDetail,
                        ident: Some("o/r".into()),
                        status: 200,
                        etag: None,
                        rate: None,
                        body: Some(detail_body.to_string()),
                        network: None,
                        cursor: None,
                    },
                    cx,
                );
            });
        });
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| p.toggle_check_log("build", cx));
        });
        vcx.run_until_parked();
        let recorded = mock.recorded();
        // [0] is the page's own detail fetch (a GraphQL POST, failed
        // benignly by the script pad) — the chain is [1..=4].
        assert_eq!(recorded.len(), 5, "the whole chain fires: {:?}", recorded);
        assert_eq!(recorded[0].verb, crate::http::Verb::Post);
        assert!(
            recorded[1]
                .url
                .contains("/commits/abc123/check-runs?check_name=build")
        );
        assert!(recorded[2].url.contains("/actions/runs?check_suite_id=5"));
        assert!(recorded[3].url.contains("/actions/runs/7/jobs"));
        assert!(recorded[4].url.contains("/actions/jobs/9/logs"));
        // Paint the landed pane so the scroll_to_bottom flag is consumed,
        // then assert state and the pinned-to-bottom offset.
        vcx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let (landed, offset) = vcx.read(|cx| {
            let p = page.read(cx);
            let landed = match p.check_logs.get("build") {
                Some(CheckLogState::Ready { text, truncated }) => {
                    assert!(!truncated);
                    assert!(text.starts_with("step1\nstep2 failed\n"));
                    assert_eq!(text.lines().last(), Some("filler line"));
                    true
                }
                other => panic!("expected Ready, got {other:?}"),
            };
            (landed, p.check_log_scroll.offset())
        });
        assert!(landed);
        // docs/0039 D4: the pane opens at the end — the offset is pinned
        // to the bottom (negative y), not the top.
        assert!(
            offset.y < px(0.),
            "the pane must open scrolled to the end, offset = {offset:?}"
        );
    }

    #[gpui::test]
    fn check_log_chain_failure_names_the_step(cx: &mut gpui::TestAppContext) {
        use crate::http::HttpResult;
        use crate::http::mock::MockHttp;
        use crate::service::{AuthStatus, GithubCache};
        use std::sync::Arc;
        use std::sync::RwLock;

        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
        cx.update(dashboard_core::status::init);
        // Step 3 misses the name: the job was renamed upstream.
        let mock = Arc::new(MockHttp::new(vec![
            // The page's own detail fetch fires first — fail it benignly.
            Err(crate::http::HttpError("probe: detail fetch ignored".into())),
            Ok(HttpResult {
                status: 200,
                etag: None,
                rate: None,
                body: Some(
                    r#"{"check_runs":[{"name":"build","status":"COMPLETED","conclusion":"FAILURE","check_suite":{"id":5},"app":{"slug":"github-actions"}}]}"#.into(),
                ),
            }),
            Ok(HttpResult {
                status: 200,
                etag: None,
                rate: None,
                body: Some(r#"{"total_count":1,"workflow_runs":[{"id":7}]}"#.into()),
            }),
            Ok(HttpResult {
                status: 200,
                etag: None,
                rate: None,
                body: Some(r#"{"total_count":1,"jobs":[{"id":2,"name":"renamed-job"}]}"#.into()),
            }),
        ]));
        let state = cx.update(|cx| {
            let shared: crate::http::SharedToken = Arc::new(RwLock::new(None));
            let refresh: crate::http::SharedToken = Arc::new(RwLock::new(None));
            let state = cx.new(|_| {
                GithubState::new(
                    mock.clone() as Arc<dyn crate::http::Http>,
                    Arc::new(crate::auth::MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::at_tmp("check-log-fail"),
                )
            });
            state.update(cx, |s, _| s.auth = AuthStatus::Connected);
            state
        });
        let (page, _vcx) = cx.add_window_view(|window, cx| {
            let summary = probe_summary("o/r#7", 7);
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            let value = serde_json::json!({ "pullRequest": {
                "id": "PR_1", "number": 7, "title": "t", "url": "u",
                "state": "OPEN", "isDraft": false, "headRefOid": "abc123",
            }});
            page.detail = Fetch::ready(gql::pr_detail_from_value(&value).unwrap());
            page
        });
        cx.update(|cx| {
            page.update(cx, |p, cx| p.toggle_check_log("build", cx));
        });
        cx.run_until_parked();
        cx.update(|cx| {
            page.update(cx, |p, _| match p.check_logs.get("build") {
                Some(CheckLogState::Failed(message)) => {
                    assert!(message.contains("No job with this name"), "{message}");
                }
                other => panic!("expected Failed, got {other:?}"),
            });
        });
    }

    #[gpui::test]
    fn check_log_expander_gates_on_origin_and_completion(cx: &mut gpui::TestAppContext) {
        use crate::service::{AuthStatus, GithubCache};
        use std::sync::Arc;
        use std::sync::RwLock;

        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
        cx.update(dashboard_core::status::init);
        let state = cx.update(|cx| {
            let shared: crate::http::SharedToken = Arc::new(RwLock::new(None));
            let refresh: crate::http::SharedToken = Arc::new(RwLock::new(None));
            let state = cx.new(|_| {
                GithubState::new(
                    Arc::new(ProbeHttp) as Arc<dyn crate::http::Http>,
                    Arc::new(ProbeStore) as Arc<dyn crate::auth::TokenStore>,
                    shared,
                    refresh,
                    GithubCache::at_tmp("check-log-gate"),
                )
            });
            state.update(cx, |s, _| s.auth = AuthStatus::Connected);
            state
        });
        let detail_body = r#"{"data":{"repository":{"pullRequest":{
            "id":"PR_1","number":7,"title":"t","url":"u","state":"OPEN","isDraft":false,
            "headRefOid":"abc123",
            "statusCheckRollup":{"state":"FAILURE","contexts":{"totalCount":3,"nodes":[
                {"__typename":"CheckRun","name":"done","status":"COMPLETED",
                 "conclusion":"FAILURE","checkSuite":{"app":{"slug":"github-actions"}}},
                {"__typename":"CheckRun","name":"running","status":"IN_PROGRESS",
                 "conclusion":null,"checkSuite":{"app":{"slug":"github-actions"}}},
                {"__typename":"StatusContext","context":"cla","state":"SUCCESS"}
            ]}}}}}}"#;
        let (page, vcx) = cx.add_window_view(|window, cx| {
            let summary = probe_summary("o/r#7", 7);
            PrDetailPage::new(state, "o/r".into(), summary, None, window, cx)
        });
        vcx.simulate_resize(gpui::Size {
            width: px(1024.),
            height: px(768.),
        });
        // The checks tab is the visible one, and the detail lands the way
        // the real fetch delivers it — through apply_detail_result.
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                page.tab = PrTab::Checks;
                page.apply_detail_result(
                    SpecResult {
                        kind: Kind::PrDetail,
                        ident: Some("o/r".into()),
                        status: 200,
                        etag: None,
                        rate: None,
                        body: Some(detail_body.to_string()),
                        network: None,
                        cursor: None,
                    },
                    cx,
                );
            });
        });
        // Paint the frame the applied detail drives before asserting
        // bounds (the anchor probes' explicit draw).
        vcx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let _ = vcx.debug_bounds("pr-checks-0"); // flush frames
        let (ready, nchecks, tab) = vcx.read(|cx| {
            let p = page.read(cx);
            (
                p.detail.is_ready(),
                p.detail.data().map(|d| d.checks.nodes.len()),
                p.tab.index(),
            )
        });
        // The expander wrappers are the probe surface (debug_selector);
        // toggle-0 Some proves the checks rows rendered at all.
        assert!(
            vcx.debug_bounds("check-log-toggle-0").is_some(),
            "completed Actions row gets the expander (ready={ready}, nchecks={nchecks:?}, tab={tab})"
        );
        assert!(
            vcx.debug_bounds("check-log-toggle-1").is_none(),
            "in-progress row: no expander (D3)"
        );
        assert!(
            vcx.debug_bounds("check-log-toggle-2").is_none(),
            "status context row: no expander (D3)"
        );
    }

    #[test]
    fn checks_header_line_segments() {
        // The doc example: "8 checks · 7 passed · 1 failed".
        assert_eq!(
            checks_header_line(&ChecksCounts {
                total: 8,
                passed: 7,
                failed: 1,
                ..Default::default()
            }),
            "8 checks · 7 passed · 1 failed"
        );
        // Singular total, zero buckets dropped.
        assert_eq!(
            checks_header_line(&ChecksCounts {
                total: 1,
                passed: 1,
                ..Default::default()
            }),
            "1 check · 1 passed"
        );
        // Every bucket present, "running" for pending (GitHub's
        // in-flight wording), "skipped" for neutral.
        assert_eq!(
            checks_header_line(&ChecksCounts {
                total: 4,
                passed: 1,
                failed: 1,
                pending: 1,
                muted: 1
            }),
            "4 checks · 1 passed · 1 failed · 1 running · 1 skipped"
        );
    }

    #[test]
    fn short_sha_takes_seven_chars() {
        assert_eq!(short_sha("abcdef1234567890"), "abcdef1");
        // Short oids don't panic and don't pad.
        assert_eq!(short_sha("abc"), "abc");
        assert_eq!(short_sha(""), "");
    }

    #[test]
    fn split_path_dir_and_basename() {
        assert_eq!(
            split_path("crates/modules/github/src/pr_page.rs"),
            (Some("crates/modules/github/src"), "pr_page.rs")
        );
        assert_eq!(split_path("README.md"), (None, "README.md"));
        // Trailing slash: empty basename, same as github.com shows.
        assert_eq!(split_path("dir/"), (Some("dir"), ""));
    }

    #[test]
    fn files_summary_carries_the_line_numbers() {
        // The doc example (D8): "23 files changed · +812 · −94".
        assert_eq!(
            files_summary(23, 812, 94),
            FilesSummary {
                files: 23,
                adds: 812,
                dels: 94
            }
        );
    }
    fn comment(login: &str, at: &str) -> gql::PrComment {
        gql::PrComment {
            author: Some(gql::Author {
                login: login.into(),
                avatar_url: String::new(),
            }),
            body: format!("by {login}"),
            created_at: Some(at.into()),
            id: String::new(),
            database_id: None,
            viewer_can_update: false,
            viewer_can_delete: false,
            updated_at: None,
            // 0023 D1: the PR query doesn't select reaction fields yet —
            // the degraded defaults the production parse would produce.
            viewer_can_react: false,
            reaction_groups: Vec::new(),
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
            id: String::new(),
            viewer_can_update: false,
            viewer_can_delete: false,
            updated_at: None,
            // 0023 D1 (#196): the degraded defaults the production parse
            // produces when the payload omits the reaction fields.
            viewer_can_react: false,
            reaction_groups: Vec::new(),
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

    #[test]
    fn merge_state_line_table() {
        assert_eq!(
            merge_state_line(Some("CLEAN")),
            (
                "This branch has no conflicts with the base branch",
                MergeLineTone::Success,
            )
        );
        assert_eq!(
            merge_state_line(Some("HAS_HOOKS")),
            (
                "This branch has no conflicts with the base branch",
                MergeLineTone::Success,
            )
        );
        assert_eq!(
            merge_state_line(Some("BEHIND")),
            (
                "This branch is out-of-date with the base branch",
                MergeLineTone::Warning,
            )
        );
        assert_eq!(
            merge_state_line(Some("BLOCKED")),
            (
                "Merging is blocked by required reviews or status checks",
                MergeLineTone::Warning,
            )
        );
        assert_eq!(
            merge_state_line(Some("DIRTY")),
            (
                "This branch has conflicts that must be resolved",
                MergeLineTone::Danger,
            )
        );
        for status in [
            Some("DRAFT"),
            Some("UNSTABLE"),
            Some("UNKNOWN"),
            Some("WEIRD"),
            None,
        ] {
            let (line, tone) = merge_state_line(status);
            assert_eq!(tone, MergeLineTone::Muted, "status {status:?}");
            assert!(!line.is_empty());
        }
    }

    #[test]
    fn allowed_methods_covers_zero_through_three() {
        assert_eq!(
            allowed_methods(&MergeMethods {
                merge: true,
                squash: true,
                rebase: true,
            }),
            vec![MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase]
        );
        assert_eq!(
            allowed_methods(&MergeMethods {
                merge: false,
                squash: true,
                rebase: false
            }),
            vec![MergeMethod::Squash]
        );
        assert_eq!(
            allowed_methods(&MergeMethods {
                merge: true,
                squash: false,
                rebase: true
            }),
            vec![MergeMethod::Merge, MergeMethod::Rebase]
        );
        assert!(
            allowed_methods(&MergeMethods {
                merge: false,
                squash: false,
                rebase: false
            })
            .is_empty()
        );
    }

    #[test]
    fn default_method_prefers_merge_else_first_allowed() {
        assert_eq!(
            default_method(&[MergeMethod::Squash, MergeMethod::Merge]),
            Some(MergeMethod::Merge)
        );
        assert_eq!(
            default_method(&[MergeMethod::Rebase, MergeMethod::Squash]),
            Some(MergeMethod::Rebase)
        );
        assert_eq!(default_method(&[]), None);
    }

    #[test]
    fn comment_action_gating_table() {
        // Comment and Request-changes need a body (server-side rule,
        // #171); only Approve may go empty.
        assert!(!comment_action_allowed("Comment", true));
        assert!(comment_action_allowed("Comment", false));
        assert!(comment_action_allowed("Approve", true));
        assert!(comment_action_allowed("Approve", false));
        assert!(!comment_action_allowed("Request changes", true));
        assert!(comment_action_allowed("Request changes", false));
        assert!(!comment_action_allowed("Something else", false));
    }

    #[test]
    fn review_event_mapping() {
        assert_eq!(review_event("Approve"), Some("APPROVE"));
        assert_eq!(review_event("Request changes"), Some("REQUEST_CHANGES"));
        assert_eq!(review_event("Comment"), None);
    }

    #[test]
    fn merge_enabled_table() {
        let yes = (Some("OPEN"), false, Some("MERGEABLE"), false, false);
        assert!(merge_enabled(yes.0, yes.1, yes.2, yes.3, yes.4));
        // Each disqualifier alone flips it off.
        assert!(!merge_enabled(
            Some("MERGED"),
            false,
            Some("MERGEABLE"),
            false,
            false
        ));
        assert!(!merge_enabled(
            Some("CLOSED"),
            false,
            Some("MERGEABLE"),
            false,
            false
        ));
        assert!(!merge_enabled(
            Some("OPEN"),
            true,
            Some("MERGEABLE"),
            false,
            false
        ));
        assert!(!merge_enabled(
            Some("OPEN"),
            false,
            Some("CONFLICTING"),
            false,
            false
        ));
        assert!(!merge_enabled(
            Some("OPEN"),
            false,
            Some("UNKNOWN"),
            false,
            false
        ));
        assert!(!merge_enabled(Some("OPEN"), false, None, false, false));
        assert!(!merge_enabled(
            Some("OPEN"),
            false,
            Some("MERGEABLE"),
            true,
            false
        ));
        assert!(!merge_enabled(
            Some("OPEN"),
            false,
            Some("MERGEABLE"),
            false,
            true
        ));
    }

    #[test]
    fn rollup_summary_table() {
        assert_eq!(
            rollup_summary(Some("SUCCESS")),
            ("All checks passed", ChecksTone::Success)
        );
        assert_eq!(
            rollup_summary(Some("FAILURE")),
            ("Some checks failed", ChecksTone::Danger)
        );
        assert_eq!(
            rollup_summary(Some("ERROR")),
            ("Some checks errored", ChecksTone::Danger)
        );
        assert_eq!(
            rollup_summary(Some("PENDING")),
            ("Checks are running", ChecksTone::Warning)
        );
        assert_eq!(
            rollup_summary(Some("EXPECTED")),
            ("Checks are queued", ChecksTone::Muted)
        );
        assert_eq!(
            rollup_summary(None),
            ("Checks status unknown", ChecksTone::Muted)
        );
        assert_eq!(
            rollup_summary(Some("WEIRD")),
            ("Checks status unknown", ChecksTone::Muted)
        );
    }

    #[test]
    fn busy_op_error_scopes() {
        assert_eq!(BusyOp::Comment.error_scope(), ErrorScope::Timeline);
        assert_eq!(BusyOp::Review.error_scope(), ErrorScope::Timeline);
        assert_eq!(BusyOp::Description.error_scope(), ErrorScope::Timeline);
        assert_eq!(BusyOp::Merge.error_scope(), ErrorScope::Panel);
        assert_eq!(BusyOp::Close.error_scope(), ErrorScope::Panel);
    }

    #[test]
    fn files_op_error_scopes_and_wire_values() {
        // docs/0030 D7: every Files-tab write errors into the diff pane;
        // D4: the event choice maps to PullRequestReviewEvent wire values;
        // DISMISS is never offered.
        assert_eq!(BusyOp::LineComment.error_scope(), ErrorScope::Files);
        assert_eq!(BusyOp::ReviewReply.error_scope(), ErrorScope::Files);
        assert_eq!(BusyOp::ReviewBatch.error_scope(), ErrorScope::Files);
        assert_eq!(BusyOp::ThreadResolve.error_scope(), ErrorScope::Files);
        assert_eq!(BusyOp::ThreadUnresolve.error_scope(), ErrorScope::Files);
        assert_eq!(BusyOp::ReviewCommentEdit.error_scope(), ErrorScope::Files);
        assert_eq!(BusyOp::ReviewCommentDelete.error_scope(), ErrorScope::Files);
        assert_eq!(FinishEvent::default(), FinishEvent::Comment);
        assert_eq!(FinishEvent::Comment.as_str(), "COMMENT");
        assert_eq!(FinishEvent::Approve.as_str(), "APPROVE");
        assert_eq!(FinishEvent::RequestChanges.as_str(), "REQUEST_CHANGES");
        assert_eq!(FinishEvent::Comment.id_token(), "comment");
        assert_eq!(FinishEvent::Approve.id_token(), "approve");
        assert_eq!(FinishEvent::RequestChanges.id_token(), "request-changes");
        assert_eq!(FinishEvent::CHOICES.len(), 3);
    }

    #[test]
    fn composer_and_batch_gating_tables() {
        // docs/0030 §5: the composer's gating is the same pure fn the
        // buttons render with — blank bodies never send, empty batches
        // and busy pages never submit.
        assert!(composer_body_allowed("nit"));
        assert!(!composer_body_allowed(""));
        assert!(!composer_body_allowed("   \n  "));
        assert!(review_batch_allowed(1, false));
        assert!(review_batch_allowed(4, false));
        assert!(!review_batch_allowed(0, false));
        assert!(!review_batch_allowed(2, true));
    }

    #[gpui::test]
    fn finish_bar_and_draft_rows_track_page_state(cx: &mut gpui::TestAppContext) {
        // docs/0030 D4/§5: the Finish-review bar exists only while drafts
        // exist; each queued draft renders as a Pending row under its
        // anchored diff line; the row composer renders only while armed.
        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
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
        let (page, vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "finish bar probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.tab = PrTab::Files;
            page.files = Fetch::ready(vec![probe_file("src/main.rs")]);
            page.threads = Fetch::ready(gql::PrThreads::default());
            page.extend_threads_md();
            page.apply_files_payload(cx);
            page
        });
        // No drafts → no bar, no pending rows.
        assert!(vcx.debug_bounds("pr-finish-review-key").is_none());
        // Arm the composer on the +b row (Right side, line 1).
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| {
                p.open_line_composer("src/main.rs".into(), gql::DiffSide::Right, 1, window, cx);
            });
        });
        assert!(
            vcx.debug_bounds("pr-line-composer-key").is_some(),
            "composer card must render while armed"
        );
        // Queue a draft — the composer closes, the bar and a Pending row
        // appear.
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| {
                let editor = p.line_composer.as_ref().unwrap().editor.clone();
                editor.update(cx, |state, cx| {
                    state.set_value("please reconsider", window, cx);
                });
                p.start_review_draft(window, cx);
            });
        });
        assert!(vcx.debug_bounds("pr-line-composer-key").is_none());
        assert!(
            vcx.debug_bounds("pr-finish-review-key").is_some(),
            "finish bar must render while drafts exist"
        );
        assert!(
            vcx.debug_bounds("pr-draft-0-key").is_some(),
            "queued draft must render as a Pending row"
        );
        // Discard everything — bar and rows vanish with the drafts.
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| p.discard_review(window, cx));
        });
        assert!(vcx.debug_bounds("pr-finish-review-key").is_none());
        assert!(vcx.debug_bounds("pr-draft-0-key").is_none());
    }

    #[gpui::test]
    fn files_scoped_errors_render_inline(cx: &mut gpui::TestAppContext) {
        // docs/0030 D7: Files-scope mutation failures land in the diff
        // pane as an inline row — never modal, and other scopes never
        // leak into it.
        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
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
        let (page, vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "files error probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.tab = PrTab::Files;
            page.files = Fetch::ready(vec![probe_file("src/main.rs")]);
            page.threads = Fetch::ready(gql::PrThreads::default());
            page.extend_threads_md();
            page.apply_files_payload(cx);
            page.mutation_error = Some(super::MutationError {
                scope: ErrorScope::Files,
                message: "GitHub answered HTTP 422".into(),
            });
            page
        });
        assert!(
            vcx.debug_bounds("pr-files-error-key").is_some(),
            "Files-scope error must render in the diff pane"
        );
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.mutation_error = Some(super::MutationError {
                    scope: ErrorScope::Timeline,
                    message: "conversation scope".into(),
                });
                cx.notify();
            });
        });
        assert!(vcx.debug_bounds("pr-files-error-key").is_none());
    }

    #[test]
    fn resolution_reason_wire_values() {
        // #213: `PullRequestReviewThreadResolutionReason`, verified live
        // (2026-09-16 introspection) — the read schema has no such field,
        // so these tokens only ever ride the resolve input.
        assert_eq!(ResolutionReason::Addressed.as_str(), "ADDRESSED");
        assert_eq!(ResolutionReason::WontFix.as_str(), "WONT_FIX");
        assert_eq!(ResolutionReason::Invalid.as_str(), "INVALID");
        assert_eq!(ResolutionReason::Addressed.id_token(), "addressed");
        assert_eq!(ResolutionReason::WontFix.id_token(), "wont-fix");
        assert_eq!(ResolutionReason::Invalid.id_token(), "invalid");
        assert_eq!(ResolutionReason::CHOICES.len(), 3);
    }

    #[gpui::test]
    fn resolve_picker_opens_under_its_thread(cx: &mut gpui::TestAppContext) {
        // #213: Resolve no longer fires immediately — the picker row
        // renders under the thread card until a choice or Cancel.
        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
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
        let (page, vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "resolve picker probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.tab = PrTab::Files;
            page.files = Fetch::ready(vec![probe_file("src/main.rs")]);
            let mut thread = probe_thread(0, Some(1), false, "please fix");
            thread.viewer_can_resolve = true;
            page.threads = Fetch::ready(gql::PrThreads {
                total_count: 1,
                nodes: vec![thread],
            });
            page.extend_threads_md();
            page.apply_files_payload(cx);
            page
        });
        assert!(vcx.debug_bounds("pr-resolve-picker-key").is_none());
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| {
                p.open_resolve_picker("T_0".into(), window, cx);
            });
        });
        assert!(
            vcx.debug_bounds("pr-resolve-picker-key").is_some(),
            "picker row must render while open"
        );
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| p.cancel_resolve_picker(cx));
        });
        assert!(vcx.debug_bounds("pr-resolve-picker-key").is_none());
    }

    #[gpui::test]
    fn thread_comment_edit_and_delete_swap_in(cx: &mut gpui::TestAppContext) {
        // #212: the 0019 D4 row swap applied to thread comments — the
        // editor and the delete confirm replace the comment body while
        // armed, keyed to that one comment.
        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
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
        let (page, vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "thread edit probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.tab = PrTab::Files;
            page.files = Fetch::ready(vec![probe_file("src/main.rs")]);
            let mut thread = probe_thread(0, Some(1), false, "please fix");
            thread.comments[0].viewer_can_update = true;
            thread.comments[0].viewer_can_delete = true;
            page.threads = Fetch::ready(gql::PrThreads {
                total_count: 1,
                nodes: vec![thread],
            });
            page.extend_threads_md();
            page.apply_files_payload(cx);
            page
        });
        assert!(
            vcx.debug_bounds("pr-thread-0-comment-0-edit-card")
                .is_none()
        );
        assert!(
            vcx.debug_bounds("pr-thread-0-comment-0-delete-confirm-card")
                .is_none()
        );
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| {
                p.start_thread_edit("T_0_C_0".into(), "please fix".into(), window, cx);
            });
        });
        assert!(
            vcx.debug_bounds("pr-thread-0-comment-0-edit-card")
                .is_some(),
            "editor must swap in while armed"
        );
        // Editors yield to the delete confirm (one editor anywhere).
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.start_thread_delete_confirm("T_0_C_0".into(), cx);
            });
        });
        assert!(
            vcx.debug_bounds("pr-thread-0-comment-0-edit-card")
                .is_none()
        );
        assert!(
            vcx.debug_bounds("pr-thread-0-comment-0-delete-confirm-card")
                .is_some(),
            "delete confirm must swap in while armed"
        );
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| p.cancel_thread_delete_confirm(cx));
        });
        assert!(
            vcx.debug_bounds("pr-thread-0-comment-0-delete-confirm-card")
                .is_none()
        );
    }
    #[test]
    fn normalize_range_and_start_fields_tables() {
        // #211: same-side anchors order into (start, end); different
        // sides can't extend in v1; equal lines collapse. start_fields
        // emits the exact DraftPullRequestReviewThread range fields and
        // stays empty for single-line comments.
        let (r, l) = (gql::DiffSide::Right, gql::DiffSide::Left);
        assert_eq!(normalize_range((r, 5), (r, 2)), Some(((r, 2), (r, 5))));
        assert_eq!(normalize_range((r, 2), (r, 5)), Some(((r, 2), (r, 5))));
        assert_eq!(normalize_range((r, 3), (r, 3)), Some(((r, 3), (r, 3))));
        assert_eq!(normalize_range((r, 3), (l, 3)), None);
        assert_eq!(start_fields(None), serde_json::Map::new());
        let mut want = serde_json::Map::new();
        want.insert("startLine".into(), serde_json::json!(2));
        want.insert("startSide".into(), serde_json::json!("RIGHT"));
        assert_eq!(start_fields(Some((r, 2))), want);
    }

    #[gpui::test]
    fn range_composer_extends_and_draft_lands_on_end_row(cx: &mut gpui::TestAppContext) {
        // #211: clicking a second row extends the composer's range; the
        // queued draft renders under the range's END row (GitHub renders
        // range threads at the end line).
        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5) —
        // let the deterministic test executor park instead of spinning
        // on the outstanding task.
        cx.update(|cx| cx.background_executor().allow_parking());
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
        let (page, vcx) = cx.add_window_view(|window, cx| {
            let summary = gql::PullRequest {
                key: "o/r#7".into(),
                database_id: None,
                number: 7,
                title: "range probe".into(),
                draft: false,
                state: None,
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.tab = PrTab::Files;
            page.files = Fetch::ready(vec![PrFile {
                filename: "src/main.rs".into(),
                status: "modified".into(),
                additions: 2,
                deletions: 2,
                changes: 4,
                patch: Some("@@ -1,2 +1,2 @@\n-a\n-b\n+c\n+d".into()),
            }]);
            page.threads = Fetch::ready(gql::PrThreads::default());
            page.extend_threads_md();
            page.apply_files_payload(cx);
            page
        });
        // Arm on the +c row (RIGHT, line 1), extend on the +d row (RIGHT,
        // line 2) — the range is lines 1\u{2013}2.
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| {
                p.open_line_composer("src/main.rs".into(), gql::DiffSide::Right, 1, window, cx);
            });
        });
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| {
                p.open_line_composer("src/main.rs".into(), gql::DiffSide::Right, 2, window, cx);
            });
        });
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| {
                assert_eq!(
                    p.line_composer.as_ref().map(|c| (c.start, c.line)),
                    Some((Some((gql::DiffSide::Right, 1)), 2)),
                    "composer must hold the extended range"
                );
                let editor = p.line_composer.as_ref().unwrap().editor.clone();
                editor.update(cx, |state, cx| {
                    state.set_value("both lines", window, cx);
                });
                p.start_review_draft(window, cx);
            });
        });
        // The composer closed; the draft renders under the range's END
        // row (line 2), not the start row.
        assert!(vcx.debug_bounds("pr-line-composer-key").is_none());
        assert!(
            vcx.debug_bounds("pr-draft-0-key").is_some(),
            "range draft must render under its end row"
        );
    }
}
