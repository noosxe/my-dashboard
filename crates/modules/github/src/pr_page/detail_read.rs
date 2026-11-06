//! The store half of a detail read (docs/0072 D2/D3, #743).
//!
//! `begin_detail_read` arms the entry's flight record when the page
//! dispatches; the result must clear it whether or not that page is
//! still alive. A page dropped mid-flight (Esc) used to take the result
//! with it and leave `in_flight` set for good, so a reopen skipped its
//! dispatch and rendered a skeleton until a manual refresh.

use gpui::{AsyncApp, Context, Entity};

use crate::gql;
use crate::service::{GithubState, SpecResult};

/// Land or fail a detail read on the document. Returns the detail that
/// landed so a live page can fold it into its own caches.
pub(super) fn land_detail_result(
    s: &mut GithubState,
    cx: &mut Context<GithubState>,
    repo: &str,
    number: i64,
    result: SpecResult,
) -> Option<gql::PrDetail> {
    if let Some(m) = &result.network {
        s.fail_detail_read(repo, number, format!("Network error: {m}"));
        return None;
    }
    if result.status != 200 {
        // Forward failures through `apply` so the shared 401 (reactive
        // refresh / TokenRejected) and 403 (rate-limit pause) handling
        // runs exactly as for every other fetch (docs/0016 D2).
        let status = result.status;
        s.apply(result);
        cx.notify();
        s.fail_detail_read(repo, number, format!("GitHub answered HTTP {status}"));
        return None;
    }
    let Some(body) = result.body else {
        s.fail_detail_read(repo, number, "Empty response".into());
        return None;
    };
    match serde_json::from_str::<gql::Envelope<gql::PrDetailData>>(body.as_str()) {
        Ok(env) => {
            for e in &env.errors {
                log::warn!("pr-detail: {}", e.message);
            }
            match env
                .data
                .and_then(|d| d.repository)
                .as_ref()
                .and_then(gql::pr_detail_from_value)
            {
                Some(detail) => s
                    .land_detail(repo, number, detail.clone())
                    .then_some(detail),
                None => {
                    log::warn!("pr-detail: repository/pullRequest null — keeping header");
                    s.fail_detail_read(
                        repo,
                        number,
                        "PR not found (removed, or access lost)".into(),
                    );
                    None
                }
            }
        }
        Err(e) => {
            log::warn!("pr-detail payload: {e}");
            s.fail_detail_read(repo, number, "Malformed payload".into());
            None
        }
    }
}

/// The page is gone: land the result on the store alone.
pub(super) fn land_orphaned(
    state: &Entity<GithubState>,
    repo: &str,
    number: i64,
    result: SpecResult,
    cx: &mut AsyncApp,
) {
    log::debug!("pr-detail: page gone — landing {repo}#{number} on the store");
    cx.update(|cx| {
        state.update(cx, |s, cx| {
            land_detail_result(s, cx, repo, number, result);
            cx.notify();
        })
    });
}

#[cfg(test)]
mod tests;
