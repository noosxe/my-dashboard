//! #731: a failing detail read under a dirty rollup must not refetch
//! itself. Every test drives a real page and counts `PrDetail` requests
//! at the transport.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};

use crate::drafts::DraftStore;
use crate::fixture::operation_name;
use crate::gql;
use crate::http::{Http, HttpError, HttpResult, Verb};
use crate::pr_page::PrDetailPage;
use crate::rate_pause::{Lane, RateLimitPause};
use crate::service::{AuthStatus, GithubCache, GithubState, PrKey};
use crate::usage::budget_now;

const REPO: &str = "o/r";
const NUMBER: i64 = 7;
const HOT_LOOP_READS: usize = 32;

/// How the scripted transport answers a `PrDetail` read.
#[derive(Clone, Copy)]
enum Answer {
    Status(u16),
    NetworkError,
}

/// Counts `PrDetail` reads and fails them as scripted; every other lane
/// (threads, files, runs) errors out without counting.
struct DetailHttp {
    reads: AtomicUsize,
    answer: Mutex<Answer>,
}

impl Http for DetailHttp {
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
        let reads = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        // A regression spins forever under `run_until_parked`: fail it.
        assert!(reads <= HOT_LOOP_READS, "hot refetch loop: {reads} reads");
        match *self.answer.lock().unwrap() {
            Answer::Status(status) => Ok(HttpResult {
                status,
                etag: None,
                rate: None,
                body: Some(r#"{"message":"scripted failure"}"#.into()),
            }),
            Answer::NetworkError => Err(HttpError("scripted network failure".into())),
        }
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

fn row(status: &str) -> gql::PullRequest {
    gql::PullRequest {
        key: format!("{REPO}#{NUMBER}"),
        database_id: None,
        number: NUMBER,
        title: "catch-up probe".into(),
        draft: false,
        state: None,
        author: None,
        updated_at: None,
        url: "https://probe".into(),
        status: Some(status.into()),
        labels: None,
        review_requests: None,
    }
}

fn detail() -> gql::PrDetail {
    gql::pr_detail_from_value(&serde_json::json!({
        "pullRequest": {
            "id": "PR_7", "number": NUMBER, "title": "catch-up probe", "url": "u",
            "state": "OPEN", "isDraft": false, "createdAt": "2026-09-01T00:00:00Z",
        }
    }))
    .expect("the probe detail parses")
}

struct Rig {
    http: Arc<DetailHttp>,
    state: Entity<GithubState>,
}

impl Rig {
    /// A connected store holding a landed, young document.
    fn new(cx: &mut TestAppContext, answer: Answer) -> Self {
        cx.update(dashboard_core::status::init);
        cx.update(gpui_component::init);
        // The page schedules background tokenization (docs/0040 D5).
        cx.update(|cx| cx.background_executor().allow_parking());
        let http = Arc::new(DetailHttp {
            reads: AtomicUsize::new(0),
            answer: Mutex::new(answer),
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
                    DraftStore::at_tmp("pr-catch-up"),
                );
                s.auth = AuthStatus::Connected;
                s.land_summary(REPO, &row("SUCCESS"));
                s.land_detail(REPO, NUMBER, detail());
                s
            })
        });
        Self { http, state }
    }

    fn open(&self, cx: &mut TestAppContext) -> Entity<PrDetailPage> {
        let state = self.state.clone();
        let (page, _): (_, &mut VisualTestContext) = cx.add_window_view(|window, cx| {
            PrDetailPage::new(state, REPO.into(), NUMBER, None, window, cx)
        });
        cx.run_until_parked();
        page
    }

    fn reads(&self) -> usize {
        self.http.reads.load(Ordering::SeqCst)
    }

    /// A list lane lands `status`, the way `apply` does it: write, then
    /// notify the store's observers.
    fn land_summary(&self, cx: &mut TestAppContext, status: &str) {
        let row = row(status);
        cx.update(|cx| {
            self.state.update(cx, |s, cx| {
                s.land_summary(REPO, &row);
                cx.notify();
            })
        });
        cx.run_until_parked();
    }

    /// An unrelated store write: any lane landing wakes the observer.
    fn poke_store(&self, cx: &mut TestAppContext) {
        cx.update(|cx| self.state.update(cx, |_, cx| cx.notify()));
        cx.run_until_parked();
    }

    /// Run the failure backoff out by rewinding the attempt clock.
    fn elapse_backoff(&self, cx: &mut TestAppContext) {
        cx.update(|cx| {
            self.state.update(cx, |s, _| {
                let key = PrKey::new(REPO, NUMBER);
                let entry = s.pulls.get_mut(&key).expect("the document exists");
                entry.books.attempted_at = Some(Instant::now() - Duration::from_secs(3600));
            })
        });
    }

    fn pause(&self, cx: &mut TestAppContext, lane: Lane) {
        cx.update(|cx| {
            self.state.update(cx, |s, _| {
                s.rate_limited_until.arm(lane, budget_now() + 3600);
            })
        });
    }
}

/// The issue's regression, for each failure class: the read fails while
/// the mark is armed, and nothing re-dispatches, not on the failure's
/// own notify and not on any later store write.
#[gpui::test]
fn failed_read_with_a_dirty_mark_dispatches_exactly_once(cx: &mut TestAppContext) {
    for answer in [
        Answer::Status(404),
        Answer::Status(403),
        Answer::Status(502),
        Answer::NetworkError,
    ] {
        let rig = Rig::new(cx, answer);
        let _page = rig.open(cx);
        assert_eq!(rig.reads(), 0, "a young document skips the open dispatch");

        rig.land_summary(cx, "PENDING");
        assert_eq!(rig.reads(), 1, "the drift fires one catch-up");

        rig.poke_store(cx);
        rig.elapse_backoff(cx);
        rig.poke_store(cx);
        assert_eq!(
            rig.reads(),
            1,
            "a spent arming stays spent past the backoff"
        );

        let entry = cx.update(|cx| rig.state.read(cx).pr_entry(REPO, NUMBER).cloned());
        let entry = entry.expect("the document exists");
        assert!(entry.rollup_dirty, "the failure did not land the rollup");
        assert!(!entry.detail_fresh(), "reopen still revalidates");
    }
}

/// The reopen path from the issue's repro: the mark is armed while the
/// page is closed, so the open read is the arming's one dispatch.
#[gpui::test]
fn reopening_with_a_dirty_mark_dispatches_exactly_once(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, Answer::Status(404));
    rig.land_summary(cx, "PENDING");
    assert_eq!(rig.reads(), 0, "no page is open");

    let _page = rig.open(cx);
    assert_eq!(rig.reads(), 1, "the open read revalidates");

    rig.elapse_backoff(cx);
    rig.poke_store(cx);
    assert_eq!(rig.reads(), 1, "the open read spent the arming");
}

/// A later genuine move re-arms the catch-up, paced by the backoff the
/// failed read left behind.
#[gpui::test]
fn a_moved_rollup_rearms_behind_the_backoff(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, Answer::Status(404));
    let _page = rig.open(cx);
    rig.land_summary(cx, "PENDING");
    assert_eq!(rig.reads(), 1);

    rig.land_summary(cx, "FAILURE");
    assert_eq!(rig.reads(), 1, "re-armed, but the backoff has not elapsed");

    rig.elapse_backoff(cx);
    rig.poke_store(cx);
    assert_eq!(rig.reads(), 2, "past the backoff the new arming fires");

    rig.elapse_backoff(cx);
    rig.poke_store(cx);
    assert_eq!(rig.reads(), 2, "and it fires once");

    rig.land_summary(cx, "FAILURE");
    rig.elapse_backoff(cx);
    rig.poke_store(cx);
    assert_eq!(rig.reads(), 2, "an unchanged landing never re-arms");
}

/// A paused GraphQL budget holds the catch-up without spending it; the
/// first store write after the pause lifts fires it.
#[gpui::test]
fn a_paused_graphql_budget_holds_the_dispatch(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, Answer::Status(404));
    let _page = rig.open(cx);
    rig.pause(cx, Lane::Graphql);

    rig.land_summary(cx, "PENDING");
    rig.poke_store(cx);
    assert_eq!(rig.reads(), 0, "no dispatch while the budget is paused");

    cx.update(|cx| {
        rig.state
            .update(cx, |s, _| s.rate_limited_until = RateLimitPause::default())
    });
    rig.poke_store(cx);
    assert_eq!(rig.reads(), 1, "the held arming fires once the pause lifts");
}

/// docs/0082 Q10: the other budget's pause never gates this lane.
#[gpui::test]
fn a_paused_rest_budget_does_not_hold_the_dispatch(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, Answer::Status(404));
    let _page = rig.open(cx);
    rig.pause(cx, Lane::Rest);

    rig.land_summary(cx, "PENDING");
    assert_eq!(rig.reads(), 1);
}

/// A landed detail settles the arming, so the next move starts clean.
#[gpui::test]
fn a_landed_read_resets_the_arming(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, Answer::Status(404));
    let _page = rig.open(cx);
    rig.land_summary(cx, "PENDING");
    assert_eq!(rig.reads(), 1);

    cx.update(|cx| {
        rig.state
            .update(cx, |s, _| s.land_detail(REPO, NUMBER, detail()))
    });
    rig.land_summary(cx, "FAILURE");
    assert_eq!(rig.reads(), 2, "a clean lane catches up with no wait");
}
