//! docs/0084 D2: closing the PR page refreshes the list it returns to
//! when that list is older than 60 s, and the module shown again with a
//! PR page open fires the refocus lanes at the 15 s floor.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, TestAppContext};

use super::GithubPage;
use crate::auth::MockTokenStore;
use crate::drafts::DraftStore;
use crate::http::mock::MockHttp;
use crate::http::{Http, SharedToken};
use crate::pr_page::PrDetailPage;
use crate::service::{AuthStatus, GithubCache, GithubState, PrKey, RepoLane};
use dashboard_core::nav::{Navigator, Page};
use dashboard_core::paths::AppPaths;

const REPO: &str = "acme/one";

fn summary() -> crate::gql::PullRequest {
    crate::gql::PullRequest {
        key: format!("{REPO}#7"),
        database_id: None,
        number: 7,
        title: "return probe".into(),
        draft: false,
        state: None,
        author: None,
        updated_at: None,
        url: "https://probe".into(),
        status: None,
        labels: None,
        review_requests: None,
    }
}

type Rig = (
    Arc<MockHttp>,
    gpui::Entity<GithubState>,
    gpui::Entity<Navigator>,
    gpui::Entity<GithubPage>,
);

/// A page with PR #7 open over a repo selection, every request failing so
/// only the count matters. The PR's own detail is stamped young.
fn rig(cx: &mut gpui::VisualTestContext) -> Rig {
    let mock = Arc::new(MockHttp::new(vec![
        Err(crate::http::HttpError("probe".into())),
        Err(crate::http::HttpError("probe".into())),
        Err(crate::http::HttpError("probe".into())),
        Err(crate::http::HttpError("probe".into())),
    ]));
    let (state, nav, page) = cx.update(|window, cx| {
        let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
        let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
        let state = cx.new(|_| {
            GithubState::new(
                mock.clone() as Arc<dyn Http>,
                Arc::new(MockTokenStore::with("t0k".into())),
                shared,
                refresh,
                GithubCache::new(AppPaths::init()),
                DraftStore::at_tmp("page-list-return"),
            )
        });
        state.update(cx, |s, _| {
            s.selected = Some(REPO.into());
            s.auth = AuthStatus::Connected;
            s.orgs.clear();
            s.user = None;
            s.notifications_landed = Some(Instant::now());
        });
        let nav = cx.new(|_| Navigator::default());
        let page = cx.new(|cx| GithubPage::new(state.clone(), nav.clone(), window, cx));
        page.update(cx, |p, cx| {
            state.update(cx, |s, _| {
                s.land_summary(REPO, &summary());
                // A young detail: the opening page dispatches nothing.
                let doc = serde_json::json!({ "data": { "repository": { "pullRequest": {
                    "id": "PR_7", "number": 7, "title": "probe", "url": "u",
                    "state": "OPEN", "isDraft": false, "createdAt": "2026-09-01T00:00:00Z",
                    "headRefName": "feature", "headRefOid": "aaa", "mergeable": "MERGEABLE",
                } } } });
                let detail = crate::gql::pr_detail_from_value(&doc["data"]["repository"])
                    .expect("the probe detail parses");
                s.land_detail(REPO, 7, detail);
            });
            p.pr = Some(
                cx.new(|cx| PrDetailPage::new(state.clone(), REPO.into(), 7, None, window, cx)),
            );
        });
        (state, nav, page)
    });
    cx.run_until_parked();
    (mock, state, nav, page)
}

fn go_to(nav: &gpui::Entity<Navigator>, cx: &mut gpui::VisualTestContext, page: Page) {
    cx.update(|_, cx| {
        nav.update(cx, |n, cx| {
            n.page = page;
            cx.notify();
        })
    });
}

fn age_rows(state: &gpui::Entity<GithubState>, cx: &mut gpui::VisualTestContext, age: Duration) {
    cx.update(|_, cx| {
        state.update(cx, |s, _| {
            s.rows_landed
                .insert((REPO.to_string(), RepoLane::Pulls), Instant::now() - age);
        })
    });
}

#[gpui::test]
fn returning_to_a_list_respects_the_60_s_floor(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(dashboard_core::status::init);
    let cx = cx.add_empty_window();
    let (mock, state, _nav, page) = rig(cx);
    let base = mock.recorded().len();

    age_rows(&state, cx, Duration::from_secs(59));
    cx.update(|_, cx| page.update(cx, |p, cx| p.close_pr(cx)));
    cx.run_until_parked();
    assert_eq!(mock.recorded().len(), base, "a 59 s list stays");

    // Reopen, then close over a list 61 s old.
    cx.update(|window, cx| {
        page.update(cx, |p, cx| {
            p.pr = Some(
                cx.new(|cx| PrDetailPage::new(state.clone(), REPO.into(), 7, None, window, cx)),
            );
        })
    });
    cx.run_until_parked();
    let base = mock.recorded().len();
    age_rows(&state, cx, Duration::from_secs(61));
    cx.update(|_, cx| page.update(cx, |p, cx| p.close_pr(cx)));
    cx.run_until_parked();
    assert_eq!(mock.recorded().len(), base + 1, "a 61 s list refreshes");
}

#[gpui::test]
fn the_module_shown_again_follows_the_15_s_floor(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(dashboard_core::status::init);
    let cx = cx.add_empty_window();
    let (mock, state, nav, _page) = rig(cx);
    let age_detail = |cx: &mut gpui::VisualTestContext, age: u64| {
        cx.update(|_, cx| {
            state.update(cx, |s, _| {
                s.pulls
                    .get_mut(&PrKey::new(REPO, 7))
                    .expect("the document")
                    .detail_at = Some(Instant::now() - Duration::from_secs(age));
            })
        })
    };
    let detail_reads = |mock: &MockHttp| {
        mock.recorded()
            .iter()
            .filter(|r| r.url.contains("graphql"))
            .count()
    };

    // Leave the module, then come back.
    go_to(&nav, cx, Page::Home);
    cx.run_until_parked();
    let before = detail_reads(&mock);
    age_detail(cx, 14);
    go_to(&nav, cx, Page::Module("github".into()));
    cx.run_until_parked();
    assert_eq!(detail_reads(&mock), before, "14 s: quiet");

    go_to(&nav, cx, Page::Home);
    cx.run_until_parked();
    age_detail(cx, 16);
    go_to(&nav, cx, Page::Module("github".into()));
    cx.run_until_parked();
    assert_eq!(detail_reads(&mock), before + 1, "16 s: one refetch");
}
