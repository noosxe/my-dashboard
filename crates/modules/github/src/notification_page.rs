//! Notification detail page (docs/0026) — the PR/issue pages' third
//! sibling takeover: opened from a repo-view notification row (superseding
//! docs/0015's browser hand-off, which lives on in the header's ↗
//! button), it replaces the module content with the notification's
//! metadata — kind/reason badges, subject title, repo, age, read state —
//! plus the kind-aware web hand-off and, for Issue/PR subjects, the
//! `Open #n` links into the native pages (owner decision, docs/0026 §4.2).
//!
//! Read state (docs/0026 D3/D4, owner decisions §4.1/§4.3): the page
//! fires `PATCH /notifications/threads/{id}` the instant it starts
//! loading its contents — before the thread GET returns — and flips its
//! rendered state to Read optimistically. GitHub's API has no
//! mark-as-unread, so the only write is read-ward: a failed mark reverts
//! the dot and the in-page **Mark as read** button retries; a settled
//! mark asks the host to refetch the unread lists (`MarkSettled`).
//!
//! Data: the page paints instantly from the carried row and upgrades via
//! the page-owned `GET /notifications/threads/{id}` (`Kind::ThreadDetail`,
//! docs/0026 D2) — the 200 body parses here (the thread schema equals the
//! list item, §2.1); only non-200 results are forwarded to
//! `GithubState::apply` for the shared 401/403 handling. Session-only,
//! no cache.

use gpui::prelude::FluentBuilder as _;
use gpui::*;

use gpui_component::{
    ActiveTheme, Disableable, IconName, Sizable as _,
    badge::Badge,
    button::{Button, ButtonVariants},
    group_box::GroupBox,
    h_flex,
    spinner::Spinner,
    v_flex,
};

use crate::api;
use crate::page::{notification_url, relative_age};
use crate::service::{FetchSpec, GithubState, Kind, ThreadOp, run_spec_tracked};

actions!(github, [DismissNotificationPage]);

/// Events to the hosting `GithubPage` (docs/0026 D1/D3/D6).
#[derive(Clone)]
pub(crate) enum NotificationPageEvent {
    /// Esc — ask the host to close the page (takeover convention).
    CloseRequested,
    /// The open-time mark-read settled OK — the host refetches the repo +
    /// global unread lists (docs/0026 D3 refetch-after-mutation).
    MarkSettled,
    /// Issue/PR hand-off (docs/0026 D6, owner decision §4.2): the host
    /// closes this page and opens the native detail page with a
    /// synthesized seed row.
    OpenSubject {
        /// `true` = PullRequest, `false` = Issue.
        is_pr: bool,
        full_name: String,
        number: i64,
        title: String,
    },
}

/// The open-time mark lifecycle (docs/0026 D3): fired in `new`, settled
/// once — `Failed` arms the retry button until it lands or the thread
/// payload proves the thread was already read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MarkState {
    Idle,
    InFlight,
    Failed,
}

pub(crate) struct NotificationDetailPage {
    state: Entity<GithubState>,
    /// The row that opened the page — instant header paint while the
    /// thread GET is in flight (docs/0026 D2).
    seed: api::Notification,
    /// The full thread payload (docs/0026 §2.1) — `None` until the GET
    /// lands; the page degrades to the seed render when it never does.
    thread: Option<api::Notification>,
    /// The rendered read state (docs/0026 D3): starts from the row,
    /// flips to Read the instant the mark fires, reverts only on a
    /// failed mark. The thread GET can confirm Read, never undo it.
    read: bool,
    mark: MarkState,
    /// The thread GET in flight.
    loading: bool,
    load_error: Option<String>,
    focus_handle: FocusHandle,
}

impl Focusable for NotificationDetailPage {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl EventEmitter<NotificationPageEvent> for NotificationDetailPage {}

impl NotificationDetailPage {
    pub(crate) fn new(
        state: Entity<GithubState>,
        seed: api::Notification,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let _ = window;
        // docs/0026 D3/§4.3: the mark fires before anything renders — the
        // thread GET (fired right after) runs in parallel and may land
        // first or second; neither waits for the other.
        let read = !seed.unread;
        let mut page = Self {
            state,
            seed,
            thread: None,
            read,
            mark: MarkState::Idle,
            loading: false,
            load_error: None,
            focus_handle: cx.focus_handle(),
        };
        if !read {
            page.fire_mark_read(cx);
        }
        page.fetch_thread(cx);
        page
    }

    /// The page's thread id — the REST id the row already carried.
    fn thread_id(&self) -> &str {
        &self.seed.id
    }

    /// The effective payload for display: the fetched thread once it
    /// lands, the seed until then (docs/0026 D2 — the page is never
    /// blank while loading).
    fn effective(&self) -> &api::Notification {
        self.thread.as_ref().unwrap_or(&self.seed)
    }

    // --- network -------------------------------------------------------------

    fn mark_spec(&self) -> FetchSpec {
        FetchSpec {
            kind: Kind::ThreadMutation(ThreadOp::MarkRead),
            url: api::thread_url(self.thread_id()),
            etag: None,
            ident: Some(self.seed.id.clone()),
            max_pages: 1,
            variables: None,
        }
    }

    /// The auto-seen write (docs/0026 D3): PATCH, optimistic flip, one
    /// result — settled marks emit `MarkSettled` for the host refetch.
    fn fire_mark_read(&mut self, cx: &mut Context<Self>) {
        if !self.state.read(cx).auth.connected() {
            return; // the page is about to be torn down by the auth gate
        }
        let client = self.state.update(cx, |s, _| s.client.clone());
        let spec = self.mark_spec();
        log::debug!("thread-mutation: mark-read {}", self.thread_id());
        self.mark = MarkState::InFlight;
        self.read = true; // optimistic (D3)
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = run_spec_tracked(client, spec, cx).await;
            let _ = this.update(cx, |page, cx| page.apply_mark_result(result, cx));
        })
        .detach();
    }

    fn apply_mark_result(&mut self, result: crate::service::SpecResult, cx: &mut Context<Self>) {
        if self.mark != MarkState::InFlight {
            return; // a retry superseded this round
        }
        if result.network.is_none() && (200..300).contains(&result.status) {
            log::info!("thread-mutation: mark-read settled ({})", result.status);
            self.mark = MarkState::Idle;
            self.read = true;
            // Refetch-after-mutation (D3): the host owns the window, so
            // it runs the specs on this event (created-issue precedent).
            cx.emit(NotificationPageEvent::MarkSettled);
        } else {
            if result.network.is_none() {
                // Shared 401/403/rate-limit handling runs for real HTTP
                // failures; network errors stay page-local (detail-page
                // convention, docs/0020 D2).
                let status = result.status;
                self.state.update(cx, |s, cx| {
                    s.apply(result);
                    cx.notify();
                });
                log::warn!("thread-mutation: mark-read failed (HTTP {status})");
            } else {
                log::warn!("thread-mutation: mark-read failed (network)");
            }
            // Revert the optimistic flip (D3); the button arms as retry.
            self.mark = MarkState::Failed;
            self.read = false;
        }
        cx.notify();
    }

    fn thread_spec(&self) -> FetchSpec {
        FetchSpec {
            kind: Kind::ThreadDetail,
            url: api::thread_url(self.thread_id()),
            etag: None,
            ident: Some(self.seed.id.clone()),
            max_pages: 1,
            variables: None,
        }
    }

    fn fetch_thread(&mut self, cx: &mut Context<Self>) {
        if !self.state.read(cx).auth.connected() {
            return;
        }
        let client = self.state.update(cx, |s, _| s.client.clone());
        let spec = self.thread_spec();
        log::debug!("thread-detail: fetching {}", self.thread_id());
        self.loading = true;
        self.load_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = run_spec_tracked(client, spec, cx).await;
            let _ = this.update(cx, |page, cx| page.apply_thread_result(result, cx));
        })
        .detach();
    }

    fn apply_thread_result(&mut self, result: crate::service::SpecResult, cx: &mut Context<Self>) {
        self.loading = false;
        if let Some(m) = &result.network {
            self.load_error = Some(format!("Network error: {m}"));
        } else if result.status != 200 {
            // Shared auth/rate-limit handling (page-owned convention).
            let status = result.status;
            self.state.update(cx, |s, cx| {
                s.apply(result);
                cx.notify();
            });
            self.load_error = Some(format!("GitHub answered HTTP {status}"));
        } else if let Some(body) = result.body {
            match serde_json::from_str::<api::Thread>(&body) {
                Ok(thread) => {
                    log::info!(
                        "thread-detail applied: {} — {}",
                        thread.id,
                        if thread.unread { "unread" } else { "read" }
                    );
                    // docs/0026 D3: the payload can confirm Read, never
                    // undo an in-flight/succeeded optimistic mark — the
                    // GET may simply have raced the PATCH server-side.
                    if !thread.unread {
                        self.read = true;
                    }
                    self.thread = Some(thread);
                }
                Err(e) => self.load_error = Some(format!("thread payload: {e}")),
            }
        }
        cx.notify();
    }

    // --- intents -------------------------------------------------------------

    fn request_close(&mut self, cx: &mut Context<Self>) {
        cx.emit(NotificationPageEvent::CloseRequested);
    }

    /// The Issue/PR hand-off (docs/0026 D6): parse the subject's API URL;
    /// other kinds never emit — the ↗ button is their whole hand-off.
    fn open_subject(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let subject = &self.effective().subject;
        let Some((full_name, number)) = api::subject_handoff(&subject.kind, subject.url.as_deref())
        else {
            return;
        };
        cx.emit(NotificationPageEvent::OpenSubject {
            is_pr: subject.kind == "PullRequest",
            full_name,
            number,
            title: subject.title.clone(),
        });
    }

    // --- render ---------------------------------------------------------------

    /// The ↗ target (docs/0026 D5): kind-aware derivation from the
    /// subject's API URL, falling back to the repo's notifications page
    /// (docs/0015 target) whenever no faithful web page exists.
    fn web_url(&self) -> String {
        let subject = &self.effective().subject;
        subject
            .url
            .as_deref()
            .and_then(api::web_url)
            .unwrap_or_else(|| notification_url(&self.seed.repository.full_name))
    }

    /// Whether the subject hands off in-app (docs/0026 D6).
    fn subject_handoff(&self) -> Option<(String, i64)> {
        let subject = &self.effective().subject;
        api::subject_handoff(&subject.kind, subject.url.as_deref())
    }

    fn render_header(&self, cx: &Context<Self>) -> AnyElement {
        let n = self.effective();
        let repo = if self.thread.is_some() {
            n.repository.full_name.clone()
        } else {
            self.seed.repository.full_name.clone()
        };
        h_flex()
            .id("notification-header")
            .justify_between()
            .gap_3()
            .items_start()
            .child(
                v_flex()
                    .gap_1()
                    .min_w_0()
                    .flex_1()
                    // Badge row: kind (raw GitHub type), reason, state.
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .flex_wrap()
                            .child(Badge::new().xsmall().child(n.subject.kind.clone()))
                            .when_some(n.reason.as_deref(), |row, reason| {
                                row.child(
                                    Badge::new()
                                        .xsmall()
                                        .color(cx.theme().muted_foreground)
                                        .child(reason.to_string()),
                                )
                            })
                            .child(if self.read {
                                Badge::new()
                                    .xsmall()
                                    .color(cx.theme().muted_foreground)
                                    .child("Read")
                            } else {
                                Badge::new()
                                    .xsmall()
                                    .color(cx.theme().warning)
                                    .child("Unread")
                            })
                            .when(self.loading || self.mark == MarkState::InFlight, |row| {
                                row.child(Spinner::new().xsmall())
                            }),
                    )
                    // The subject title — a link into the native page for
                    // Issue/PR subjects (docs/0026 D6), plain text
                    // otherwise (their only hand-off is the ↗ button).
                    .child(match self.subject_handoff() {
                        Some((_, number)) => div()
                            .id("notification-subject-link")
                            .debug_selector(|| "notification-subject-link".to_string())
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().accent)
                            .cursor_pointer()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_subject(window, cx)),
                            )
                            .child(format!("{}  #{}", n.subject.title, number)),
                        None => div()
                            .id("notification-subject-text")
                            .debug_selector(|| "notification-subject-text".to_string())
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(n.subject.title.clone()),
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(repo)
                            .child(relative_age(n.updated_at.as_deref())),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    // Mark as read: hidden while Read (hidden-not-
                    // disabled) — visible as the auto-mark's retry (D4).
                    .when(!self.read, |row| {
                        row.child(
                            Button::new("notification-mark-read")
                                .debug_selector(|| "notification-mark-read".to_string())
                                .ghost()
                                .small()
                                .label("Mark as read")
                                .tooltip("Mark this notification as read")
                                .disabled(self.mark == MarkState::InFlight)
                                .on_click(cx.listener(|this, _, _, cx| this.fire_mark_read(cx))),
                        )
                    })
                    .child(
                        Button::new("notification-refresh")
                            .ghost()
                            .small()
                            .icon(IconName::RotateCw)
                            .tooltip("Refresh")
                            .on_click(cx.listener(|this, _, _, cx| this.fetch_thread(cx))),
                    )
                    .child(
                        Button::new("notification-open-github")
                            .debug_selector(|| "notification-open-github".to_string())
                            .ghost()
                            .small()
                            .icon(IconName::ExternalLink)
                            .tooltip("Open on GitHub")
                            .on_click({
                                let url = self.web_url();
                                move |_, _, cx| cx.open_url(&url)
                            }),
                    ),
            )
            .into_any_element()
    }

    /// The metadata card (docs/0026 §3 D1: "metadata + ↗ is the whole
    /// page" for non-Issue/PR kinds) — label/value rows from the
    /// effective payload, never blank while the GET is in flight.
    fn render_meta(&self, cx: &Context<Self>) -> AnyElement {
        let n = self.effective();
        GroupBox::new()
            .id("notification-meta")
            .title("Notification")
            .child(
                v_flex()
                    .gap_1()
                    .text_sm()
                    .child(meta_row(
                        "notification-meta-subject",
                        "Subject",
                        &n.subject.title,
                        cx,
                    ))
                    .child(meta_row(
                        "notification-meta-repo",
                        "Repository",
                        &n.repository.full_name,
                        cx,
                    ))
                    .child(meta_row(
                        "notification-meta-type",
                        "Type",
                        &n.subject.kind,
                        cx,
                    ))
                    .child(meta_row(
                        "notification-meta-reason",
                        "Reason",
                        n.reason.as_deref().unwrap_or("—"),
                        cx,
                    ))
                    .child(meta_row(
                        "notification-meta-state",
                        "State",
                        if self.read { "Read" } else { "Unread" },
                        cx,
                    ))
                    .child(meta_row(
                        "notification-meta-updated",
                        "Updated",
                        &relative_age(n.updated_at.as_deref()),
                        cx,
                    )),
            )
            .into_any_element()
    }
}

/// One label/value row of the metadata card — pure shape, muted label.
fn meta_row(
    id: &'static str,
    label: &str,
    value: &str,
    cx: &Context<NotificationDetailPage>,
) -> AnyElement {
    h_flex()
        .id(id)
        .debug_selector(move || id.to_string())
        .gap_2()
        .child(
            div()
                .w(px(96.))
                .flex_none()
                .text_color(cx.theme().muted_foreground)
                .child(label.to_string()),
        )
        .child(div().min_w_0().child(value.to_string()))
        .into_any_element()
}

impl Render for NotificationDetailPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("notification-detail")
            .key_context("GithubNotificationPage")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &DismissNotificationPage, _, cx| {
                this.request_close(cx);
            }))
            .size_full()
            .min_w_0()
            .p_4()
            .gap_3()
            .child(self.render_header(cx))
            .when_some(self.load_error.clone(), |col, err| {
                col.child(
                    div()
                        .id("notification-load-error")
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(err),
                )
            })
            .child(self.render_meta(cx))
    }
}

#[cfg(test)]
mod tests {
    use gpui::AppContext as _;

    use crate::api;
    use crate::http::{HttpError, HttpResult, Verb};
    use crate::service::{AuthStatus, GithubState};

    use super::{MarkState, NotificationDetailPage, NotificationPageEvent};

    /// Verb-routed transport: PATCH answers `patch_status`, everything
    /// else answers the thread GET shape. Deterministic — no script
    /// ordering races between the page's two parallel requests.
    struct LaneMock {
        patch_failures: std::sync::atomic::AtomicUsize,
        get_status: u16,
        get_body: String,
        verbs: std::sync::Mutex<Vec<Verb>>,
    }

    impl LaneMock {
        fn verbs(&self) -> Vec<Verb> {
            self.verbs.lock().unwrap().clone()
        }
    }

    impl crate::http::Http for LaneMock {
        fn request(
            &self,
            verb: Verb,
            _: &str,
            _: Option<&str>,
            _: Option<&str>,
        ) -> Result<HttpResult, HttpError> {
            self.verbs.lock().unwrap().push(verb);
            let (status, body) = match verb {
                Verb::Patch => {
                    let left = self
                        .patch_failures
                        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    (if left > 0 { 500 } else { 205 }, None)
                }
                _ => (self.get_status, Some(self.get_body.clone())),
            };
            Ok(HttpResult {
                status,
                etag: None,
                rate: None,
                body,
            })
        }

        fn post_graphql(&self, _: &str, _: &serde_json::Value) -> Result<HttpResult, HttpError> {
            Err(HttpError("probe: no graphql".into()))
        }
    }

    fn thread_payload(kind: &str, url: Option<&str>, unread: bool) -> String {
        let subject = match url {
            Some(u) => serde_json::json!({"title": "Fix the flume", "type": kind, "url": u}),
            None => serde_json::json!({"title": "Fix the flume", "type": kind}),
        };
        serde_json::json!({
            "id": "19", "unread": unread,
            "subject": subject,
            "repository": {"full_name": "o/r"},
            "reason": "review_requested",
            "updated_at": "2026-09-01T00:00:00Z"
        })
        .to_string()
    }

    fn probe_seed(unread: bool, kind: &str, url: Option<&str>) -> api::Notification {
        let subject = match url {
            Some(u) => serde_json::json!({"title": "Fix the flume", "type": kind, "url": u}),
            None => serde_json::json!({"title": "Fix the flume", "type": kind}),
        };
        serde_json::from_value(serde_json::json!({
            "id": "19", "unread": unread,
            "subject": subject,
            "repository": {"full_name": "o/r"},
            "reason": "review_requested",
            "updated_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap()
    }

    fn lane_mock(
        patch_failures: usize,
        get_status: u16,
        thread: String,
    ) -> std::sync::Arc<LaneMock> {
        std::sync::Arc::new(LaneMock {
            patch_failures: std::sync::atomic::AtomicUsize::new(patch_failures),
            get_status,
            get_body: thread,
            verbs: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// A connected state over the verb-routed mock + the page built on
    /// it. Requests fire in `new` (docs/0026 §4.3) — everything after
    /// this runs on the returned `VisualTestContext` (it parks itself
    /// once before returning).
    fn probe_page(
        cx: &mut gpui::TestAppContext,
        seed: api::Notification,
        mock: std::sync::Arc<LaneMock>,
    ) -> (
        gpui::Entity<NotificationDetailPage>,
        &mut gpui::VisualTestContext,
    ) {
        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let state = cx.update(|cx| {
            let store: std::sync::Arc<dyn crate::auth::TokenStore> =
                std::sync::Arc::new(crate::auth::MockTokenStore::with("t0k".into()));
            let entity = cx.new(|_| {
                GithubState::new(
                    mock.clone(),
                    store,
                    Default::default(),
                    Default::default(),
                    crate::service::GithubCache::new(dashboard_core::paths::AppPaths::init()),
                )
            });
            entity.update(cx, |s, _| s.auth = AuthStatus::Connected);
            entity
        });
        cx.add_window_view(|window, cx| NotificationDetailPage::new(state, seed, window, cx))
    }

    #[gpui::test]
    fn mark_fires_before_thread_get_and_flips_read(cx: &mut gpui::TestAppContext) {
        // docs/0026 D3/§4.3: the PATCH is the page's first act — fired
        // before the thread GET — and the dot flips optimistically.
        let mock = lane_mock(0, 200, thread_payload("PullRequest", Some("u"), false));
        let (page, vcx) = probe_page(cx, probe_seed(true, "PullRequest", Some("u")), mock.clone());
        vcx.run_until_parked();
        assert_eq!(mock.verbs(), vec![Verb::Patch, Verb::Get]);
        let (read, mark) = vcx.read(|cx| {
            let p = page.read(cx);
            (p.read, p.mark)
        });
        assert!(read);
        assert_eq!(mark, MarkState::Idle);
    }

    #[gpui::test]
    fn already_read_row_never_marks(cx: &mut gpui::TestAppContext) {
        // docs/0026 D3: only an unread row fires the auto-mark; a read
        // row just upgrades via the thread GET.
        let mock = lane_mock(0, 200, thread_payload("Issue", Some("u"), false));
        let (page, vcx) = probe_page(cx, probe_seed(false, "Issue", Some("u")), mock.clone());
        vcx.run_until_parked();
        assert_eq!(mock.verbs(), vec![Verb::Get]);
        assert!(vcx.read(|cx| page.read(cx).read));
    }

    #[gpui::test]
    fn failed_mark_reverts_and_the_button_retries(cx: &mut gpui::TestAppContext) {
        // docs/0026 D4: a failed auto-mark reverts the optimistic flip
        // and arms the in-page Mark-as-read button as the retry.
        let mock = lane_mock(1, 200, thread_payload("PullRequest", Some("u"), true));
        let (page, vcx) = probe_page(cx, probe_seed(true, "PullRequest", Some("u")), mock.clone());
        vcx.run_until_parked();
        let (read, mark) = vcx.read(|cx| {
            let p = page.read(cx);
            (p.read, p.mark)
        });
        assert!(!read, "optimistic flip must revert on failure");
        assert_eq!(mark, MarkState::Failed);
        // Thread GET landed fine — no load error, and the retry button
        // is visible (hidden-not-disabled while Read).
        assert!(vcx.read(|cx| page.read(cx).load_error.is_none()));
        assert!(vcx.debug_bounds("notification-mark-read").is_some());
        // Real click on the retry button.
        let button = vcx
            .debug_bounds("notification-mark-read")
            .expect("retry button visible");
        vcx.simulate_click(button.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
        let (read, mark) = vcx.read(|cx| {
            let p = page.read(cx);
            (p.read, p.mark)
        });
        assert!(read);
        assert_eq!(mark, MarkState::Idle);
        assert_eq!(mock.verbs(), vec![Verb::Patch, Verb::Get, Verb::Patch]);
    }

    #[gpui::test]
    fn optimistic_read_survives_a_racing_stale_get(cx: &mut gpui::TestAppContext) {
        // docs/0026 D3: the thread GET may race the PATCH server-side and
        // still say unread — it can confirm Read, never undo the mark.
        let mock = lane_mock(0, 200, thread_payload("Issue", Some("u"), true));
        let (page, vcx) = probe_page(cx, probe_seed(true, "Issue", Some("u")), mock);
        vcx.run_until_parked();
        assert!(vcx.read(|cx| page.read(cx).read));
    }

    #[gpui::test]
    fn thread_get_failure_degrades_to_seed(cx: &mut gpui::TestAppContext) {
        // docs/0026 D2: a failed thread GET keeps the seed-rendered page
        // (degrade, stderr — no dialog, no blank page).
        let mock = lane_mock(0, 500, thread_payload("Issue", Some("u"), false));
        let (page, vcx) = probe_page(cx, probe_seed(true, "Issue", Some("u")), mock);
        vcx.run_until_parked();
        let (err, thread) = vcx.read(|cx| {
            let p = page.read(cx);
            (p.load_error.clone(), p.thread.is_some())
        });
        assert!(err.is_some());
        assert!(!thread);
        // The seed still paints the metadata card and the subject.
        assert!(vcx.debug_bounds("notification-meta-state").is_some());
        assert!(vcx.debug_bounds("notification-subject-text").is_some());
    }

    #[gpui::test]
    fn handoff_link_only_for_issue_and_pr_subjects(cx: &mut gpui::TestAppContext) {
        // docs/0026 D6: Issue/PR subjects carry the in-app link; other
        // kinds keep a plain title (their hand-off is the ↗ button).
        let (issue_page, vcx) = probe_page(
            cx,
            probe_seed(
                false,
                "Issue",
                Some("https://api.github.com/repos/o/r/issues/7"),
            ),
            lane_mock(
                0,
                200,
                thread_payload(
                    "Issue",
                    Some("https://api.github.com/repos/o/r/issues/7"),
                    false,
                ),
            ),
        );
        vcx.run_until_parked();
        assert!(
            vcx.debug_bounds("notification-subject-link").is_some(),
            "issue subject must render as a link"
        );
        // Capture the OpenSubject event through a real click on it.
        let handed = std::rc::Rc::new(std::cell::RefCell::new(None));
        let sink = handed.clone();
        vcx.update(|window, cx| {
            cx.subscribe(&issue_page, move |_, ev, _| {
                let ev = ev.clone();
                if let NotificationPageEvent::OpenSubject {
                    is_pr,
                    full_name,
                    number,
                    ..
                } = ev
                {
                    *sink.borrow_mut() = Some((is_pr, full_name, number));
                }
            })
            .detach();
            // Drive the hand-off through open_subject — the click path
            // runs this same listener.
            issue_page.update(cx, |p, cx| p.open_subject(window, cx));
        });
        vcx.run_until_parked();
        let handed = handed.borrow().clone();
        assert_eq!(handed, Some((false, "o/r".into(), 7)));
        // A Release subject: plain title, no in-app link.
        let (release_page, vcx) = probe_page(
            cx,
            probe_seed(false, "Release", None),
            lane_mock(0, 200, thread_payload("Release", None, false)),
        );
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("notification-subject-link").is_none());
        assert!(vcx.debug_bounds("notification-subject-text").is_some());
        assert!(vcx.read(|cx| release_page.read(cx).subject_handoff().is_none()));
    }
}
