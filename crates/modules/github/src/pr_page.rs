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
    group_box::GroupBox,
    h_flex,
    input::{InputEvent, InputState, Textarea, TextareaState},
    list::{ListEvent, ListState},
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
use crate::markdown;
use crate::page::relative_age;
use crate::service::{FetchSpec, GithubState, Kind, Mutation, run_mutation, run_spec_tracked};

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
    /// Comment box (D4): draft textarea + action select.
    comment: Entity<TextareaState>,
    action_select: Entity<SelectState<SearchableVec<SharedString>>>,
    /// The write path in flight (D4/D5) — one at a time; every write
    /// surface disables while set.
    busy: Option<BusyOp>,
    /// The last mutation failure, scoped to the surface that shows it.
    mutation_error: Option<MutationError>,
    /// Prefilled textarea while the description is being edited (D4).
    description_edit: Option<Entity<TextareaState>>,
    /// Row-level editor (0019 D4): (node id, editor). At most one editor
    /// is open anywhere on the page — opening one closes the description
    /// editor, other row editors, and any pending delete confirm.
    timeline_edit: Option<(String, Entity<TextareaState>)>,
    /// Node id awaiting delete confirmation (0019 D4, inline two-step).
    delete_confirm: Option<String>,
    /// Methods the repo allows (D5), from the detail payload — drives the
    /// method Select and the Merge button verb.
    methods: Vec<MergeMethod>,
    /// Method Select state — built once, on the first render that has a
    /// detail payload (SelectState needs a window; render has one).
    method_select: Option<Entity<SelectState<SearchableVec<SharedString>>>>,
    /// Changed-files fetch lifecycle (D8): lazy — `Idle` until the
    /// Files tab is first opened, then one REST walk; `Ready` keeps the
    /// parsed list (a refresh refetches it).
    files: FilesFetch,
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
    md: BTreeMap<String, Arc<[markdown::Block]>>,
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

/// The Files tab's fetch lifecycle (docs/0016 D8).
enum FilesFetch {
    Idle,
    Fetching,
    Ready(Vec<PrFile>),
    Failed(String),
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
            detail: None,
            loading: false,
            load_error: None,
            tab: PrTab::Conversation,
            focus_handle: cx.focus_handle(),
            comment,
            action_select,
            busy: None,
            mutation_error: None,
            description_edit: None,
            timeline_edit: None,
            delete_confirm: None,
            methods: Vec::new(),
            method_select: None,
            files: FilesFetch::Idle,
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
        self.loading = true;
        self.load_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = run_spec_tracked(state, spec, cx).await;
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
                            self.md = build_md_cache(&detail);
                            self.detail = Some(detail);
                            // 0019 D4: an editor/confirm whose node is gone
                            // with the new payload has nothing left to edit —
                            // drop it (deleted elsewhere, refetch race).
                            let d = self.detail.as_ref().unwrap();
                            let still_there = |id: &str| {
                                node_kind(&d.comments.nodes, &d.reviews.nodes, id).is_some()
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
                            // docs/0026 D9: the payload is in — try to arm
                            // the notification jump against it.
                            self.resolve_comment_anchor();
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

    /// docs/0026 D9: resolve the pending REST-id anchor against the landed
    /// detail — the matching comment node arms the jump; a miss gives up.
    fn resolve_comment_anchor(&mut self) {
        let CommentAnchor::Pending(target) = self.anchor else {
            return;
        };
        self.anchor = self
            .detail
            .as_ref()
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
    fn files_spec(repo: &str, number: i64) -> FetchSpec {
        let (owner, name) = split_repo(repo);
        FetchSpec {
            kind: Kind::PrFiles,
            url: api::pr_files_url(owner, name, number, 1),
            etag: None,
            ident: Some(format!("{repo}#{number}")),
            max_pages: 30,
            variables: None,
        }
    }

    /// Lazy fetch (D8): fired once, when the Files tab is first opened,
    /// and again on refresh once a list is showing.
    fn fetch_files(&mut self, cx: &mut Context<Self>) {
        if !self.state.read(cx).auth.connected() {
            return; // the page is about to be torn down by the auth gate
        }
        let state = self.state.clone();
        let spec = Self::files_spec(&self.repo, self.number);
        log::debug!(
            "pr-files: fetching {}",
            spec.ident.as_deref().unwrap_or("?")
        );
        self.files = FilesFetch::Fetching;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = run_spec_tracked(state, spec, cx).await;
            let _ = this.update(cx, |page, cx| page.apply_files_result(result, cx));
        })
        .detach();
    }

    fn apply_files_result(&mut self, result: crate::service::SpecResult, cx: &mut Context<Self>) {
        if let Some(m) = &result.network {
            self.files = FilesFetch::Failed(format!("Network error: {m}"));
        } else if result.status != 200 {
            // Shared 401/403 handling, same as the detail fetch (D2).
            let status = result.status;
            self.state.update(cx, |s, cx| {
                s.apply(result);
                cx.notify();
            });
            self.files = FilesFetch::Failed(format!("GitHub answered HTTP {status}"));
        } else {
            match serde_json::from_str::<Vec<PrFile>>(result.body.as_deref().unwrap_or("[]")) {
                Ok(files) => {
                    log::info!(
                        "pr-files applied: {}#{} — {} files",
                        self.repo,
                        self.number,
                        files.len()
                    );
                    self.files = FilesFetch::Ready(files);
                    self.apply_files_payload(cx);
                }
                Err(e) => {
                    log::warn!("pr-files payload: {e}");
                    self.files = FilesFetch::Failed("Malformed payload".into());
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
        let FilesFetch::Ready(files) = &self.files else {
            return;
        };
        let entries: Vec<FileEntry> = files.iter().map(FileEntry::from_api).collect();
        let query = self.file_filter.read(cx).value().trim().to_string();
        let selected = entries
            .iter()
            .find(|f| path_matches(f.path.as_ref(), &query))
            .map(|f| f.path.clone());
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
        let Some(d) = self.detail.clone() else { return };
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
        // One editor at a time (0019 D4): a row editor or delete confirm
        // yields to the description editor.
        self.timeline_edit = None;
        self.delete_confirm = None;
        let body = self
            .detail
            .as_ref()
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

    /// Save the edited description (`updateIssue`, D4) — the PR node id is
    /// the issue id; empty bodies never get here (Save renders disabled).
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
        let Some(d) = self.detail.as_ref() else {
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
        let Some(d) = self.detail.clone() else { return };
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
        let Some(d) = self.detail.clone() else { return };
        if self.busy.is_some() || d.id.is_empty() || d.state.as_deref() != Some("OPEN") {
            return;
        }
        let variables = json!({ "input": { "pullRequestId": d.id } });
        self.mutation_error = None;
        self.busy = Some(BusyOp::Close);
        cx.notify();
        self.spawn_mutation(BusyOp::Close, Mutation::Close, variables, window, cx);
    }

    /// The description editor holds keyboard focus while open; dropping
    /// its entity (cancel, or save success) would leave the page with a
    /// dead focus handle — no key context, so `Esc` would stop working
    /// (found via the bridge during stage-2 verification). Hand focus
    /// back to the page root.
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
            }
        }
        self.fetch_detail(cx);
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
                                // A refresh with files already loaded
                                // refetches the list too (D8) — collapse
                                // states reset with the new payload.
                                if matches!(this.files, FilesFetch::Ready(_)) {
                                    this.fetch_files(cx);
                                }
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
                    // Lazy files fetch (D8): the REST walk starts the
                    // first time the Files tab opens, not on page open.
                    if tab == PrTab::Files && matches!(this.files, FilesFetch::Idle) {
                        this.fetch_files(cx);
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

    fn render_tab_content(&self, detail: &Option<gql::PrDetail>, cx: &Context<Self>) -> AnyElement {
        match self.tab {
            PrTab::Conversation => self.render_conversation(detail, cx),
            PrTab::Commits => self.render_commits(detail, cx),
            PrTab::Checks => self.render_checks(detail, cx),
            PrTab::Files => render_files(self, cx),
        }
    }

    /// Commits tab (D6): scrollable rows — avatar/name (login, falling
    /// back to `author.name`), `messageHeadline` (`text_sm`), 7-char SHA
    /// (mono, muted), relative age; muted truncation caption when the
    /// connection holds more than the `first: 100` page.
    fn render_commits(&self, detail: &Option<gql::PrDetail>, cx: &Context<Self>) -> AnyElement {
        let Some(d) = detail else {
            if let Some(err) = &self.load_error {
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
    fn render_checks(&self, detail: &Option<gql::PrDetail>, cx: &Context<Self>) -> AnyElement {
        let Some(d) = detail else {
            if let Some(err) = &self.load_error {
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
                h_flex()
                    .id(SharedString::from(format!("pr-check-{i}")))
                    .gap_2()
                    .items_center()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(Icon::new(icon).size_4().text_color(color))
                    .child(
                        div()
                            .text_sm()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .child(check.name.clone()),
                    )
                    .when_some(check.details_url.clone(), |row, url| {
                        row.child(
                            Button::new(SharedString::from(format!("pr-check-open-{i}")))
                                .ghost()
                                .xsmall()
                                .icon(IconName::ExternalLink)
                                .tooltip("View details")
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
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
                    None,
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
                        None,
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
                        None,
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
            .child(self.render_checks_summary(d, cx))
            .into_any_element()
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
    /// Comment requires a body; Approve/Request-changes may go empty
    /// (same pure fn gates button and handler). Failures keep the draft
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
            if let Some(d) = &self.detail {
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
            | BusyOp::ReviewDelete => ErrorScope::Timeline,
            BusyOp::Merge | BusyOp::Close => ErrorScope::Panel,
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
}

/// The last mutation failure — kept until the next attempt or success, so
/// it survives the post-mutation refetch that repaints everything else.
#[derive(Clone, Debug)]
pub(crate) struct MutationError {
    pub(crate) scope: ErrorScope,
    pub(crate) message: String,
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

/// Comment-box gating (D4): a plain comment needs a body; review events
/// (Approve / Request changes) may go empty. Unknown actions never fire.
pub(crate) fn comment_action_allowed(action: &str, body_empty: bool) -> bool {
    match action {
        "Comment" => !body_empty,
        "Approve" | "Request changes" => true,
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
fn build_md_cache(d: &gql::PrDetail) -> BTreeMap<String, Arc<[markdown::Block]>> {
    let mut map = BTreeMap::new();
    if let Some(body) = &d.body {
        map.insert("pr-description".to_string(), markdown::parse_markdown(body));
    }
    for (ix, comment) in d.comments.nodes.iter().enumerate() {
        map.insert(
            format!("pr-comment-{ix}"),
            markdown::parse_markdown(&comment.body),
        );
    }
    for (ix, review) in d.reviews.nodes.iter().enumerate() {
        if let Some(body) = review.body.as_deref().filter(|b| !b.is_empty()) {
            map.insert(format!("pr-review-{ix}"), markdown::parse_markdown(body));
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
fn render_files(page: &PrDetailPage, cx: &Context<PrDetailPage>) -> AnyElement {
    match &page.files {
        FilesFetch::Idle => skeleton_list(),
        FilesFetch::Fetching => skeleton_list(),
        FilesFetch::Failed(msg) => v_flex()
            .id("pr-files-error")
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_2()
            .text_sm()
            .text_color(cx.theme().danger)
            .child(msg.clone())
            .child(
                Button::new("pr-files-retry")
                    .ghost()
                    .small()
                    .icon(IconName::RotateCw)
                    .tooltip("Retry")
                    .on_click(cx.listener(|this, _, _, cx| this.fetch_files(cx))),
            )
            .into_any_element(),
        FilesFetch::Ready(files) if files.is_empty() => empty_cell("No file changes", cx),
        FilesFetch::Ready(files) => files::render_split(page, files, cx),
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
    use crate::pr_page::{
        BusyOp, ChecksCounts, ChecksTone, ErrorScope, FilesFetch, MergeLineTone, MergeMethod,
        PrDetailPage, PrStateTone, PrTab, ReviewTone, TimelineKind, allowed_methods, checks_counts,
        checks_header_line, comment_action_allowed, default_method, merge_enabled,
        merge_state_line, pr_state_tone, review_event, review_state_label, rollup_summary,
        short_sha, split_repo, timeline_order,
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

    #[gpui::test]
    fn files_split_has_real_layout(cx: &mut gpui::TestAppContext) {
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
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.tab = PrTab::Files;
            page.files = FilesFetch::Ready(vec![
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

    // --- timeline-row edit/delete (docs/0019 §5) ------------------------

    use crate::pr_page::{build_md_cache, delete_row, edit_row, node_kind, row_is_edited};
    use crate::service::{Kind, Mutation, SpecResult};

    /// docs/0026 D9: the anchor variant — the notification hand-off's jump
    /// opens the page with `Some(rest_comment_id)`; the detail fixture rides
    /// in directly, as the real fetch would.
    fn probe_pr_anchored(
        cx: &mut gpui::TestAppContext,
        anchor: Option<i64>,
    ) -> (gpui::Entity<PrDetailPage>, &mut gpui::VisualTestContext) {
        cx.update(gpui_component::init);
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
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, anchor, window, cx);
            page.detail = Some(probe_detail());
            page.md = build_md_cache(page.detail.as_ref().unwrap());
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
        let value = serde_json::json!({ "pullRequest": {
            "id": "PR_2", "number": 7, "title": "layout probe", "url": "u",
            "state": "OPEN", "isDraft": false,
            "createdAt": "2026-09-01T00:00:00Z",
            "reviews": {"totalCount": 2, "nodes": [
                {"id": "PRR_1", "viewerCanUpdate": true, "viewerCanDelete": true,
                 "author": {"login": "bob"}, "state": "APPROVED", "body": "lgtm",
                 "submittedAt": "2026-09-01T01:00:00Z"},
                {"author": {"login": "carol"}, "state": "COMMENTED", "body": "note",
                 "submittedAt": "2026-09-02T01:00:00Z"}
            ]},
            "comments": {"totalCount": 2, "nodes": [
                {"id": "IC_1", "databaseId": 987654,
                 "viewerCanUpdate": true, "viewerCanDelete": false,
                 "updatedAt": "2026-09-03T02:00:00Z",
                 "author": {"login": "dave"}, "body": "nice",
                 "createdAt": "2026-09-03T01:00:00Z"},
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
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.detail = Some(probe_detail());
            page.md = build_md_cache(page.detail.as_ref().unwrap());
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

    /// #123: the shared cap row appears only when a connection was
    /// truncated, and its id/role survive on the PR page.
    #[gpui::test]
    fn cap_row_covers_truncated_connections(cx: &mut gpui::TestAppContext) {
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
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.detail = Some(probe_detail());
            page.md = build_md_cache(page.detail.as_ref().unwrap());
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
            };
            let mut page = PrDetailPage::new(state, "o/r".into(), summary, None, window, cx);
            page.detail = Some(probe_detail());
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
        // Comment requires a body; review events may go empty.
        assert!(!comment_action_allowed("Comment", true));
        assert!(comment_action_allowed("Comment", false));
        assert!(comment_action_allowed("Approve", true));
        assert!(comment_action_allowed("Approve", false));
        assert!(comment_action_allowed("Request changes", true));
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
}
