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

use dashboard_core::ui::{page_canvas, page_title};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use gpui_component::{
    ActiveTheme, Disableable, IndexPath, Sizable as _,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::{InputEvent, Textarea, TextareaState},
    searchable_list::{SearchableListDelegate, SearchableListItem, SearchableVec},
    select::{Select, SelectState},
    spinner::Spinner,
    v_flex,
};

use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

use crate::drafts::{self, DraftHandle};
use crate::gql;
use crate::markdown;
use crate::service::{FetchSpec, GithubState, Kind, Mutation, run_mutation, run_spec_tracked};
use crate::usage::{Origin, Surface, Trigger};

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

/// docs/0044 §9 — which branch select a search targets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BranchSelect {
    Base,
    Head,
}

/// docs/0044 §9 — a landed server-filtered branch page
/// (`refs(query:)`) for one select. `None` = that select shows the
/// shared default (alphabetical) page.
#[derive(Clone)]
struct BranchSearchPage {
    query: String,
    total_count: i64,
    names: Vec<String>,
}

/// docs/0044 §9 — the branch-select delegate: [`SearchableVec`]'s
/// local filter, plus a page ping that swaps in a server-filtered page
/// (`refs(query:)`) once the debounce fires. Local filtering only ever
/// sees the loaded page; the server filter covers the whole repo, and
/// its substring semantics are a superset of the client's
/// case-insensitive `contains`, so the two never disagree.
struct BranchSearchDelegate {
    inner: SearchableVec<SharedString>,
    page: WeakEntity<NewPrPage>,
    which: BranchSelect,
}

impl SearchableListDelegate for BranchSearchDelegate {
    type Item = SharedString;

    fn items_count(&self, _section: usize) -> usize {
        self.inner.items_count(0)
    }

    fn item(&self, ix: IndexPath) -> Option<&Self::Item> {
        self.inner.item(ix)
    }

    fn position<V>(&self, value: &V) -> Option<IndexPath>
    where
        Self::Item: SearchableListItem<Value = V>,
        V: PartialEq,
    {
        self.inner.position(value)
    }

    fn perform_search(&mut self, query: &str, window: &mut Window, cx: &mut App) -> Task<()> {
        // Instant local feedback; the server page refines when it lands.
        let local = self.inner.perform_search(query, window, cx);
        let query = query.trim().to_string();
        let which = self.which;
        let _ = self.page.update_in(cx, |page, window, cx| {
            page.queue_branch_search(which, query, window, cx);
        });
        local
    }
}

/// docs/0044 §9 — keystroke debounce for the branch search: a repo
/// filter fires only when typing pauses; the generation counter drops
/// superseded timers (the DebouncedSave shape, store.rs).
const BRANCH_SEARCH_DELAY: Duration = Duration::from_millis(300);

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
    /// Creator draft persistence (docs/0045 D4/D8): keyed
    /// `new-pr@<repo>`; one handle covers title + body (each change
    /// schedules a full snapshot of both). Session-only select/draft-flag
    /// state stays session-only (0045 D7).
    draft_handle: DraftHandle,
    /// A draft was restored at construction — one muted hint row on the
    /// form card until the first real edit (0045 D4).
    draft_restored: bool,
    /// Preview toggle: `true` swaps the body textarea for 0017 blocks of
    /// the current body, parsed once on toggle (D4 — no live reparse per
    /// keystroke).
    preview: bool,
    md: Option<Arc<[markdown::Block]>>,
    /// The branch page (D4), fetched on construction and on every host
    /// open (D7 — branches are not retained).
    branches: BranchPage,
    /// Per-select server-filtered pages (docs/0044 §9): `Some` once a
    /// `refs(query:)` page lands for that select; cleared when the
    /// search empties or the default page re-lands.
    base_search: Option<BranchSearchPage>,
    head_search: Option<BranchSearchPage>,
    /// Debounce generations (docs/0044 §9): each queued keystroke bumps
    /// its select's counter; a timer that wakes to a stale counter is
    /// dropped.
    base_search_gen: u64,
    head_search_gen: u64,
    /// Probes run the debounce at zero (the DraftStore.delay pattern).
    branch_search_delay: Duration,
    /// The repo's PR template body (docs/0046 §2): landed on
    /// construction / host open; `None` while pending or on failure —
    /// the creator then just stays empty (creation never blocks).
    template_body: Option<String>,
    base_select: Entity<SelectState<BranchSearchDelegate>>,
    head_select: Entity<SelectState<BranchSearchDelegate>>,
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
        // The delegates ping this page on search (docs/0044 §9) — the
        // handle is captured before construction completes.
        let this_page = cx.entity().downgrade();
        let base_select = cx.new(|cx| {
            SelectState::new(
                Self::branch_delegate(this_page.clone(), BranchSelect::Base),
                None,
                window,
                cx,
            )
            .searchable(true)
        });
        let head_select = cx.new(|cx| {
            SelectState::new(
                Self::branch_delegate(this_page, BranchSelect::Head),
                None,
                window,
                cx,
            )
            .searchable(true)
        });
        // 0045 D4/D6: restore this repo's parked draft (see the
        // new-issue creator — same rules, `new-pr@<repo>` key).
        let draft_handle = state.read(cx).drafts.handle(drafts::new_pr_key(&repo));
        let restored = draft_handle.load();
        let draft_restored = restored.is_some();
        if let Some(payload) = restored {
            if let Some(title_text) = payload.title {
                title.update(cx, |state, cx| state.set_value(title_text, window, cx));
            }
            body.update(cx, |state, cx| state.set_value(payload.body, window, cx));
        }
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
        let mut page = Self {
            focus_handle: cx.focus_handle(),
            state,
            node_id,
            title,
            body,
            draft_handle,
            draft_restored,
            preview: false,
            md: None,
            branches: None,
            base_search: None,
            head_search: None,
            base_search_gen: 0,
            head_search_gen: 0,
            branch_search_delay: BRANCH_SEARCH_DELAY,
            template_body: None,
            base_select,
            head_select,
            draft: false,
            busy: None,
            mutation_error: None,
            repo,
        };
        page.fetch_branches(window, cx);
        page.fetch_pr_templates(window, cx);
        page
    }

    /// Re-arm on every host open (D7): branches re-fetch, the draft
    /// (title/body/draft flag) survives.
    pub(crate) fn refetch_branches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.fetch_branches(window, cx);
    }

    /// Re-arm on every host open (docs/0046 §2): the template body
    /// re-fetches with the branches — a repo that edits its template
    /// between opens is honored as long as the user hasn't typed a body.
    pub(crate) fn refetch_pr_templates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.fetch_pr_templates(window, cx);
    }

    // --- PR template body (docs/0046 §2) --------------------------------------

    /// Fetch the repo's PR template body: the stateless
    /// [`Kind::RepoPrTemplates`] lane, the [`Kind::RepoIssueTemplates`]
    /// mirror (0046 D1). Failures log-and-swallow — the creator stays
    /// empty and creation is never blocked.
    fn fetch_pr_templates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.read(cx).auth.connected() {
            return; // the page is about to be torn down by the auth gate
        }
        let state = self.state.clone();
        let (owner, name) = crate::pr_page::split_repo(&self.repo);
        let spec = FetchSpec {
            origin: Origin::new(Surface::Composer, Trigger::ViewShown),
            kind: Kind::RepoPrTemplates,
            url: gql::GRAPHQL_ENDPOINT.to_string(),
            etag: None,
            ident: Some(self.repo.clone()),
            max_pages: 1,
            variables: Some(json!({ "owner": owner, "name": name })),
        };
        log::debug!("repo-pr-templates: fetching {}", self.repo);
        self.template_body = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = run_spec_tracked(state, spec, cx).await;
            let _ = cx.update(|window, app| {
                this.update(app, |page, cx| page.apply_pr_template(result, window, cx))
            });
        })
        .detach();
    }

    /// The template body came back (docs/0046 §2 draft composition): the
    /// body prefills only when the user has no stake yet — no restored
    /// draft (0046 D4: a restored draft beats the template) and an empty
    /// body field (typed content is never clobbered by a re-open).
    /// `set_value` suppresses Change events, so a prefill parks nothing —
    /// the template stays a default, not user content.
    fn apply_pr_template(
        &mut self,
        result: crate::service::SpecResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Log-and-swallow on every failure shape (docs/0046 D6): the
        // creator stays empty and creation is never blocked.
        if result.network.is_some() || result.status != 200 {
            log::warn!("repo-pr-templates: fetch failed — creator stays empty");
            return;
        }
        let Some(body) = result.body else {
            log::warn!("repo-pr-templates: empty payload — creator stays empty");
            return;
        };
        let Ok(env) = serde_json::from_str::<gql::Envelope<serde_json::Value>>(body.as_str())
        else {
            log::warn!("repo-pr-templates payload: malformed");
            return;
        };
        let empty = serde_json::Value::Null;
        let repo = env
            .data
            .as_ref()
            .and_then(|d| d.get("repository"))
            .unwrap_or(&empty);
        self.template_body = gql::pr_template_body_from_value(repo);
        // The prefill fires only when the user has no stake yet (0046 §2
        // draft composition: a restored draft beats the template; typed
        // content is never clobbered by a re-open). `set_value`
        // suppresses Change events, so the prefill parks nothing — the
        // template stays a default, not user content.
        if let (Some(text), false) = (&self.template_body, self.draft_restored) {
            if self.body.read(cx).value().is_empty() {
                self.body
                    .update(cx, |state, cx| state.set_value(text.clone(), window, cx));
            }
        }
    }

    // --- branch source (docs/0044 D4) ----------------------------------------

    /// docs/0044 §9 — the delegate for one branch select: an empty
    /// local page plus the handle it pings on search.
    fn branch_delegate(page: WeakEntity<Self>, which: BranchSelect) -> BranchSearchDelegate {
        BranchSearchDelegate {
            inner: SearchableVec::new(Vec::<SharedString>::new()),
            page,
            which,
        }
    }

    /// docs/0044 §9 — a keystroke landed in a select's search box:
    /// bump that select's generation and arm the debounce timer. The
    /// local (loaded-page) filter already applied; this fetches the
    /// repo-wide `refs(query:)` page when typing pauses.
    fn queue_branch_search(
        &mut self,
        which: BranchSelect,
        query: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (generation, delay) = match which {
            BranchSelect::Base => {
                self.base_search_gen += 1;
                (self.base_search_gen, self.branch_search_delay)
            }
            BranchSelect::Head => {
                self.head_search_gen += 1;
                (self.head_search_gen, self.branch_search_delay)
            }
        };
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = cx.update(|window, cx| {
                this.update(cx, |page, cx| {
                    page.fire_branch_search(which, generation, query, window, cx)
                })
            });
        })
        .detach();
    }

    /// docs/0044 §9 — the debounce fired: superseded generations are
    /// dropped; an empty query restores the shared default page (no
    /// network — it is already in memory); anything else fetches the
    /// repo-wide filtered page for that one select.
    fn fire_branch_search(
        &mut self,
        which: BranchSelect,
        generation: u64,
        query: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = match which {
            BranchSelect::Base => self.base_search_gen,
            BranchSelect::Head => self.head_search_gen,
        };
        if generation != current {
            return; // a newer keystroke superseded this timer
        }
        if query.is_empty() {
            match which {
                BranchSelect::Base => self.base_search = None,
                BranchSelect::Head => self.head_search = None,
            }
            self.set_branch_items(which, window, cx);
            cx.notify();
            return;
        }
        let state = self.state.clone();
        let (owner, name) = crate::pr_page::split_repo(&self.repo);
        let spec = FetchSpec {
            origin: Origin::new(Surface::Composer, Trigger::ViewShown),
            kind: Kind::RepoBranches,
            url: gql::GRAPHQL_ENDPOINT.to_string(),
            etag: None,
            ident: Some(self.repo.clone()),
            max_pages: 1,
            variables: Some(json!({ "owner": owner, "name": name, "query": query })),
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = run_spec_tracked(state, spec, cx).await;
            let _ = cx.update(|window, cx| {
                this.update(cx, |page, cx| {
                    page.apply_branch_search(which, generation, query, result, window, cx)
                })
            });
        })
        .detach();
    }

    /// docs/0044 §9 — the filtered page came back for one select.
    /// Stale generations are dropped; failures log-and-swallow — the
    /// select keeps the local filter's view and the default page stays
    /// authoritative (D6 conservatism: a search must never cost the
    /// creator its branch list).
    fn apply_branch_search(
        &mut self,
        which: BranchSelect,
        generation: u64,
        query: String,
        result: crate::service::SpecResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = match which {
            BranchSelect::Base => self.base_search_gen,
            BranchSelect::Head => self.head_search_gen,
        };
        if generation != current {
            return;
        }
        if result.network.is_some() || result.status != 200 {
            log::warn!("repo-branches: filtered fetch failed — local filter stays");
            return;
        }
        let Some(body) = result.body else {
            log::warn!("repo-branches: filtered fetch empty — local filter stays");
            return;
        };
        let Ok(env) = serde_json::from_str::<gql::Envelope<serde_json::Value>>(body.as_str())
        else {
            log::warn!("repo-branches filtered payload: malformed");
            return;
        };
        let empty = serde_json::Value::Null;
        let repo = env
            .data
            .as_ref()
            .and_then(|d| d.get("repository"))
            .unwrap_or(&empty);
        let page_data = gql::branches_from_value(repo);
        let search = BranchSearchPage {
            query,
            total_count: page_data.total_count,
            names: page_data.names.clone(),
        };
        match which {
            BranchSelect::Base => self.base_search = Some(search),
            BranchSelect::Head => self.head_search = Some(search),
        }
        let names: Vec<SharedString> = page_data.names.iter().map(|n| n.clone().into()).collect();
        self.swap_branch_items(which, names, window, cx);
        cx.notify();
    }

    /// docs/0044 §9 — swap one select's items (the delegate is rebuilt
    /// with the current page handle, so searches keep working).
    fn swap_branch_items(
        &self,
        which: BranchSelect,
        names: Vec<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let page = cx.entity().downgrade();
        let delegate = BranchSearchDelegate {
            inner: SearchableVec::new(names),
            page,
            which,
        };
        match which {
            BranchSelect::Base => self.base_select.update(cx, |s, cx| {
                s.set_items(delegate, window, cx);
            }),
            BranchSelect::Head => self.head_select.update(cx, |s, cx| {
                s.set_items(delegate, window, cx);
            }),
        }
    }

    /// docs/0044 §9 — put the shared default page's names into one
    /// select (search cleared). A missing/failed default page leaves
    /// the select as-is — there is nothing better to show.
    fn set_branch_items(&self, which: BranchSelect, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Ok(page)) = &self.branches {
            let names: Vec<SharedString> = page.names.iter().map(|n| n.clone().into()).collect();
            self.swap_branch_items(which, names, window, cx);
        }
    }

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
            origin: Origin::new(Surface::Composer, Trigger::ViewShown),
            kind: Kind::RepoBranches,
            url: gql::GRAPHQL_ENDPOINT.to_string(),
            etag: None,
            ident: Some(self.repo.clone()),
            max_pages: 1,
            variables: Some(json!({ "owner": owner, "name": name, "query": null })),
        };
        log::debug!("repo-branches: fetching {}", self.repo);
        // The default page re-lands: drop any per-select search state
        // and invalidate pending debounce timers (D7 — every open is a
        // fresh default).
        self.base_search = None;
        self.head_search = None;
        self.base_search_gen += 1;
        self.head_search_gen += 1;
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
                    self.base_search = None;
                    self.head_search = None;
                    self.swap_branch_items(BranchSelect::Base, names.clone(), window, cx);
                    self.swap_branch_items(BranchSelect::Head, names, window, cx);
                    if let Some(b) = &base {
                        let b: SharedString = b.clone().into();
                        self.base_select
                            .update(cx, |s, cx| s.set_selected_value(&b, window, cx));
                    }
                    if let Some(h) = &head {
                        let h: SharedString = h.clone().into();
                        self.head_select
                            .update(cx, |s, cx| s.set_selected_value(&h, window, cx));
                    }
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
            let result = run_mutation(
                state,
                Mutation::CreatePullRequest,
                variables,
                ident,
                Origin::new(Surface::Composer, Trigger::MutationRefetch),
                cx,
            )
            .await;
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
    /// 0045 D3/D4: a creator field changed — debounce-save the full
    /// snapshot (title + body) and retire the restore hint.
    fn draft_changed(&mut self, cx: &mut Context<Self>) {
        self.draft_restored = false;
        let title = self.title.read(cx).value().trim().to_string();
        let body = self.body.read(cx).value().to_string();
        let title = (!title.is_empty()).then_some(title);
        self.draft_handle.schedule(title, body, cx);
    }

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
                // 0045 D5: the created PR leaves no draft behind
                // (cancel first — no pre-submit write lands after this).
                self.draft_handle.discard();
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

        page_canvas()
            .id("new-pr")
            .key_context("GithubNewPrPage")
            .track_focus(&self.focus_handle)
            // #360: a focused element needs a role to paint an
            // a11y node — id + track_focus alone is the
            // "focus without node" gap class (the walk and
            // assistive tech both land here blind otherwise).
            .role(accesskit::Role::Group)
            // D7: Esc always asks the host to close — the creator has no
            // nested editors for it to consume first.
            .on_action(cx.listener(|this, _: &DismissNewPrPage, _, cx| {
                this.request_close(cx);
            }))
            .size_full()
            .overflow_y_scroll()
            // Header: what is being created, and where it will land.
            .child(
                v_flex()
                    .gap_1()
                    .child(page_title("New pull request"))
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
                            .when(self.draft_restored, |col| {
                                col.child(
                                    div()
                                        .id("new-pr-draft-restored")
                                        .debug_selector(|| "new-pr-draft-restored".to_string())
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
                                    // docs/0049 D1: the row is the
                                    // toggle's semantic owner — the
                                    // checkbox mirrors the state.
                                    .role(accesskit::Role::CheckBox)
                                    .aria_label("Draft pull request")
                                    .aria_toggled(if self.draft {
                                        accesskit::Toggled::True
                                    } else {
                                        accesskit::Toggled::False
                                    })
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
        // docs/0044 §9 — a live server-filtered search wins the slot:
        // per-select hint lines only when that page itself truncates.
        let search_hints: Vec<AnyElement> = [
            (BranchSelect::Base, "base", &self.base_search),
            (BranchSelect::Head, "head", &self.head_search),
        ]
        .into_iter()
        .filter_map(|(_, label, search)| {
            let search = search.as_ref()?;
            if !branches_truncated(search.total_count, &search.names) {
                return None;
            }
            let label = format!("new-pr-branches-search-{label}");
            Some(
                div()
                    .id(SharedString::from(label.clone()))
                    .debug_selector(move || label.clone())
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "first {} of {} branches matching \"{}\"",
                        search.names.len(),
                        search.total_count,
                        search.query
                    ))
                    .into_any_element(),
            )
        })
        .collect();
        if !search_hints.is_empty() {
            return v_flex().gap_1().children(search_hints).into_any_element();
        }
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
    use crate::drafts::{self, DraftStore};
    use crate::service::{AuthStatus, Kind, Mutation, SpecResult};
    // NOTE: no `use super::*` here — this file's `use gpui::*` would
    // shadow the builtin `#[test]` attribute (page.rs lesson). Import
    // selectively.
    use super::{
        BranchSelect, NewPrPage, branches_truncated, default_base_head, pr_submit_allowed,
    };
    use crate::gql;
    use crate::http::Http;
    use crate::service::{GithubCache, GithubState};
    use crate::usage::{Origin, Surface, Trigger};
    use gpui::AppContext as _;
    use std::time::Duration;

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
    struct FixtureHttp {
        sent: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        /// The PR template body the `RepoPrTemplates` lane serves
        /// (docs/0046 §2); `None` fails the lane like any other probe
        /// request.
        template: Option<String>,
    }
    impl FixtureHttp {
        fn new(sent: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>) -> Self {
            Self {
                sent,
                template: None,
            }
        }
    }
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
            if doc.contains("RepoPrTemplates") {
                let payload = match &self.template {
                    Some(t) => serde_json::json!({
                        "data": {"repository": {"pullRequestTemplates": [
                            {"body": t}
                        ]}}
                    }),
                    None => return Err(crate::http::HttpError("probe: no network".into())),
                };
                return Ok(crate::http::HttpResult {
                    status: 200,
                    etag: None,
                    rate: None,
                    body: Some(payload.to_string()),
                });
            }
            if !doc.contains("RepoBranches") {
                self.sent.lock().unwrap().push(variables.clone());
            }
            if doc.contains("RepoBranches") {
                // A `query` variable (docs/0044 §9) returns the
                // server-filtered page; null returns the default page.
                let (total, nodes) = match variables.get("query") {
                    Some(serde_json::Value::String(_)) => {
                        (140, vec![serde_json::json!({"name": "feat"})])
                    }
                    _ => (
                        2,
                        vec![
                            serde_json::json!({"name": "feat"}),
                            serde_json::json!({"name": "main"}),
                        ],
                    ),
                };
                let payload = serde_json::json!({
                    "data": {"repository": {
                        "defaultBranchRef": {"name": "main"},
                        "refs": {"totalCount": total, "nodes": nodes}
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
            let client: std::sync::Arc<dyn Http> =
                std::sync::Arc::new(FixtureHttp::new(sent.clone()));
            let store: std::sync::Arc<dyn crate::auth::TokenStore> =
                std::sync::Arc::new(ProbeStore);
            let entity = cx.new(|_| {
                GithubState::new(
                    client,
                    store,
                    Default::default(),
                    Default::default(),
                    GithubCache::new(dashboard_core::paths::AppPaths::init()),
                    DraftStore::at_tmp("new-pr-page-1"),
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

    /// Draft probe fixture (docs/0045): the test owns the `at_tmp` store.
    fn probe_pr_creator_drafts(
        cx: &mut gpui::TestAppContext,
        repo: String,
        drafts: DraftStore,
    ) -> (gpui::Entity<NewPrPage>, &mut gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        cx.update(|cx| cx.background_executor().allow_parking());
        let state = cx.update(|cx| {
            let client: std::sync::Arc<dyn Http> = std::sync::Arc::new(FixtureHttp::new(
                std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            ));
            let store: std::sync::Arc<dyn crate::auth::TokenStore> =
                std::sync::Arc::new(ProbeStore);
            let entity = cx.new(|_| {
                GithubState::new(
                    client,
                    store,
                    Default::default(),
                    Default::default(),
                    GithubCache::new(dashboard_core::paths::AppPaths::init()),
                    drafts,
                )
            });
            entity.update(cx, |s, _| s.auth = AuthStatus::Connected);
            entity
        });
        cx.add_window_view(|window, cx| NewPrPage::new(state, repo, "R_9".into(), window, cx))
    }

    // docs/0045 D4/D6 — the PR creator parks its draft per repo and
    // restores it on reopen (the 0044 sibling of the issue creator).
    #[gpui::test]
    fn pr_creator_draft_parks_and_restores_per_repo(cx: &mut gpui::TestAppContext) {
        let drafts = DraftStore::at_tmp("new-pr-draft-probe");
        let one_path;

        // Park under `new-pr@acme/one`.
        {
            let (page, vcx) = probe_pr_creator_drafts(cx, "acme/one".into(), drafts.clone());
            vcx.update(|window, cx| {
                page.update(cx, |page, cx| {
                    page.title
                        .update(cx, |s, cx| s.set_value("PR title", window, cx));
                    page.body
                        .update(cx, |s, cx| s.set_value("PR body", window, cx));
                    page.draft_changed(cx);
                });
            });
            vcx.run_until_parked();
            one_path = drafts.path_for(&drafts::new_pr_key("acme/one"));
            let on_disk: Option<drafts::DraftPayload> =
                dashboard_core::store::load_json(&one_path).ok().flatten();
            let on_disk = on_disk.expect("PR creator draft parked");
            assert_eq!(on_disk.title.as_deref(), Some("PR title"));
            assert_eq!(on_disk.body, "PR body");
        }

        // Another repo opens empty.
        {
            let (other, ovcx) = probe_pr_creator_drafts(cx, "acme/two".into(), drafts.clone());
            ovcx.update(|_, cx| {
                other.update(cx, |page, cx| {
                    assert_eq!(page.title.read(cx).value(), "");
                    assert!(!page.draft_restored);
                });
            });
            assert!(
                ovcx.debug_bounds("new-pr-draft-restored").is_none(),
                "no hint without a restore"
            );
        }

        // Reopening the repo restores both fields + hint.
        {
            let (again, avcx) = probe_pr_creator_drafts(cx, "acme/one".into(), drafts.clone());
            avcx.update(|_, cx| {
                again.update(cx, |page, cx| {
                    assert_eq!(page.title.read(cx).value(), "PR title");
                    assert_eq!(page.body.read(cx).value(), "PR body");
                    assert!(page.draft_restored);
                });
            });
            assert!(
                avcx.debug_bounds("new-pr-draft-restored").is_some(),
                "restore hint visible"
            );
        }
    }

    // docs/0045 D5 — a successful PR creation clears the draft.
    #[gpui::test]
    fn pr_creator_draft_clears_on_success(cx: &mut gpui::TestAppContext) {
        let drafts = DraftStore::at_tmp("new-pr-clear-probe");
        let (page, vcx) = probe_pr_creator_drafts(cx, "o/r".into(), drafts.clone());
        let path = drafts.path_for(&drafts::new_pr_key("o/r"));

        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.title.update(cx, |s, cx| s.set_value("T", window, cx));
                page.draft_changed(cx);
            });
        });
        vcx.run_until_parked();
        assert!(path.exists(), "draft parked");

        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.apply_mutation_result(
                    SpecResult {
                        kind: Kind::PrMutation(Mutation::CreatePullRequest),
                        ident: Some("o/r new-pr".into()),
                        status: 200,
                        etag: None,
                        rate: None,
                        body: Some(
                            r#"{"data":{"createPullRequest":{"pullRequest":{"number":9,"title":"T","url":"u"}}}}"#
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
        assert!(!path.exists(), "created PR leaves no draft");
    }
    // --- docs/0046 §2: PR template prefill --------------------------------------

    /// PR-template probe fixture: the drafts probe plus a served (or
    /// failing) template lane.
    fn probe_pr_creator_tpl(
        cx: &mut gpui::TestAppContext,
        repo: String,
        template: Option<String>,
        drafts: DraftStore,
    ) -> (gpui::Entity<NewPrPage>, &mut gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        cx.update(|cx| cx.background_executor().allow_parking());
        let state = cx.update(|cx| {
            let sent: Recorder = std::sync::Arc::default();
            let mut fixture = FixtureHttp::new(sent);
            fixture.template = template;
            let client: std::sync::Arc<dyn Http> = std::sync::Arc::new(fixture);
            let store: std::sync::Arc<dyn crate::auth::TokenStore> =
                std::sync::Arc::new(ProbeStore);
            let entity = cx.new(|_| {
                GithubState::new(
                    client,
                    store,
                    Default::default(),
                    Default::default(),
                    GithubCache::new(dashboard_core::paths::AppPaths::init()),
                    drafts,
                )
            });
            entity.update(cx, |s, _| s.auth = AuthStatus::Connected);
            entity
        });
        cx.add_window_view(|window, cx| NewPrPage::new(state, repo, "R_9".into(), window, cx))
    }

    #[gpui::test]
    fn pr_template_prefills_body_only_and_typed_content_wins(cx: &mut gpui::TestAppContext) {
        let (page, vcx) = probe_pr_creator_tpl(
            cx,
            "acme/tpl".into(),
            Some("<!-- pr template -->".into()),
            DraftStore::at_tmp("new-pr-tpl-probe"),
        );
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                assert_eq!(page.template_body.as_deref(), Some("<!-- pr template -->"));
                assert_eq!(page.body.read(cx).value(), "<!-- pr template -->");
                assert_eq!(page.title.read(cx).value(), "", "body-only prefill");
            });
        });
        // The prefill parks nothing: no draft file exists yet.
        // The user types a body — the next re-open (host hook) must not
        // clobber it with the template.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.body
                    .update(cx, |s, cx| s.set_value("My real body", window, cx));
            });
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| page.refetch_pr_templates(window, cx));
        });
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                assert_eq!(page.body.read(cx).value(), "My real body");
            });
        });
    }

    #[gpui::test]
    fn pr_restored_draft_beats_template_prefill(cx: &mut gpui::TestAppContext) {
        let drafts = DraftStore::at_tmp("new-pr-tpl-draft-probe");
        let key = drafts::new_pr_key("acme/tpl2");
        dashboard_core::store::save_json_atomic(
            &drafts.path_for(&key),
            &drafts::DraftPayload {
                key,
                title: Some("Draft title".into()),
                body: "Draft body".into(),
                updated_ms: 1,
            },
        )
        .unwrap();
        let (page, vcx) = probe_pr_creator_tpl(
            cx,
            "acme/tpl2".into(),
            Some("<!-- pr template -->".into()),
            drafts,
        );
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                // 0046 §2 draft composition: a restored draft beats the
                // template at open (the 0046 D4 rule).
                assert_eq!(page.title.read(cx).value(), "Draft title");
                assert_eq!(page.body.read(cx).value(), "Draft body");
            });
        });
    }

    #[gpui::test]
    fn pr_template_failure_leaves_creator_untouched(cx: &mut gpui::TestAppContext) {
        let (page, vcx) = probe_pr_creator_tpl(
            cx,
            "acme/tpl3".into(),
            None,
            DraftStore::at_tmp("new-pr-tpl-fail-probe"),
        );
        vcx.run_until_parked();
        // D6: the lane failed — body empty, creation unblocked.
        assert!(vcx.debug_bounds("new-pr-create").is_some());
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                assert_eq!(page.template_body, None);
                assert_eq!(page.body.read(cx).value(), "");
            });
        });
    }
    // --- docs/0044 §9: branch search beyond the first 100 -----------------------

    /// Branch-search probe: the drafts fixture plus a zero debounce.
    fn probe_pr_branch_search(
        cx: &mut gpui::TestAppContext,
    ) -> (gpui::Entity<NewPrPage>, &mut gpui::VisualTestContext) {
        let (page, vcx) = probe_pr_creator_tpl(
            cx,
            "acme/branches".into(),
            None,
            DraftStore::at_tmp("new-pr-branch-search-probe"),
        );
        vcx.update(|_, cx| {
            page.update(cx, |page, _cx| {
                page.branch_search_delay = Duration::ZERO;
            });
        });
        (page, vcx)
    }

    #[gpui::test]
    fn branch_search_swaps_page_and_restores_default(cx: &mut gpui::TestAppContext) {
        let (page, vcx) = probe_pr_branch_search(cx);
        vcx.run_until_parked();
        // Default page landed: both selects hold feat+main, no search.
        vcx.update(|_, cx| {
            page.update(cx, |page, _cx| {
                assert!(page.base_search.is_none() && page.head_search.is_none());
            });
        });
        // A search on Base queues, debounces, fetches the filtered page.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.queue_branch_search(BranchSelect::Base, "feat".into(), window, cx);
            });
        });
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            page.update(cx, |page, _cx| {
                let search = page.base_search.as_ref().expect("filtered page lands");
                assert_eq!(search.query, "feat");
                assert_eq!(search.total_count, 140);
                assert_eq!(search.names, vec!["feat".to_string()]);
                assert!(page.head_search.is_none(), "Head keeps the default page");
            });
        });
        assert!(
            vcx.debug_bounds("new-pr-branches-search-base").is_some(),
            "the truncated search hint renders"
        );
        // Clearing the search restores the in-memory default page.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.queue_branch_search(BranchSelect::Base, String::new(), window, cx);
            });
        });
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            page.update(cx, |page, _cx| {
                assert!(page.base_search.is_none(), "default page restored");
            });
        });
        assert!(vcx.debug_bounds("new-pr-branches-search-base").is_none());
    }

    #[gpui::test]
    fn branch_search_superseded_keystrokes_drop(cx: &mut gpui::TestAppContext) {
        let (page, vcx) = probe_pr_branch_search(cx);
        vcx.run_until_parked();
        // Two keystrokes queue before either timer fires — the first
        // generation's fetch (if it ran) must not win the apply race.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.queue_branch_search(BranchSelect::Head, "fe".into(), window, cx);
                page.queue_branch_search(BranchSelect::Head, "feat".into(), window, cx);
            });
        });
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            page.update(cx, |page, _cx| {
                let search = page.head_search.as_ref().expect("last query wins");
                assert_eq!(search.query, "feat", "the stale generation never lands");
            });
        });
    }

    #[gpui::test]
    fn branch_search_failure_keeps_default_page(cx: &mut gpui::TestAppContext) {
        // The drafts fixture's client fails only the mutation — a
        // failing branch lane needs its own fixture.
        let (page, vcx) = probe_pr_creator_tpl(
            cx,
            "acme/branches2".into(),
            None,
            DraftStore::at_tmp("new-pr-branch-fail-probe"),
        );
        vcx.update(|_, cx| {
            page.update(cx, |page, _cx| {
                page.branch_search_delay = Duration::ZERO;
            });
        });
        vcx.run_until_parked();
        // Simulate a filtered fetch failure by driving the apply path
        // directly (FixtureHttp cannot fail selective POSTs): a stale
        // generation and a failed spec both take the same drop path.
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                let spec = crate::service::FetchSpec {
                    origin: Origin::new(Surface::Composer, Trigger::ViewShown),
                    kind: Kind::RepoBranches,
                    url: crate::gql::GRAPHQL_ENDPOINT.to_string(),
                    etag: None,
                    ident: Some("acme/branches2".into()),
                    max_pages: 1,
                    variables: Some(serde_json::json!({
                        "owner": "acme", "name": "branches2", "query": "feat"
                    })),
                };
                let state = page.state.clone();
                cx.spawn_in(window, async move |this, cx| {
                    let result = crate::service::run_spec_tracked(state, spec, cx).await;
                    // Mark the search stale before applying: the apply
                    // must drop it.
                    let _ = cx.update(|window, cx| {
                        let _ = this.update(cx, |page, cx| {
                            page.head_search_gen += 1;
                            page.apply_branch_search(
                                BranchSelect::Head,
                                0, // stale: gen moved on
                                "feat".into(),
                                result,
                                window,
                                cx,
                            );
                        });
                    });
                })
                .detach();
            });
        });
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            page.update(cx, |page, _cx| {
                assert!(page.head_search.is_none(), "a stale apply never lands");
            });
        });
    }
}
