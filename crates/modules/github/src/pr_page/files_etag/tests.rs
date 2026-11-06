//! #750: drive a real page against a scripted transport and watch the
//! `PrFiles` requests (URL + `If-None-Match`) across refreshes.

use std::sync::{Arc, Mutex};

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};

use crate::drafts::DraftStore;
use crate::http::{Http, HttpError, HttpResult, Verb};
use crate::pr_page::{PrDetailPage, PrTab};
use crate::service::{AuthStatus, GithubCache, GithubState, Kind};
use crate::usage::{Origin, Surface, Trigger};

const REPO: &str = "o/r";
const NUMBER: i64 = 7;
const IDENT: &str = "o/r#7";

/// The server side of the scripted walk.
struct Script {
    /// Page-1 etag the server currently holds.
    page1_etag: String,
    /// Files per page (page 1 is full, so the walk continues).
    pages: Vec<usize>,
    /// Every files request: (page, if-none-match).
    seen: Vec<(u32, Option<String>)>,
}

struct FilesHttp(Mutex<Script>);

fn files_json(n: usize, tag: &str) -> String {
    let items: Vec<String> = (0..n)
        .map(|i| {
            format!(
                r#"{{"filename":"{tag}/f{i}.rs","status":"modified","additions":1,"deletions":0,"changes":1}}"#
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}

impl Http for FilesHttp {
    fn request(
        &self,
        _: Verb,
        url: &str,
        _: Option<&str>,
        if_none_match: Option<&str>,
    ) -> Result<HttpResult, HttpError> {
        if !url.contains("/pulls/7/files") {
            return Err(HttpError("not the lane under test".into()));
        }
        let mut s = self.0.lock().unwrap();
        let page = (2..=9)
            .find(|p| url.contains(&format!("&page={p}")))
            .unwrap_or(1);
        s.seen.push((page, if_none_match.map(str::to_string)));
        if page == 1 && if_none_match == Some(s.page1_etag.as_str()) {
            return Ok(HttpResult {
                status: 304,
                etag: Some(s.page1_etag.clone()),
                rate: None,
                body: None,
            });
        }
        let n = s.pages[page as usize - 1];
        // Later pages carry their own, different etags.
        let etag = if page == 1 {
            s.page1_etag.clone()
        } else {
            format!("W/\"page{page}\"")
        };
        Ok(HttpResult {
            status: 200,
            etag: Some(etag),
            rate: None,
            body: Some(files_json(n, "src")),
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
    http: Arc<FilesHttp>,
    state: Entity<GithubState>,
    page: Entity<PrDetailPage>,
}

impl Rig {
    fn new(cx: &mut TestAppContext, etag: &str, pages: Vec<usize>) -> Self {
        cx.update(dashboard_core::status::init);
        cx.update(gpui_component::init);
        cx.update(|cx| cx.background_executor().allow_parking());
        let http = Arc::new(FilesHttp(Mutex::new(Script {
            page1_etag: etag.into(),
            pages,
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
                    DraftStore::at_tmp("pr-files-etag"),
                );
                s.auth = AuthStatus::Connected;
                s
            })
        });
        let st = state.clone();
        let (page, _): (_, &mut VisualTestContext) = cx.add_window_view(|window, cx| {
            let mut p = PrDetailPage::new(st, REPO.into(), NUMBER, None, window, cx);
            p.tab = PrTab::Files;
            p
        });
        Self { http, state, page }
    }

    fn refresh(&self, cx: &mut TestAppContext) {
        cx.update(|cx| {
            self.page.update(cx, |p, cx| {
                p.fetch_files(Origin::new(Surface::PrPage, Trigger::Expand), cx)
            })
        });
        // The walk runs on a real background thread: wait it out.
        for _ in 0..200 {
            cx.run_until_parked();
            if !cx.update(|cx| self.page.read(cx).files.is_loading()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the files walk never landed");
    }

    fn stored(&self, cx: &mut TestAppContext) -> Option<String> {
        cx.update(|cx| self.state.read(cx).etag_for(Kind::PrFiles, Some(IDENT)))
    }

    fn files_len(&self, cx: &mut TestAppContext) -> usize {
        cx.update(|cx| self.page.read(cx).files.data().map_or(0, Vec::len))
    }

    fn take_seen(&self) -> Vec<(u32, Option<String>)> {
        std::mem::take(&mut self.http.0.lock().unwrap().seen)
    }
}

/// First load walks both pages and stores *page 1's* etag, not the last
/// page's.
#[gpui::test]
fn first_load_stores_the_page_one_etag(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, "W/\"e1\"", vec![100, 3]);
    rig.refresh(cx);
    assert_eq!(rig.take_seen(), vec![(1, None), (2, None)]);
    assert_eq!(rig.files_len(cx), 103);
    assert_eq!(rig.stored(cx).as_deref(), Some("W/\"e1\""));
}

/// A refresh sends the etag; the 304 makes no page-2 request, keeps the
/// list and counts as fresh.
#[gpui::test]
fn refresh_revalidates_and_a_304_keeps_the_list(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, "W/\"e1\"", vec![100, 3]);
    rig.refresh(cx);
    rig.take_seen();

    cx.update(|cx| {
        rig.page.update(cx, |p, _| {
            // Age the landing so a freshness bump is observable.
            p.files.backdate(std::time::Duration::from_secs(3600));
        })
    });
    rig.refresh(cx);
    assert_eq!(rig.take_seen(), vec![(1, Some("W/\"e1\"".to_string()))]);
    assert_eq!(rig.files_len(cx), 103, "the cached list survives");
    assert_eq!(rig.stored(cx).as_deref(), Some("W/\"e1\""));
    cx.update(|cx| {
        let age = rig.page.read(cx).files.age().expect("still ready");
        assert!(
            age < std::time::Duration::from_secs(60),
            "a 304 stamps freshness"
        );
    });
}

/// A changed list answers 200: the list and the etag are replaced.
#[gpui::test]
fn a_changed_list_replaces_the_list_and_the_etag(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, "W/\"e1\"", vec![100, 3]);
    rig.refresh(cx);
    rig.take_seen();

    {
        let mut s = rig.http.0.lock().unwrap();
        s.page1_etag = "W/\"e2\"".into();
        s.pages = vec![100, 5];
    }
    rig.refresh(cx);
    assert_eq!(
        rig.take_seen(),
        vec![(1, Some("W/\"e1\"".to_string())), (2, None)]
    );
    assert_eq!(rig.files_len(cx), 105);
    assert_eq!(rig.stored(cx).as_deref(), Some("W/\"e2\""));
}
