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
    collapsible::Collapsible,
    group_box::GroupBox,
    h_flex,
    input::{Textarea, TextareaState},
    searchable_list::SearchableVec,
    select::{Select, SelectState},
    skeleton::Skeleton,
    spinner::Spinner,
    tab::{Tab, TabBar},
    v_flex,
};

use serde_json::json;

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use crate::api::{self, PrFile};
use crate::gql;
use crate::markdown;
use crate::page::relative_age;
use crate::service::{
    FetchSpec, GithubState, Kind, Mutation, PatchLine, parse_patch, run_mutation, run_spec_tracked,
};

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
    /// Collapsed file-card indexes (D8) — default open (github.com
    /// parity); indexes key into `FilesFetch::Ready`'s list.
    collapsed_files: HashSet<usize>,
    /// Parsed markdown bodies (docs/0017 D6) keyed by card id — rebuilt
    /// whenever a detail payload lands, never re-parsed per frame.
    md: BTreeMap<String, Arc<[markdown::Block]>>,
}

/// The Files tab's fetch lifecycle (docs/0016 D8).
enum FilesFetch {
    Idle,
    Fetching,
    Ready(Vec<PrFile>),
    Failed(String),
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
            busy: None,
            mutation_error: None,
            description_edit: None,
            methods: Vec::new(),
            method_select: None,
            files: FilesFetch::Idle,
            collapsed_files: HashSet::new(),
            md: BTreeMap::new(),
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
                            self.md = build_md_cache(&detail);
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
        let client = self.state.update(cx, |s, _| s.client.clone());
        let spec = Self::files_spec(&self.repo, self.number);
        log::debug!(
            "pr-files: fetching {}",
            spec.ident.as_deref().unwrap_or("?")
        );
        self.files = FilesFetch::Fetching;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = run_spec_tracked(client, spec, cx).await;
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
                    self.collapsed_files.clear();
                    self.files = FilesFetch::Ready(files);
                }
                Err(e) => {
                    log::warn!("pr-files payload: {e}");
                    self.files = FilesFetch::Failed("Malformed payload".into());
                }
            }
        }
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
        let body = self
            .detail
            .as_ref()
            .and_then(|d| d.body.clone())
            .unwrap_or_default();
        let editor = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx)
                .rows(8)
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
        let client = self.state.update(cx, |s, _| s.client.clone());
        let ident = format!("{}#{} {:?}", self.repo, self.number, op);
        // `spawn_in` keeps the window reachable across the await so the
        // result handler can clear the draft (set_value needs a Window).
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
            Some(editor) => description_edit_card(
                editor,
                self.busy.is_some(),
                editor.read(cx).value().trim().is_empty(),
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
                    cx,
                )
            }
        };
        let mut children = vec![description];
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
                        self.md.get(&format!("pr-comment-{ix}")).map(Arc::as_ref),
                        None,
                        cx,
                    ));
                }
                TimelineKind::Review => {
                    let r = &d.reviews.nodes[ix];
                    let badge = review_badge(r.state.as_deref(), cx);
                    children.push(body_card(
                        &format!("pr-review-{ix}"),
                        r.author
                            .as_ref()
                            .map(|a| a.login.as_str())
                            .unwrap_or("ghost"),
                        r.author.as_ref().map(|a| a.avatar_url.as_str()),
                        relative_age(r.submitted_at.as_deref()),
                        r.body.as_deref().filter(|b| !b.is_empty()),
                        self.md.get(&format!("pr-review-{ix}")).map(Arc::as_ref),
                        badge,
                        cx,
                    ));
                }
            }
        }
        if d.comments.nodes.is_empty() && d.reviews.nodes.is_empty() {
            children.push(muted_centered("No conversation yet", cx));
        }
        children.push(self.render_comment_box(cx));
        // D4/D5: the timeline scrolls; the rail (merge/close panel + checks
        // mini-summary) stands beside it.
        let timeline = div()
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
            .child(v_flex().gap_3().py_1().children(children));
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
}

impl BusyOp {
    /// Where a failure of this op renders inline (D4/D5).
    fn error_scope(self) -> ErrorScope {
        match self {
            BusyOp::Comment | BusyOp::Review | BusyOp::Description => ErrorScope::Timeline,
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
fn body_card(
    id: &str,
    login: &str,
    avatar: Option<&str>,
    age: String,
    body: Option<&str>,
    md: Option<&[markdown::Block]>,
    trailing: Option<AnyElement>,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
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
fn empty_cell(text: &str, cx: &Context<PrDetailPage>) -> AnyElement {
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

/// The description card in edit mode (D4): same card chrome, body swapped
/// for the prefilled Textarea + Save (disabled while empty/busy) / Cancel.
fn description_edit_card(
    editor: &Entity<TextareaState>,
    busy: bool,
    empty: bool,
    on_save: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    on_cancel: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    v_flex()
        .id("pr-description-edit")
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
                        .child("Editing description"),
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
                            Button::new("pr-description-cancel")
                                .ghost()
                                .small()
                                .label("Cancel")
                                .on_click(on_cancel),
                        )
                        .child(
                            Button::new("pr-description-save")
                                .primary()
                                .small()
                                .label("Save")
                                .disabled(busy || empty)
                                .tooltip(if empty {
                                    "The description can't be empty"
                                } else {
                                    "Save the description"
                                })
                                .on_click(on_save),
                        ),
                ),
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
        FilesFetch::Ready(files) => render_files_list(files, &page.collapsed_files, cx),
    }
}

fn render_files_list(
    files: &[PrFile],
    collapsed: &HashSet<usize>,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    let (adds, dels): (i64, i64) = files
        .iter()
        .map(|f| (f.additions, f.deletions))
        .fold((0, 0), |(a, d), (fa, fd)| (a + fa, d + fd));
    let summary = files_summary(files.len(), adds, dels);
    let cards: Vec<AnyElement> = files
        .iter()
        .enumerate()
        .map(|(i, file)| file_card(i, file, collapsed.contains(&i), cx))
        .collect();
    v_flex()
        .id("pr-files-scroll")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .gap_2()
        .py_3()
        .child(
            h_flex().px_3().gap_2().text_sm().child(
                h_flex()
                    .gap_1()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} files changed", summary.files))
                    .child(format!("· +{}", summary.adds))
                    .child(format!("−{}", summary.dels)),
            ),
        )
        .children(cards)
        .into_any_element()
}

/// Summary-line text pieces (D8): kept as a struct so the render path
/// colors each piece without re-formatting, and the pure fn is
/// table-testable.
#[derive(Debug, PartialEq, Eq)]
struct FilesSummary {
    files: usize,
    adds: i64,
    dels: i64,
}

fn files_summary(files: usize, adds: i64, dels: i64) -> FilesSummary {
    FilesSummary { files, adds, dels }
}

/// Change-type badge (D8): the four GitHub kinds map to tones; anything
/// else (future API kinds, 0007 D3) renders as the raw string, muted.
fn file_status_badge(status: &str, cx: &Context<PrDetailPage>) -> AnyElement {
    let (label, tone) = match status {
        "added" => (status, cx.theme().success),
        "removed" => (status, cx.theme().danger),
        "renamed" => (status, cx.theme().muted_foreground),
        _ => (status, cx.theme().muted_foreground),
    };
    Badge::new()
        .xsmall()
        .color(tone)
        .child(label.to_string())
        .into_any_element()
}

/// Split a repo path for the card header (D8): dir segments render
/// muted ahead of the basename, github.com-style. Pure for the table
/// tests.
fn split_path(path: &str) -> (Option<&str>, &str) {
    match path.rsplit_once('/') {
        Some((dir, name)) => (Some(dir), name),
        None => (None, path),
    }
}

/// One file's card (D8): header = toggle button (path with muted dir
/// prefix, `+adds/−dels`, change-type badge); body = the parsed diff
/// inside the `Collapsible` reveal.
fn file_card(ix: usize, file: &PrFile, collapsed: bool, cx: &Context<PrDetailPage>) -> AnyElement {
    let (dir, name) = split_path(&file.filename);
    let chevron = if collapsed {
        IconName::ChevronRight
    } else {
        IconName::ChevronDown
    };
    let id = SharedString::from(format!("pr-file-{ix}"));
    Collapsible::new()
        .open(!collapsed)
        .motion_id(id.clone())
        .child(
            Button::new(id)
                .ghost()
                .xsmall()
                .w_full()
                .justify_start()
                .on_click(cx.listener(move |this, _, _, cx| {
                    // Collapse is index-keyed; a refetch clears the set so
                    // stale indexes can never leak into a new list.
                    if !this.collapsed_files.remove(&ix) {
                        this.collapsed_files.insert(ix);
                    }
                    cx.notify();
                }))
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .items_center()
                        .child(Icon::new(chevron).xsmall())
                        .child(
                            h_flex()
                                .min_w_0()
                                .gap_1()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_xs()
                                .when_some(dir, |row, dir| {
                                    row.child(
                                        div()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("{dir}/")),
                                    )
                                })
                                .child(div().child(name.to_string())),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .gap_2()
                                .text_xs()
                                .child(
                                    div()
                                        .text_color(cx.theme().success)
                                        .child(format!("+{}", file.additions)),
                                )
                                .child(
                                    div()
                                        .text_color(cx.theme().danger)
                                        .child(format!("−{}", file.deletions)),
                                )
                                .child(file_status_badge(&file.status, cx)),
                        ),
                ),
        )
        .content(diff_body(file, cx))
        .into_any_element()
}

/// The diff body (D8): horizontal scroll, mono `text_xs`, `flex_none`
/// rows — no soft wrap. Tints come from the semantic tokens at low
/// opacity (D10); the sign glyph carries the saturated color.
fn diff_body(file: &PrFile, cx: &Context<PrDetailPage>) -> AnyElement {
    let Some(patch) = file.patch.as_deref() else {
        return div()
            .px_3()
            .py_2()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child("Binary file or diff not shown")
            .into_any_element();
    };
    let rows: Vec<AnyElement> = parse_patch(patch)
        .into_iter()
        .map(|line| diff_row(line, cx))
        .collect();
    v_flex()
        .id(SharedString::from(format!("pr-diff-{}", file.filename)))
        .min_w_0()
        .overflow_x_scroll()
        .border_t_1()
        .border_color(cx.theme().border)
        .children(rows)
        .into_any_element()
}

fn diff_row(line: PatchLine, cx: &Context<PrDetailPage>) -> AnyElement {
    let mono = cx.theme().mono_font_family.clone();
    match line {
        PatchLine::Hunk(text) => div()
            .flex_none()
            .whitespace_nowrap()
            .px_2()
            .py_0p5()
            .text_xs()
            .font_family(mono)
            .text_color(cx.theme().muted_foreground)
            .bg(cx.theme().accent.opacity(0.15))
            .child(text),
        PatchLine::Context(text) => div()
            .flex_none()
            .whitespace_nowrap()
            .px_2()
            .text_xs()
            .font_family(mono)
            .child(format!(" {text}")),
        PatchLine::Add(text) => div()
            .flex_none()
            .whitespace_nowrap()
            .px_2()
            .text_xs()
            .font_family(mono)
            .bg(cx.theme().success.opacity(0.12))
            .text_color(cx.theme().success)
            .child(format!("+{text}")),
        PatchLine::Del(text) => div()
            .flex_none()
            .whitespace_nowrap()
            .px_2()
            .text_xs()
            .font_family(mono)
            .bg(cx.theme().danger.opacity(0.12))
            .text_color(cx.theme().danger)
            .child(format!("−{text}")),
    }
    .into_any_element()
}

#[cfg(test)]
mod tests {
    // No `use super::*` — page.rs documents the gpui::* shadowing hazard.
    use crate::gql;
    use crate::gql::MergeMethods;
    use crate::pr_page::{
        BusyOp, ChecksCounts, ChecksTone, ErrorScope, FilesSummary, MergeLineTone, MergeMethod,
        PrStateTone, ReviewTone, TimelineKind, allowed_methods, checks_counts, checks_header_line,
        comment_action_allowed, default_method, files_summary, merge_enabled, merge_state_line,
        pr_state_tone, review_event, review_state_label, rollup_summary, short_sha, split_path,
        split_repo, timeline_order,
    };

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
