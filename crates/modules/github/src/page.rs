//! Module page (docs/0005 §6.3): the owner-specified two-column layout —
//! org dropdown + repo list (left, independently scrollable), repo header +
//! GroupBoxes (right, independently scrollable) — plus the connect panel
//! (§6.2) for every non-connected state.
//!
//! Note: the org dropdown is a lightweight inline popover rather than
//! gpui-component's `Select`: `SelectState::set_items` requires `&mut
//! Window`, which reactive entity updates don't have — our org list arrives
//! asynchronously from the service.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Icon, Sizable as _, ThemeStyled as _,
    avatar::Avatar,
    badge::Badge,
    button::{Button, ButtonVariants},
    group_box::GroupBox,
    h_flex,
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
    /// Org login chosen in the dropdown (`None` = the first org, i.e. the
    /// personal account — Q10).
    org: Option<SharedString>,
    orgs_open: bool,
}

impl GithubPage {
    pub fn new(state: Entity<GithubState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            org: None,
            orgs_open: false,
        }
    }

    /// Effective org login: the dropdown choice or the default (first =
    /// personal, Q10).
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

    fn select_repo(
        &mut self,
        full_name: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Docs/0007 D6: the view paints immediately from the in-memory
        // snapshot; these specs are the edge-freshness repo-scoped
        // snapshot plus the one-shot repo notifications fetch.
        let specs = self.state.update(cx, |s, _| {
            s.selected = Some(full_name.to_string());
            s.repo_open_specs(&full_name)
        });
        self.run_specs(specs, window, cx);
    }

    /// Manual refresh (Q7): notifications + a fresh global snapshot, now.
    fn refresh_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    fn render_repo_row(&self, repo: &gql::Repo, cx: &mut Context<Self>) -> AnyElement {
        let full_name: SharedString = repo.full_name.clone().into();
        let selected = self
            .state
            .read(cx)
            .selected
            .as_deref()
            .is_some_and(|s| s == repo.full_name);
        let theme = cx.theme().clone();
        div()
            .id(SharedString::from(format!("repo-{}", repo.key)))
            .px_2()
            .py_1p5()
            .rounded_sm()
            .text_sm()
            .cursor_pointer()
            .when(selected, |s| s.bg(theme.accent.opacity(0.14)))
            .hover(|s| s.bg(theme.accent.opacity(0.08)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_repo(full_name.clone(), window, cx);
            }))
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(div().truncate().child(repo.name.clone()))
                    .when(repo.private, |row| {
                        // GitHub's own list treatment: small muted lock glyph,
                        // no text badge.
                        row.child(private_lock(theme.muted_foreground))
                    }),
            )
            .into_any_element()
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

    /// Org dropdown trigger. The open menu itself renders at the page root
    /// (see `render_org_overlay`) — gpui has no z-index: later children
    /// paint above earlier ones, so a menu nested in this column would be
    /// painted over by the repo rows below it (owner-reported).
    fn render_org_dropdown(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let logins: Vec<SharedString> = self
            .state
            .read(cx)
            .orgs
            .iter()
            .map(|o| o.login.clone().into())
            .collect();
        let current: SharedString = self
            .effective_org(cx)
            .unwrap_or_else(|| "Organization…".into());
        let theme = cx.theme().clone();

        div()
            .id("org-trigger")
            .w_full()
            .px_2()
            .py_1p5()
            .rounded_sm()
            .border_1()
            .border_color(theme.border)
            .cursor_pointer()
            .text_sm()
            .hover(|s| s.bg(theme.accent.opacity(0.06)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.orgs_open = !this.orgs_open;
                cx.notify();
            }))
            .child(
                h_flex().justify_between().items_center().child(
                    div()
                        .when(logins.is_empty(), |t| t.text_color(theme.muted_foreground))
                        .child(current),
                ),
            )
            .into_any_element()
    }

    /// Backdrop + open org menu, rendered as the LAST child of the page
    /// root so it paints above both columns. The backdrop catches every
    /// outside click (including the right column) to close the menu.
    /// Geometry: left column is p_2 (8 px) wide-padded, 280 px wide, and
    /// the trigger ends ~40 px from the page top — so the menu sits at
    /// top 42, left 8, width 264.
    fn render_org_overlay(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let logins: Vec<SharedString> = self
            .state
            .read(cx)
            .orgs
            .iter()
            .map(|o| o.login.clone().into())
            .collect();
        let theme = cx.theme().clone();

        div()
            .id("org-overlay")
            .absolute()
            .inset_0()
            .child(
                div()
                    .id("org-backdrop")
                    .absolute()
                    .inset_0()
                    // Swallow the click: closing on outside-click must not
                    // also activate the repo row underneath.
                    .occlude()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.orgs_open = false;
                        cx.notify();
                    })),
            )
            .child(
                v_flex()
                    .id("org-menu")
                    .absolute()
                    // Same reason as the backdrop: without a blocking hitbox,
                    // gpui dispatches the click to the repo row under the
                    // menu too (owner-reported click-through).
                    .occlude()
                    .top(px(42.))
                    .left(px(8.))
                    .w(px(264.))
                    .max_h(px(280.))
                    .overflow_y_scroll()
                    // Floating-surface semantic token — same paint path as
                    // gpui-component's own popup menus.
                    .popover_style(cx)
                    .p_1()
                    .children(logins.iter().map(|login| {
                        let login: SharedString = login.clone();
                        let login_for_click = login.clone();
                        div()
                            .id(SharedString::from(format!("org-{login}")))
                            .px_2()
                            .py_1p5()
                            .rounded_sm()
                            .text_sm()
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.accent.opacity(0.08)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.org = Some(login_for_click.clone());
                                this.orgs_open = false;
                                // Switching orgs clears the selection —
                                // the selected repo belongs to another org.
                                this.state.update(cx, |s, _| s.selected = None);
                                cx.notify();
                            }))
                            .child(login.clone())
                    })),
            )
            .into_any_element()
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
        let repos = self.visible_repos(cx);

        let left = self.render_left_column(&repos, cx);
        let right = self.render_right_column(&selected_repo, &prs, &repo_notifs, cx);
        // Root is the stacking context: the org overlay is appended last so
        // it paints above both columns (gpui paint order = child order).
        let mut root = h_flex().size_full().relative().child(left).child(right);
        if self.orgs_open {
            root = root.child(self.render_org_overlay(cx));
        }
        root.into_any_element()
    }
}

impl GithubPage {
    fn render_left_column(&mut self, repos: &[gql::Repo], cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .w(px(280.))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .p_2()
            .gap_2()
            .child(self.render_org_dropdown(cx))
            .child(
                div()
                    .id("repo-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .child(if repos.is_empty() {
                        muted_cell("No repositories yet", cx)
                    } else {
                        v_flex()
                            .gap_0p5()
                            .children(repos.iter().map(|r| self.render_repo_row(r, cx)))
                            .into_any_element()
                    }),
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
