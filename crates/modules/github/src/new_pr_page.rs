//! PR creation page (docs/0044) — the repo view's "New pull request"
//! entry point opens this full-content creator sub-page (the 0016 D1
//! takeover pattern, 0024's issue-creator chrome applied to PRs): base/
//! head branch selects (the branch page fetched on open, D4), a required
//! title, a markdown body with a 0017-preview toggle, a draft toggle,
//! one `createPullRequest` mutation (D5 — the minimum input), then the
//! host opens the in-app PR page for the new number and refetches the
//! repo list (D6). Same-repo PRs only in v1 (forks: §9). No optimistic
//! writes; a failure renders a scoped inline error on the form and
//! leaves the draft and branch choices untouched.
//!
//! Draft retention (D7): the 0024 D7 mechanism — the entity stays alive
//! once opened; Cancel/Esc only *hide* it, so reopening restores title,
//! body and the draft flag. Branch lists are the exception: each open
//! re-fetches (fresh branch state beats a stale list), and the choices
//! re-default only when the branch page lands.
//!
//! Close paths: `Esc` (the `DismissNewPrPage` action, bound to this
//! page's `GithubNewPrPage` key context), the Cancel button, the repo
//! crumb, and sign-out (the host auto-drops it when auth drops).

use gpui::prelude::FluentBuilder as _;
use gpui::*;

use gpui_component::{
    ActiveTheme, Disableable, Sizable as _,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::{Textarea, TextareaState},
    searchable_list::SearchableVec,
    select::{Select, SelectState},
    spinner::Spinner,
    v_flex,
};

use serde_json::json;
use std::sync::Arc;

use crate::gql;
use crate::markdown;
use crate::service::{FetchSpec, GithubState, Kind, Mutation, run_mutation, run_spec_tracked};

actions!(github, [DismissNewPrPage]);

/// Ask the hosting `GithubPage` to close (hide) the creator (docs/0044
/// D7 — the draft survives; only the takeover ends).
pub(crate) enum NewPrPageEvent {
    CloseRequested,
    /// The mutation landed: the host drops the creator, opens the PR
    /// page for this row, and refetches the repo list (D6). Boxed — the
    /// synthesized row is fat (docs/0042 D4 shapes) and the event fires
    /// exactly once.
    Created(Box<gql::PullRequest>),
}

/// The branch page's load state (D4): `None` = loading (initial and
/// every reopen), `Ok` = ready, `Err` = a retryable in-form failure —
/// fetch errors are retryable by design, never autofail.
pub(crate) type BranchPage = Option<Result<gql::RepoBranches, String>>;

pub(crate) struct NewPrPage {
    state: Entity<GithubState>,
    /// `owner/repo` the creator was opened for — the host keys draft
    /// retention to it (D7: a repo switch drops the draft).
    pub(crate) repo: String,
    /// The repository's GraphQL node id — the mutation target (D1/D5).
    node_id: String,
    focus_handle: FocusHandle,
    /// Title draft (required — the Create gate reads it, D4). The host
    /// reads it in the retention test; ownership stays with the creator.
    pub(crate) title: Entity<TextareaState>,
    /// Body draft, markdown (optional).
    pub(crate) body: Entity<TextareaState>,
    /// Preview toggle: `true` swaps the body textarea for 0017 blocks of
    /// the current body, parsed once on toggle (D4 — no live reparse per
    /// keystroke).
    preview: bool,
    md: Option<Arc<[markdown::Block]>>,
    /// The branch page (D4), fetched on construction and on every host
    /// open (D7 — branches are not retained).
    branches: BranchPage,
    base_select: Entity<SelectState<SearchableVec<SharedString>>>,
    head_select: Entity<SelectState<SearchableVec<SharedString>>>,
    /// The draft toggle (D4/D5): posts `draft: true`.
    pub(crate) draft: bool,
    /// Single-flight in-flight marker (no optimistic writes, D6).
    pub(crate) busy: Option<CreateBusy>,
    /// The scoped form error (D4): network/HTTP/GraphQL-reject copy,
    /// the draft and branch choices untouched.
    pub(crate) mutation_error: Option<String>,
}

/// The creator's only write — one op identity for busy labeling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CreateBusy {
    Create,
}

impl NewPrPage {
    pub(crate) fn new(
        state: Entity<GithubState>,
        repo: String,
        node_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = cx.new(|cx| TextareaState::new(window, cx).rows(2).placeholder("Title"));
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(8, 40)
                .placeholder("Describe the pull request (Markdown supported — Preview renders it)")
        });
        let base_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(Vec::<SharedString>::new()),
                None,
                window,
                cx,
            )
            .searchable(true)
        });
        let head_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(Vec::<SharedString>::new()),
                None,
                window,
                cx,
            )
            .searchable(true)
        });
        let mut page = Self {
            focus_handle: cx.focus_handle(),
            state,
            node_id,
            title,
            body,
            preview: false,
            md: None,
            branches: None,
            base_select,
            head_select,
            draft: false,
            busy: None,
            mutation_error: None,
            repo,
        };
        page.fetch_branches(window, cx);
        page
    }

    /// Re-arm on every host open (D7): branches re-fetch, the draft
    /// (title/body/draft flag) survives.
    pub(crate) fn refetch_branches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.fetch_branches(window, cx);
    }

    // --- branch source (docs/0044 D4) ----------------------------------------

    /// Fetch the branch page (the picker-source pattern, 0022 §5): a
    /// stateless lane read of [`Kind::RepoBranches`], open-immediately
    /// loading state, failures retryable.
    fn fetch_branches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.read(cx).auth.connected() {
            return; // the page is about to be torn down by the auth gate
        }
        let state = self.state.clone();
        let (owner, name) = crate::pr_page::split_repo(&self.repo);
        let spec = FetchSpec {
            kind: Kind::RepoBranches,
            url: gql::GRAPHQL_ENDPOINT.to_string(),
            etag: None,
            ident: Some(self.repo.clone()),
            max_pages: 1,
            variables: Some(json!({ "owner": owner, "name": name })),
        };
        log::debug!("repo-branches: fetching {}", self.repo);
        self.branches = None;
        cx.notify();
        // The page lands select items + defaults — it needs the window
        // on the way back (spawn_in, the 0024 submit pattern).
        cx.spawn_in(window, async move |this, cx| {
            let result = run_spec_tracked(state, spec, cx).await;
            let _ = cx.update(|window, app| {
                this.update(app, |page, cx| page.apply_branches(result, window, cx))
            });
        })
        .detach();
    }

    /// The branch page came back (200 parses page-side; non-200
    /// forwards to the shared handling, then lands as a retryable
    /// in-form failure).
    fn apply_branches(
        &mut self,
        result: crate::service::SpecResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(m) = &result.network {
            self.branches = Some(Err(format!("Network error: {m}")));
        } else if result.status != 200 {
            let status = result.status;
            self.state.update(cx, |s, cx| {
                s.apply(result);
                cx.notify();
            });
            self.branches = Some(Err(format!("GitHub answered HTTP {status}")));
        } else if let Some(body) = result.body {
            match serde_json::from_str::<gql::Envelope<serde_json::Value>>(body.as_str()) {
                Ok(env) => {
                    let empty = serde_json::Value::Null;
                    let repo = env
                        .data
                        .as_ref()
                        .and_then(|d| d.get("repository"))
                        .unwrap_or(&empty);
                    let page_data = gql::branches_from_value(repo);
                    let (base, head) = default_base_head(&page_data);
                    let names: Vec<SharedString> =
                        page_data.names.iter().map(|n| n.clone().into()).collect();
                    self.base_select.update(cx, |s, cx| {
                        s.set_items(SearchableVec::from(names.clone()), window, cx);
                        if let Some(b) = &base {
                            let b: SharedString = b.clone().into();
                            s.set_selected_value(&b, window, cx);
                        }
                    });
                    self.head_select.update(cx, |s, cx| {
                        s.set_items(SearchableVec::from(names), window, cx);
                        if let Some(h) = &head {
                            let h: SharedString = h.clone().into();
                            s.set_selected_value(&h, window, cx);
                        }
                    });
                    self.branches = Some(Ok(page_data));
                }
                Err(e) => {
                    log::warn!("repo-branches payload: {e}");
                    self.branches = Some(Err("Malformed payload".into()));
                }
            }
        }
        cx.notify();
    }

    /// Retry a failed branch fetch (D4 — retryable, never autofail).
    fn retry_branches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.fetch_branches(window, cx);
    }

    // --- write path (docs/0044 D5/D6) ----------------------------------------

    /// Post `createPullRequest` (D5). The pure gate the button renders
    /// with is re-checked here as the honest guard; single-flight via
    /// `busy`. The selections read straight off the selects at submit
    /// time (no event mirroring — `selected_value` is the truth).
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.node_id.is_empty() {
            return;
        }
        let title = self.title.read(cx).value().trim().to_string();
        let body = self.body.read(cx).value().trim().to_string();
        let base = self
            .base_select
            .read(cx)
            .selected_value()
            .map(|v| v.to_string());
        let head = self
            .head_select
            .read(cx)
            .selected_value()
            .map(|v| v.to_string());
        if !pr_submit_allowed(
            title.is_empty(),
            base.as_deref(),
            head.as_deref(),
            self.busy.is_some(),
        ) {
            return;
        }
        let variables = json!({ "input": {
            "repositoryId": self.node_id,
            "baseRefName": base,
            "headRefName": head,
            "title": title,
            "body": body,
            "draft": self.draft,
        } });
        self.mutation_error = None;
        self.busy = Some(CreateBusy::Create);
        cx.notify();
        let state = self.state.clone();
        let ident = format!("{} new-pr", self.repo);
        cx.spawn_in(window, async move |this, cx| {
            let result =
                run_mutation(state, Mutation::CreatePullRequest, variables, ident, cx).await;
            let _ = cx.update(|window, app| {
                this.update(app, |page, cx| {
                    page.apply_mutation_result(result, window, cx)
                })
            });
        })
        .detach();
    }

    /// The mutation came back (D6): success parses the payload into the
    /// minimal snapshot row and hands it to the host; any failure lands
    /// on the form's error slot with the draft untouched. There is no
    /// page-side refetch — the host owns the list refetch and the new
    /// page's detail fetch re-anchors everything else.
    fn apply_mutation_result(
        &mut self,
        result: crate::service::SpecResult,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = None;
        // The body outlives the branches: only the 200-without-errors
        // path parses it, but `result` itself may be moved into the
        // shared `apply` (non-200 → shared 401/403 handling).
        let body = result.body.clone();
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
            let gql_error = body.as_deref().and_then(|body| {
                serde_json::from_str::<gql::Envelope<serde_json::Value>>(body)
                    .ok()
                    .and_then(|env| env.errors.first().map(|e| e.message.clone()))
            });
            if let Some(message) = gql_error {
                log::warn!("pr-create {}: {message}", self.repo);
                failed = Some(message);
            }
        }
        if let Some(message) = failed {
            self.mutation_error = Some(message);
            cx.notify();
            return;
        }
        let created = body
            .as_deref()
            .and_then(|body| gql::created_pr_from_body(body, &self.repo));
        match created {
            Some(pr) => {
                log::info!("pr created: {}", pr.key);
                cx.emit(NewPrPageEvent::Created(Box::new(pr)));
            }
            None => {
                log::warn!("pr-create {}: malformed payload", self.repo);
                self.mutation_error = Some("Malformed payload".into());
            }
        }
        cx.notify();
    }

    /// Preview toggle (D4): parse the current body once, cache the
    /// blocks; toggling back re-mounts the textarea with the same draft.
    fn toggle_preview(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        if self.preview {
            self.preview = false;
        } else {
            let body = self.body.read(cx).value().trim().to_string();
            // Ambient repo for bare `#123` refs (docs/0017 D1 extension).
            self.md = Some(markdown::parse_markdown_in_repo(&body, Some(&self.repo)));
            self.preview = true;
        }
        cx.notify();
    }

    fn toggle_draft(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.draft = !self.draft;
        cx.notify();
    }

    fn request_close(&mut self, cx: &mut Context<Self>) {
        cx.emit(NewPrPageEvent::CloseRequested);
    }
}

/// The pure default choice (docs/0044 D4): base = the repository's
/// default branch when it is known (fallback: the first branch), head =
/// the first branch that differs from base. `None`s mean "no differing
/// pair exists" — the Create gate reads them as not-ready.
pub(crate) fn default_base_head(page: &gql::RepoBranches) -> (Option<String>, Option<String>) {
    let first = page.names.first().cloned();
    let base = page.default_branch.clone().or_else(|| first.clone());
    // The find already guarantees head != base; a single-branch repo
    // yields head = None and an empty list yields both None — the Create
    // gate reads either as not-ready (D4).
    let head = page
        .names
        .iter()
        .find(|n| Some(n.as_str()) != base.as_deref())
        .cloned();
    (base, head)
}

/// The Create gate (docs/0044 D4): a non-empty title, two differing
/// branch selections, no in-flight write. Input validity, not a
/// permission gate.
pub(crate) fn pr_submit_allowed(
    title_empty: bool,
    base: Option<&str>,
    head: Option<&str>,
    busy: bool,
) -> bool {
    !title_empty && !busy && base.is_some() && head.is_some() && base != head
}

/// docs/0044 D4 — the select footer's truncation copy: the true count
/// exceeds the alphabetical page we hold.
pub(crate) fn branches_truncated(total_count: i64, names: &[String]) -> bool {
    total_count > names.len() as i64
}

impl Render for NewPrPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy.is_some();
        let title_empty = self.title.read(cx).value().trim().is_empty();
        let base = self
            .base_select
            .read(cx)
            .selected_value()
            .map(|v| v.to_string());
        let head = self
            .head_select
            .read(cx)
            .selected_value()
            .map(|v| v.to_string());
        // D4: Create disabled until the title is non-empty and the two
        // branches differ — input validity, not a permission gate.
        let submit_ok = pr_submit_allowed(title_empty, base.as_deref(), head.as_deref(), busy);

        v_flex()
            .id("new-pr")
            .key_context("GithubNewPrPage")
            .track_focus(&self.focus_handle)
            // D7: Esc always asks the host to close — the creator has no
            // nested editors for it to consume first.
            .on_action(cx.listener(|this, _: &DismissNewPrPage, _, cx| {
                this.request_close(cx);
            }))
            .size_full()
            .min_w_0()
            .overflow_y_scroll()
            .p_4()
            .gap_3()
            // Header: what is being created, and where it will land.
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::MEDIUM)
                            .child("New pull request"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.repo.clone()),
                    ),
            )
            // The form card (D4).
            .child(
                v_flex()
                    .id("new-pr-form")
                    .debug_selector(|| "new-pr-form".to_string())
                    .w_full()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .gap_2()
                            .p_3()
                            // The branch pair (D4): defaults land with the
                            // page; while loading the selects sit empty.
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        v_flex()
                                            .gap_1()
                                            .flex_1()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child("Base"),
                                            )
                                            .child(
                                                div()
                                                    .id("new-pr-base")
                                                    .debug_selector(|| "new-pr-base".to_string())
                                                    .w_full()
                                                    .child(
                                                        Select::new(&self.base_select)
                                                            .placeholder("Base branch…")
                                                            .small()
                                                            .w_full(),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        v_flex()
                                            .gap_1()
                                            .flex_1()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child("Head"),
                                            )
                                            .child(
                                                div()
                                                    .id("new-pr-head")
                                                    .debug_selector(|| "new-pr-head".to_string())
                                                    .w_full()
                                                    .child(
                                                        Select::new(&self.head_select)
                                                            .placeholder("Head branch…")
                                                            .small()
                                                            .w_full(),
                                                    ),
                                            ),
                                    ),
                            )
                            // The branch slot (D8): loading / retryable
                            // failure / truncation hint.
                            .child(self.render_branch_slot(cx))
                            .child(Textarea::new(&self.title).w_full())
                            // Body + its Preview toggle.
                            .child(
                                h_flex().justify_end().child(
                                    Button::new("new-pr-preview")
                                        .debug_selector(|| "new-pr-preview".to_string())
                                        .label(if self.preview { "Edit" } else { "Preview" })
                                        .ghost()
                                        .small()
                                        .disabled(busy)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.toggle_preview(cx);
                                        })),
                                ),
                            )
                            .child(if self.preview {
                                markdown::render_blocks(
                                    self.md.as_deref().unwrap_or_default(),
                                    "new-pr-body-preview",
                                    cx,
                                )
                            } else {
                                Textarea::new(&self.body).w_full().into_any_element()
                            })
                            // The draft toggle (D4): a row toggle, the
                            // checkbox display-only (the house pattern).
                            .child(
                                h_flex()
                                    .id("new-pr-draft")
                                    .debug_selector(|| "new-pr-draft".to_string())
                                    .cursor_pointer()
                                    .gap_2()
                                    .items_center()
                                    .w_full()
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .hover(|s| s.bg(cx.theme().accent.opacity(0.08)))
                                    .on_click(cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
                                        this.toggle_draft(cx);
                                    }))
                                    .child(
                                        Checkbox::new("new-pr-draft-check")
                                            .checked(self.draft)
                                            .small()
                                            .tab_stop(false),
                                    )
                                    .child(div().text_sm().child("Draft pull request")),
                            ),
                    ),
            )
            // Scoped error slot (D4): failure copy stays on the form.
            .when_some(self.mutation_error.clone(), |col, message| {
                col.child(
                    div()
                        .id("new-pr-error")
                        .debug_selector(|| "new-pr-error".to_string())
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(message),
                )
            })
            // Footer: Cancel retains the draft (D7); Create posts.
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .when(busy, |row| row.child(Spinner::new().small()))
                    .child(
                        Button::new("new-pr-cancel")
                            .debug_selector(|| "new-pr-cancel".to_string())
                            .label("Cancel")
                            .ghost()
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.request_close(cx);
                            })),
                    )
                    .child(
                        Button::new("new-pr-create")
                            .debug_selector(|| "new-pr-create".to_string())
                            .label("Create pull request")
                            .primary()
                            .disabled(!submit_ok)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.submit(window, cx);
                            })),
                    ),
            )
    }
}

impl NewPrPage {
    /// The branch slot (docs/0044 D4/D8): a loading cell, a retryable
    /// failure row, or the truncation hint when the alphabetical page
    /// covers fewer branches than the repo has.
    fn render_branch_slot(&self, cx: &Context<Self>) -> AnyElement {
        match &self.branches {
            None => div()
                .id("new-pr-branches")
                .debug_selector(|| "new-pr-branches".to_string())
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Loading branches…")
                .into_any_element(),
            Some(Err(message)) => h_flex()
                .id("new-pr-branches")
                .debug_selector(|| "new-pr-branches".to_string())
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(message.clone()),
                )
                .child(
                    Button::new("new-pr-branches-retry")
                        .debug_selector(|| "new-pr-branches-retry".to_string())
                        .label("Retry")
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.retry_branches(window, cx);
                        })),
                )
                .into_any_element(),
            Some(Ok(page)) => {
                if branches_truncated(page.total_count, &page.names) {
                    div()
                        .id("new-pr-branches")
                        .debug_selector(|| "new-pr-branches".to_string())
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "first {} of {} branches (alphabetical)",
                            page.names.len(),
                            page.total_count
                        ))
                        .into_any_element()
                } else {
                    v_flex().into_any_element()
                }
            }
        }
    }
}

impl EventEmitter<NewPrPageEvent> for NewPrPage {}

impl Focusable for NewPrPage {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    // NOTE: no `use super::*` here — this file's `use gpui::*` would
    // shadow the builtin `#[test]` attribute (page.rs lesson). Import
    // selectively.
    use super::{NewPrPage, branches_truncated, default_base_head, pr_submit_allowed};
    use crate::gql;
    use crate::http::Http;
    use crate::service::{GithubCache, GithubState};
    use gpui::AppContext as _;

    #[test]
    fn pr_submit_gate_table() {
        // Title validity + a differing branch pair gate the post; busy
        // never allows a second one (docs/0044 D4).
        assert!(pr_submit_allowed(false, Some("main"), Some("feat"), false));
        assert!(!pr_submit_allowed(true, Some("main"), Some("feat"), false));
        assert!(!pr_submit_allowed(false, None, Some("feat"), false));
        assert!(!pr_submit_allowed(false, Some("main"), None, false));
        assert!(!pr_submit_allowed(false, Some("main"), Some("main"), false));
        assert!(!pr_submit_allowed(false, Some("main"), Some("feat"), true));
    }

    #[test]
    fn default_base_head_table() {
        // Default branch present → base; head = first differing.
        let page = gql::RepoBranches {
            default_branch: Some("main".into()),
            total_count: 3,
            names: vec!["feat".into(), "main".into(), "wip".into()],
        };
        let (base, head) = default_base_head(&page);
        assert_eq!(base.as_deref(), Some("main"));
        assert_eq!(head.as_deref(), Some("feat"));
        // No defaultBranchRef → the first branch is the base.
        let page = gql::RepoBranches {
            default_branch: None,
            total_count: 2,
            names: vec!["dev".into(), "main".into()],
        };
        let (base, head) = default_base_head(&page);
        assert_eq!(base.as_deref(), Some("dev"));
        assert_eq!(head.as_deref(), Some("main"));
        // Single branch → no differing head, no PR (D4: not-ready).
        let page = gql::RepoBranches {
            default_branch: Some("main".into()),
            total_count: 1,
            names: vec!["main".into()],
        };
        let (base, head) = default_base_head(&page);
        assert_eq!(base.as_deref(), Some("main"));
        assert_eq!(head, None);
        // Empty list → nothing at all.
        let page = gql::RepoBranches::default();
        assert_eq!(default_base_head(&page), (None, None));
    }

    #[test]
    fn branches_truncated_table() {
        let names: Vec<String> = (0..100).map(|i| format!("b{i:03}")).collect();
        assert!(branches_truncated(140, &names));
        assert!(!branches_truncated(100, &names));
        assert!(!branches_truncated(3, &names));
    }

    #[test]
    fn branches_parse_degrades() {
        let repo = serde_json::json!({
            "defaultBranchRef": {"name": "main"},
            "refs": {
                "totalCount": 3,
                "nodes": [{"name": "feat"}, {"name": "main"}, {"name": null}]
            }
        });
        let page = gql::branches_from_value(&repo);
        assert_eq!(page.default_branch.as_deref(), Some("main"));
        assert_eq!(page.total_count, 3);
        // A name-less node drops alone (0007 D3).
        assert_eq!(page.names, vec!["feat".to_string(), "main".to_string()]);
        // Absent refs/defaultBranchRef degrade to the zero page.
        let page = gql::branches_from_value(&serde_json::json!({}));
        assert_eq!(page, gql::RepoBranches::default());
    }

    #[test]
    fn created_pr_parses_minimal_row() {
        let body = serde_json::json!({
            "data": {"createPullRequest": {"pullRequest": {
                "number": 42,
                "title": "Fresh PR",
                "url": "https://github.com/o/r/pull/42",
                "state": "OPEN",
                "isDraft": true,
                "updatedAt": "2026-09-18T10:00:00Z"
            }}}
        })
        .to_string();
        let row = gql::created_pr_from_body(&body, "o/r").expect("row");
        assert_eq!(row.key, "o/r#42");
        assert_eq!(row.number, 42);
        assert_eq!(row.title, "Fresh PR");
        assert!(row.draft, "the draft flag round-trips (D5)");
        assert_eq!(row.state.as_deref(), Some("OPEN"));
        assert_eq!(row.url, "https://github.com/o/r/pull/42");
        assert_eq!(row.database_id, None);
        assert!(row.author.is_none());
        assert!(row.labels.is_none() && row.review_requests.is_none());
    }

    #[test]
    fn created_pr_rejects_errors_and_malformed() {
        // GraphQL-level rejection (data null) — e.g. "No commits between
        // main and feat" lands here as a scoped form error (D4).
        let errors = serde_json::json!({
            "data": null,
            "errors": [{"message": "Head and base branches must be different"}]
        })
        .to_string();
        assert!(gql::created_pr_from_body(&errors, "o/r").is_none());
        assert!(gql::created_pr_from_body("not json", "o/r").is_none());
    }

    // --- probes ---------------------------------------------------------------

    /// Serves the branch page for `RepoBranches` reads, fails everything
    /// else (the mutation) — the probe drives open → branches land →
    /// create → scoped error (docs/0044 §5). The mutation's variables
    /// are recorded for the minimum-input assertion (D5).
    struct FixtureHttp(std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>);
    impl Http for FixtureHttp {
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
            doc: &str,
            variables: &serde_json::Value,
        ) -> Result<crate::http::HttpResult, crate::http::HttpError> {
            if !doc.contains("RepoBranches") {
                self.0.lock().unwrap().push(variables.clone());
            }
            if doc.contains("RepoBranches") {
                let payload = serde_json::json!({
                    "data": {"repository": {
                        "defaultBranchRef": {"name": "main"},
                        "refs": {"totalCount": 2, "nodes": [
                            {"name": "feat"}, {"name": "main"}
                        ]}
                    }}
                });
                return Ok(crate::http::HttpResult {
                    status: 200,
                    etag: None,
                    rate: None,
                    body: Some(payload.to_string()),
                });
            }
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

    /// Creator probe page: FixtureHttp serves the branch page and fails
    /// the mutation, so Create runs the full chain — busy → error slot
    /// → busy cleared, draft intact (docs/0044 §5).
    type Recorder = std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>;

    fn probe_creator(
        cx: &mut gpui::TestAppContext,
    ) -> (
        gpui::Entity<NewPrPage>,
        &mut gpui::VisualTestContext,
        Recorder,
    ) {
        cx.update(gpui_component::init);
        // The write really spawns (FixtureHttp fails it) — the in-flight
        // tracker global must exist, same contract as main().
        cx.update(dashboard_core::status::init);
        let sent: Recorder = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let state = cx.update(|cx| {
            let client: std::sync::Arc<dyn Http> = std::sync::Arc::new(FixtureHttp(sent.clone()));
            let store: std::sync::Arc<dyn crate::auth::TokenStore> =
                std::sync::Arc::new(ProbeStore);
            let entity = cx.new(|_| {
                GithubState::new(
                    client,
                    store,
                    Default::default(),
                    Default::default(),
                    GithubCache::new(dashboard_core::paths::AppPaths::init()),
                )
            });
            // The fetch gates on auth (the page would be torn down
            // otherwise) — force the connected state (0026 §4.3 pattern).
            entity.update(cx, |s, _| s.auth = crate::service::AuthStatus::Connected);
            entity
        });
        let page_and_vcx = cx.add_window_view(|window, cx| {
            NewPrPage::new(state, "o/r".into(), "R_9".into(), window, cx)
        });
        (page_and_vcx.0, page_and_vcx.1, sent)
    }

    #[gpui::test]
    async fn branches_land_and_defaults_apply(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.background_executor().allow_parking());
        let (page, vcx, _) = probe_creator(cx);
        // The fetch really runs; let it land.
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                let page_data = match &page.branches {
                    Some(Ok(p)) => p.clone(),
                    other => panic!("branch page expected, got {other:?}"),
                };
                assert_eq!(page_data.names.len(), 2);
                // Defaults applied to the selects (D4): base=main,
                // head=feat — read straight off the widgets, the submit
                // path's own source of truth.
                let base = page
                    .base_select
                    .read(cx)
                    .selected_value()
                    .map(|v| v.to_string());
                let head = page
                    .head_select
                    .read(cx)
                    .selected_value()
                    .map(|v| v.to_string());
                assert_eq!(base.as_deref(), Some("main"));
                assert_eq!(head.as_deref(), Some("feat"));
            });
        });
    }

    #[gpui::test]
    async fn create_posts_once_error_lands_draft_survives(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.background_executor().allow_parking());
        let (page, vcx, sent) = probe_creator(cx);
        vcx.run_until_parked();
        // Title empty → Create disabled (D4): clicking is a no-op.
        let create = vcx
            .debug_bounds("new-pr-create")
            .expect("create button visible");
        vcx.simulate_click(create.center(), gpui::Modifiers::default());
        let _ = vcx.debug_bounds("new-pr-create"); // flush frames
        vcx.update(|_, cx| {
            page.update(cx, |page, _| {
                assert!(page.mutation_error.is_none(), "no post expected");
                assert_eq!(page.busy, None);
            });
        });
        // Type a title, flip the draft flag, then Create.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.title
                    .update(cx, |s, cx| s.set_value("A title", window, cx));
                page.toggle_draft(cx);
                cx.notify();
            });
        });
        let create = vcx.debug_bounds("new-pr-create").expect("create visible");
        vcx.simulate_click(create.center(), gpui::Modifiers::default());
        let _ = vcx.debug_bounds("new-pr-create"); // flush frames
        vcx.update(|_, cx| {
            page.update(cx, |page, _| {
                // The full chain ran: post → FixtureHttp failed it → the
                // error landed on the form slot; busy already cycled
                // back to None; draft and title survived (D4/D7).
                let error = page.mutation_error.as_deref().expect("error expected");
                assert!(error.contains("Network error"));
                assert_eq!(page.busy, None);
                assert!(page.draft, "the draft flag survives a failure");
            });
        });
        // The scoped error slot renders (D8 probe).
        assert!(vcx.debug_bounds("new-pr-error").is_some());

        // D5 — the mutation posted the minimum input, once, with the
        // draft flag the row toggle set.
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1, "exactly one post");
        let input = &sent[0]["input"];
        let keys: Vec<&str> = input
            .as_object()
            .expect("input object")
            .keys()
            .map(|k| k.as_str())
            .collect();
        assert_eq!(
            keys,
            vec![
                "repositoryId",
                "baseRefName",
                "headRefName",
                "title",
                "body",
                "draft"
            ],
            "no headRepositoryId, no maintainerCanModify — the §2 minimum"
        );
        assert_eq!(input["repositoryId"], "R_9");
        assert_eq!(input["baseRefName"], "main");
        assert_eq!(input["headRefName"], "feat");
        assert_eq!(input["draft"], true);
        assert_eq!(input["title"], "A title");
    }
}
