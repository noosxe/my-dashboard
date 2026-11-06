//! #743: a page dropped mid-flight must not strand the document's flight
//! record. Every test drives a real page against a transport whose
//! `PrDetail` read is armed but unanswered until the test runs the
//! executors: the test dispatcher is single-threaded, so "mid-flight" is
//! the stretch before the first `run_until_parked`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::{AppContext as _, Entity, TestAppContext};

use crate::drafts::DraftStore;
use crate::fixture::operation_name;
use crate::http::{Http, HttpError, HttpResult, Verb};
use crate::pr_page::PrDetailPage;
use crate::service::{AuthStatus, GithubCache, GithubState};

const REPO: &str = "o/r";
const NUMBER: i64 = 7;

/// Answers `PrDetail` reads; counts them.
struct GatedHttp {
    reads: AtomicUsize,
}

impl Http for GatedHttp {
    fn request(
        &self,
        _: Verb,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<HttpResult, HttpError> {
        Err(HttpError("not the lane under test".into()))
    }

    fn post_graphql(&self, query: &str, _: &serde_json::Value) -> Result<HttpResult, HttpError> {
        if operation_name(query) != Some("PrDetail") {
            return Err(HttpError("not the lane under test".into()));
        }
        self.reads.fetch_add(1, Ordering::SeqCst);
        let body = serde_json::json!({ "data": { "repository": { "pullRequest": {
            "id": "PR_7", "number": NUMBER, "title": "fresh title", "url": "u",
            "state": "OPEN", "isDraft": false, "createdAt": "2026-09-01T00:00:00Z",
        } } } });
        Ok(HttpResult {
            status: 200,
            etag: None,
            rate: None,
            body: Some(body.to_string()),
        })
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

struct Rig {
    http: Arc<GatedHttp>,
    state: Entity<GithubState>,
}

impl Rig {
    /// A connected store holding only a summary row: a cold document.
    fn new(cx: &mut TestAppContext) -> Self {
        cx.update(dashboard_core::status::init);
        cx.update(gpui_component::init);
        cx.update(|cx| cx.background_executor().allow_parking());
        let http = Arc::new(GatedHttp {
            reads: AtomicUsize::new(0),
        });
        let client: Arc<dyn Http> = http.clone();
        let state = cx.update(|cx| {
            cx.new(|_| {
                let mut s = GithubState::new(
                    client,
                    Arc::new(NoStore),
                    Default::default(),
                    Default::default(),
                    GithubCache::new(dashboard_core::paths::AppPaths::init()),
                    DraftStore::at_tmp("pr-detail-read"),
                );
                s.auth = AuthStatus::Connected;
                s.land_summary(
                    REPO,
                    &crate::gql::PullRequest {
                        key: format!("{REPO}#{NUMBER}"),
                        database_id: None,
                        number: NUMBER,
                        title: "probe".into(),
                        draft: false,
                        state: None,
                        author: None,
                        updated_at: None,
                        url: "https://probe".into(),
                        status: None,
                        labels: None,
                        review_requests: None,
                    },
                );
                s
            })
        });
        Self { http, state }
    }

    /// Open a page without running the executors, so its open read is
    /// armed but unanswered; returns the window so a test can drop it. No test
    /// holds the page entity: a held handle would keep it alive after Esc.
    fn open(&self, cx: &mut TestAppContext) -> gpui::AnyWindowHandle {
        let state = self.state.clone();
        cx.update(|cx| {
            let window = cx
                .open_window(gpui::WindowOptions::default(), |window, cx| {
                    cx.new(|cx| PrDetailPage::new(state, REPO.into(), NUMBER, None, window, cx))
                })
                .expect("the window opens");
            window.into()
        })
    }

    fn close(&self, cx: &mut TestAppContext, handle: gpui::AnyWindowHandle) {
        cx.update(|cx| {
            handle
                .update(cx, |_, window, _| window.remove_window())
                .ok();
        });
    }

    fn in_flight(&self, cx: &mut TestAppContext) -> bool {
        cx.update(|cx| {
            self.state
                .read(cx)
                .pr_entry(REPO, NUMBER)
                .is_some_and(|e| e.books.in_flight)
        })
    }

    fn has_detail(&self, cx: &mut TestAppContext) -> bool {
        cx.update(|cx| {
            self.state
                .read(cx)
                .pr_entry(REPO, NUMBER)
                .is_some_and(|e| e.detail.is_some())
        })
    }

    /// Run the executors: the armed read answers.
    fn release(&self, cx: &mut TestAppContext) {
        cx.run_until_parked();
    }
}

#[gpui::test]
fn page_dropped_mid_flight_still_lands_the_result(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let handle = rig.open(cx);
    assert!(rig.in_flight(cx), "the open read is flying");

    rig.close(cx, handle);
    rig.release(cx);

    assert!(!rig.in_flight(cx), "the flight record cleared");
    assert!(rig.has_detail(cx), "the result landed on the store");
}

#[gpui::test]
fn reopen_after_a_dropped_flight_shows_the_detail(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let handle = rig.open(cx);
    rig.close(cx, handle);
    rig.release(cx);

    let _handle = rig.open(cx);
    rig.release(cx);
    assert!(!rig.in_flight(cx));
    assert!(
        rig.has_detail(cx),
        "the reopened document is not a skeleton"
    );
}

/// The window the issue names: reopen while the orphaned flight is still
/// in the air. The shared guard skips the new dispatch, so the orphan's
/// landing is the only thing that can clear the record.
#[gpui::test]
fn reopen_during_a_dropped_flight_recovers_when_it_lands(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let handle = rig.open(cx);
    rig.close(cx, handle);

    let _handle = rig.open(cx);
    assert_eq!(
        rig.http.reads.load(Ordering::SeqCst),
        0,
        "nothing has answered yet"
    );
    rig.release(cx);
    assert_eq!(
        rig.http.reads.load(Ordering::SeqCst),
        1,
        "the shared guard skipped the duplicate"
    );

    assert!(!rig.in_flight(cx));
    assert!(rig.has_detail(cx));
}
