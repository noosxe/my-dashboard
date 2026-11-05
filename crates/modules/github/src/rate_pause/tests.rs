//! Both lanes of the per-budget gate (docs/0082 Q10): REST paused while
//! GraphQL keeps fetching, the reverse, both paused, and each pause
//! resuming at its own reset.

use std::sync::Arc;

use super::*;
use crate::auth::MockTokenStore;
use crate::drafts::DraftStore;
use crate::http::mock::MockHttp;
use crate::http::{Http, SharedToken};
use crate::service::{AuthStatus, FOCUS_MIN_AGE, GithubCache, GithubState};
use crate::usage::{BudgetSnapshot, Origin, budget_now};

/// A connected state whose usage book is fresh, so the free probe stays
/// quiet unless a test wants it.
fn state() -> GithubState {
    let shared: SharedToken = Arc::new(std::sync::RwLock::new(Some("t0k".into())));
    let client: Arc<dyn Http> = Arc::new(MockHttp::new(vec![]));
    let mut state = GithubState::new(
        client,
        Arc::new(MockTokenStore::with("t0k".into())),
        shared,
        Arc::new(std::sync::RwLock::new(None)),
        GithubCache::at_tmp("rate-pause"),
        DraftStore::at_tmp("rate-pause"),
    );
    state.auth = AuthStatus::Connected;
    let now = budget_now();
    let full = BudgetSnapshot {
        limit: 5000,
        remaining: 5000,
        reset_at: now + 3600,
    };
    state.usage.lock().unwrap().seed_snapshots(full, full, now);
    state
}

fn kinds(state: &mut GithubState) -> Vec<Kind> {
    state.plan_tick(1).iter().map(|s| s.kind).collect()
}

fn notifications(state: &GithubState) -> bool {
    state
        .notifications_spec_if_stale(FOCUS_MIN_AGE, Origin::BACKGROUND)
        .is_some()
}

#[test]
fn each_lane_pauses_and_resumes_on_its_own() {
    let mut p = RateLimitPause::default();
    p.arm(Lane::Graphql, 1_000);
    assert!(p.paused(Lane::Graphql, 999));
    assert!(!p.paused(Lane::Rest, 999), "REST keeps fetching");
    p.arm(Lane::Rest, 2_000);
    assert_eq!(p.expire(1_000), vec![Lane::Graphql]);
    assert!(!p.paused(Lane::Graphql, 1_000));
    assert!(p.paused(Lane::Rest, 1_000), "REST waits for its own reset");
    assert_eq!(p.expire(2_000), vec![Lane::Rest]);
    assert_eq!(p, RateLimitPause::default());
}

#[test]
fn kinds_split_by_transport() {
    assert_eq!(Kind::CardTotals.lane(), Lane::Graphql);
    assert_eq!(Kind::SessionInventory.lane(), Lane::Graphql);
    assert_eq!(Kind::AccountPullRequests.lane(), Lane::Graphql);
    assert_eq!(Kind::Notifications.lane(), Lane::Rest);
    assert_eq!(Kind::PrFiles.lane(), Lane::Rest);
    assert_eq!(Kind::RateLimit.lane(), Lane::Rest);
}

#[test]
fn rest_paused_graphql_keeps_fetching() {
    let mut s = state();
    s.rate_limited_until.arm(Lane::Rest, budget_now() + 600);
    assert!(s.gated_for(Lane::Rest));
    assert!(!s.gated_for(Lane::Graphql));
    assert!(!s.fully_gated());
    assert!(!notifications(&s), "the REST lane waits");
    s.needs_bootstrap = true;
    assert_eq!(
        kinds(&mut s),
        vec![Kind::SessionInventory],
        "the GraphQL bootstrap flies; its REST notifications wait"
    );
    assert!(!s.needs_bootstrap);
}

#[test]
fn graphql_paused_rest_keeps_fetching() {
    let mut s = state();
    s.rate_limited_until.arm(Lane::Graphql, budget_now() + 600);
    assert!(s.gated_for(Lane::Graphql));
    assert!(!s.gated_for(Lane::Rest));
    assert!(notifications(&s), "the REST lane stays live");
    s.needs_bootstrap = true;
    assert!(
        kinds(&mut s).is_empty(),
        "the GraphQL bootstrap waits for its window"
    );
    assert!(s.needs_bootstrap, "the flag survives the pause");
}

#[test]
fn both_paused_nothing_flies() {
    let mut s = state();
    let until = budget_now() + 600;
    s.rate_limited_until = RateLimitPause::both(until);
    assert!(s.fully_gated());
    assert!(!notifications(&s));
    s.needs_bootstrap = true;
    assert!(kinds(&mut s).is_empty());
}

#[test]
fn each_pause_resumes_at_its_own_reset() {
    let mut s = state();
    let now = budget_now();
    // GraphQL's window already passed; REST's is still open.
    s.rate_limited_until.arm(Lane::Graphql, now);
    s.rate_limited_until.arm(Lane::Rest, now + 600);
    s.needs_bootstrap = true;
    assert_eq!(kinds(&mut s), vec![Kind::SessionInventory]);
    assert_eq!(s.rate_limited_until.graphql, None, "GraphQL resumed");
    assert_eq!(
        s.rate_limited_until.rest,
        Some(now + 600),
        "REST still paused"
    );
    assert!(!notifications(&s));

    // REST's window passes too: it clears on the next tick.
    s.rate_limited_until.arm(Lane::Rest, now);
    s.plan_tick(2);
    assert_eq!(s.rate_limited_until, RateLimitPause::default());
    assert!(notifications(&s));
}

#[test]
fn a_rest_exhaustion_pauses_rest_only() {
    use crate::http::RateLimit;
    use crate::service::SpecResult;
    let mut s = state();
    s.apply(SpecResult {
        kind: Kind::Notifications,
        ident: None,
        status: 403,
        etag: None,
        rate: Some(RateLimit {
            limit: 5000,
            remaining: 0,
            reset_epoch: 9_999_999_999,
        }),
        body: None,
        network: None,
        cursor: None,
    });
    assert_eq!(s.rate_limited_until.rest, Some(9_999_999_999));
    assert_eq!(s.rate_limited_until.graphql, None);
    assert!(!s.gated_for(Lane::Graphql));
}

#[test]
fn a_graphql_exhaustion_pauses_graphql_only() {
    use crate::http::RateLimit;
    use crate::service::SpecResult;
    let mut s = state();
    s.apply(SpecResult {
        kind: Kind::CardTotals,
        ident: None,
        status: 200,
        etag: None,
        rate: Some(RateLimit {
            limit: 5000,
            remaining: 0,
            reset_epoch: 9_999_999_999,
        }),
        body: None,
        network: None,
        cursor: None,
    });
    assert_eq!(s.rate_limited_until.graphql, Some(9_999_999_999));
    assert_eq!(s.rate_limited_until.rest, None);
    assert!(notifications(&s));
}
