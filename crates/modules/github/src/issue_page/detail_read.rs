//! The store half of the issue detail read (docs/0072 D2/D3, #806).
//!
//! `begin_issue_detail_read` arms the entry's flight record when the
//! page dispatches; the result must clear it whether or not that page is
//! still alive — the same rule the PR page follows (#743). A page dropped
//! mid-flight (Esc) used to take the result with it and leave `in_flight`
//! set for good, so a reopen skipped its dispatch and rendered a skeleton.

use gpui::{AsyncApp, Context, Entity};

use crate::gql;
use crate::service::{GithubState, SpecResult};

/// Land or fail an issue detail read on the document. Returns the detail
/// that landed so a live page can fold it into its own caches.
pub(super) fn land_detail_result(
    s: &mut GithubState,
    cx: &mut Context<GithubState>,
    repo: &str,
    number: i64,
    result: SpecResult,
) -> Option<gql::IssueDetail> {
    if let Some(m) = &result.network {
        s.fail_issue_detail_read(repo, number, format!("Network error: {m}"));
        return None;
    }
    if result.status != 200 {
        // Forward failures through `apply` so the shared 401/403
        // handling runs exactly as for every other fetch (0020 D2).
        let status = result.status;
        s.apply(result);
        cx.notify();
        s.fail_issue_detail_read(repo, number, format!("GitHub answered HTTP {status}"));
        return None;
    }
    let Some(body) = result.body else {
        s.fail_issue_detail_read(repo, number, "Empty response".into());
        return None;
    };
    match serde_json::from_str::<gql::Envelope<gql::IssueDetailData>>(body.as_str()) {
        Ok(env) => {
            for e in &env.errors {
                log::warn!("issue-detail: {}", e.message);
            }
            match env
                .data
                .and_then(|d| d.repository)
                .as_ref()
                .and_then(gql::issue_detail_from_value)
            {
                Some(detail) => s
                    .land_issue_detail(repo, number, detail.clone())
                    .then_some(detail),
                None => {
                    log::warn!("issue-detail: repository/issue null — keeping header");
                    s.fail_issue_detail_read(
                        repo,
                        number,
                        "Issue not found (removed, or access lost)".into(),
                    );
                    None
                }
            }
        }
        Err(e) => {
            log::warn!("issue-detail payload: {e}");
            s.fail_issue_detail_read(repo, number, "Malformed payload".into());
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
    log::debug!("issue-detail: page gone — landing {repo}#{number} on the store");
    cx.update(|cx| {
        state.update(cx, |s, cx| {
            land_detail_result(s, cx, repo, number, result);
            cx.notify();
        })
    });
}

#[cfg(test)]
mod tests;
