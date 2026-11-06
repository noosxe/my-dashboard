//! Shared harness for the issue page's transport-level tests (#806): a
//! real page over a real store, the only fake being the scripted `Http`.
//! The test dispatcher is single-threaded, so "the read is flying" is the
//! stretch before the next `run_until_parked`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, WindowHandle};

use crate::drafts::DraftStore;
use crate::fixture::operation_name;
use crate::gql;
use crate::http::{Http, HttpError, HttpResult, Verb};
use crate::issue_page::IssueDetailPage;
use crate::service::{AuthStatus, GithubCache, GithubState, IssuePendingKind, PrKey};
use crate::usage::{Origin, Surface, Trigger};

pub(super) const REPO: &str = "o/r";
pub(super) const NUMBER: i64 = 7;
const HOT_LOOP_READS: usize = 16;

pub(super) fn origin(trigger: Trigger) -> Origin {
    Origin::new(Surface::IssuePage, trigger)
}

#[derive(Clone)]
pub(super) enum Reply {
    Body(String),
    Status(u16),
}

/// What the server knows at the time of the read.
pub(super) fn detail_at_server(comments: usize, state: &str) -> Reply {
    let nodes: Vec<_> = (0..comments)
        .map(|i| serde_json::json!({ "id": format!("IC_{i}"), "body": "posted" }))
        .collect();
    Reply::Body(
        serde_json::json!({ "data": { "repository": { "issue": {
            "id": "I_7", "number": NUMBER, "title": "probe", "url": "u",
            "state": state, "createdAt": "2026-09-01T00:00:00Z",
            "comments": { "nodes": nodes },
        } } } })
        .to_string(),
    )
}

/// Scripted replies, in order; the last one repeats.
pub(super) struct Script {
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

pub(super) struct ScriptedHttp {
    pub(super) detail: Script,
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
            Some("IssueDetail") => self.detail.answer(),
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

pub(super) fn row() -> gql::Issue {
    gql::Issue {
        key: format!("{REPO}#{NUMBER}"),
        database_id: None,
        number: NUMBER,
        title: "probe".into(),
        state: None,
        author: None,
        updated_at: None,
        url: "https://probe".into(),
        labels: None,
    }
}

fn landed_detail() -> gql::IssueDetail {
    gql::issue_detail_from_value(&serde_json::json!({
        "issue": {
            "id": "I_7", "number": NUMBER, "title": "probe", "url": "u",
            "state": "OPEN", "createdAt": "2026-09-01T00:00:00Z",
        }
    }))
    .expect("the probe detail parses")
}

pub(super) struct Rig {
    pub(super) http: Arc<ScriptedHttp>,
    pub(super) state: Entity<GithubState>,
}

impl Rig {
    /// A connected store holding a summary row, and a landed young
    /// detail when `landed` — otherwise a cold document.
    pub(super) fn new(cx: &mut TestAppContext, detail: Vec<Reply>, landed: bool) -> Self {
        cx.update(dashboard_core::status::init);
        cx.update(gpui_component::init);
        cx.update(|cx| cx.background_executor().allow_parking());
        let http = Arc::new(ScriptedHttp {
            detail: Script::new(detail),
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
                    DraftStore::at_tmp("issue-page-rig"),
                );
                s.auth = AuthStatus::Connected;
                s.land_issue_summary(REPO, &row());
                if landed {
                    s.land_issue_detail(REPO, NUMBER, landed_detail());
                }
                s
            })
        });
        Self { http, state }
    }

    /// Open a page that stays alive through the test handle.
    pub(super) fn open<'a>(
        &self,
        cx: &'a mut TestAppContext,
    ) -> (Entity<IssueDetailPage>, &'a mut VisualTestContext) {
        let state = self.state.clone();
        let (page, vcx) = cx.add_window_view(|window, cx| {
            IssueDetailPage::new(state, REPO.into(), row(), None, window, cx)
        });
        vcx.run_until_parked();
        (page, vcx)
    }

    /// Open a page no test handle keeps alive, so closing the window
    /// drops it. Executors don't run: its open read, if any, is flying.
    pub(super) fn open_droppable(&self, cx: &mut TestAppContext) -> WindowHandle<IssueDetailPage> {
        let state = self.state.clone();
        cx.update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                cx.new(|cx| IssueDetailPage::new(state, REPO.into(), row(), None, window, cx))
            })
            .expect("the window opens")
        })
    }

    pub(super) fn close(&self, cx: &mut TestAppContext, window: WindowHandle<IssueDetailPage>) {
        cx.update(|cx| {
            window
                .update(cx, |_, window, _| window.remove_window())
                .ok();
        });
    }

    pub(super) fn with_entry<R>(
        &self,
        cx: &mut TestAppContext,
        f: impl FnOnce(&crate::service::IssueEntry) -> R,
    ) -> R {
        cx.update(|cx| {
            f(self
                .state
                .read(cx)
                .issue_entry(REPO, NUMBER)
                .expect("the document"))
        })
    }

    pub(super) fn in_flight(&self, cx: &mut TestAppContext) -> bool {
        self.with_entry(cx, |e| e.books.in_flight)
    }

    pub(super) fn has_detail(&self, cx: &mut TestAppContext) -> bool {
        self.with_entry(cx, |e| e.detail.is_some())
    }

    pub(super) fn comments(&self, cx: &mut TestAppContext) -> usize {
        self.with_entry(cx, |e| {
            e.detail.as_ref().map_or(0, |d| d.comments.nodes.len())
        })
    }

    pub(super) fn stamped(&self, cx: &mut TestAppContext) -> bool {
        self.with_entry(cx, |e| e.detail_at.is_some())
    }

    pub(super) fn detail_reads(&self) -> usize {
        self.http.detail.reads.load(Ordering::SeqCst)
    }

    /// Age the document out, the way a read comes to be flying at all.
    pub(super) fn void_freshness(&self, cx: &mut TestAppContext) {
        cx.update(|cx| {
            self.state.update(cx, |s, _| {
                s.issues
                    .get_mut(&PrKey::new(REPO, NUMBER))
                    .expect("the document")
                    .detail_at = None;
            })
        });
    }

    /// The store half of a write confirming.
    pub(super) fn confirm_on_store(&self, cx: &mut TestAppContext, kind: IssuePendingKind) {
        cx.update(|cx| {
            self.state.update(cx, |s, _| {
                let token = s
                    .begin_issue_pending(REPO, NUMBER, kind)
                    .expect("no write pending");
                assert!(s.resolve_issue_pending(REPO, NUMBER, token));
            })
        });
    }

    pub(super) fn poke_store(&self, cx: &mut TestAppContext) {
        cx.update(|cx| self.state.update(cx, |_, cx| cx.notify()));
        cx.run_until_parked();
    }
}
