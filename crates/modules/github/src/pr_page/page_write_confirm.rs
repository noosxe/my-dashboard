//! Page-modal writes confirm on the store (docs/0072 D4, #807).
//!
//! Labels, assignees, reviewers, milestone and reactions are quick
//! writes with no pending state of their own: they arm `busy` on the
//! page and used to resolve only through it. A page dropped mid-write
//! took the confirmation with it, nothing voided `detail_at`, and a
//! reopen inside the TTL served the pre-write document as fresh.
//!
//! The spawn now hands the result to the state entity before it asks the
//! page to do anything, so the void and the outdated-read mark (#739)
//! hold whether or not the page is alive. Failures change nothing, as
//! with `resolve_pending`; the page still owns the inline error, the
//! 401/403 forwarding and its own refetch.

use crate::gql;
use crate::service::{GithubState, PrKey, SpecResult};

/// A clean 200: no transport failure, no HTTP error, no GraphQL `errors`.
/// The same classification the page and the store resolver apply.
fn confirmed(result: &SpecResult) -> bool {
    if result.network.is_some() || result.status != 200 {
        return false;
    }
    !result.body.as_deref().is_some_and(|body| {
        serde_json::from_str::<gql::Envelope<serde_json::Value>>(body)
            .is_ok_and(|env| !env.errors.is_empty())
    })
}

impl GithubState {
    /// A page-modal write that changes the detail came back. On a clean
    /// confirmation the cached detail predates the write: void its
    /// freshness, and mark a read that flies now as outdated, because it
    /// was dispatched before the write confirmed. Returns whether it did.
    pub(crate) fn confirm_page_write(
        &mut self,
        repo: &str,
        number: i64,
        result: &SpecResult,
    ) -> bool {
        if !confirmed(result) {
            return false;
        }
        let Some(entry) = self.pulls.get_mut(&PrKey::new(repo, number)) else {
            return false;
        };
        entry.books.read_outdated |= entry.books.in_flight;
        entry.detail_at = None;
        self.persist_pull(repo, number);
        true
    }
}

#[cfg(test)]
mod tests;
