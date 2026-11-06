//! #803: drive a real page against a scripted transport and watch the
//! `ThreadDetail` requests (`If-None-Match`) across refreshes.

use std::sync::{Arc, Mutex};

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};

use crate::api;
use crate::drafts::DraftStore;
use crate::http::{Http, HttpError, HttpResult, Verb};
use crate::notification_page::NotificationDetailPage;
use crate::service::{AuthStatus, GithubCache, GithubState, Kind};
use crate::usage::{Origin, Surface, Trigger};

const ID: &str = "19";

struct Script {
    /// The etag and title the server currently holds.
    etag: String,
    title: String,
    /// Every thread GET's `If-None-Match`.
    seen: Vec<Option<String>>,
}

struct ThreadHttp(Mutex<Script>);

fn thread_json(title: &str) -> String {
    serde_json::json!({
        "id": ID, "unread": false,
        "subject": {"title": title, "type": "Issue"},
        "repository": {"full_name": "o/r"},
        "reason": "mention",
        "updated_at": "2026-09-01T00:00:00Z"
    })
    .to_string()
}

impl Http for ThreadHttp {
    fn request(
        &self,
        _: Verb,
        url: &str,
        _: Option<&str>,
        if_none_match: Option<&str>,
    ) -> Result<HttpResult, HttpError> {
        if !url.ends_with("/notifications/threads/19") {
            return Err(HttpError("not the lane under test".into()));
        }
        let mut s = self.0.lock().unwrap();
        s.seen.push(if_none_match.map(str::to_string));
        if if_none_match == Some(s.etag.as_str()) {
            return Ok(HttpResult {
                status: 304,
                etag: Some(s.etag.clone()),
                rate: None,
                body: None,
            });
        }
        Ok(HttpResult {
            status: 200,
            etag: Some(s.etag.clone()),
            rate: None,
            body: Some(thread_json(&s.title)),
        })
    }

    fn post_graphql(&self, _: &str, _: &serde_json::Value) -> Result<HttpResult, HttpError> {
        Err(HttpError("not the lane under test".into()))
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
    http: Arc<ThreadHttp>,
    state: Entity<GithubState>,
    page: Entity<NotificationDetailPage>,
}

impl Rig {
    /// Builds the page; its constructor fires the first thread GET, which
    /// is waited out before returning.
    fn new(cx: &mut TestAppContext, etag: &str) -> Self {
        cx.update(dashboard_core::status::init);
        cx.update(gpui_component::init);
        cx.update(|cx| cx.background_executor().allow_parking());
        let http = Arc::new(ThreadHttp(Mutex::new(Script {
            etag: etag.into(),
            title: "first".into(),
            seen: vec![],
        })));
        let client: Arc<dyn Http> = http.clone();
        let state = cx.update(|cx| {
            cx.new(|_| {
                let mut s = GithubState::new(
                    client,
                    Arc::new(NoStore),
                    Default::default(),
                    Default::default(),
                    GithubCache::new(dashboard_core::paths::AppPaths::init()),
                    DraftStore::at_tmp("thread-etag"),
                );
                s.auth = AuthStatus::Connected;
                s
            })
        });
        let seed: api::Notification = serde_json::from_str(&thread_json("seed")).unwrap();
        let st = state.clone();
        let (page, _): (_, &mut VisualTestContext) =
            cx.add_window_view(|window, cx| NotificationDetailPage::new(st, seed, window, cx));
        let rig = Self { http, state, page };
        rig.settle(cx);
        rig
    }

    fn settle(&self, cx: &mut TestAppContext) {
        // The read runs on a real background thread: wait it out.
        for _ in 0..200 {
            cx.run_until_parked();
            if !cx.update(|cx| self.page.read(cx).thread.is_loading()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the thread read never landed");
    }

    fn refresh(&self, cx: &mut TestAppContext) {
        cx.update(|cx| {
            self.page.update(cx, |p, cx| {
                p.fetch_thread(Origin::new(Surface::NotificationPage, Trigger::Refocus), cx)
            })
        });
        self.settle(cx);
    }

    fn stored(&self, cx: &mut TestAppContext) -> Option<String> {
        cx.update(|cx| self.state.read(cx).etag_for(Kind::ThreadDetail, Some(ID)))
    }

    fn title(&self, cx: &mut TestAppContext) -> String {
        cx.update(|cx| self.page.read(cx).effective().subject.title.clone())
    }

    fn take_seen(&self) -> Vec<Option<String>> {
        std::mem::take(&mut self.http.0.lock().unwrap().seen)
    }
}

/// The first load is unconditional and its 200 stores the etag.
#[gpui::test]
fn first_load_stores_the_etag(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, "W/\"t1\"");
    assert_eq!(rig.take_seen(), vec![None]);
    assert_eq!(rig.title(cx), "first");
    assert_eq!(rig.stored(cx).as_deref(), Some("W/\"t1\""));
}

/// A refresh sends the etag; the 304 keeps the shown thread and stamps
/// freshness.
#[gpui::test]
fn refresh_revalidates_and_a_304_keeps_the_thread(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, "W/\"t1\"");
    rig.take_seen();
    cx.update(|cx| {
        rig.page.update(cx, |p, _| {
            p.thread.backdate(std::time::Duration::from_secs(3600));
        })
    });
    rig.refresh(cx);
    assert_eq!(rig.take_seen(), vec![Some("W/\"t1\"".to_string())]);
    assert_eq!(rig.title(cx), "first", "the shown thread survives");
    assert_eq!(rig.stored(cx).as_deref(), Some("W/\"t1\""));
    cx.update(|cx| {
        let age = rig.page.read(cx).thread.age().expect("still ready");
        assert!(
            age < std::time::Duration::from_secs(60),
            "a 304 stamps freshness"
        );
    });
}

/// A changed thread answers 200 with a new etag: both are replaced.
#[gpui::test]
fn a_changed_thread_replaces_the_thread_and_the_etag(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, "W/\"t1\"");
    rig.take_seen();
    {
        let mut s = rig.http.0.lock().unwrap();
        s.etag = "W/\"t2\"".into();
        s.title = "second".into();
    }
    rig.refresh(cx);
    assert_eq!(rig.take_seen(), vec![Some("W/\"t1\"".to_string())]);
    assert_eq!(rig.title(cx), "second");
    assert_eq!(rig.stored(cx).as_deref(), Some("W/\"t2\""));
}
