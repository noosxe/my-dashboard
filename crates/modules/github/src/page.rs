//! Module page (docs/0005 §6.3, amended by 0009): two-column layout —
//! searchable org `Select` + virtualized searchable repo `List` (left),
//! repo header + GroupBoxes (right, independently scrollable) — plus the
//! connect panel (§6.2) for every non-connected state.
//!
//! Both left-column components are gpui-component entities constructed
//! once here (0009 D3): their item-replacement and selection APIs need
//! `&mut Window`, so background data sync goes through the stored window
//! handle (D6) while user interaction arrives via `subscribe_in`.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Icon, IndexPath, Sizable as _,
    avatar::Avatar,
    badge::Badge,
    button::{Button, ButtonVariants},
    group_box::GroupBox,
    h_flex,
    list::{List, ListDelegate, ListEvent, ListItem, ListState},
    searchable_list::SearchableVec,
    select::{Select, SelectEvent, SelectState},
    spinner::Spinner,
    v_flex,
};

use crate::api;
use crate::card::{REPO_LOCK, github_mark};
use crate::gql;
use crate::service::{
    AuthStatus, ConnectPhase, FetchSpec, GithubState, Kind, run_spec, start_connect,
};

pub(crate) struct GithubPage {
    state: Entity<GithubState>,
    /// Org login chosen in the Select (`None` = the first org, i.e. the
    /// personal account — Q10; resolved on orgs arrival, 0009 D6).
    org: Option<SharedString>,
    org_select: Entity<SelectState<SearchableVec<SharedString>>>,
    repo_list: Entity<ListState<RepoListDelegate>>,
    /// Window access for observer-time entity updates (0009 D6).
    window: AnyWindowHandle,
    /// Last-synced item keys — compare-first background sync so untouched
    /// ticks cost two vec comparisons and no entity writes (0009 D6).
    synced_orgs: Vec<SharedString>,
    synced_repos: Vec<String>,
}

impl GithubPage {
    pub fn new(state: Entity<GithubState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let org_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(Vec::<SharedString>::new()),
                None,
                window,
                cx,
            )
            .searchable(true)
        });
        let repo_list =
            cx.new(|cx| ListState::new(RepoListDelegate::default(), window, cx).searchable(true));

        // Org switch (0009 D5): adopt the login, drop the repo selection
        // (it belongs to the old org), repopulate, reset the search.
        cx.subscribe_in(
            &org_select,
            window,
            |this, _, event: &SelectEvent<SearchableVec<SharedString>>, window, cx| {
                let SelectEvent::Confirm(Some(login)) = event else {
                    return;
                };
                this.org = Some(login.clone());
                this.state.update(cx, |s, _| s.selected = None);
                this.synced_repos.clear();
                this.sync_from_state_in(window, cx);
                this.repo_list.update(cx, |list, cx| {
                    list.set_query("", window, cx);
                });
                cx.notify();
            },
        )
        .detach();

        // Repo selection (0009 D4): click / Enter = confirm → the same
        // fetch path the manual rows used.
        cx.subscribe_in(
            &repo_list,
            window,
            |this, _, event: &ListEvent, window, cx| {
                if let ListEvent::Confirm(ix) = event {
                    let full_name = this.repo_list.read(cx).delegate().repo_full_name(*ix);
                    if let Some(full_name) = full_name {
                        this.select_repo(full_name, window, cx);
                    }
                }
            },
        )
        .detach();

        // Background data arrival (0009 D6): orgs/repos land on the state
        // entity asynchronously; this observer shuttles them into the
        // Select/List entities through the stored window handle.
        cx.observe(&state, |this, _, cx| this.sync_from_state(cx))
            .detach();

        let page = Self {
            state,
            org: None,
            org_select,
            repo_list,
            window: window.window_handle(),
            synced_orgs: Vec::new(),
            synced_repos: Vec::new(),
        };
        // Seed from whatever the state already holds (last-known
        // caches) — but NOT here: Module::init runs inside
        // `cx.open_window`'s build closure, before the window handle is
        // registered (gpui `App::open_window` inserts it only after
        // `build_root_view` returns), so `window.update` would fail
        // here and the failure would be invisible. `defer_in` runs at
        // the end of the current effect cycle, window in hand.
        cx.defer_in(window, |this, window, cx| {
            this.sync_from_state_in(window, cx);
        });
        page
    }

    /// Effective org login: the Select choice or the default (first =
    /// personal, Q10). Only relevant before orgs arrive (D6 resolves the
    /// choice on arrival).
    fn effective_org(&self, cx: &Context<Self>) -> Option<SharedString> {
        self.org.clone().or_else(|| {
            self.state
                .read(cx)
                .orgs
                .first()
                .map(|o| o.login.clone().into())
        })
    }

    /// Repos under the effective org (all repos before orgs are loaded).
    fn visible_repos(&self, cx: &Context<Self>) -> Vec<gql::Repo> {
        let state = self.state.read(cx);
        match self.effective_org(cx) {
            Some(org) => state
                .repos
                .iter()
                .filter(|r| r.owner.login == org)
                .cloned()
                .collect(),
            None => state.repos.clone(),
        }
    }

    /// Observer entry (0009 D6): `cx.observe` has no window, so go
    /// through the stored handle. A failed update (window closing)
    /// leaves the guards below stale — the next observer pass retries.
    fn sync_from_state(&mut self, cx: &mut Context<Self>) {
        let window = self.window;
        let page = cx.entity();
        let _ = window.update(cx, |_, window, cx| {
            page.update(cx, |this, cx| this.sync_from_state_in(window, cx));
        });
    }

    /// Compare-first sync of the Select/List entities from the state
    /// (0009 D6). The `synced_*` guards are committed only AFTER the
    /// entity updates ran — a sync that never reached a window is not
    /// recorded as done.
    fn sync_from_state_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let logins: Vec<SharedString> = self
            .state
            .read(cx)
            .orgs
            .iter()
            .map(|o| o.login.clone().into())
            .collect();
        if logins != self.synced_orgs {
            let choice = next_org_selection(self.org.as_deref(), &logins);
            self.org_select.update(cx, |s, cx| {
                s.set_items(SearchableVec::from(logins.clone()), window, cx);
                if let Some(value) = &choice {
                    s.set_selected_value(value, window, cx);
                }
            });
            self.org = choice;
            self.synced_orgs = logins.clone();
            log::debug!(
                "org select synced: {} orgs, selected: {:?}",
                self.synced_orgs.len(),
                self.org
            );
        }

        let repos = self.visible_repos(cx);
        let keys: Vec<String> = repos.iter().map(|r| r.full_name.clone()).collect();
        if keys != self.synced_repos {
            let selected = self.state.read(cx).selected.clone();
            let position = selected
                .as_deref()
                .and_then(|f| repos.iter().position(|r| r.full_name == f))
                .map(IndexPath::new);
            self.repo_list.update(cx, |l, cx| {
                l.delegate_mut().set_repos(repos);
                // Non-scrolling selection sync: keep the highlight on
                // the selected repo without yanking the viewport.
                l.set_selected_index(position, window, cx);
            });
            self.synced_repos = keys.clone();
            log::debug!("repo list synced: {} repos", self.synced_repos.len());
        }

        cx.notify();
    }

    fn select_repo(
        &mut self,
        full_name: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Docs/0007 D6: the view paints immediately from the in-memory
        // snapshot; these specs are the edge-freshness repo-scoped
        // snapshot plus the one-shot repo notifications fetch.
        log::debug!("repo selected: {full_name}");
        let specs = self.state.update(cx, |s, _| {
            s.selected = Some(full_name.to_string());
            s.repo_open_specs(&full_name)
        });
        self.run_specs(specs, window, cx);
    }

    /// Manual refresh (Q7): notifications + a fresh global snapshot, now.
    fn refresh_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        log::debug!("manual refresh");
        let mut specs = self.state.update(cx, |s, _| {
            vec![
                s.spec(Kind::Notifications, api::notifications_url(1), Some(10)),
                s.snapshot_spec(),
            ]
        });
        if let Some(full_name) = self.state.read(cx).selected.clone() {
            specs.extend(self.state.update(cx, |s, _| s.repo_open_specs(&full_name)));
        }
        self.run_specs(specs, window, cx);
    }

    /// Page-local counterpart of the tick loop: run specs on the background
    /// executor, apply on the main thread, then the §3.12 refresh.
    fn run_specs(&mut self, specs: Vec<FetchSpec>, window: &mut Window, cx: &mut Context<Self>) {
        if specs.is_empty() {
            return;
        }
        let client = self.state.update(cx, |s, _| s.client.clone());
        let state = self.state.clone();
        let window_handle = window.window_handle();
        cx.spawn(async move |_this, cx| {
            for spec in specs {
                let client = client.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move { run_spec(&*client, &spec) })
                    .await;
                cx.update(|cx| state.update(cx, |s, _| s.apply(result)));
            }
            let _ = window_handle.update(cx, |_, window, _| window.refresh());
        })
        .detach();
    }

    fn render_pr_row(&self, pr: &gql::PullRequest, cx: &Context<Self>) -> AnyElement {
        let url = pr.url.clone();
        div()
            .id(SharedString::from(format!("pr-{}", pr.key)))
            .px_2()
            .py_1p5()
            .rounded_sm()
            .text_sm()
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().accent.opacity(0.08)))
            .on_click(move |_, _, cx| cx.open_url(&url))
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .when(pr.draft, |row| {
                                row.child(Badge::new().xsmall().child("Draft"))
                            })
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(pr.title.clone()),
                            )
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("#{}", pr.number)),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .when_some(pr.author.as_ref(), |row, author| {
                                row.child(
                                    h_flex()
                                        .gap_1()
                                        .items_center()
                                        .child(
                                            Avatar::new().src(author.avatar_url.clone()).xsmall(),
                                        )
                                        .child(
                                            div()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(author.login.clone()),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(relative_age(pr.updated_at.as_deref())),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_notification_row(&self, n: &api::Notification, cx: &Context<Self>) -> AnyElement {
        div()
            .px_2()
            .py_1p5()
            .text_sm()
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Badge::new().xsmall().child(n.subject.kind.clone()))
                            .child(n.subject.title.clone()),
                    )
                    .child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(relative_age(n.updated_at.as_deref())),
                    ),
            )
            .into_any_element()
    }
}

/// Resolve the org choice after the login list changes (0009 D6): keep the
/// current org when it still exists, otherwise fall back to the first
/// login (the personal account, Q10). Pure — table-tested.
fn next_org_selection(current: Option<&str>, logins: &[SharedString]) -> Option<SharedString> {
    match current {
        Some(current) if logins.iter().any(|l| &**l == current) => Some(current.to_string().into()),
        _ => logins.first().cloned(),
    }
}

/// Repo-list delegate (0009 D2): the visible repos plus the filtered view
/// driven by the List's search input. Data-only — selection reacts flow
/// through `ListEvent` on the page, not through the delegate.
#[derive(Default)]
struct RepoListDelegate {
    all: Vec<gql::Repo>,
    matched: Vec<gql::Repo>,
    query: String,
    selected: Option<IndexPath>,
}

impl RepoListDelegate {
    /// Replace the data, re-applying the active query (background
    /// refreshes must not clear the user's filter).
    fn set_repos(&mut self, repos: Vec<gql::Repo>) {
        self.all = repos;
        self.apply_query(&self.query.clone());
    }

    /// Case-insensitive substring match on name or description (D2).
    fn apply_query(&mut self, query: &str) {
        self.query = query.to_string();
        let q = self.query.to_lowercase();
        self.matched = self
            .all
            .iter()
            .filter(|r| {
                q.is_empty()
                    || r.name.to_lowercase().contains(&q)
                    || r.description
                        .as_deref()
                        .is_some_and(|d| d.to_lowercase().contains(&q))
            })
            .cloned()
            .collect();
    }

    fn repo_at(&self, ix: IndexPath) -> Option<&gql::Repo> {
        self.matched.get(ix.row)
    }

    fn repo_full_name(&self, ix: IndexPath) -> Option<SharedString> {
        self.repo_at(ix).map(|r| r.full_name.clone().into())
    }
}

impl ListDelegate for RepoListDelegate {
    type Item = ListItem;

    fn perform_search(
        &mut self,
        query: &str,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.apply_query(query);
        Task::ready(())
    }

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.matched.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let repo = self.repo_at(ix)?.clone();
        Some(
            ListItem::new(SharedString::from(format!("repo-{}", repo.key)))
                .text_sm()
                .child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .child(div().truncate().child(repo.name.clone()))
                        .when(repo.private, |row| {
                            // GitHub's own list treatment: small muted lock
                            // glyph, no text badge.
                            row.child(private_lock(cx.theme().muted_foreground))
                        }),
                ),
        )
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        let text = if self.query.trim().is_empty() {
            "No repositories yet"
        } else {
            "No matches"
        };
        h_flex()
            .size_full()
            .justify_center()
            .pt_6()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(text)
            .into_any_element()
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix;
    }
}

impl Render for GithubPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let auth = self.state.read(cx).auth.clone();

        // Non-connected: the connect panel owns the page (§6.4 auth row).
        if !auth.connected() {
            return connect_panel(&auth, cx);
        }

        let (selected_full, prs, repo_notifs) = {
            let state = self.state.read(cx);
            let prs = state
                .selected
                .as_ref()
                // Docs/0007 D2: repo views paint from the in-memory
                // global snapshot — every repo's PRs are already here.
                .and_then(|f| state.repos.iter().find(|r| &r.full_name == f))
                .map(|r| r.prs.nodes.clone())
                .unwrap_or_default();
            let notifs = state
                .selected
                .as_ref()
                .and_then(|f| state.repo_notifications.get(f))
                .cloned()
                .unwrap_or_default();
            (state.selected.clone(), prs, notifs)
        };
        let selected_repo = selected_full.as_ref().and_then(|full| {
            self.state
                .read(cx)
                .repos
                .iter()
                .find(|r| &r.full_name == full)
                .cloned()
        });

        let left = self.render_left_column(cx);
        let right = self.render_right_column(&selected_repo, &prs, &repo_notifs, cx);
        h_flex()
            .size_full()
            .child(left)
            .child(right)
            .into_any_element()
    }
}

impl GithubPage {
    fn render_left_column(&mut self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .w(px(280.))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .p_2()
            .gap_2()
            .child(
                Select::new(&self.org_select)
                    .placeholder("Organization…")
                    .search_placeholder("Filter organizations…")
                    .empty(|_, cx| {
                        h_flex()
                            .w_full()
                            .justify_center()
                            .py_4()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("No organizations yet")
                    })
                    .small()
                    .w_full(),
            )
            // The List is a direct flex child with flex_1 — the blessed
            // upstream usage (list_story.rs): wrapping it in an extra
            // flex_1 div makes the List's size_full() resolve against a
            // flex-derived height and the whole list collapses to zero.
            .child(
                List::new(&self.repo_list)
                    .search_placeholder("Filter repositories…")
                    .small()
                    .flex_1()
                    .min_h_0()
                    .w_full(),
            )
            .into_any_element()
    }

    /// Right column — independently scrollable (docs/0005 §6.3).
    fn render_right_column(
        &mut self,
        selected_repo: &Option<gql::Repo>,
        prs: &[gql::PullRequest],
        repo_notifs: &[api::Notification],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let detail: AnyElement = match selected_repo {
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child("Select a repository")
                .into_any_element(),
            Some(repo) => v_flex()
                .p_4()
                .gap_3()
                // Header: exactly two text rows (owner-specified).
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            h_flex()
                                .id("repo-link")
                                .gap_1p5()
                                .items_center()
                                .cursor_pointer()
                                .hover(|s| s.underline())
                                // Same affordance as PR rows: the repo title
                                // links to the repo on github.com. URL is
                                // derived from full_name — cache-proof (no
                                // dependency on html_url being present in
                                // last-known payloads).
                                .on_click({
                                    let full_name = repo.full_name.clone();
                                    move |_, _, cx| {
                                        cx.open_url(&format!("https://github.com/{full_name}"))
                                    }
                                })
                                .child(
                                    div()
                                        .text_base()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(repo.full_name.clone()),
                                )
                                .when(repo.private, |row| {
                                    row.child(private_lock(cx.theme().muted_foreground))
                                }),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    repo.description
                                        .clone()
                                        .unwrap_or_else(|| "No description".into()),
                                ),
                        ),
                )
                .child(
                    h_flex().justify_end().child(
                        Button::new("refresh")
                            .label("Refresh")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.refresh_now(window, cx);
                            })),
                    ),
                )
                .child(
                    GroupBox::new()
                        .id("prs-box")
                        .title(format!("Open PRs ({})", prs.len()))
                        .child(if prs.is_empty() {
                            muted_cell("No open PRs", cx)
                        } else {
                            v_flex()
                                .gap_0p5()
                                .children(prs.iter().map(|p| self.render_pr_row(p, cx)))
                                .into_any_element()
                        }),
                )
                .child(
                    GroupBox::new()
                        .id("notifs-box")
                        .title(format!("Notifications ({})", repo_notifs.len()))
                        .child(if repo_notifs.is_empty() {
                            muted_cell("No unread notifications", cx)
                        } else {
                            v_flex()
                                .gap_0p5()
                                .children(
                                    repo_notifs
                                        .iter()
                                        .map(|n| self.render_notification_row(n, cx)),
                                )
                                .into_any_element()
                        }),
                )
                .into_any_element(),
        };
        div()
            .id("repo-detail")
            .flex_1()
            .h_full()
            .overflow_y_scroll()
            .child(detail)
            .into_any_element()
    }
}

/// Private-repo indicator: octicon lock-16 at 12 px in the muted color —
/// the same treatment as github.com's own repo lists (glyph only, no text).
fn private_lock(color: gpui::Hsla) -> Icon {
    Icon::empty().path(REPO_LOCK).size_3().text_color(color)
}
fn muted_cell(text: &str, cx: &Context<GithubPage>) -> AnyElement {
    div()
        .px_2()
        .py_1p5()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.to_string())
        .into_any_element()
}

/// The §6.2 connect panel — owns the page until the app is connected.
fn connect_panel(auth: &AuthStatus, cx: &mut Context<GithubPage>) -> AnyElement {
    let (body, action_label) = match auth {
        AuthStatus::ConnectFlow(ConnectPhase::RequestingCode) => {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(Spinner::new())
                .child("Contacting GitHub…")
                .into_any_element();
        }
        AuthStatus::ConnectFlow(ConnectPhase::AwaitApproval {
            user_code,
            verification_uri,
        }) => {
            let code = user_code.clone();
            let uri = verification_uri.clone();
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(github_mark(cx).into_any_element())
                .child("Enter this code on github.com/login/device:")
                .child(
                    div()
                        .text_2xl()
                        .font_family("monospace")
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(code.clone()),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("copy-code")
                                .label("Copy code")
                                .ghost()
                                .small()
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                                }),
                        )
                        .child(
                            Button::new("open-device")
                                .label("Open github.com/login/device")
                                .small()
                                .on_click(move |_, _, cx| {
                                    cx.open_url(&uri);
                                }),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Spinner::new().small())
                        .child("Waiting for approval…"),
                )
                .into_any_element();
        }
        AuthStatus::ConnectFlow(ConnectPhase::Denied) => (
            "Connection was cancelled on GitHub. You can start again.".to_string(),
            Some("Try again"),
        ),
        AuthStatus::ConnectFlow(ConnectPhase::Expired) => (
            "The code expired after 15 minutes. Start again?".to_string(),
            Some("Start again"),
        ),
        AuthStatus::ConnectFlow(ConnectPhase::Failed(m)) => {
            (format!("Something went wrong: {m}"), Some("Retry"))
        }
        AuthStatus::KeychainError(e) => (
            format!(
                "No OS keychain is available ({e}). Start gnome-keyring or a \
                 compatible Secret Service provider, then retry."
            ),
            None,
        ),
        AuthStatus::TokenRejected => (
            "GitHub revoked this connection. Re-authorize to continue.".to_string(),
            Some("Reconnect"),
        ),
        AuthStatus::Disconnected => (
            "Connect your GitHub account to see notifications and pull \
             requests across your repositories."
                .to_string(),
            Some("Connect GitHub"),
        ),
        AuthStatus::Connected => unreachable!("panel only renders when not connected"),
    };

    let mut panel = v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_3()
        .child(github_mark(cx).into_any_element())
        .child(
            div()
                .max_w(px(420.))
                .text_center()
                .text_color(cx.theme().muted_foreground)
                .child(body),
        );
    if let Some(label) = action_label {
        panel = panel.child(
            Button::new("page-connect")
                .label(label)
                .on_click(cx.listener(|this, _, window, cx| {
                    start_connect(this.state.clone(), window.window_handle(), cx);
                })),
        );
    }
    panel.into_any_element()
}

/// "2h" / "3d" / "now" from an RFC-3339 timestamp.
pub(crate) fn relative_age(updated_at: Option<&str>) -> String {
    let Some(ts) = updated_at.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()) else {
        return String::new();
    };
    let secs = (chrono::Utc::now() - ts.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0);
    match secs {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    // NOTE: no `use super::*` here — page.rs glob-imports `gpui::*`, whose
    // `test` re-export would shadow the builtin `#[test]` attribute and
    // blow the proc-macro stack (rustc SIGSEGV). Import selectively.
    use super::{IndexPath, RepoListDelegate, next_org_selection};
    use crate::gql;
    use gpui::AppContext as _;
    use gpui::{SharedString, TestAppContext};
    use gpui_component::list::ListState;
    use gpui_component::searchable_list::SearchableVec;
    use gpui_component::select::SelectState;

    fn repo(key: &str, description: Option<&str>) -> gql::Repo {
        let (owner, name) = key.split_once('/').unwrap_or(("acme", key));
        gql::Repo {
            key: key.to_string(),
            database_id: None,
            name: name.to_string(),
            full_name: key.to_string(),
            owner: gql::Owner {
                login: owner.to_string(),
            },
            description: description.map(str::to_string),
            private: false,
            pushed_at: None,
            prs: gql::PrSummary {
                total_count: 0,
                nodes: Vec::new(),
            },
            issues: gql::IssueSummary {
                total_count: 0,
                nodes: Vec::new(),
            },
        }
    }

    fn keys(delegate: &RepoListDelegate) -> Vec<&str> {
        delegate
            .matched
            .iter()
            .map(|r| r.full_name.as_str())
            .collect()
    }

    #[test]
    fn repo_filter_matches_name_or_description_case_insensitively() {
        let mut d = RepoListDelegate::default();
        d.set_repos(vec![
            repo("acme/widget", Some("A delightful toolkit")),
            repo("acme/other", None),
            repo("acme/zephyr", Some("wind speed monitor")),
        ]);
        assert_eq!(keys(&d), ["acme/widget", "acme/other", "acme/zephyr"]);

        d.apply_query("WIDGET");
        assert_eq!(keys(&d), ["acme/widget"]);

        d.apply_query("wind");
        assert_eq!(keys(&d), ["acme/zephyr"]);

        // `full_name` is deliberately not a match key (D2): the rows show
        // only the name, and the org filter already scopes the owner.
        d.apply_query("acme/");
        assert_eq!(keys(&d), [] as [&str; 0]);
    }

    #[test]
    fn set_repos_keeps_the_active_query() {
        let mut d = RepoListDelegate::default();
        d.set_repos(vec![repo("acme/one", None), repo("acme/two", None)]);
        d.apply_query("two");
        assert_eq!(keys(&d), ["acme/two"]);

        // Background refresh arrives: the filter survives, the data swaps.
        d.set_repos(vec![repo("acme/two", None), repo("acme/three", None)]);
        assert_eq!(keys(&d), ["acme/two"]);

        // Clearing the query restores the full new list.
        d.apply_query("");
        assert_eq!(keys(&d), ["acme/two", "acme/three"]);
    }

    #[test]
    fn no_matches_after_filter() {
        let mut d = RepoListDelegate::default();
        d.set_repos(vec![repo("acme/one", None)]);
        d.apply_query("nothing-matches");
        assert!(d.matched.is_empty());
        assert_eq!(d.repo_full_name(IndexPath::new(0)), None);
    }

    #[test]
    fn repo_full_name_maps_indices_into_matched() {
        let mut d = RepoListDelegate::default();
        d.set_repos(vec![repo("acme/one", None), repo("acme/two", None)]);
        d.apply_query("two");
        assert_eq!(d.repo_full_name(IndexPath::new(0)), Some("acme/two".into()));
        // Out-of-range index (row 1 exists only unfiltered).
        assert_eq!(d.repo_full_name(IndexPath::new(1)), None);
    }

    #[test]
    fn next_org_selection_table() {
        let empty: Vec<SharedString> = Vec::new();
        let logins: Vec<SharedString> = vec!["acme".into(), "beta".into()];

        // Nothing to pick.
        assert_eq!(next_org_selection(None, &empty), None);
        // Q10 default: the first login (the personal account).
        assert_eq!(next_org_selection(None, &logins), Some("acme".into()));
        // Keep the current org when it still exists.
        assert_eq!(
            next_org_selection(Some("beta"), &logins),
            Some("beta".into())
        );
        // Vanished: reset to the first login.
        assert_eq!(
            next_org_selection(Some("gone"), &logins),
            Some("acme".into())
        );
        // Vanished and nothing left.
        assert_eq!(next_org_selection(Some("gone"), &empty), None);
    }

    /// Entity wiring smoke test (0009 §4): both left-column components
    /// construct against a real window, and their selection APIs round-trip.
    #[gpui::test]
    fn select_and_list_entities_construct_and_select(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let cx = cx.add_empty_window();
        cx.update(|window, cx| {
            let logins: Vec<SharedString> = vec!["acme".into(), "beta".into()];
            let select = cx.new(|cx| {
                SelectState::new(SearchableVec::from(logins), None, window, cx).searchable(true)
            });
            select.update(cx, |s, cx| {
                let beta: SharedString = "beta".into();
                s.set_selected_value(&beta, window, cx);
                assert_eq!(s.selected_value(), Some(&beta));
            });

            let mut delegate = RepoListDelegate::default();
            delegate.set_repos(vec![repo("acme/one", None), repo("acme/two", None)]);
            let list = cx.new(|cx| ListState::new(delegate, window, cx).searchable(true));
            list.update(cx, |l, cx| {
                l.set_selected_index(Some(IndexPath::new(1)), window, cx);
                assert_eq!(l.selected_index(), Some(IndexPath::new(1)));
                assert_eq!(
                    l.delegate().repo_full_name(IndexPath::new(1)),
                    Some("acme/two".into())
                );
            });
        });
    }
}
