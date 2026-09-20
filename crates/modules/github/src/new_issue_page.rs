//! Issue creation page (docs/0024) — the repo view's "New issue" entry
//! point opens this full-content creator sub-page (the 0016 D1 takeover
//! pattern): a required title, a markdown body with a 0017-preview
//! toggle, one `createIssue` mutation (D5 — the minimum input), then the
//! host opens the in-app issue page for the new number and refetches the
//! repo list (D6). No optimistic writes; a failure renders a scoped
//! inline error on the form and leaves the draft untouched.
//!
//! Draft retention (D7): the entity stays alive once opened — Cancel/Esc
//! only *hide* it (the host keeps it in memory keyed to its repo), so
//! reopening restores title and body. The host drops it on successful
//! creation and on repo switch.
//!
//! Close paths: `Esc` (the `DismissNewIssuePage` action, bound to this
//! page's `GithubNewIssuePage` key context), the Cancel button, the repo
//! crumb, and sign-out (the host auto-drops it when auth drops).

use gpui::prelude::FluentBuilder as _;
use gpui::*;

use gpui_component::{
    ActiveTheme, Disableable, Sizable as _,
    button::{Button, ButtonVariants},
    h_flex,
    input::{InputEvent, Textarea, TextareaState},
    spinner::Spinner,
    v_flex,
};

use serde_json::json;
use std::sync::Arc;

use crate::drafts::{self, DraftHandle};
use crate::gql;
use crate::markdown;
use crate::service::{
    FetchSpec, GithubState, Kind, Mutation, SpecResult, run_mutation, run_spec_tracked,
};

actions!(github, [DismissNewIssuePage]);

/// Ask the hosting `GithubPage` to close (hide) the creator (docs/0024
/// D7 — the draft survives; only the takeover ends).
pub(crate) enum NewIssuePageEvent {
    CloseRequested,
    /// The mutation landed: the host drops the creator, opens the issue
    /// page for this row, and refetches the repo list (D6).
    Created(gql::Issue),
}

pub(crate) struct NewIssuePage {
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
    body: Entity<TextareaState>,
    /// Creator draft persistence (docs/0045 D4/D8): keyed
    /// `new-issue@<repo>`; one handle covers title + body (each change
    /// schedules a full snapshot of both).
    draft: DraftHandle,
    /// A draft was restored at construction — one muted hint row on the
    /// form card until the first real edit (0045 D4).
    draft_restored: bool,
    /// docs/0046 D1 — the repo's Markdown issue templates, fetched on
    /// creator open (the [`Kind::RepoBranches`] mirror). `None` until a
    /// 200 parses (pending *and* failed look identical — D6: the row
    /// stays hidden either way, creation never blocks); an empty list
    /// means the repo simply has none.
    templates: Option<Vec<gql::IssueTemplate>>,
    /// docs/0046 D5 — `blank_issues_enabled` (config.yml via the
    /// Contents lane). `true` until a successful fetch + parse says
    /// otherwise; a failed/absent config keeps blank allowed (D6: the
    /// wrong constraint is worse than an over-offered pill).
    blank_allowed: bool,
    /// docs/0046 D2 — the selected pill (`None` = Blank). Selection is
    /// a prefill affordance, not persistent state: it survives re-opens
    /// so the highlight keeps matching parked text.
    selected_template: Option<usize>,
    /// docs/0046 D5 — the blank-disabled auto-selection is armed until
    /// something claims the fields: a restored draft (D4 — the user's
    /// own words win), any edit, or one auto/pick firing. One-shot per
    /// entity lifetime.
    autoselect_armed: bool,
    /// Preview toggle: `true` swaps the body textarea for 0017 blocks of
    /// the current body, parsed once on toggle (D4 — no live reparse per
    /// keystroke).
    preview: bool,
    md: Option<Arc<[markdown::Block]>>,
    /// Single-flight in-flight marker (no optimistic writes, D6).
    pub(crate) busy: Option<CreateBusy>,
    /// The scoped form error (D4): network/HTTP/GraphQL-reject copy, the
    /// draft untouched.
    pub(crate) mutation_error: Option<String>,
}

/// The creator's only write — one op identity for busy labeling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CreateBusy {
    Create,
}

impl NewIssuePage {
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
                .placeholder("Describe the issue (Markdown supported — Preview renders it)")
        });
        // 0045 D4/D6: restore this repo's parked draft. `set_value`
        // suppresses events — the prefill cannot schedule a save. The
        // entity only rebuilds after a drop (repo switch, restart), so a
        // live creator's text is never touched by restore logic.
        let draft = state.read(cx).drafts.handle(drafts::new_issue_key(&repo));
        let restored = draft.load();
        let draft_restored = restored.is_some();
        if let Some(payload) = restored {
            if let Some(title_text) = payload.title {
                title.update(cx, |state, cx| state.set_value(title_text, window, cx));
            }
            body.update(cx, |state, cx| state.set_value(payload.body, window, cx));
        }
        // 0045 D3/D4: either field changing schedules a full snapshot and
        // retires the restore hint.
        cx.subscribe_in(&title, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.draft_changed(cx);
            }
        })
        .detach();
        cx.subscribe_in(&body, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.draft_changed(cx);
            }
        })
        .detach();
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            state,
            node_id,
            title,
            body,
            draft,
            draft_restored,
            preview: false,
            md: None,
            busy: None,
            mutation_error: None,
            templates: None,
            blank_allowed: true,
            selected_template: None,
            // D4: a restored draft owns the fields — the blank-disabled
            // auto-selection never fires over it.
            autoselect_armed: !draft_restored,
            repo,
        };
        // docs/0046 D1/D5: templates + blank config fetch on open (the
        // 0044 branches pattern — stateless lanes, page-owned results).
        this.fetch_templates(window, cx);
        this
    }

    /// 0045 D3/D4: a creator field changed — debounce-save the full
    /// snapshot (title + body) and retire the restore hint.
    fn draft_changed(&mut self, cx: &mut Context<Self>) {
        self.draft_restored = false;
        let title = self.title.read(cx).value().trim().to_string();
        let body = self.body.read(cx).value().to_string();
        let title = (!title.is_empty()).then_some(title);
        self.draft.schedule(title, body, cx);
    }

    // --- templates (docs/0046) -----------------------------------------------

    /// docs/0046 D1/D5 — fetch the repo's Markdown templates and its
    /// `blank_issues_enabled` config, one stateless lane each (the 0044
    /// `fetch_branches` mirror). Both are optional affordances: any
    /// failure log-and-swallows (0005 §4.7) and the creator stays
    /// exactly as before — creation never blocks (D6).
    fn fetch_templates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.read(cx).auth.connected() {
            return; // the page is about to be torn down by the auth gate
        }
        let state = self.state.clone();
        let (owner, name) = crate::pr_page::split_repo(&self.repo);
        let templates_spec = FetchSpec {
            kind: Kind::RepoIssueTemplates,
            url: gql::GRAPHQL_ENDPOINT.to_string(),
            etag: None,
            ident: Some(self.repo.clone()),
            max_pages: 1,
            variables: Some(json!({ "owner": owner, "name": name })),
        };
        let config_spec = FetchSpec {
            kind: Kind::RepoTemplatesConfig,
            url: crate::api::issue_template_config_url(owner, name),
            etag: None,
            ident: Some(self.repo.clone()),
            max_pages: 1,
            variables: None,
        };
        self.templates = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let templates = run_spec_tracked(state.clone(), templates_spec, cx).await;
            let _ = cx.update(|window, app| {
                this.update(app, |page, cx| page.apply_templates(templates, window, cx))
            });
            let config = run_spec_tracked(state, config_spec, cx).await;
            let _ = cx.update(|window, app| {
                this.update(app, |page, cx| page.apply_config(config, window, cx))
            });
        })
        .detach();
    }

    /// 0044 D7 — the host re-arms the lanes on every creator re-open;
    /// the draft (and whatever text is in the fields) survives.
    pub(crate) fn refetch_templates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.fetch_templates(window, cx);
    }

    /// The templates page came back. A 200 always resolves the lane —
    /// an empty list means the repo has no Markdown templates (the row
    /// stays hidden); anything else log-and-swallows (D6).
    fn apply_templates(&mut self, result: SpecResult, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(m) = &result.network {
            log::warn!("repo-issue-templates: network: {m}");
            return;
        }
        if result.status != 200 {
            log::warn!("repo-issue-templates: HTTP {}", result.status);
            return;
        }
        let Some(body) = result.body else {
            return;
        };
        let Ok(env) = serde_json::from_str::<gql::Envelope<serde_json::Value>>(body.as_str())
        else {
            log::warn!("repo-issue-templates: unparseable body");
            return;
        };
        let empty = serde_json::Value::Null;
        let repo = env
            .data
            .as_ref()
            .and_then(|d| d.get("repository"))
            .unwrap_or(&empty);
        self.templates = Some(gql::templates_from_value(repo));
        self.maybe_autoselect(window, cx);
        cx.notify();
    }

    /// The config.yml Contents GET came back. 404 is the common shape
    /// (no config file) — blank stays allowed; only an explicit
    /// `blank_issues_enabled: false` hides the Blank pill (D5/D6).
    fn apply_config(&mut self, result: SpecResult, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(m) = &result.network {
            log::warn!("repo-templates-config: network: {m}");
            return;
        }
        if result.status != 200 {
            log::debug!(
                "repo-templates-config: HTTP {} — blank allowed",
                result.status
            );
            return;
        }
        let Some(body) = result.body else {
            return;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(body.as_str()) else {
            log::warn!("repo-templates-config: unparseable body");
            return;
        };
        let blank_allowed = value
            .get("content")
            .and_then(serde_json::Value::as_str)
            .and_then(decode_base64)
            .and_then(|yaml| blank_issues_enabled_from_config(&yaml));
        if let Some(enabled) = blank_allowed {
            self.blank_allowed = enabled;
            self.maybe_autoselect(window, cx);
        }
        cx.notify();
    }

    /// docs/0046 D5 — blank is disabled and nothing claims the fields:
    /// the first template preselects so the creator never starts empty.
    /// One-shot per entity — a restored draft (D4), an edit, or one
    /// firing disarms it.
    fn maybe_autoselect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.autoselect_armed || self.blank_allowed || self.selected_template.is_some() {
            return;
        }
        let Some(templates) = self.templates.as_deref().filter(|t| !t.is_empty()) else {
            return;
        };
        let first = templates[0].clone();
        self.apply_template(first, 0, window, cx);
    }

    /// docs/0046 D2 — a pill click: the explicit template pick.
    fn select_template(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(template) = self
            .templates
            .as_deref()
            .and_then(|t| t.get(index))
            .cloned()
        else {
            return;
        };
        self.apply_template(template, index, window, cx);
    }

    /// docs/0046 D3/D4 — one template lands in the fields: the parked
    /// draft is discarded first (scaffolding never resurrects as "your
    /// draft"), body always prefills, title only when the template
    /// ships a non-empty one (never overwrite with ""), then the new
    /// content parks as the live draft (`draft_changed`).
    fn apply_template(
        &mut self,
        template: gql::IssueTemplate,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.draft.discard();
        if let Some(title) = &template.title {
            self.title.update(cx, |state, cx| {
                state.set_value(title.clone(), window, cx);
            });
        }
        let body = template.body.unwrap_or_default();
        self.body
            .update(cx, |state, cx| state.set_value(body, window, cx));
        self.autoselect_armed = false;
        self.selected_template = Some(index);
        self.draft_changed(cx);
        cx.notify();
    }

    /// docs/0046 D2/D4 — the Blank pill: the explicit "start over".
    /// Same discard-first contract as a template pick, then both fields
    /// clear (the empty snapshot discards — no residue file, 0045 D5).
    fn pick_blank(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.draft.discard();
        self.title
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.body
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.autoselect_armed = false;
        self.selected_template = None;
        self.draft_changed(cx);
        cx.notify();
    }

    // --- write path (docs/0024 D5/D6) ---------------------------------------

    /// Post `createIssue` (D5). The pure gate the button renders with is
    /// re-checked here as the honest guard; single-flight via `busy`.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.node_id.is_empty() {
            return;
        }
        let title = self.title.read(cx).value().trim().to_string();
        let body = self.body.read(cx).value().trim().to_string();
        if !create_submit_allowed(title.is_empty(), self.busy.is_some()) {
            return;
        }
        let variables = json!({ "input": {
            "repositoryId": self.node_id,
            "title": title,
            "body": body,
        } });
        self.mutation_error = None;
        self.busy = Some(CreateBusy::Create);
        cx.notify();
        let state = self.state.clone();
        let ident = format!("{} new-issue", self.repo);
        cx.spawn_in(window, async move |this, cx| {
            let result = run_mutation(state, Mutation::CreateIssue, variables, ident, cx).await;
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
                log::warn!("issue-create {}: {message}", self.repo);
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
            .and_then(|body| gql::created_issue_from_body(body, &self.repo));
        match created {
            Some(issue) => {
                log::info!("issue created: {}", issue.key);
                // 0045 D5: the created issue leaves no draft behind
                // (cancel first — no pre-submit write lands after this).
                self.draft.discard();
                cx.emit(NewIssuePageEvent::Created(issue));
            }
            None => {
                log::warn!("issue-create {}: malformed payload", self.repo);
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

    fn request_close(&mut self, cx: &mut Context<Self>) {
        cx.emit(NewIssuePageEvent::CloseRequested);
    }
}

impl Render for NewIssuePage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy.is_some();
        let title_empty = self.title.read(cx).value().trim().is_empty();
        // D4: Create disabled until the title is non-empty — input
        // validity, not a permission gate.
        let submit_ok = create_submit_allowed(title_empty, busy);
        // docs/0046 D2 — the template pill row (built before the element
        // chain: an imperative loop over an optional list reads cleaner
        // than nested when_some/children combinators). Hidden entirely
        // while the lane is pending/failed/empty; the Blank pill hides
        // when config.yml disables blank issues (D5).
        let mut template_row: Option<AnyElement> = None;
        if let Some(templates) = self.templates.as_ref().filter(|t| !t.is_empty()) {
            let mut row = h_flex()
                .id("new-issue-template-row")
                .debug_selector(|| "new-issue-template-row".to_string())
                .flex_wrap()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Template"),
                );
            for (index, template) in templates.iter().enumerate() {
                let selected = self.selected_template == Some(index);
                let id: SharedString = format!("new-issue-tpl-{index}").into();
                let mut button = if selected {
                    Button::new(id).primary().xsmall()
                } else {
                    Button::new(id).ghost().xsmall()
                };
                button = button
                    .debug_selector(move || format!("new-issue-tpl-{index}"))
                    .label(template.name.clone());
                if let Some(about) = &template.about {
                    button = button.tooltip(about.clone());
                }
                row = row.child(button.on_click(cx.listener(move |this, _, window, cx| {
                    this.select_template(index, window, cx);
                })));
            }
            if self.blank_allowed {
                let blank_selected = self.selected_template.is_none();
                let blank = if blank_selected {
                    Button::new("new-issue-tpl-blank").primary().xsmall()
                } else {
                    Button::new("new-issue-tpl-blank").ghost().xsmall()
                };
                row = row.child(
                    blank
                        .debug_selector(|| "new-issue-tpl-blank".to_string())
                        .label("Blank")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.pick_blank(window, cx);
                        })),
                );
            }
            template_row = Some(row.into_any_element());
        }
        v_flex()
            .id("new-issue")
            .key_context("GithubNewIssuePage")
            .track_focus(&self.focus_handle)
            // D7: Esc always asks the host to close — the creator has no
            // nested editors for it to consume first.
            .on_action(cx.listener(|this, _: &DismissNewIssuePage, _, cx| {
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
                            .child("New issue"),
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
                    .id("new-issue-form")
                    .debug_selector(|| "new-issue-form".to_string())
                    .w_full()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .gap_2()
                            .p_3()
                            // docs/0046 D2 — templates live above the title
                            // field: scaffolding choice before input. The
                            // row only exists once the lane landed non-empty
                            // (pending/failed/empty → nothing, D6).
                            .when_some(template_row, |col, row| col.child(row))
                            .when(self.draft_restored, |col| {
                                col.child(
                                    div()
                                        .id("new-issue-draft-restored")
                                        .debug_selector(|| "new-issue-draft-restored".to_string())
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            "Draft restored — clearing both fields discards it.",
                                        ),
                                )
                            })
                            .child(Textarea::new(&self.title).w_full())
                            // Body + its Preview toggle.
                            .child(
                                h_flex().justify_end().child(
                                    Button::new("new-issue-preview")
                                        .debug_selector(|| "new-issue-preview".to_string())
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
                                    "new-issue-body-preview",
                                    cx,
                                )
                            } else {
                                Textarea::new(&self.body).w_full().into_any_element()
                            }),
                    ),
            )
            // Scoped error slot (D4): failure copy stays on the form.
            .when_some(self.mutation_error.clone(), |col, message| {
                col.child(
                    div()
                        .id("new-issue-error")
                        .debug_selector(|| "new-issue-error".to_string())
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
                        Button::new("new-issue-cancel")
                            .debug_selector(|| "new-issue-cancel".to_string())
                            .label("Cancel")
                            .ghost()
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.request_close(cx);
                            })),
                    )
                    .child(
                        Button::new("new-issue-create")
                            .debug_selector(|| "new-issue-create".to_string())
                            .label("Create issue")
                            .primary()
                            .disabled(!submit_ok)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.submit(window, cx);
                            })),
                    ),
            )
    }
}

impl EventEmitter<NewIssuePageEvent> for NewIssuePage {}

impl Focusable for NewIssuePage {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

// ---------------------------------------------------------------------------
// Pure helpers — table-tested.
// ---------------------------------------------------------------------------

/// The Create gate (docs/0024 D4): a non-empty title and no in-flight
/// write. Input validity, not a permission gate.
pub(crate) fn create_submit_allowed(title_empty: bool, busy: bool) -> bool {
    !title_empty && !busy
}

/// docs/0046 D5 — decode one standard base64 payload (RFC 4648, the
/// Contents API's `content` field): whitespace (the API wraps at 60
/// chars) is skipped, the alphabet is strict, `=` padding only ever
/// terminates. Any other byte → `None` (the config fetch then leaves
/// blank issues enabled — D6). A ~30-line table decode beats a new
/// dependency for one one-line file (docs/0046 §3 D5).
fn decode_base64(input: &str) -> Option<String> {
    fn value(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let unpadded = bytes.len() - bytes.iter().rev().take_while(|b| **b == b'=').count();
    // Padding is only ever terminal: one or two `=`, total length a
    // multiple of 4, and the data part leaves no impossible tail.
    if bytes.len() % 4 != 0 || bytes.len() - unpadded > 2 {
        return None;
    }
    let mut out = Vec::with_capacity(unpadded / 4 * 3 + 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().filter(|b| **b == b'=').count();
        let mut group = 0u32;
        for (i, b) in chunk.iter().enumerate() {
            let v = if *b == b'=' {
                if i < 2 {
                    return None; // `=` inside the data section
                }
                0
            } else {
                u32::from(value(*b)?)
            };
            group = (group << 6) | v;
        }
        out.push((group >> 16) as u8);
        if pad < 2 {
            out.push((group >> 8) as u8);
        }
        if pad < 1 {
            out.push(group as u8);
        }
    }
    String::from_utf8(out).ok()
}

/// docs/0046 D5 — the one key we need from `.github/ISSUE_TEMPLATE/`
/// `config.yml`. First `blank_issues_enabled:` wins; values parse
/// case-insensitively as `true`/`false`; comments, absent keys and
/// anything else → `None` (blank stays allowed — GitHub's default).
/// A line scan, not a YAML dependency: the file has exactly one key
/// we care about (docs/0046 §3 D5).
fn blank_issues_enabled_from_config(yaml: &str) -> Option<bool> {
    for line in yaml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        if key.trim() != "blank_issues_enabled" {
            continue;
        }
        return match value.trim().to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::drafts::{self, DraftStore};
    use crate::service::{Kind, Mutation, SpecResult};
    // NOTE: no `use super::*` here — this file's `use gpui::*` would
    // shadow the builtin `#[test]` attribute (page.rs lesson). Import
    // selectively.
    use super::{
        NewIssuePage, blank_issues_enabled_from_config, create_submit_allowed, decode_base64,
    };
    use crate::gql;
    use crate::http::Http;
    use crate::service::{GithubCache, GithubState};
    use gpui::AppContext as _;
    use gpui_component::ActiveTheme as _;

    #[test]
    fn create_submit_gate_table() {
        // Title validity is the gate; busy never allows a second post.
        assert!(create_submit_allowed(false, false));
        assert!(!create_submit_allowed(true, false));
        assert!(!create_submit_allowed(false, true));
        assert!(!create_submit_allowed(true, true));
    }

    #[test]
    fn created_issue_parses_minimal_row() {
        let body = serde_json::json!({
            "data": {"createIssue": {"issue": {
                "number": 489,
                "title": "Fresh one",
                "url": "https://github.com/o/r/issues/489",
                "updatedAt": "2026-09-07T10:00:00Z"
            }}}
        })
        .to_string();
        let row = gql::created_issue_from_body(&body, "o/r").expect("row");
        assert_eq!(row.key, "o/r#489");
        assert_eq!(row.number, 489);
        assert_eq!(row.title, "Fresh one");
        assert_eq!(row.url, "https://github.com/o/r/issues/489");
        assert_eq!(row.updated_at.as_deref(), Some("2026-09-07T10:00:00Z"));
        assert_eq!(row.database_id, None);
        assert!(row.author.is_none());
    }

    #[test]
    fn created_issue_rejects_errors_and_malformed() {
        // GraphQL-level rejection (data null).
        let errors = serde_json::json!({
            "data": null,
            "errors": [{"message": "Issues has been disabled in this repository"}]
        })
        .to_string();
        assert!(gql::created_issue_from_body(&errors, "o/r").is_none());
        // Malformed body.
        assert!(gql::created_issue_from_body("not json", "o/r").is_none());
    }

    // --- probes -------------------------------------------------------------

    struct ProbeHttp;
    impl Http for ProbeHttp {
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

    /// Creator probe page: ProbeHttp fails the mutation, so a click on
    /// Create runs the full chain — busy → error slot → busy cleared.
    fn probe_creator(
        cx: &mut gpui::TestAppContext,
    ) -> (gpui::Entity<NewIssuePage>, &mut gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        // The mutation really spawns (ProbeHttp fails it) — the in-flight
        // tracker global must exist, same contract as main().
        cx.update(dashboard_core::status::init);
        let state = cx.update(|cx| {
            let client: std::sync::Arc<dyn Http> = std::sync::Arc::new(ProbeHttp);
            let store: std::sync::Arc<dyn crate::auth::TokenStore> =
                std::sync::Arc::new(ProbeStore);
            cx.new(|_| {
                GithubState::new(
                    client,
                    store,
                    Default::default(),
                    Default::default(),
                    GithubCache::new(dashboard_core::paths::AppPaths::init()),
                    DraftStore::at_tmp("new-issue-page-1"),
                )
            })
        });
        cx.add_window_view(|window, cx| {
            NewIssuePage::new(state, "o/r".into(), "R_9".into(), window, cx)
        })
    }

    #[gpui::test]
    fn create_click_posts_and_error_lands_on_form(cx: &mut gpui::TestAppContext) {
        let (page, vcx) = probe_creator(cx);
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.title
                    .update(cx, |s, cx| s.set_value("A title", window, cx));
                page.body
                    .update(cx, |s, cx| s.set_value("A body", window, cx));
                cx.notify();
            });
        });
        let create = vcx
            .debug_bounds("new-issue-create")
            .expect("create button visible");
        vcx.simulate_click(create.center(), gpui::Modifiers::default());
        let _ = vcx.debug_bounds("new-issue-create"); // flush frames
        vcx.update(|_, cx| {
            page.update(cx, |page, _| {
                // The full chain ran: post → ProbeHttp failed it → the
                // error landed on the form slot; busy already cycled back
                // to None, and the draft survived (D4).
                assert!(page.mutation_error.is_some(), "error expected");
                assert!(
                    page.mutation_error
                        .as_deref()
                        .unwrap()
                        .contains("Network error")
                );
                assert_eq!(page.busy, None);
            });
        });
    }

    #[gpui::test]
    fn empty_title_gates_create_and_preview_renders(cx: &mut gpui::TestAppContext) {
        let (page, vcx) = probe_creator(cx);
        // Title empty → Create disabled (D4).
        let create = vcx
            .debug_bounds("new-issue-create")
            .expect("create visible");
        vcx.simulate_click(create.center(), gpui::Modifiers::default());
        let _ = vcx.debug_bounds("new-issue-create"); // flush frames
        vcx.update(|_, cx| {
            page.update(cx, |page, _| {
                assert!(page.mutation_error.is_none(), "no post expected");
                assert_eq!(page.busy, None);
            });
        });
        // Preview toggle renders the 0017 blocks of the current body (D4).
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.body
                    .update(cx, |s, cx| s.set_value("**bold**", window, cx));
                cx.notify();
            });
        });
        let preview = vcx
            .debug_bounds("new-issue-preview")
            .expect("preview toggle visible");
        vcx.simulate_click(preview.center(), gpui::Modifiers::default());
        let _ = vcx.debug_bounds("new-issue-preview"); // flush frames
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                assert!(page.preview);
                // Danger token reachable only through the theme (DG).
                let _ = cx.theme().danger;
            });
        });
    }

    /// Draft probe fixture (docs/0045): shared `at_tmp` store returned so
    /// the test can seed and inspect per-repo draft files.
    fn probe_creator_drafts(
        cx: &mut gpui::TestAppContext,
        repo: String,
        drafts: DraftStore,
    ) -> (gpui::Entity<NewIssuePage>, &mut gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        cx.update(|cx| cx.background_executor().allow_parking());
        let state = cx.update(|cx| {
            let client: std::sync::Arc<dyn Http> = std::sync::Arc::new(ProbeHttp);
            let store: std::sync::Arc<dyn crate::auth::TokenStore> =
                std::sync::Arc::new(ProbeStore);
            cx.new(|_| {
                GithubState::new(
                    client,
                    store,
                    Default::default(),
                    Default::default(),
                    GithubCache::new(dashboard_core::paths::AppPaths::init()),
                    drafts.clone(),
                )
            })
        });
        let (page, vcx) = cx
            .add_window_view(|window, cx| NewIssuePage::new(state, repo, "R_9".into(), window, cx));
        (page, vcx)
    }

    // docs/0045 D4/D6 — the creator's draft parks per repo and restores
    // per repo: typing parks under `new-issue@<repo>`; a creator for a
    // different repo opens empty; reopening the same repo restores both
    // fields with the hint.
    #[gpui::test]
    fn creator_draft_parks_and_restores_per_repo(cx: &mut gpui::TestAppContext) {
        // Park a draft under `new-issue@acme/one`.
        let one_path;
        let drafts = DraftStore::at_tmp("new-issue-draft-probe");
        {
            let (page, vcx) = probe_creator_drafts(cx, "acme/one".into(), drafts.clone());
            vcx.update(|window, cx| {
                page.update(cx, |page, cx| {
                    page.title
                        .update(cx, |s, cx| s.set_value("Parked title", window, cx));
                    page.body
                        .update(cx, |s, cx| s.set_value("Parked body", window, cx));
                    page.draft_changed(cx);
                });
            });
            vcx.run_until_parked();
            one_path = drafts.path_for(&drafts::new_issue_key("acme/one"));
            let on_disk: Option<drafts::DraftPayload> =
                dashboard_core::store::load_json(&one_path).ok().flatten();
            let on_disk = on_disk.expect("creator draft parked");
            assert_eq!(on_disk.title.as_deref(), Some("Parked title"));
            assert_eq!(on_disk.body, "Parked body");
        }

        // Another repo: the creator opens empty — no draft, no hint (D6).
        {
            let (other, ovcx) = probe_creator_drafts(cx, "acme/two".into(), drafts.clone());
            ovcx.update(|_, cx| {
                other.update(cx, |page, cx| {
                    assert_eq!(page.title.read(cx).value(), "");
                    assert_eq!(page.body.read(cx).value(), "");
                    assert!(!page.draft_restored);
                });
            });
            assert!(
                ovcx.debug_bounds("new-issue-draft-restored").is_none(),
                "no hint without a restore"
            );
        }

        // Reopen the first repo (the host rebuilt the creator after the
        // repo switch): title + body restored, hint visible (D4/D6).
        {
            let (again, avcx) = probe_creator_drafts(cx, "acme/one".into(), drafts.clone());
            avcx.update(|_, cx| {
                again.update(cx, |page, cx| {
                    assert_eq!(page.title.read(cx).value(), "Parked title");
                    assert_eq!(page.body.read(cx).value(), "Parked body");
                    assert!(page.draft_restored);
                });
            });
            assert!(
                avcx.debug_bounds("new-issue-draft-restored").is_some(),
                "restore hint visible"
            );
        }
    }

    // docs/0045 D5 — a successful creation clears the draft; the
    // failure path keeps it (0045 D4: the draft survives errors).
    #[gpui::test]
    fn creator_draft_clears_on_success_survives_error(cx: &mut gpui::TestAppContext) {
        let drafts = DraftStore::at_tmp("new-issue-clear-probe");
        let (page, vcx) = probe_creator_drafts(cx, "o/r".into(), drafts.clone());
        let key = drafts::new_issue_key("o/r");
        let path = drafts.path_for(&key);

        // Park a draft.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.title.update(cx, |s, cx| s.set_value("T", window, cx));
                page.draft_changed(cx);
            });
        });
        vcx.run_until_parked();
        assert!(path.exists(), "draft parked");

        // A failed creation (network error) keeps the draft on disk.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.apply_mutation_result(
                    SpecResult {
                        kind: Kind::PrMutation(Mutation::CreateIssue),
                        ident: Some("o/r new-issue".into()),
                        status: 0,
                        etag: None,
                        rate: None,
                        body: None,
                        network: Some("probe: no network".into()),
                        cursor: None,
                    },
                    window,
                    cx,
                );
            });
        });
        assert!(path.exists(), "error keeps the draft");

        // A successful creation clears it (cancel-before-unlink).
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.apply_mutation_result(
                    SpecResult {
                        kind: Kind::PrMutation(Mutation::CreateIssue),
                        ident: Some("o/r new-issue".into()),
                        status: 200,
                        etag: None,
                        rate: None,
                        body: Some(
                            r#"{"data":{"createIssue":{"issue":{"number":9,"title":"T","url":"u"}}}}"#
                                .into(),
                        ),
                        network: None,
                        cursor: None,
                    },
                    window,
                    cx,
                );
            });
        });
        assert!(!path.exists(), "created issue leaves no draft");
    }
    // --- docs/0046: template parsers ------------------------------------------

    #[test]
    fn base64_decodes_wrapped_payloads_and_rejects_garbage() {
        assert_eq!(decode_base64("aGVsbG8=").as_deref(), Some("hello"));
        // The Contents API wraps at 60 chars — newlines vanish.
        assert_eq!(decode_base64("aGVs\nbG8=").as_deref(), Some("hello"));
        // No padding, multiple of 4.
        assert_eq!(decode_base64("dGVzdA==").as_deref(), Some("test"));
        assert_eq!(decode_base64("YQ==").as_deref(), Some("a"));
        assert!(decode_base64("!").is_none(), "bad alphabet");
        assert!(decode_base64("aGVsbG8").is_none(), "bad length");
        assert!(decode_base64("a=b").is_none(), "mid padding");
    }

    #[test]
    fn blank_config_scans_one_key_and_defaults_none() {
        assert_eq!(
            blank_issues_enabled_from_config("blank_issues_enabled: false\n"),
            Some(false)
        );
        // Comments, ordering, real-world shape (next.js config).
        assert_eq!(
            blank_issues_enabled_from_config(
                "# config\nblank_issues_enabled: false\ncontact_links: []\n"
            ),
            Some(false)
        );
        assert_eq!(
            blank_issues_enabled_from_config("blank_issues_enabled: TRUE"),
            Some(true)
        );
        assert_eq!(
            blank_issues_enabled_from_config("contact_links: []\n"),
            None,
            "absent key → blank allowed"
        );
        assert_eq!(
            blank_issues_enabled_from_config("blank_issues_enabled: maybe"),
            None,
            "unparsable value → blank allowed"
        );
    }
    // --- docs/0046: template probes --------------------------------------------

    /// The two lanes' scripted replies, consumed per transport: the
    /// templates lane POSTs GraphQL, the config lane GETs Contents —
    /// type-checking the pop makes a crossed script loud.
    #[derive(Debug, Clone)]
    enum TplReply {
        Templates(String),
        Config(u16, String),
        Fail(String),
    }

    struct TplHttp(std::sync::Mutex<std::collections::VecDeque<TplReply>>);

    impl TplHttp {
        fn new(script: Vec<TplReply>) -> Self {
            Self(std::sync::Mutex::new(script.into()))
        }
        fn pop(&self) -> TplReply {
            self.0
                .lock()
                .unwrap()
                .pop_front()
                .expect("TplHttp: exhausted script")
        }
    }

    impl Http for TplHttp {
        fn request(
            &self,
            _: crate::http::Verb,
            _: &str,
            _: Option<&str>,
            _: Option<&str>,
        ) -> Result<crate::http::HttpResult, crate::http::HttpError> {
            match self.pop() {
                TplReply::Config(status, body) => Ok(crate::http::HttpResult {
                    status,
                    etag: None,
                    rate: None,
                    body: Some(body),
                }),
                TplReply::Fail(m) => Err(crate::http::HttpError(m)),
                other => panic!("TplHttp: unexpected GET, script had {other:?}"),
            }
        }
        fn post_graphql(
            &self,
            _: &str,
            _: &serde_json::Value,
        ) -> Result<crate::http::HttpResult, crate::http::HttpError> {
            match self.pop() {
                TplReply::Templates(body) => Ok(crate::http::HttpResult {
                    status: 200,
                    etag: None,
                    rate: None,
                    body: Some(body),
                }),
                TplReply::Fail(m) => Err(crate::http::HttpError(m)),
                other => panic!("TplHttp: unexpected POST, script had {other:?}"),
            }
        }
    }

    const TEMPLATES_BODY: &str = r#"{"data":{"repository":{"issueTemplates":[
        {"name":"Bug report","title":"[Bug]: ","about":"Create a report","body":"<!-- bug -->"},
        {"name":"Feature","title":null,"about":null,"body":"<!-- feature -->"}]}}}"#;
    /// `blank_issues_enabled: false\n`
    const CONFIG_BLANK_OFF: &str =
        r#"{"content":"YmxhbmtfaXNzdWVzX2VuYWJsZWQ6IGZhbHNlCg==","encoding":"base64"}"#;

    /// Template probe fixture: a shared `at_tmp` store the test can seed
    /// and inspect, connected auth (the lanes gate on it), and a TplHttp
    /// script consumed by the creator's two open fetches.
    fn probe_creator_templates(
        cx: &mut gpui::TestAppContext,
        script: Vec<TplReply>,
        drafts: DraftStore,
    ) -> (gpui::Entity<NewIssuePage>, &mut gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        cx.update(|cx| cx.background_executor().allow_parking());
        let state = cx.update(|cx| {
            let client: std::sync::Arc<dyn Http> = std::sync::Arc::new(TplHttp::new(script));
            let store: std::sync::Arc<dyn crate::auth::TokenStore> =
                std::sync::Arc::new(ProbeStore);
            cx.new(|_| {
                GithubState::new(
                    client,
                    store,
                    Default::default(),
                    Default::default(),
                    GithubCache::new(dashboard_core::paths::AppPaths::init()),
                    drafts.clone(),
                )
            })
        });
        state.update(cx, |s, _| s.auth = crate::service::AuthStatus::Connected);
        let (page, vcx) = cx.add_window_view(|window, cx| {
            NewIssuePage::new(state, "o/r".into(), "R_9".into(), window, cx)
        });
        (page, vcx)
    }

    #[gpui::test]
    fn templates_render_and_pick_prefills_and_parks(cx: &mut gpui::TestAppContext) {
        let drafts = DraftStore::at_tmp("tpl-probe-1");
        // Config 404 — no config file: blank stays allowed (D5/D6).
        let (page, vcx) = probe_creator_templates(
            cx,
            vec![
                TplReply::Templates(TEMPLATES_BODY.into()),
                TplReply::Config(404, String::new()),
            ],
            drafts.clone(),
        );
        vcx.run_until_parked();
        assert!(
            vcx.debug_bounds("new-issue-template-row").is_some(),
            "template row renders"
        );
        assert!(vcx.debug_bounds("new-issue-tpl-0").is_some());
        assert!(vcx.debug_bounds("new-issue-tpl-1").is_some());
        assert!(vcx.debug_bounds("new-issue-tpl-blank").is_some());
        vcx.update(|_, cx| {
            page.update(cx, |page, _| {
                assert_eq!(page.selected_template, None, "starts on Blank");
            });
        });
        // Pick template 0: title+body prefill (D3), the content parks as
        // the live draft (D4).
        let tpl0 = vcx.debug_bounds("new-issue-tpl-0").unwrap().center();
        vcx.simulate_click(tpl0, gpui::Modifiers::default());
        let _ = vcx.debug_bounds("new-issue-tpl-0"); // flush frames
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                assert_eq!(page.selected_template, Some(0));
                assert_eq!(page.title.read(cx).value(), "[Bug]: ");
                assert_eq!(page.body.read(cx).value(), "<!-- bug -->");
            });
        });
        vcx.run_until_parked();
        let on_disk: Option<drafts::DraftPayload> =
            dashboard_core::store::load_json(&drafts.path_for(&drafts::new_issue_key("o/r")))
                .ok()
                .flatten();
        let on_disk = on_disk.expect("template content parks as the draft");
        // draft_changed parks the trimmed title (0045 semantics).
        assert_eq!(on_disk.title.as_deref(), Some("[Bug]:"));
        assert_eq!(on_disk.body, "<!-- bug -->");
    }

    #[gpui::test]
    fn restored_draft_beats_autoselect_and_blank_off_hides_blank(cx: &mut gpui::TestAppContext) {
        let drafts = DraftStore::at_tmp("tpl-probe-2");
        // Seed a parked draft, then open on a blank-disabled repo (D4/D5):
        // the draft wins — no auto-prefill — and the Blank pill hides.
        let payload = drafts::DraftPayload {
            key: drafts::new_issue_key("o/r"),
            title: Some("My own title".into()),
            body: "My own words".into(),
            updated_ms: 1,
        };
        dashboard_core::store::save_json_atomic(
            &drafts.path_for(&drafts::new_issue_key("o/r")),
            &payload,
        )
        .unwrap();
        let (page, vcx) = probe_creator_templates(
            cx,
            vec![
                TplReply::Templates(TEMPLATES_BODY.into()),
                TplReply::Config(200, CONFIG_BLANK_OFF.into()),
            ],
            drafts,
        );
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                assert_eq!(page.title.read(cx).value(), "My own title", "draft wins");
                assert_eq!(page.body.read(cx).value(), "My own words");
                assert_eq!(page.selected_template, None);
                assert!(!page.blank_allowed);
            });
        });
        assert!(
            vcx.debug_bounds("new-issue-tpl-blank").is_none(),
            "blank_issues_enabled: false hides the Blank pill"
        );
        assert!(vcx.debug_bounds("new-issue-tpl-0").is_some());
    }

    #[gpui::test]
    fn blank_disabled_preselects_first_template(cx: &mut gpui::TestAppContext) {
        let drafts = DraftStore::at_tmp("tpl-probe-3");
        let (page, vcx) = probe_creator_templates(
            cx,
            vec![
                TplReply::Templates(TEMPLATES_BODY.into()),
                TplReply::Config(200, CONFIG_BLANK_OFF.into()),
            ],
            drafts,
        );
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                // D5: the creator never starts empty on a blank-disabled
                // repo — the first template preselects.
                assert_eq!(page.selected_template, Some(0));
                assert_eq!(page.title.read(cx).value(), "[Bug]: ");
                assert_eq!(page.body.read(cx).value(), "<!-- bug -->");
            });
        });
        assert!(vcx.debug_bounds("new-issue-tpl-blank").is_none());
    }

    #[gpui::test]
    fn template_fetch_failure_leaves_creator_unchanged(cx: &mut gpui::TestAppContext) {
        let drafts = DraftStore::at_tmp("tpl-probe-4");
        let (page, vcx) = probe_creator_templates(
            cx,
            vec![
                TplReply::Fail("probe".into()),
                TplReply::Fail("probe".into()),
            ],
            drafts,
        );
        vcx.run_until_parked();
        // D6: both lanes failed — no row, no Blank pill, fields untouched,
        // creation unblocked.
        assert!(vcx.debug_bounds("new-issue-template-row").is_none());
        assert!(vcx.debug_bounds("new-issue-create").is_some());
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                assert_eq!(page.title.read(cx).value(), "");
                assert_eq!(page.body.read(cx).value(), "");
                assert!(page.templates.is_none());
            });
        });
    }
}
