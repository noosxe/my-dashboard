//! Home overview card (docs/0005 §6.1): counts when connected, a connect
//! prompt otherwise — every [DG] state designed.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Sizable as _, button::Button, skeleton::Skeleton, spinner::Spinner, v_flex,
};

use dashboard_core::nav::{Navigator, Page};

use crate::page::stale_chip_label;
use crate::service::{AuthStatus, ConnectPhase, GithubState, run_read_deduped, start_connect};

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
    /// View-shown edge tracking (docs/0051 D4): the card fires its
    /// totals lane when the home page (re)becomes the active page.
    last_home_active: bool,
    /// Auth edge tracking: a reconnect while home is showing refetches
    /// even when retained totals exist.
    last_connected: bool,
    window: AnyWindowHandle,
}

impl GithubCard {
    pub fn new(
        state: Entity<GithubState>,
        nav: Entity<Navigator>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // docs/0051 D4 — view-shown fetch: nav transitions into Home fire
        // the totals lane; the docs/0031 inflight dedup absorbs repeats.
        let home_active = matches!(nav.read(cx).page, Page::Home);
        let connected = matches!(state.read(cx).auth, AuthStatus::Connected);
        let mut this = Self {
            state: state.clone(),
            nav: nav.clone(),
            last_home_active: home_active,
            last_connected: connected,
            window: window.window_handle(),
        };
        if home_active && connected {
            this.fire_card_totals(cx);
        }
        cx.observe(&nav, |this, nav, cx| {
            let active = matches!(nav.read(cx).page, Page::Home);
            let edge = active && !this.last_home_active;
            this.last_home_active = active;
            if edge {
                this.fire_card_totals(cx);
                // docs/0052 D5: view-shown fires the bell's lane too
                // (age-gated — the module's fire keeps it fresh).
                this.fire_notifications_if_stale(cx);
            }
        })
        .detach();
        cx.observe(&state, |this, state, cx| {
            cx.notify();
            // Post-connect while home is showing (docs/0051 D4): the OAuth
            // flow finishes on the home page — no nav edge will come.
            let connected = matches!(state.read(cx).auth, AuthStatus::Connected);
            let reconnected = connected && !this.last_connected;
            this.last_connected = connected;
            if reconnected && this.last_home_active {
                this.fire_card_totals(cx);
                // docs/0052 D5: same for the bell's lane.
                this.fire_notifications_if_stale(cx);
            }
        })
        .detach();
        // docs/0052 D4 — fetch on app refocus: the card is the app
        // home page's surface, so window activation fires its lanes
        // (totals + the bell's notifications, both at the 15 s floor —
        // the state-side gates decide; the home-active check keeps a
        // module-screen refocus off the card's lanes).
        cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() || !this.last_home_active {
                return;
            }
            this.fire_card_totals_if_stale(cx);
            this.fire_notifications_if_stale(cx);
        })
        .detach();
        // docs/0052 D6 — the render-only wake: relative ages advance
        // without a fetch. The loop dies with the card.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(crate::page::RENDER_WAKE)
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
        this
    }

    /// Fire the card's totals lane (docs/0051 D2/D4): spec through the
    /// shared docs/0031 dedup, result through the state's apply arm.
    fn fire_card_totals(&mut self, cx: &mut Context<Self>) {
        let (spec, gated) = {
            let s = self.state.read(cx);
            (s.card_totals_spec(), s.gated())
        };
        if gated {
            return;
        }
        let state = self.state.clone();
        let window = self.window;
        cx.spawn(async move |_this, cx| {
            if let Some(result) = run_read_deduped(state.clone(), spec, cx).await {
                cx.update(|cx| {
                    state.update(cx, |s, cx| {
                        s.apply(result);
                        cx.notify();
                    })
                });
                let _ = window.update(cx, |_, window, _| window.refresh());
            }
        })
        .detach();
    }

    /// docs/0052 D4: the refocus floor on the totals lane — a card
    /// younger than `FOCUS_MIN_AGE` skips (the view-shown edges above
    /// fire unconditionally; activation must not become a cadence).
    fn fire_card_totals_if_stale(&mut self, cx: &mut Context<Self>) {
        let fresh = self
            .state
            .read(cx)
            .card_totals
            .as_ref()
            .is_some_and(|t| t.landed.elapsed() < crate::service::FOCUS_MIN_AGE);
        if fresh {
            return;
        }
        self.fire_card_totals(cx);
    }

    /// docs/0052 D5: the bell's lane on the card's triggers — the same
    /// state-side gate the module's view-shown fire uses (pause, backoff,
    /// floor), then the same dispatch shape as the totals lane.
    fn fire_notifications_if_stale(&mut self, cx: &mut Context<Self>) {
        let Some(spec) = self
            .state
            .read(cx)
            .notifications_spec_if_stale(crate::service::FOCUS_MIN_AGE)
        else {
            return;
        };
        let state = self.state.clone();
        let window = self.window;
        cx.spawn(async move |_this, cx| {
            if let Some(result) = run_read_deduped(state.clone(), spec, cx).await {
                cx.update(|cx| {
                    state.update(cx, |s, cx| {
                        s.apply(result);
                        cx.notify();
                    })
                });
                let _ = window.update(cx, |_, window, _| window.refresh());
            }
        })
        .detach();
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
                let (totals, notifications) = state.card_counts();
                let aria = match totals {
                    Some((prs, issues)) => format!(
                        "Open GitHub module — {notifications} notifications, {prs} open pull requests, {issues} open issues"
                    ),
                    // docs/0051 D5: no totals lane yet — the card is
                    // loading, and the label says so instead of lying 0s.
                    None => format!(
                        "Open GitHub module — {notifications} notifications, loading counts"
                    ),
                };
                div()
                    .id("github-card")
                    .size_full()
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().accent.opacity(0.06)))
                    .role(accesskit::Role::ListItem)
                    .aria_label(aria)
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
                            .when_some(totals, |col, (prs, issues)| {
                                col.child(
                                    div()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("Open PRs {prs}")),
                                )
                                .child(
                                    div()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("Open Issues {issues}")),
                                )
                            })
                            .when(totals.is_none(), |col| {
                                // docs/0051 D5 — skeleton-first: the totals
                                // lane has not landed yet (~2 pts, sub-second
                                // on a warm token).
                                col.child(
                                    v_flex()
                                        .gap_2()
                                        .child(Skeleton::new().w_20().h_4())
                                        .child(Skeleton::new().w_24().h_4()),
                                )
                            })
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
