//! GitHub settings subpage (docs/0006 §4.6): account state + disconnect.
//!
//! Deliberately thin — the connect flow's full UX (device code, keychain
//! errors) lives on the module's own page; settings mirrors the live
//! `AuthStatus` and drives the same `start_connect`/`sign_out_now` service
//! entry points. The provider runs on every settings render, so rows always
//! reflect current state (including `window.refresh()` repaints fired by the
//! service paths).

use gpui::{
    App, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, WeakEntity,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, Sizable as _,
    avatar::Avatar,
    button::{Button, ButtonVariants as _},
    h_flex,
    label::Label,
    setting::{SettingField, SettingGroup, SettingItem, SettingPage},
    v_flex,
};

use crate::card::{GITHUB_MARK, github_mark};
use crate::gql;
use crate::page::GithubPage;
use crate::service::{self, AuthStatus, ConnectPhase, GithubState};

/// May a new sign-in flow start from settings right now? Sign-in is
/// blocked while a flow is already in flight (`RequestingCode` /
/// `AwaitApproval`) and when `Connected`; terminal/retry states
/// (Disconnected, Denied, Expired, Failed, TokenRejected,
/// KeychainError) allow it — the status copy in those states says
/// "sign in again", mirroring the module page's Try-again panels
/// (#172).
fn sign_in_allowed(auth: &AuthStatus) -> bool {
    match auth {
        AuthStatus::Disconnected => true,
        AuthStatus::ConnectFlow(ConnectPhase::Denied)
        | AuthStatus::ConnectFlow(ConnectPhase::Expired)
        | AuthStatus::ConnectFlow(ConnectPhase::Failed(_)) => true,
        AuthStatus::TokenRejected | AuthStatus::KeychainError(_) => true,
        AuthStatus::ConnectFlow(ConnectPhase::RequestingCode)
        | AuthStatus::ConnectFlow(ConnectPhase::AwaitApproval { .. }) => false,
        AuthStatus::Connected => false,
    }
}

/// Build the module's settings page from the live state entity.
pub(crate) fn page(
    state: &Entity<GithubState>,
    page: &WeakEntity<GithubPage>,
    cx: &App,
) -> SettingPage {
    let state = state.clone();
    let page = page.clone();

    let (status_title, status_detail) = {
        let state = state.read(cx);
        status_strings(&state.auth, state.user.as_ref())
    };

    SettingPage::new("GitHub")
        .icon(Icon::empty().path(GITHUB_MARK))
        .description("Account and connection.")
        .group(
            SettingGroup::new()
                .title("Account")
                .item(SettingItem::render({
                    // Re-read at element-build time: the row shows the live
                    // status even if it changed since the page was planned.
                    let state = state.clone();
                    move |_, _, cx| status_row(&state, cx).into_any_element()
                }))
                .description(format!("{status_title} — {status_detail}")),
        )
        .group(SettingGroup::new().title("Session").items(vec![
            SettingItem::new(
                "Sign in",
                SettingField::render({
                    let state = state.clone();
                    move |options, _, cx| {
                        Button::new("github-sign-in")
                            .primary()
                            .label("Sign in…")
                            .with_size(options.size())
                            .disabled(!sign_in_allowed(&state.read(cx).auth))
                            .on_click({
                                let state = state.clone();
                                move |_, window, cx| {
                                    service::start_connect(
                                        state.clone(),
                                        window.window_handle(),
                                        cx,
                                    );
                                }
                            })
                            .into_any_element()
                    }
                }),
            )
            .description("Start the device sign-in flow (token goes to the OS keychain).")
            .keywords(["connect", "device", "oauth", "login"]),
            SettingItem::new(
                "Disconnect",
                SettingField::render({
                    let state = state.clone();
                    let page = page.clone();
                    move |options, _, cx| {
                        disconnect_control(&state, &page, options.size(), cx)
                    }
                }),
            )
            .description("Remove the token from the keychain and clear cached data.")
            .keywords(["sign out", "logout", "revoke", "token"]),
        ]))
}

/// The Disconnect control. With unsent review drafts on an open PR page
/// the first click turns it into a confirm (#841); otherwise it signs out
/// at once.
fn disconnect_control(
    state: &Entity<GithubState>,
    page: &WeakEntity<GithubPage>,
    size: gpui_component::Size,
    cx: &App,
) -> gpui::AnyElement {
    if let Some(count) = page
        .upgrade()
        .and_then(|p| p.read(cx).pending_disconnect_drafts(cx))
    {
        let prompt = crate::page::disconnect_prompt(count);
        return v_flex()
            .id("github-disconnect-confirm")
            .role(gpui::accesskit::Role::Group)
            .aria_label(prompt.clone())
            .gap_2()
            .child(Label::new(prompt).text_color(cx.theme().warning))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("github-disconnect-go")
                            .danger()
                            .label("Discard and disconnect")
                            .with_size(size)
                            .on_click({
                                let page = page.clone();
                                move |_, window, cx| {
                                    if let Some(page) = page.upgrade() {
                                        page.update(cx, |p, cx| p.confirm_disconnect(window, cx));
                                    }
                                }
                            }),
                    )
                    .child(
                        Button::new("github-disconnect-keep")
                            .ghost()
                            .label("Stay connected")
                            .with_size(size)
                            .on_click({
                                let page = page.clone();
                                move |_, window, cx| {
                                    if let Some(page) = page.upgrade() {
                                        page.update(cx, |p, cx| p.cancel_disconnect(window, cx));
                                    }
                                }
                            }),
                    ),
            )
            .into_any_element();
    }
    Button::new("github-disconnect")
        .danger()
        .label("Disconnect")
        .with_size(size)
        .disabled(state.read(cx).auth == AuthStatus::Disconnected)
        .on_click({
            let state = state.clone();
            let page = page.clone();
            move |_, window, cx| match page.upgrade() {
                Some(page) => page.update(cx, |p, cx| p.request_disconnect(window, cx)),
                None => service::sign_out_now(state.clone(), window.window_handle(), cx),
            }
        })
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Status row
// ---------------------------------------------------------------------------

/// One-line human summary of the current auth state (mirrors the module
/// page's connect panel).
fn status_strings(auth: &AuthStatus, user: Option<&gql::User>) -> (String, String) {
    match auth {
        AuthStatus::Connected => match user {
            Some(user) => (format!("@{}", user.login), "Signed in.".into()),
            None => ("Signed in".into(), "Signed in.".into()),
        },
        AuthStatus::Disconnected => (
            "Not connected".into(),
            "Sign in to load your GitHub data.".into(),
        ),
        AuthStatus::ConnectFlow(ConnectPhase::RequestingCode) => {
            ("Connecting…".into(), "Requesting a device code…".into())
        }
        AuthStatus::ConnectFlow(ConnectPhase::AwaitApproval {
            user_code,
            verification_uri,
        }) => (
            "Finish sign-in".into(),
            format!("Enter code {user_code} at {verification_uri}"),
        ),
        AuthStatus::ConnectFlow(ConnectPhase::Denied) => (
            "Sign-in denied".into(),
            "You denied the request — sign in again.".into(),
        ),
        AuthStatus::ConnectFlow(ConnectPhase::Expired) => (
            "Code expired".into(),
            "The device code timed out — sign in again.".into(),
        ),
        AuthStatus::ConnectFlow(ConnectPhase::Failed(err)) => {
            ("Sign-in failed".into(), err.clone())
        }
        AuthStatus::TokenRejected => (
            "Token rejected".into(),
            "The stored token was revoked — sign in again.".into(),
        ),
        AuthStatus::KeychainError(err) => ("Keychain error".into(), err.clone()),
    }
}

/// The status row element for the current state: avatar (or mark) + login +
/// status detail line, all from the live entity.
fn status_row(state: &Entity<GithubState>, cx: &App) -> impl IntoElement {
    let state = state.read(cx);
    let (title, detail) = status_strings(&state.auth, state.user.as_ref());

    h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .child(match state.user.as_ref() {
            Some(user) => Avatar::new()
                .src(user.avatar_url.clone())
                .xsmall()
                .into_any_element(),
            None => github_mark(cx).into_any_element(),
        })
        .child(
            v_flex().min_w_0().child(Label::new(title).text_sm()).child(
                Label::new(detail)
                    .text_sm()
                    .text_color(cx.theme().muted_foreground),
            ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_in_gating_table() {
        // Terminal/retry states allow a fresh flow; active flows and
        // Connected do not (#172).
        assert!(sign_in_allowed(&AuthStatus::Disconnected));
        assert!(!sign_in_allowed(&AuthStatus::Connected));
        assert!(!sign_in_allowed(&AuthStatus::ConnectFlow(
            ConnectPhase::RequestingCode
        )));
        assert!(!sign_in_allowed(&AuthStatus::ConnectFlow(
            ConnectPhase::AwaitApproval {
                user_code: "ABCD-1234".into(),
                verification_uri: "https://github.com/login/device".into(),
            }
        )));
        assert!(sign_in_allowed(&AuthStatus::ConnectFlow(
            ConnectPhase::Denied
        )));
        assert!(sign_in_allowed(&AuthStatus::ConnectFlow(
            ConnectPhase::Expired
        )));
        assert!(sign_in_allowed(&AuthStatus::ConnectFlow(
            ConnectPhase::Failed("boom".into())
        )));
        assert!(sign_in_allowed(&AuthStatus::TokenRejected));
        assert!(sign_in_allowed(&AuthStatus::KeychainError(
            "no secret service".into()
        )));
    }
}
