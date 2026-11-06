//! #739: a read older than a confirmed write must not land, must not
//! stamp fresh, and is replaced by exactly one read. Every test drives a
//! real page against a scripted transport and counts requests there. The
//! test dispatcher is single-threaded, so "the read is flying" is the
//! stretch before the next `run_until_parked`, and the write confirms in
//! that stretch through the same store and page calls the mutation's
//! continuation makes.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, WindowHandle};

use crate::drafts::DraftStore;
use crate::fixture::operation_name;
use crate::gql;
use crate::http::{Http, HttpError, HttpResult, Verb};
use crate::pr_page::{BusyOp, PrDetailPage};
use crate::rate_pause::Lane;
use crate::service::{AuthStatus, GithubCache, GithubState, Kind, PendingKind, PrKey, SpecResult};
use crate::usage::{Origin, Surface, Trigger, budget_now};

const REPO: &str = "o/r";
const NUMBER: i64 = 7;
const HOT_LOOP_READS: usize = 16;

fn origin(trigger: Trigger) -> Origin {
    Origin::new(Surface::PrPage, trigger)
}

#[derive(Clone)]
enum Reply {
    Body(String),
    Status(u16),
}

/// What the server knows at the time of the read.
fn detail_at_server(comments: usize, state: &str) -> Reply {
    let nodes: Vec<_> = (0..comments)
        .map(|i| serde_json::json!({ "id": format!("IC_{i}"), "body": "posted" }))
        .collect();
    Reply::Body(
        serde_json::json!({ "data": { "repository": { "pullRequest": {
            "id": "PR_7", "number": NUMBER, "title": "probe", "url": "u",
            "state": state, "isDraft": false, "createdAt": "2026-09-01T00:00:00Z",
            "comments": { "nodes": nodes },
        } } } })
        .to_string(),
    )
}

fn threads_at_server(threads: usize) -> Reply {
    let nodes: Vec<_> = (0..threads)
        .map(|i| serde_json::json!({ "id": format!("T_{i}"), "path": "a.rs" }))
        .collect();
    Reply::Body(
        serde_json::json!({ "data": { "repository": { "pullRequest": {
            "reviewThreads": { "nodes": nodes },
        } } } })
        .to_string(),
    )
}

/// One lane's scripted replies, in order; the last one repeats.
struct Script {
    reads: AtomicUsize,
    replies: Mutex<VecDeque<Reply>>,
}

impl Script {
    fn new(replies: Vec<Reply>) -> Self {
        Self {
            reads: AtomicUsize::new(0),
            replies: Mutex::new(replies.into()),
        }
    }

    fn answer(&self) -> Result<HttpResult, HttpError> {
        let reads = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        // A regression spins forever under `run_until_parked`: fail it.
        assert!(reads <= HOT_LOOP_READS, "hot refetch loop: {reads} reads");
        let mut replies = self.replies.lock().unwrap();
        let reply = if replies.len() > 1 {
            replies.pop_front()
        } else {
            replies.front().cloned()
        };
        let (status, body) = match reply.expect("a scripted reply") {
            Reply::Body(body) => (200, body),
            Reply::Status(status) => (status, r#"{"message":"scripted failure"}"#.to_string()),
        };
        Ok(HttpResult {
            status,
            etag: None,
            rate: None,
            body: Some(body),
        })
    }
}

struct ScriptedHttp {
    detail: Script,
    threads: Script,
}

impl Http for ScriptedHttp {
    fn request(
        &self,
        _: Verb,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<HttpResult, HttpError> {
        Err(HttpError("not a lane under test".into()))
    }

    fn post_graphql(&self, query: &str, _: &serde_json::Value) -> Result<HttpResult, HttpError> {
        match operation_name(query) {
            Some("PrDetail") => self.detail.answer(),
            Some("PrThreads") => self.threads.answer(),
            _ => Err(HttpError("not a lane under test".into())),
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
        title: "probe".into(),
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

fn landed_detail() -> gql::PrDetail {
    gql::pr_detail_from_value(&serde_json::json!({
        "pullRequest": {
            "id": "PR_7", "number": NUMBER, "title": "probe", "url": "u",
            "state": "OPEN", "isDraft": false, "createdAt": "2026-09-01T00:00:00Z",
        }
    }))
    .expect("the probe detail parses")
}

struct Rig {
    http: Arc<ScriptedHttp>,
    state: Entity<GithubState>,
}

impl Rig {
    /// A connected store holding a landed, young document.
    fn new(cx: &mut TestAppContext, detail: Vec<Reply>, threads: Vec<Reply>) -> Self {
        cx.update(dashboard_core::status::init);
        cx.update(gpui_component::init);
        cx.update(|cx| cx.background_executor().allow_parking());
        let http = Arc::new(ScriptedHttp {
            detail: Script::new(detail),
            threads: Script::new(threads),
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
                    DraftStore::at_tmp("pr-write-refetch"),
                );
                s.auth = AuthStatus::Connected;
                s.land_summary(REPO, &row("SUCCESS"));
                s.land_detail(REPO, NUMBER, landed_detail());
                s
            })
        });
        Self { http, state }
    }

    /// Open a page over the young document: nothing dispatches.
    fn open<'a>(
        &self,
        cx: &'a mut TestAppContext,
    ) -> (Entity<PrDetailPage>, &'a mut VisualTestContext) {
        let state = self.state.clone();
        let (page, vcx) = cx.add_window_view(|window, cx| {
            PrDetailPage::new(state, REPO.into(), NUMBER, None, window, cx)
        });
        vcx.run_until_parked();
        (page, vcx)
    }

    /// Open a page no test handle keeps alive, so closing the window
    /// drops it. Executors don't run: its open read, if any, is flying.
    fn open_droppable(&self, cx: &mut TestAppContext) -> WindowHandle<PrDetailPage> {
        let state = self.state.clone();
        cx.update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                cx.new(|cx| PrDetailPage::new(state, REPO.into(), NUMBER, None, window, cx))
            })
            .expect("the window opens")
        })
    }

    fn close(&self, cx: &mut TestAppContext, window: WindowHandle<PrDetailPage>) {
        cx.update(|cx| {
            window
                .update(cx, |_, window, _| window.remove_window())
                .ok();
        });
    }

    /// Age the document out, the way a read comes to be flying at all.
    fn void_freshness(&self, cx: &mut TestAppContext) {
        cx.update(|cx| {
            self.state.update(cx, |s, _| {
                let key = PrKey::new(REPO, NUMBER);
                s.pulls.get_mut(&key).expect("the document").detail_at = None;
            })
        });
    }

    /// A background detail read leaves, unanswered.
    fn fly_detail(&self, cx: &mut TestAppContext, page: &Entity<PrDetailPage>) {
        self.void_freshness(cx);
        cx.update(|cx| {
            page.update(cx, |p, cx| p.fetch_detail(origin(Trigger::Refocus), cx));
        });
    }

    /// The store half of a write confirming while a read flies.
    fn confirm_on_store(&self, cx: &mut TestAppContext, kind: PendingKind) {
        cx.update(|cx| {
            self.state.update(cx, |s, _| {
                let token = s
                    .begin_pending(REPO, NUMBER, kind)
                    .expect("no write pending");
                assert!(s.resolve_pending(REPO, NUMBER, token));
            })
        });
    }

    /// A store write confirms and the live page sends its refetch.
    fn confirm_write(
        &self,
        vcx: &mut VisualTestContext,
        page: &Entity<PrDetailPage>,
        kind: PendingKind,
    ) {
        self.confirm_on_store(vcx, kind);
        page.update_in(vcx, |p, window, cx| {
            p.revalidate_after_store_mutation(window, cx);
        });
    }

    fn poke_store(&self, cx: &mut TestAppContext) {
        cx.update(|cx| self.state.update(cx, |_, cx| cx.notify()));
        cx.run_until_parked();
    }

    fn pause_graphql(&self, cx: &mut TestAppContext, until: u64) {
        cx.update(|cx| {
            self.state.update(cx, |s, _| {
                s.rate_limited_until = Default::default();
                s.rate_limited_until.arm(Lane::Graphql, until);
            })
        });
    }

    fn elapse_backoff(&self, cx: &mut TestAppContext) {
        cx.update(|cx| {
            self.state.update(cx, |s, _| {
                let key = PrKey::new(REPO, NUMBER);
                let entry = s.pulls.get_mut(&key).expect("the document");
                entry.books.attempted_at = Some(Instant::now() - Duration::from_secs(3600));
            })
        });
    }

    fn detail_reads(&self) -> usize {
        self.http.detail.reads.load(Ordering::SeqCst)
    }

    fn threads_reads(&self) -> usize {
        self.http.threads.reads.load(Ordering::SeqCst)
    }

    fn with_entry<R>(
        &self,
        cx: &mut TestAppContext,
        f: impl FnOnce(&crate::service::PrEntry) -> R,
    ) -> R {
        cx.update(|cx| {
            f(self
                .state
                .read(cx)
                .pr_entry(REPO, NUMBER)
                .expect("the document"))
        })
    }

    fn comments(&self, cx: &mut TestAppContext) -> usize {
        self.with_entry(cx, |e| {
            e.detail.as_ref().map_or(0, |d| d.comments.nodes.len())
        })
    }

    fn stamped(&self, cx: &mut TestAppContext) -> bool {
        self.with_entry(cx, |e| e.detail_at.is_some())
    }
}

fn thread_count(cx: &mut TestAppContext, page: &Entity<PrDetailPage>) -> Option<usize> {
    cx.update(|cx| page.read(cx).threads.data().map(|t| t.nodes.len()))
}

// --- the detail lane -------------------------------------------------------

/// The issue's scenario: the forced refetch is skipped while the earlier
/// read flies. That read must not land, and the follow-up must.
#[gpui::test]
fn write_during_a_read_gets_one_follow_up_that_shows_it(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        vec![detail_at_server(0, "OPEN"), detail_at_server(1, "OPEN")],
        vec![threads_at_server(0)],
    );
    let (page, vcx) = rig.open(cx);
    rig.fly_detail(vcx, &page);
    rig.confirm_write(vcx, &page, PendingKind::Comment);

    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 2, "the read, then one follow-up");
    assert_eq!(rig.comments(vcx), 1, "the final detail shows the write");
    assert!(rig.stamped(vcx), "the follow-up postdates the write");

    rig.poke_store(vcx);
    assert_eq!(rig.detail_reads(), 2, "settled: no further read");
}

/// Labels, assignees, reviewers and reactions resolve on the page, not
/// through `resolve_pending`: their skipped refetch is the only signal.
#[gpui::test]
fn page_modal_write_during_a_read_gets_one_follow_up(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        vec![detail_at_server(0, "OPEN"), detail_at_server(1, "OPEN")],
        vec![threads_at_server(0)],
    );
    let (page, vcx) = rig.open(cx);
    rig.fly_detail(vcx, &page);
    page.update_in(vcx, |p, window, cx| {
        let confirmed = SpecResult {
            kind: Kind::PrDetail,
            ident: None,
            status: 200,
            etag: None,
            rate: None,
            body: Some(r#"{"data":{}}"#.into()),
            network: None,
            cursor: None,
        };
        p.apply_mutation_result(BusyOp::PrSetLabels, confirmed, window, cx);
    });

    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 2);
    assert_eq!(rig.comments(vcx), 1);
    assert!(rig.stamped(vcx));
}

/// A Files-tab write refetches threads only, so no detail refetch ever
/// hits the guard: the store's mark is what stops the flying detail read
/// from cancelling the void.
#[gpui::test]
fn threads_scoped_write_during_a_detail_read_voids_and_reruns(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        vec![detail_at_server(0, "OPEN"), detail_at_server(1, "OPEN")],
        vec![threads_at_server(1)],
    );
    let (page, vcx) = rig.open(cx);
    rig.fly_detail(vcx, &page);
    rig.confirm_on_store(vcx, PendingKind::LineComment);

    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 2);
    assert_eq!(rig.comments(vcx), 1);
}

/// A rate pause holds the follow-up, which shows the stale landing alone:
/// no body, no stamp. The first notification past the pause fires it.
#[gpui::test]
fn stale_read_does_not_stamp_fresh_and_a_paused_rerun_waits(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        vec![detail_at_server(0, "OPEN"), detail_at_server(1, "OPEN")],
        vec![threads_at_server(0)],
    );
    let (page, vcx) = rig.open(cx);
    rig.fly_detail(vcx, &page);
    rig.confirm_write(vcx, &page, PendingKind::Comment);
    rig.pause_graphql(vcx, budget_now() + 3600);

    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1, "the pause holds the follow-up");
    assert!(!rig.stamped(vcx), "the outdated read claims no freshness");
    assert!(
        !rig.with_entry(vcx, |e| e.books.in_flight),
        "the flight closed"
    );

    rig.poke_store(vcx);
    assert_eq!(rig.detail_reads(), 1, "still paused");

    rig.pause_graphql(vcx, budget_now().saturating_sub(1));
    rig.poke_store(vcx);
    assert_eq!(rig.detail_reads(), 2, "the held follow-up fires once");
    assert_eq!(rig.comments(vcx), 1);
    assert!(rig.stamped(vcx));
}

/// #802's rule holds: the flight record clears with the page gone. The
/// outdated body does not land, the void survives, and the reopen
/// revalidates.
#[gpui::test]
fn page_gone_stale_read_keeps_the_void_and_the_reopen_revalidates(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        vec![detail_at_server(0, "OPEN"), detail_at_server(1, "OPEN")],
        vec![threads_at_server(0)],
    );
    rig.void_freshness(cx);
    let window = rig.open_droppable(cx);
    rig.confirm_on_store(cx, PendingKind::Comment);
    rig.close(cx, window);

    cx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1, "no page, no follow-up");
    assert!(
        !rig.with_entry(cx, |e| e.books.in_flight),
        "the flight closed"
    );
    assert!(!rig.stamped(cx), "the void survives the landing");

    let (_page, vcx) = rig.open(cx);
    assert_eq!(rig.detail_reads(), 2, "the reopen is the one follow-up");
    assert_eq!(rig.comments(vcx), 1);
    assert!(rig.stamped(vcx));
}

/// A reopen during the outdated flight is skipped by the shared guard and
/// owns no landing: the mark on the document is what brings it the rerun.
#[gpui::test]
fn reopen_during_an_outdated_orphan_flight_gets_the_rerun(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        vec![detail_at_server(0, "OPEN"), detail_at_server(1, "OPEN")],
        vec![threads_at_server(0)],
    );
    rig.void_freshness(cx);
    let window = rig.open_droppable(cx);
    rig.confirm_on_store(cx, PendingKind::Comment);
    rig.close(cx, window);

    let _reopened = rig.open_droppable(cx);
    cx.run_until_parked();
    assert_eq!(rig.detail_reads(), 2);
    assert_eq!(rig.comments(cx), 1);
    assert!(rig.stamped(cx));
}

/// A confirmed merge patches the state; the read that predates it must
/// not flip the document back to OPEN (and the merge button with it).
#[gpui::test]
fn stale_read_does_not_undo_a_patched_merge(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        vec![detail_at_server(0, "OPEN")],
        vec![threads_at_server(0)],
    );
    rig.void_freshness(cx);
    let window = rig.open_droppable(cx);
    rig.confirm_on_store(cx, PendingKind::Merge);
    rig.close(cx, window);

    cx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1);
    let state = rig.with_entry(cx, |e| e.detail.as_ref().and_then(|d| d.state.clone()));
    assert_eq!(state.as_deref(), Some("MERGED"));
    assert!(!rig.stamped(cx));
}

/// No loop: the outdated read and its follow-up both fail, and nothing
/// re-dispatches, not on a later store write and not past the backoff.
#[gpui::test]
fn failing_reads_stop_after_the_one_follow_up(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, vec![Reply::Status(502)], vec![threads_at_server(0)]);
    let (page, vcx) = rig.open(cx);
    rig.fly_detail(vcx, &page);
    rig.confirm_write(vcx, &page, PendingKind::Comment);

    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 2, "failure still earns the follow-up");

    rig.poke_store(vcx);
    rig.elapse_backoff(vcx);
    rig.poke_store(vcx);
    assert_eq!(rig.detail_reads(), 2);
    assert!(!rig.stamped(vcx));
}

/// #731 stays intact: the drift catch-up is the flying read, the follow-up
/// replaces it, and the two never add up to a third read, even failing.
#[gpui::test]
fn catch_up_flight_and_follow_up_are_the_only_reads(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        vec![detail_at_server(0, "OPEN"), Reply::Status(502)],
        vec![threads_at_server(0)],
    );
    let (page, vcx) = rig.open(cx);
    vcx.update(|_, cx| {
        rig.state.update(cx, |s, cx| {
            s.land_summary(REPO, &row("PENDING"));
            cx.notify();
        })
    });
    assert_eq!(rig.detail_reads(), 0, "unanswered: the catch-up is flying");
    assert!(rig.with_entry(vcx, |e| e.books.in_flight));
    rig.confirm_write(vcx, &page, PendingKind::Comment);

    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 2);

    rig.poke_store(vcx);
    rig.elapse_backoff(vcx);
    rig.poke_store(vcx);
    assert_eq!(rig.detail_reads(), 2, "the failed arming stays spent");
    assert!(
        rig.with_entry(vcx, |e| e.rollup_dirty),
        "reopen still revalidates"
    );
}

/// Without a write, the guard's dedup is unchanged: a manual refresh
/// rides out the flight and its result lands.
#[gpui::test]
fn manual_refresh_during_a_read_still_rides_it_out(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        vec![detail_at_server(1, "OPEN")],
        vec![threads_at_server(0)],
    );
    let (page, vcx) = rig.open(cx);
    rig.fly_detail(vcx, &page);
    vcx.update(|_, cx| {
        page.update(cx, |p, cx| p.fetch_detail(origin(Trigger::Manual), cx));
    });

    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1);
    assert_eq!(rig.comments(vcx), 1);
    assert!(rig.stamped(vcx));
}

/// A write that failed changed nothing, so the flying read stays good.
#[gpui::test]
fn failed_store_write_leaves_the_flying_read_alone(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        vec![detail_at_server(1, "OPEN")],
        vec![threads_at_server(0)],
    );
    let (page, vcx) = rig.open(cx);
    rig.fly_detail(vcx, &page);
    vcx.update(|_, cx| {
        rig.state.update(cx, |s, _| {
            let token = s.begin_pending(REPO, NUMBER, PendingKind::Comment).unwrap();
            assert!(s.fail_pending(REPO, NUMBER, token, "rejected".into()));
        })
    });

    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1);
    assert_eq!(rig.comments(vcx), 1);
    assert!(rig.stamped(vcx));
}

// --- the threads lane ------------------------------------------------------

#[gpui::test]
fn thread_write_during_a_threads_read_gets_one_follow_up(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        vec![detail_at_server(0, "OPEN")],
        vec![threads_at_server(0), threads_at_server(1)],
    );
    let (page, vcx) = rig.open(cx);
    vcx.update(|_, cx| {
        page.update(cx, |p, cx| {
            p.fetch_threads(origin(Trigger::Refocus), cx);
            p.fetch_threads(origin(Trigger::MutationRefetch), cx);
        });
    });

    vcx.run_until_parked();
    assert_eq!(rig.threads_reads(), 2, "the read, then one follow-up");
    assert_eq!(thread_count(vcx, &page), Some(1), "the write shows");

    rig.poke_store(vcx);
    assert_eq!(rig.threads_reads(), 2, "settled: no further read");
}

/// A pause holds the follow-up, which shows the outdated body never lands.
#[gpui::test]
fn outdated_threads_read_does_not_land_and_a_paused_rerun_waits(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        vec![detail_at_server(0, "OPEN")],
        vec![threads_at_server(0), threads_at_server(1)],
    );
    let (page, vcx) = rig.open(cx);
    vcx.update(|_, cx| {
        page.update(cx, |p, cx| {
            p.fetch_threads(origin(Trigger::Refocus), cx);
            p.fetch_threads(origin(Trigger::MutationRefetch), cx);
        });
    });
    rig.pause_graphql(vcx, budget_now() + 3600);

    vcx.run_until_parked();
    assert_eq!(rig.threads_reads(), 1);
    assert_eq!(thread_count(vcx, &page), None, "the outdated body went");
    assert!(
        !vcx.update(|_, cx| page.read(cx).threads.in_flight),
        "the record closed"
    );

    rig.pause_graphql(vcx, budget_now().saturating_sub(1));
    rig.poke_store(vcx);
    assert_eq!(rig.threads_reads(), 2);
    assert_eq!(thread_count(vcx, &page), Some(1));
}

#[gpui::test]
fn failing_threads_reads_stop_after_the_one_follow_up(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        vec![detail_at_server(0, "OPEN")],
        vec![Reply::Status(502)],
    );
    let (page, vcx) = rig.open(cx);
    vcx.update(|_, cx| {
        page.update(cx, |p, cx| {
            p.fetch_threads(origin(Trigger::Refocus), cx);
            p.fetch_threads(origin(Trigger::MutationRefetch), cx);
        });
    });

    vcx.run_until_parked();
    assert_eq!(rig.threads_reads(), 2);

    rig.poke_store(vcx);
    assert_eq!(rig.threads_reads(), 2);
}

/// The flight belongs to a page that died, so its result lands nowhere
/// and only the freed guard can wake the page that owes the rerun.
#[gpui::test]
fn thread_write_behind_an_orphan_threads_flight_reruns_when_it_ends(cx: &mut TestAppContext) {
    let rig = Rig::new(
        cx,
        vec![detail_at_server(0, "OPEN")],
        vec![threads_at_server(0), threads_at_server(1)],
    );
    let window = rig.open_droppable(cx);
    cx.update(|cx| {
        window
            .update(cx, |p, _, cx| p.fetch_threads(origin(Trigger::Refocus), cx))
            .expect("the page is alive");
    });
    rig.close(cx, window);

    // Executors don't run: the orphan flight is still in the air.
    let reopened = rig.open_droppable(cx);
    cx.update(|cx| {
        reopened
            .update(cx, |p, _, cx| {
                p.fetch_threads(origin(Trigger::MutationRefetch), cx)
            })
            .expect("the page is alive");
    });
    assert_eq!(rig.threads_reads(), 0, "unanswered: the orphan still flies");

    cx.run_until_parked();
    assert_eq!(rig.threads_reads(), 2);
    let threads = cx.update(|cx| {
        reopened
            .update(cx, |p, _, _| p.threads.data().map(|t| t.nodes.len()))
            .expect("the page is alive")
    });
    assert_eq!(threads, Some(1));
}
