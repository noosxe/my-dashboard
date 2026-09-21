//! Home overview card (docs/0005 §6.1): counts when connected, a connect
//! prompt otherwise — every [DG] state designed.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{ActiveTheme, Sizable as _, button::Button, spinner::Spinner, v_flex};

use dashboard_core::nav::{Navigator, Page};

use crate::page::stale_chip_label;
use crate::service::{AuthStatus, ConnectPhase, GithubState, start_connect};

pub(crate) const GITHUB_MARK: &str = "icons/github.svg";
/// Private-repo lock glyph (octicon lock-16, @primer/octicons 19.33.0).
pub(crate) const REPO_LOCK: &str = "icons/lock.svg";

/// The GitHub-mark SVG element (served by the app's asset source).
pub(crate) fn github_mark(cx: &App) -> impl IntoElement {
    svg()
        .path(GITHUB_MARK)
        .size_5()
        .text_color(cx.theme().foreground)
}

pub(crate) struct GithubCard {
    state: Entity<GithubState>,
    nav: Entity<Navigator>,
}

impl GithubCard {
    pub fn new(
        state: Entity<GithubState>,
        nav: Entity<Navigator>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        // docs/0038 D7: the card is the ever-visible monitoring surface
        // — it co-maintains the engagement flag (the service tick reads
        // it) even when the module page is closed. Seeded from the real
        // window state: the observers only fire on edges.
        let active = window.is_window_active();
        state.update(cx, |s, _| s.focus_engaged = active);
        cx.observe_window_activation(window, |this, window, cx| {
            let active = window.is_window_active();
            this.state.update(cx, |s, _| s.focus_engaged = active);
        })
        .detach();
        Self { state, nav }
    }

    fn open_module(&mut self, cx: &mut Context<Self>) {
        self.nav.update(cx, |nav, cx| {
            nav.go(Page::Module("github".into()), cx);
        });
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        start_connect(self.state.clone(), window.window_handle(), cx);
    }
}

impl Render for GithubCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);

        match state.auth.clone() {
            AuthStatus::Connected => {
                let (notifications, prs, issues) = state.card_counts();
                div()
                    .id("github-card")
                    .size_full()
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().accent.opacity(0.06)))
                    .role(accesskit::Role::ListItem)
                    .aria_label(format!(
                        "Open GitHub module — {notifications} notifications, {prs} open pull requests, {issues} open issues"
                    ))
                    .on_click(cx.listener(|this, _, _, cx| this.open_module(cx)))
                    .child(
                        v_flex()
                            .size_full()
                            .justify_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("Notifications {notifications}")),
                            )
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("Open PRs {prs}")),
                            )
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("Open Issues {issues}")),
                            )
                            // Cold-start staleness chip (docs/0034 D3):
                            // the disk-seeded counts may be days old.
                            .when_some(
                                stale_chip_label(state.snapshot_age, state.notifications_age),
                                |col, chip| {
                                    col.child(
                                        div()
                                            .debug_selector(|| "github-card-stale".to_string())
                                            .text_xs()
                                            .text_color(cx.theme().warning)
                                            .child(chip),
                                    )
                                },
                            ),
                    )
                    .into_any_element()
            }
            AuthStatus::ConnectFlow(ConnectPhase::RequestingCode) => {
                centered(Spinner::new().small(), "Contacting GitHub…", None, cx)
            }
            AuthStatus::ConnectFlow(ConnectPhase::AwaitApproval { user_code, .. }) => centered(
                Spinner::new().small(),
                format!("Code {user_code} — waiting for approval…"),
                None,
                cx,
            ),
            AuthStatus::ConnectFlow(ConnectPhase::Denied) => {
                let mark = github_mark(cx).into_any_element();
                centered(mark, "Connection cancelled — retry?", Some("Retry"), cx)
            }
            AuthStatus::ConnectFlow(ConnectPhase::Expired) => {
                let mark = github_mark(cx).into_any_element();
                centered(mark, "The code expired — try again.", Some("Retry"), cx)
            }
            AuthStatus::ConnectFlow(ConnectPhase::Failed(m)) => {
                let mark = github_mark(cx).into_any_element();
                centered(mark, m, Some("Retry"), cx)
            }
            AuthStatus::KeychainError(e) => {
                let mark = github_mark(cx).into_any_element();
                centered(mark, format!("Keychain unavailable: {e}"), None, cx)
            }
            AuthStatus::TokenRejected => {
                let mark = github_mark(cx).into_any_element();
                centered(
                    mark,
                    "Access was revoked — reconnect to continue.",
                    Some("Reconnect"),
                    cx,
                )
            }
            AuthStatus::Disconnected => {
                let mark = github_mark(cx).into_any_element();
                centered(
                    mark,
                    "Connect to see notifications, pull requests, and issues.",
                    Some("Connect GitHub"),
                    cx,
                )
            }
        }
    }
}

/// Card body for every non-connected state: mark, message, optional action.
fn centered(
    mark_or_spinner: impl IntoElement,
    message: impl Into<String>,
    action: Option<&str>,
    cx: &mut Context<GithubCard>,
) -> AnyElement {
    let mut col = v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .child(mark_or_spinner.into_any_element())
        .child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child(message.into()),
        );
    if let Some(label) = action {
        col = col.child(
            Button::new("card-connect")
                .label(label)
                .small()
                .on_click(cx.listener(|this, _, window, cx| this.connect(window, cx))),
        );
    }
    col.into_any_element()
}
