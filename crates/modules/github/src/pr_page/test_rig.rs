//! A scripted transport and a store/page rig shared by the freshness
//! tests (head change #752, UNKNOWN re-poll #752, page-modal writes
//! #807). Every test counts requests at the transport, so a refetch is
//! observed where it leaves, never inferred from state. The test
//! dispatcher is single-threaded: "a read flies" is the stretch before
//! the next `run_until_parked`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, WindowHandle};

use dashboard_core::usage_records::{MemorySink, Recorder, UsageSource as _};

use crate::drafts::DraftStore;
use crate::fixture::operation_name;
use crate::gql;
use crate::http::{Http, HttpError, HttpResult, Verb};
use crate::pr_page::PrDetailPage;
use crate::service::{AuthStatus, GithubCache, GithubState, PrKey};

pub(super) const REPO: &str = "o/r";
pub(super) const NUMBER: i64 = 7;
/// A regression that loops fails here instead of spinning forever.
const HOT_LOOP_READS: usize = 24;

/// What the server answers to one detail read.
#[derive(Clone, Copy)]
pub(super) struct Server {
    pub(super) head: &'static str,
    pub(super) mergeable: &'static str,
}

pub(super) const fn server(head: &'static str, mergeable: &'static str) -> Server {
    Server { head, mergeable }
}

fn detail_json(s: Server) -> String {
    serde_json::json!({ "data": { "repository": { "pullRequest": {
        "id": "PR_7", "number": NUMBER, "title": "probe", "url": "u",
        "state": "OPEN", "isDraft": false, "createdAt": "2026-09-01T00:00:00Z",
        "headRefName": "feature", "headRefOid": s.head, "mergeable": s.mergeable,
    } } } })
    .to_string()
}

pub(super) struct ScriptedHttp {
    detail: Mutex<VecDeque<Server>>,
    pub(super) detail_reads: AtomicUsize,
    pub(super) threads_reads: AtomicUsize,
    pub(super) files_reads: AtomicUsize,
    /// REST requests to the gated-runs list.
    pub(super) runs_reads: AtomicUsize,
    pub(super) mutations: AtomicUsize,
    /// The status a mutation answers with.
    pub(super) mutation_status: AtomicUsize,
    /// When set, every detail read answers this instead of the script.
    pub(super) detail_override: Mutex<Option<HttpResult>>,
}

fn ok(body: String) -> Result<HttpResult, HttpError> {
    reply(200, body)
}

fn reply(status: u16, body: String) -> Result<HttpResult, HttpError> {
    Ok(HttpResult {
        status,
        etag: None,
        rate: None,
        body: Some(body),
    })
}

impl Http for ScriptedHttp {
    fn request(
        &self,
        _: Verb,
        url: &str,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<HttpResult, HttpError> {
        if url.contains("/actions/runs") {
            self.runs_reads.fetch_add(1, Ordering::SeqCst);
            return ok(r#"{"workflow_runs":[]}"#.into());
        }
        if url.contains("/files") {
            self.files_reads.fetch_add(1, Ordering::SeqCst);
            return ok("[]".into());
        }
        Err(HttpError("not a lane under test".into()))
    }

    fn post_graphql(&self, query: &str, _: &serde_json::Value) -> Result<HttpResult, HttpError> {
        match operation_name(query) {
            Some("PrDetail") => {
                let reads = self.detail_reads.fetch_add(1, Ordering::SeqCst) + 1;
                assert!(reads <= HOT_LOOP_READS, "hot refetch loop: {reads} reads");
                if let Some(answer) = self.detail_override.lock().unwrap().clone() {
                    return Ok(answer);
                }
                let mut script = self.detail.lock().unwrap();
                let s = if script.len() > 1 {
                    script.pop_front()
                } else {
                    script.front().copied()
                };
                ok(detail_json(s.expect("a scripted detail")))
            }
            Some("PrThreads") => {
                self.threads_reads.fetch_add(1, Ordering::SeqCst);
                ok(
                    r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[]}}}}}"#
                        .into(),
                )
            }
            // Everything else a page posts here is a write.
            _ => {
                self.mutations.fetch_add(1, Ordering::SeqCst);
                let status = self.mutation_status.load(Ordering::SeqCst) as u16;
                reply(status, r#"{"data":{}}"#.into())
            }
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

pub(super) struct Rig {
    pub(super) http: Arc<ScriptedHttp>,
    pub(super) state: Entity<GithubState>,
    /// Every request the store recorded, with its trigger.
    pub(super) records: Arc<MemorySink>,
}

impl Rig {
    /// A connected store holding a landed, young document at `first`; the
    /// server answers `script` to the reads that follow, the last entry
    /// repeating.
    pub(super) fn new(cx: &mut TestAppContext, first: Server, script: Vec<Server>) -> Self {
        cx.update(dashboard_core::status::init);
        cx.update(gpui_component::init);
        cx.update(|cx| cx.background_executor().allow_parking());
        let http = Arc::new(ScriptedHttp {
            detail: Mutex::new(script.into()),
            detail_reads: AtomicUsize::new(0),
            threads_reads: AtomicUsize::new(0),
            files_reads: AtomicUsize::new(0),
            runs_reads: AtomicUsize::new(0),
            mutations: AtomicUsize::new(0),
            mutation_status: AtomicUsize::new(200),
            detail_override: Mutex::new(None),
        });
        let client: Arc<dyn Http> = http.clone();
        let records = Arc::new(MemorySink::new(256));
        let sink = records.clone();
        let state = cx.update(|cx| {
            cx.new(|_| {
                let mut s = GithubState::with_recorder(
                    client,
                    Arc::new(NoStore),
                    Default::default(),
                    Default::default(),
                    GithubCache::new(dashboard_core::paths::AppPaths::init()),
                    DraftStore::at_tmp("pr-freshness"),
                    Recorder::new("github", sink),
                    None,
                );
                s.auth = AuthStatus::Connected;
                s.land_summary(
                    REPO,
                    &gql::PullRequest {
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
                let detail = gql::pr_detail_from_value(
                    &serde_json::from_str::<serde_json::Value>(&detail_json(first)).unwrap()["data"]
                        ["repository"],
                )
                .expect("the probe detail parses");
                s.land_detail(REPO, NUMBER, detail);
                s
            })
        });
        Self {
            http,
            state,
            records,
        }
    }

    /// Open a page over the young document: nothing dispatches.
    pub(super) fn open<'a>(
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
    pub(super) fn open_droppable(&self, cx: &mut TestAppContext) -> WindowHandle<PrDetailPage> {
        let state = self.state.clone();
        cx.update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                cx.new(|cx| PrDetailPage::new(state, REPO.into(), NUMBER, None, window, cx))
            })
            .expect("the window opens")
        })
    }

    pub(super) fn close(&self, cx: &mut TestAppContext, window: WindowHandle<PrDetailPage>) {
        cx.update(|cx| {
            window
                .update(cx, |_, window, _| window.remove_window())
                .ok();
        });
    }

    pub(super) fn stamped(&self, cx: &mut TestAppContext) -> bool {
        cx.update(|cx| {
            self.state
                .read(cx)
                .pr_entry(REPO, NUMBER)
                .is_some_and(|e| e.detail_at.is_some())
        })
    }

    pub(super) fn head(&self, cx: &mut TestAppContext) -> Option<String> {
        cx.update(|cx| {
            self.state
                .read(cx)
                .pr_entry(REPO, NUMBER)
                .and_then(|e| e.detail.as_ref())
                .and_then(|d| d.head_oid.clone())
        })
    }

    pub(super) fn void_freshness(&self, cx: &mut TestAppContext) {
        cx.update(|cx| {
            self.state.update(cx, |s, _| {
                s.pulls
                    .get_mut(&PrKey::new(REPO, NUMBER))
                    .expect("the document")
                    .detail_at = None;
            })
        });
    }

    pub(super) fn detail_reads(&self) -> usize {
        self.http.detail_reads.load(Ordering::SeqCst)
    }
    pub(super) fn threads_reads(&self) -> usize {
        self.http.threads_reads.load(Ordering::SeqCst)
    }
    pub(super) fn files_reads(&self) -> usize {
        self.http.files_reads.load(Ordering::SeqCst)
    }
    /// Answer every following detail read with `status`, `rate` and `body`.
    pub(super) fn answer_details_with(
        &self,
        status: u16,
        rate: Option<crate::http::RateLimit>,
        body: &str,
    ) {
        *self.http.detail_override.lock().unwrap() = Some(HttpResult {
            status,
            etag: None,
            rate,
            body: Some(body.to_string()),
        });
    }

    pub(super) fn runs_reads(&self) -> usize {
        self.http.runs_reads.load(Ordering::SeqCst)
    }
    /// The triggers of the recorded gated-runs requests.
    pub(super) fn gated_triggers(&self) -> Vec<String> {
        self.records
            .snapshot()
            .iter()
            .filter(|r| r.op.as_ref() == "gated-runs")
            .map(|r| r.trigger.to_string())
            .collect()
    }
    pub(super) fn mutations(&self) -> usize {
        self.http.mutations.load(Ordering::SeqCst)
    }
}
