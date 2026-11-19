//! Sign-out and the unsent PR review drafts (#841, docs/0043 D7).
//!
//! Signing out clears the PR page, and its drafts with it (they are
//! session-scoped, docs/0024 D7). Two cases:
//! - Disconnect from Settings still has a token to act on, so it asks
//!   first when drafts exist (`request_disconnect`).
//! - Losing auth (a revoked or expired token) cannot ask: the page
//!   closes and a toast says how many drafts went
//!   (`report_dropped_review_drafts`).

use gpui::{App, Context};

use dashboard_core::toasts::{self, Level, Toast};

use super::GithubPage;
use crate::service;

/// The confirm line under Settings' Disconnect.
pub(crate) fn disconnect_prompt(count: usize) -> String {
    let noun = if count == 1 { "draft" } else { "drafts" };
    format!("Disconnecting discards {count} unsent review {noun}.")
}

/// The toast for drafts dropped by an auth loss.
pub(crate) fn discarded_copy(count: usize) -> String {
    let noun = if count == 1 { "draft" } else { "drafts" };
    format!("{count} unsent review {noun} discarded")
}

impl GithubPage {
    /// Unsent review drafts on the open PR page.
    fn open_review_drafts(&self, cx: &App) -> usize {
        self.pr
            .as_ref()
            .map_or(0, |pr| pr.read(cx).review_draft_count(cx))
    }

    /// The draft count while Settings' confirm is showing; `None` once
    /// there is nothing to lose (or no confirm was asked for).
    pub(crate) fn pending_disconnect_drafts(&self, cx: &App) -> Option<usize> {
        if !self.disconnect_confirm {
            return None;
        }
        Some(self.open_review_drafts(cx)).filter(|n| *n > 0)
    }

    /// Settings' Disconnect: sign out now, or ask when drafts would go.
    pub(crate) fn request_disconnect(&mut self, cx: &mut Context<Self>) {
        if self.open_review_drafts(cx) == 0 {
            service::sign_out_now(self.state.clone(), cx);
            return;
        }
        self.disconnect_confirm = true;
        cx.notify();
    }

    /// "Discard and disconnect": the drafts go on purpose, so the page
    /// closes first and the auth-loss toast stays quiet.
    pub(crate) fn confirm_disconnect(&mut self, cx: &mut Context<Self>) {
        self.disconnect_confirm = false;
        self.pr = None;
        service::sign_out_now(self.state.clone(), cx);
    }

    /// "Stay connected".
    pub(crate) fn cancel_disconnect(&mut self, cx: &mut Context<Self>) {
        self.disconnect_confirm = false;
        cx.notify();
    }

    /// Auth was lost under an open PR page: say what the close dropped.
    /// Silent when nothing was queued.
    pub(super) fn report_dropped_review_drafts(&mut self, cx: &mut Context<Self>) {
        let count = self.open_review_drafts(cx);
        self.disconnect_confirm = false;
        if count > 0 {
            toasts::raise(
                cx,
                Toast::new(Level::Warning, discarded_copy(count))
                    .source("github")
                    .dedupe_key("review-drafts-discarded"),
            );
        }
    }
}

#[cfg(test)]
mod tests;
