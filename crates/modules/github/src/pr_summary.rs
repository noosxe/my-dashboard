//! The PR document's summary layer: how a list landing, a detail landing
//! and a notification-seeded open each update it (docs/0072 D3, docs/0073,
//! #873, #874). Pure functions over the row; the store calls them from
//! `land_summary`, `land_detail` and `seed_summary`.

use crate::gql::{
    self, LabelRef, LabelSummary, PullRequest, RequestedReviewer, ReviewRequestRef,
    ReviewRequestsSummary,
};

/// Whether `incoming` is no newer than `existing` by `updatedAt`.
/// ISO-8601 UTC strings of one shape order lexically. A missing stamp on
/// either side is unknown, never "older".
fn not_newer(existing: &PullRequest, incoming: &PullRequest) -> bool {
    match (&existing.updated_at, &incoming.updated_at) {
        (Some(have), Some(got)) => got <= have,
        _ => false,
    }
}

/// What a list landing leaves on the summary: `incoming`, except that a
/// row no newer than the one already held (a list read dispatched before
/// a merge confirmation or a newer detail landed) keeps the held
/// descriptive fields. A state change always moves `updatedAt`, so an
/// equal stamp with a different state is the older read. The checks
/// rollup and URL still follow `incoming`: CI moves without touching
/// `updatedAt`.
pub(crate) fn merge_landing(existing: &PullRequest, incoming: &PullRequest) -> PullRequest {
    if !not_newer(existing, incoming) {
        return incoming.clone();
    }
    let mut kept = existing.clone();
    kept.status = incoming.status.clone();
    if !incoming.url.is_empty() {
        kept.url = incoming.url.clone();
    }
    // Facts the held row never knew are not "newer" — fill them.
    kept.state = kept.state.or_else(|| incoming.state.clone());
    kept.author = kept.author.or_else(|| incoming.author.clone());
    kept.labels = kept.labels.or_else(|| incoming.labels.clone());
    kept.review_requests = kept
        .review_requests
        .or_else(|| incoming.review_requests.clone());
    kept
}

/// Copy every field the detail also carries into the summary (#873), so
/// the lists agree with the page. The rollup is handled by the caller:
/// it has its own drift rules (docs/0072 D3).
pub(crate) fn refresh_from_detail(summary: &mut PullRequest, detail: &gql::PrDetail) {
    summary.title.clone_from(&detail.title);
    summary.draft = detail.draft;
    if detail.state.is_some() {
        summary.state.clone_from(&detail.state);
    }
    if detail.updated_at.is_some() {
        summary.updated_at.clone_from(&detail.updated_at);
    }
    summary.labels = Some(LabelSummary {
        total_count: detail.labels_total,
        nodes: detail
            .labels
            .iter()
            .map(|l| LabelRef {
                name: l.name.clone(),
                color: l.color.clone(),
            })
            .collect(),
    });
    summary.review_requests = Some(ReviewRequestsSummary {
        total_count: detail.review_requests_total,
        nodes: detail
            .review_requests
            .iter()
            .map(|r| ReviewRequestRef {
                requested_reviewer: Some(if r.is_user() {
                    RequestedReviewer::User {
                        login: r.login.clone(),
                    }
                } else {
                    RequestedReviewer::Other
                }),
            })
            .collect(),
    });
}

/// A synthesized row (a notification subject: no state, URL, rollup or
/// labels) over a document that already landed: it only seeds what the
/// held summary is missing and never overwrites a landed field (#874).
pub(crate) fn seed_missing(existing: &mut PullRequest, seed: &PullRequest) {
    if existing.title.is_empty() {
        existing.title.clone_from(&seed.title);
    }
    if existing.url.is_empty() {
        existing.url.clone_from(&seed.url);
    }
    macro_rules! fill {
        ($($f:ident),*) => {$(
            if existing.$f.is_none() {
                existing.$f.clone_from(&seed.$f);
            }
        )*};
    }
    fill!(state, author, updated_at, status, labels, review_requests);
}

#[cfg(test)]
mod tests;
