//! Page-level Checks tab coverage (#747): a real page over a transport
//! that answers REST by URL and records what it was asked.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, px};

use super::identity::CheckTarget;
use crate::drafts::DraftStore;
use crate::gql;
use crate::http::{Http, HttpError, HttpResult, Verb};
use crate::pr_page::{CheckLogState, PrDetailPage, PrTab};
use crate::service::{AuthStatus, GithubCache, GithubState, Kind, SpecResult};

const REPO: &str = "o/r";
const NUMBER: i64 = 7;

/// REST by longest matching URL fragment; GraphQL stays down (the probes
/// land details by hand), and an unrouted URL is a loud 404.
#[derive(Default)]
struct RoutedHttp {
    routes: Mutex<BTreeMap<String, String>>,
    asked: Mutex<Vec<String>>,
}

impl Http for RoutedHttp {
    fn request(
        &self,
        _: Verb,
        url: &str,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<HttpResult, HttpError> {
        self.asked.lock().unwrap().push(url.to_string());
        let routes = self.routes.lock().unwrap();
        let body = routes
            .iter()
            .filter(|(fragment, _)| url.contains(fragment.as_str()))
            .max_by_key(|(fragment, _)| fragment.len())
            .map(|(_, body)| body.clone());
        Ok(HttpResult {
            status: if body.is_some() { 200 } else { 404 },
            etag: None,
            rate: None,
            body,
        })
    }

    fn post_graphql(&self, _: &str, _: &serde_json::Value) -> Result<HttpResult, HttpError> {
        Err(HttpError("probes land details by hand".into()))
    }
}

struct NoStore;

impl crate::auth::TokenStore for NoStore {
    fn save(&self, _: &crate::auth::StoredCredential) -> Result<(), String> {
        Ok(())
    }
    fn load(&self) -> Result<Option<crate::auth::StoredCredential>, String> {
        Ok(None)
    }
    fn delete(&self) -> Result<(), String> {
        Ok(())
    }
}

/// One Actions row on the wire.
fn wire_check(name: &str, id: u64, suite: u64) -> serde_json::Value {
    serde_json::json!({
        "__typename": "CheckRun", "databaseId": id, "name": name,
        "status": "COMPLETED", "conclusion": "FAILURE",
        "checkSuite": {"databaseId": suite, "app": {"slug": "github-actions"}},
    })
}

fn detail_body(head: &str, rollup: &str, total: usize, nodes: Vec<serde_json::Value>) -> String {
    serde_json::json!({"data": {"repository": {"pullRequest": {
        "id": "PR_7", "number": NUMBER, "title": "t",
        "url": "https://github.com/o/r/pull/7",
        "state": "OPEN", "isDraft": false, "headRefOid": head,
        "statusCheckRollup": {"state": rollup, "contexts": {"totalCount": total, "nodes": nodes}},
    }}}})
    .to_string()
}

fn row_summary() -> gql::PullRequest {
    gql::PullRequest {
        key: format!("{REPO}#{NUMBER}"),
        database_id: None,
        number: NUMBER,
        title: "t".into(),
        draft: false,
        state: None,
        author: None,
        updated_at: None,
        url: "u".into(),
        status: None,
        labels: None,
        review_requests: None,
    }
}

struct Rig<'a> {
    http: Arc<RoutedHttp>,
    page: Entity<PrDetailPage>,
    vcx: &'a mut VisualTestContext,
}

impl<'a> Rig<'a> {
    fn open(cx: &'a mut TestAppContext) -> Self {
        cx.update(dashboard_core::status::init);
        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5).
        cx.update(|cx| cx.background_executor().allow_parking());
        let http = Arc::new(RoutedHttp::default());
        let client: Arc<dyn Http> = http.clone();
        let state = cx.update(|cx| {
            cx.new(|_| {
                let mut s = GithubState::new(
                    client,
                    Arc::new(NoStore),
                    Default::default(),
                    Default::default(),
                    GithubCache::at_tmp("pr-checks-747"),
                    DraftStore::at_tmp("pr-checks-747"),
                );
                s.auth = AuthStatus::Connected;
                s.land_summary(REPO, &row_summary());
                s
            })
        });
        let (page, vcx) = cx.add_window_view(|window, cx| {
            PrDetailPage::new(state, REPO.into(), NUMBER, None, window, cx)
        });
        vcx.simulate_resize(gpui::Size {
            width: px(1024.),
            height: px(768.),
        });
        vcx.update(|_, cx| page.update(cx, |p, _| p.tab = PrTab::Checks));
        vcx.run_until_parked();
        Self { http, page, vcx }
    }

    fn route(&self, fragment: &str, body: impl ToString) {
        self.http
            .routes
            .lock()
            .unwrap()
            .insert(fragment.to_string(), body.to_string());
    }

    /// Land a detail the way the real read does, and paint it.
    fn land(&mut self, body: String) {
        let page = self.page.clone();
        self.vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.apply_detail_result(
                    SpecResult {
                        kind: Kind::PrDetail,
                        ident: Some(REPO.into()),
                        status: 200,
                        etag: None,
                        rate: None,
                        body: Some(body),
                        network: None,
                        cursor: None,
                    },
                    cx,
                )
            })
        });
        self.settle();
    }

    fn settle(&mut self) {
        self.vcx.run_until_parked();
        self.vcx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn toggle_only(&mut self, target: &CheckTarget) {
        let page = self.page.clone();
        self.vcx
            .update(|_, cx| page.update(cx, |p, cx| p.toggle_check_log(target, cx)));
    }

    fn toggle(&mut self, target: &CheckTarget) {
        self.toggle_only(target);
        self.settle();
    }

    fn log_text(&mut self, target: &CheckTarget) -> Option<String> {
        self.vcx
            .read(|cx| match self.page.read(cx).check_logs.get(&target.key) {
                Some(CheckLogState::Ready { text, .. }) => Some(text.to_string()),
                _ => None,
            })
    }

    fn asked(&self, fragment: &str) -> usize {
        let asked = self.http.asked.lock().unwrap();
        asked.iter().filter(|u| u.contains(fragment)).count()
    }

    fn pane_is_open(&mut self, row: usize) -> bool {
        self.vcx
            .debug_bounds(Box::leak(format!("check-log-pane-{row}").into_boxed_str()))
            .is_some()
    }
}

/// Two workflows, each with a `build` job: suites 10 and 20, runs 100 and
/// 200, jobs 1 and 2.
fn two_builds(rig: &Rig<'_>) -> (CheckTarget, CheckTarget) {
    rig.route(
        "/commits/head1/check-runs?check_name=build",
        serde_json::json!({"check_runs": [
            {"id": 1, "name": "build", "check_suite": {"id": 10}, "app": {"slug": "github-actions"}},
            {"id": 2, "name": "build", "check_suite": {"id": 20}, "app": {"slug": "github-actions"}},
        ]}),
    );
    for (suite, run, job) in [(10, 100, 1), (20, 200, 2)] {
        rig.route(
            &format!("/actions/runs?check_suite_id={suite}"),
            serde_json::json!({"workflow_runs": [{"id": run}]}),
        );
        rig.route(
            &format!("/actions/runs/{run}/jobs"),
            serde_json::json!({"total_count": 1, "jobs": [{
                "id": job, "name": "build", "status": "completed",
                "check_run_url": format!("https://api.github.com/repos/o/r/check-runs/{job}"),
            }]}),
        );
        rig.route(
            &format!("/actions/jobs/{job}/logs"),
            format!("log of job {job}"),
        );
    }
    (
        CheckTarget::new(Some("head1"), "build", Some(1), Some(10)),
        CheckTarget::new(Some("head1"), "build", Some(2), Some(20)),
    )
}

fn two_builds_detail() -> String {
    detail_body(
        "head1",
        "FAILURE",
        2,
        vec![wire_check("build", 1, 10), wire_check("build", 2, 20)],
    )
}

#[gpui::test]
fn same_named_checks_expand_and_fetch_independently(cx: &mut TestAppContext) {
    let mut rig = Rig::open(cx);
    let (a, b) = two_builds(&rig);
    rig.land(two_builds_detail());

    rig.toggle(&a);
    assert!(rig.pane_is_open(0), "the clicked row opens");
    assert!(!rig.pane_is_open(1), "its namesake stays shut");
    assert_eq!(rig.log_text(&a).as_deref(), Some("log of job 1"));
    assert_eq!(rig.log_text(&b), None);
    assert_eq!(rig.asked("/actions/jobs/2/logs"), 0);

    rig.toggle(&b);
    assert!(!rig.pane_is_open(0), "the accordion moved");
    assert!(rig.pane_is_open(1));
    assert_eq!(rig.log_text(&b).as_deref(), Some("log of job 2"));
    assert_eq!(rig.asked("/actions/jobs/1/logs"), 1);
    assert_eq!(rig.asked("/actions/jobs/2/logs"), 1);
}

#[gpui::test]
fn a_namesakes_chain_in_flight_does_not_absorb_this_rows(cx: &mut TestAppContext) {
    let mut rig = Rig::open(cx);
    let (a, b) = two_builds(&rig);
    rig.land(two_builds_detail());

    // No parking between the clicks: the first chain is still armed.
    rig.toggle_only(&a);
    rig.toggle_only(&b);
    rig.settle();

    assert_eq!(rig.log_text(&b).as_deref(), Some("log of job 2"));
    assert!(rig.pane_is_open(1));
}

#[gpui::test]
fn same_named_jobs_in_one_run_resolve_by_their_check_run_link(cx: &mut TestAppContext) {
    let mut rig = Rig::open(cx);
    rig.route(
        "/check-runs?check_name=build",
        serde_json::json!({"check_runs": [
            {"id": 1, "name": "build", "check_suite": {"id": 10}, "app": {"slug": "github-actions"}},
            {"id": 2, "name": "build", "check_suite": {"id": 10}, "app": {"slug": "github-actions"}},
        ]}),
    );
    rig.route(
        "/actions/runs?check_suite_id=10",
        serde_json::json!({"workflow_runs": [{"id": 100}]}),
    );
    rig.route(
        "/actions/runs/100/jobs",
        serde_json::json!({"total_count": 2, "jobs": [
            {"id": 1, "name": "build", "status": "completed",
             "check_run_url": "https://api.github.com/repos/o/r/check-runs/1"},
            {"id": 2, "name": "build", "status": "completed",
             "check_run_url": "https://api.github.com/repos/o/r/check-runs/2"},
        ]}),
    );
    rig.route("/actions/jobs/2/logs", "the second");
    rig.land(detail_body(
        "head1",
        "FAILURE",
        2,
        vec![wire_check("build", 1, 10), wire_check("build", 2, 10)],
    ));

    rig.toggle(&CheckTarget::new(Some("head1"), "build", Some(2), Some(10)));

    assert_eq!(rig.asked("/actions/jobs/1/logs"), 0);
    assert_eq!(rig.asked("/actions/jobs/2/logs"), 1);
}

#[gpui::test]
fn the_cap_row_shows_only_over_the_cap(cx: &mut TestAppContext) {
    let mut rig = Rig::open(cx);
    let hundred = |total| {
        detail_body(
            "head1",
            "FAILURE",
            total,
            (0..100).map(|i| wire_check("ci", i, 10)).collect(),
        )
    };

    rig.land(hundred(137));
    assert!(rig.vcx.debug_bounds("pr-checks-truncated").is_some());

    rig.land(hundred(100));
    assert!(rig.vcx.debug_bounds("pr-checks-truncated").is_none());
}
