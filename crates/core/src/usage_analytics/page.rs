//! The usage page (docs/0082 §4; shell-hosted, docs/0054 D1): a fixed
//! header — view switch, range presets, refresh and the honesty line
//! (D3) — above one scrolling view body.
//!
//! Code layout (0082 D15a): this file owns the page state, the read
//! lifecycle and the render composition; each concern lives next door —
//! [`header`], [`range`], [`honesty`], [`states`], [`layout`] — and
//! each view has its own module ([`budget`], [`requests`]).

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, Context, FocusHandle, Render, Task, Window};

use super::surfaces::{self, Bounds, Budget, Preset};
use crate::usage_records::{self, UsageRecord};

mod budget;
mod header;
mod honesty;
mod layout;
mod range;
mod requests;
mod states;

use honesty::Honesty;
use range::{BUDGET_MODULE, DEFAULT_PRESET, READ_SPAN};

/// The page's two views (D1), one per audience: Budget for the owner as
/// a user (GitHub's reporting), Requests for the owner as the
/// dashboard's developer (this app's records) — source follows audience
/// (0082 §4). The choice is component-local, never nav state, and not
/// persisted: the page always opens on Budget. Switching keeps the
/// frozen `as_of` and the range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageView {
    Budget,
    Requests,
}

impl UsageView {
    pub fn label(self) -> &'static str {
        match self {
            UsageView::Budget => "Budget",
            UsageView::Requests => "Requests",
        }
    }

    /// Switch order (D1): Budget first, then Requests.
    pub fn all() -> [UsageView; 2] {
        [UsageView::Budget, UsageView::Requests]
    }
}

/// The page's read state (D11 gate, D18). The 90-day read measured
/// above one frame, so it runs on the background executor; `Loading` is
/// the designed state while it is in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadState {
    Loading,
    Ready,
}

/// The honesty line's inputs that scan the rows (D3), derived once per
/// read and per control change — never per frame: with the 90-day read
/// that is ~10⁵ rows, and hovers and focus changes re-render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Derived {
    /// Requests inside the active view's bounds.
    requests: usize,
    /// The oldest retained record in the read.
    oldest_ms: Option<u64>,
    /// The selected budget's open window at `as_of` (Q5), if any.
    current_window: Option<surfaces::CurrentWindow>,
}

/// State discipline (0054 D10): `as_of_ms` moves — and the store is
/// read — on first open and on refresh only. Every control change is a
/// pure recomputation over `rows`.
pub struct UsagePage {
    /// The freeze: "now, for the data on screen". Every window in every
    /// view derives from it; only [`UsagePage::refresh`] (and first
    /// open) move it. Post-freeze arrivals have `ts >= as_of_ms` and
    /// stay invisible until refresh.
    as_of_ms: u64,
    /// The one bounded read per refresh (D11): rows in
    /// `[as_of − 90 d, as_of)` — the widest preset, so any range is a
    /// re-slice of the same fetched data. Shared with the views, never
    /// copied per view.
    rows: Arc<Vec<UsageRecord>>,
    /// Whether the last read found the store degraded (docs/0057 D6):
    /// the honesty line says history is paused (D18).
    degraded: bool,
    read: ReadState,
    /// Drops a superseded read: the newest refresh wins.
    read_gen: u64,
    _read_task: Option<Task<()>>,
    /// The active view (D1), Budget by default.
    view: UsageView,
    /// The page-global range preset (D11), 24h by default.
    preset: Preset,
    /// The budget `Current window` follows (Q5). Until #697's Budget
    /// selector lands it is the D7 default: the budget with the smaller
    /// remaining fraction, re-derived on every read.
    budget: Budget,
    /// D19: the view switch and the range switch, one Tab stop each.
    /// `None` only in pure-derivation tests.
    focus: Option<(FocusHandle, FocusHandle)>,
    /// Kept in step with rows, view, preset and budget by
    /// [`UsagePage::derive`]; `render` only reads it.
    derived: Derived,
}

impl UsagePage {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut page = Self::frozen_at(
            // docs/0081 §12.1: pinned under DASHBOARD_FIXED_NOW.
            crate::freshness::clock_epoch_ms(),
            Vec::new(),
        );
        page.read = ReadState::Loading;
        page.focus = Some((
            cx.focus_handle().tab_stop(true),
            cx.focus_handle().tab_stop(true),
        ));
        // One load on first open (D10): the page never opens empty
        // demanding a click.
        page.reload(cx);
        page
    }

    /// A page over already-read `rows` at `as_of_ms`, with the page
    /// defaults (Budget, 24h) and no focus handles.
    fn frozen_at(as_of_ms: u64, rows: Vec<UsageRecord>) -> Self {
        let mut page = Self {
            as_of_ms,
            rows: Arc::new(rows),
            degraded: false,
            read: ReadState::Ready,
            read_gen: 0,
            _read_task: None,
            view: UsageView::Budget,
            preset: DEFAULT_PRESET,
            budget: Budget::Rest,
            focus: None,
            derived: Derived::default(),
        };
        page.derive();
        page
    }

    /// The bounded read, half-open on both ends — the upper bound is
    /// what makes the freeze real (0054 D10). Off the UI thread (0082
    /// D11's gate): the page shows `Loading` until the rows land.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let from = self.as_of_ms.saturating_sub(READ_SPAN.ms());
        let to = self.as_of_ms;
        let source = usage_records::source(cx);
        self.read_gen += 1;
        let generation = self.read_gen;
        self.read = ReadState::Loading;
        self._read_task = Some(cx.spawn(async move |this, cx| {
            let (rows, degraded) = cx
                .background_spawn(async move {
                    (source.records_between(from, to), source.is_degraded())
                })
                .await;
            this.update(cx, |page, cx| {
                if page.read_gen == generation {
                    page.apply_read(rows, degraded);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Land a finished read: the rows, the degraded flag and the budget
    /// `Current window` follows.
    fn apply_read(&mut self, rows: Vec<UsageRecord>, degraded: bool) {
        self.rows = Arc::new(rows);
        self.degraded = degraded;
        self.read = ReadState::Ready;
        self.budget = surfaces::budget_cards(
            &self.rows,
            &surfaces::BudgetCardsQuery {
                module: BUDGET_MODULE.into(),
                as_of_ms: self.as_of_ms,
                preset: Preset::CurrentWindow,
                // The per-budget pause reaches the page with #699.
                rest_paused_until_ms: None,
                graphql_paused_until_ms: None,
            },
        )
        .default_budget;
        self.derive();
    }

    /// The refresh button's action (0054 D10): the ONLY path that moves
    /// `as_of` or touches the store after first open. Stamps a new
    /// freeze and re-reads the window. This is not a fetch: no `Http`,
    /// no specs.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.as_of_ms = crate::freshness::clock_epoch_ms();
        self.reload(cx);
    }

    fn set_view(&mut self, view: UsageView, cx: &mut Context<Self>) {
        self.select(view, self.preset);
        cx.notify();
    }

    fn set_preset(&mut self, preset: Preset, cx: &mut Context<Self>) {
        self.select(self.view, preset);
        cx.notify();
    }

    /// A control change (D10): pure recomputation over the frozen rows.
    fn select(&mut self, view: UsageView, preset: Preset) {
        self.view = view;
        self.preset = preset;
        self.derive();
    }

    /// Refresh the cached honesty inputs from rows, view, preset and
    /// budget. Every path that changes one of them ends here.
    fn derive(&mut self) {
        self.derived = self.derive_inputs();
    }

    /// The honesty inputs, computed from scratch (the full row scans).
    fn derive_inputs(&self) -> Derived {
        let current_window =
            surfaces::current_window(&self.rows, BUDGET_MODULE, self.budget, self.as_of_ms);
        let requests = self.bounds().map_or(0, |b| {
            self.rows
                .iter()
                .filter(|r| r.ts_epoch_ms >= b.from_ms && r.ts_epoch_ms < b.to_ms)
                .count()
        });
        Derived {
            requests,
            oldest_ms: self.rows.iter().map(|r| r.ts_epoch_ms).min(),
            current_window,
        }
    }

    /// The preset the active view reads (D11).
    fn effective_preset(&self) -> Preset {
        range::effective_preset(self.view, self.preset)
    }

    /// The frozen interval the active view derives from; `None` for
    /// `Current window` without an open window.
    fn bounds(&self) -> Option<Bounds> {
        range::bounds(
            &self.rows,
            self.effective_preset(),
            self.budget,
            self.as_of_ms,
        )
    }

    /// The honesty line's state: cached inputs only, no row scan.
    fn honesty(&self) -> Honesty {
        let budget_window = (self.effective_preset() == Preset::CurrentWindow)
            .then_some((self.budget, self.derived.current_window));
        Honesty {
            loading: self.read == ReadState::Loading,
            as_of_ms: self.as_of_ms,
            requests: self.derived.requests,
            budget_window,
            oldest_ms: self.derived.oldest_ms,
        }
    }

    /// The degraded chip shows once a read has landed (D18).
    fn degraded_shown(&self) -> bool {
        self.degraded && self.read == ReadState::Ready
    }
}

impl Render for UsagePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(window, cx);
        let body: AnyElement = if self.read == ReadState::Loading {
            states::loading().into_any_element()
        } else if self.rows.is_empty() {
            states::page_empty(self.view, cx).into_any_element()
        } else {
            match self.view {
                UsageView::Budget => budget::body(cx).into_any_element(),
                UsageView::Requests => requests::body(cx).into_any_element(),
            }
        };
        layout::frame(header, body, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_analytics::surfaces::Span;
    use crate::usage_records::{MemorySink, Transport, UsageStore, init_with, sink};
    use gpui::TestAppContext;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A freeze far past every range span.
    const AS_OF: u64 = 10_000_000_000;
    const MIN: u64 = 60_000;

    fn rec(ts: u64) -> UsageRecord {
        let mut r = UsageRecord::new("github", "pr-detail", "pr-page", "view-shown");
        r.ts_epoch_ms = ts;
        r.status = 200;
        r.cost = Some(1);
        r
    }

    /// A budget snapshot: `remaining` of 5,000 left, window resetting at
    /// `reset_ms` (GitHub's epoch seconds).
    fn snap(ts: u64, transport: Transport, remaining: u32, reset_ms: u64) -> UsageRecord {
        let mut r = rec(ts);
        r.transport = transport;
        r.remaining_after = Some(remaining);
        r.limit_after = Some(5_000);
        r.cycle_reset_epoch = Some(reset_ms / 1_000);
        r
    }

    #[test]
    fn ranges_re_slice_the_frozen_read() {
        let mut p = UsagePage::frozen_at(
            AS_OF,
            vec![
                rec(AS_OF - 1),
                rec(AS_OF - Span::H24.ms()),
                rec(AS_OF - Span::D30.ms() - 1),
            ],
        );
        // 24h (default): [as_of − 24h, as_of) — both edges counted once.
        assert_eq!(p.derived.requests, 2);
        p.select(p.view, Preset::Trailing(Span::H1));
        assert_eq!(p.derived.requests, 1);
        p.select(p.view, Preset::Trailing(Span::D90));
        assert_eq!(p.derived.requests, 3);
        // D1: a view switch keeps the range and the freeze.
        p.select(UsageView::Requests, p.preset);
        assert_eq!(
            (p.preset, p.as_of_ms, p.derived.requests),
            (Preset::Trailing(Span::D90), AS_OF, 3)
        );
    }

    #[test]
    fn current_window_follows_the_selected_budget() {
        // GraphQL's window opened 20 min ago; REST's 50 min ago.
        let gql_reset = AS_OF + 40 * MIN;
        let rest_reset = AS_OF + 10 * MIN;
        let mut p = UsagePage::frozen_at(
            AS_OF,
            vec![
                rec(AS_OF - 30 * MIN),
                snap(AS_OF - MIN, Transport::Graphql, 1_000, gql_reset),
                snap(AS_OF - MIN, Transport::Rest, 4_000, rest_reset),
            ],
        );
        p.select(p.view, Preset::CurrentWindow);
        p.budget = Budget::Graphql;
        p.derive();
        assert_eq!(
            p.bounds(),
            Some(Bounds {
                from_ms: gql_reset - 60 * MIN,
                to_ms: AS_OF
            })
        );
        assert_eq!(p.derived.requests, 2, "the 30-min-old row predates it");
        assert!(p.honesty().line().contains(" · GraphQL window "));
        p.budget = Budget::Rest;
        p.derive();
        assert_eq!(p.derived.requests, 3, "REST's window is older");
        assert!(p.honesty().line().contains(" · REST window "));
        // Requests names no window: it reads the trailing hour.
        p.select(UsageView::Requests, p.preset);
        assert!(!p.honesty().line().contains("window"));
        assert_eq!(p.derived.requests, 3);
    }

    #[test]
    fn current_window_without_an_open_window_is_empty_not_guessed() {
        // The only snapshot's window reset before `as_of`.
        let mut p = UsagePage::frozen_at(
            AS_OF,
            vec![snap(AS_OF - 30 * MIN, Transport::Rest, 4_000, AS_OF - MIN)],
        );
        p.select(p.view, Preset::CurrentWindow);
        assert_eq!(p.bounds(), None);
        assert_eq!(p.derived.requests, 0);
        assert!(
            p.honesty()
                .line()
                .starts_with("0 requests · no open REST window · as of ")
        );
    }

    #[test]
    fn the_cached_honesty_inputs_track_every_control_change() {
        let gql_reset = AS_OF + 40 * MIN;
        let mut p = UsagePage::frozen_at(
            AS_OF,
            vec![
                rec(AS_OF - 2 * Span::D7.ms()),
                rec(AS_OF - 30 * MIN),
                snap(AS_OF - MIN, Transport::Graphql, 1_000, gql_reset),
            ],
        );
        assert_eq!(p.derived, p.derive_inputs());
        for (view, preset) in [
            (UsageView::Budget, Preset::Trailing(Span::D30)),
            (UsageView::Budget, Preset::CurrentWindow),
            (UsageView::Requests, Preset::CurrentWindow),
            (UsageView::Requests, Preset::Trailing(Span::H1)),
        ] {
            p.select(view, preset);
            assert_eq!(p.derived, p.derive_inputs(), "{view:?} {preset:?}");
            assert_eq!(p.derived.oldest_ms, Some(AS_OF - 2 * Span::D7.ms()));
        }
        // A read lands: the cache follows the new rows.
        p.apply_read(vec![rec(AS_OF - 10)], false);
        assert_eq!(p.derived, p.derive_inputs());
        assert_eq!(
            (p.derived.requests, p.derived.oldest_ms),
            (1, Some(AS_OF - 10))
        );
    }

    #[test]
    fn a_read_derives_the_d7_default_budget() {
        let mut p = UsagePage::frozen_at(AS_OF, Vec::new());
        p.apply_read(
            vec![
                snap(AS_OF - MIN, Transport::Graphql, 1_000, AS_OF + 40 * MIN),
                snap(AS_OF - MIN, Transport::Rest, 4_000, AS_OF + 10 * MIN),
            ],
            false,
        );
        assert_eq!(p.budget, Budget::Graphql, "20 % left beats 80 %");
    }

    #[gpui::test]
    fn page_loads_on_first_open_and_freezes_until_refresh(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let store = Arc::new(MemorySink::default());
        cx.update(|cx| init_with(cx, store.clone() as Arc<dyn UsageStore>));
        // A request a minute before first open.
        let before = usage_records::now_epoch_ms() - MIN;
        cx.update(|cx| sink(cx).record(rec(before)));

        let (page, _window_ctx) = cx.add_window_view(UsagePage::new);
        // The read runs on the background executor (docs/0082 D11).
        cx.run_until_parked();
        let as_of0 = cx.update(|cx| page.read(cx).as_of_ms);
        assert!(before < as_of0);
        assert_eq!(cx.update(|cx| page.read(cx).rows.len()), 1);

        // Post-freeze arrival: ts == as_of is already invisible (the
        // half-open upper bound is what makes the freeze real, D10) —
        // through every control change.
        cx.update(|cx| sink(cx).record(rec(as_of0)));
        page.update(cx, |p, cx| {
            p.set_preset(Preset::Trailing(Span::D30), cx);
            p.set_view(UsageView::Requests, cx);
        });
        cx.update(|cx| {
            let p = page.read(cx);
            assert_eq!((p.rows.len(), p.derived.requests), (1, 1));
            assert_eq!(p.derived, p.derive_inputs(), "the setters re-derive");
            assert_eq!(p.as_of_ms, as_of0);
        });

        // Refresh: the only path that moves `as_of` and re-reads. Sleep
        // a real tick so the new stamp strictly exceeds the row's.
        std::thread::sleep(std::time::Duration::from_millis(2));
        page.update(cx, |p, cx| p.refresh(cx));
        cx.run_until_parked();
        cx.update(|cx| {
            let p = page.read(cx);
            assert!(p.as_of_ms > as_of0, "refresh re-stamps the freeze");
            assert_eq!(p.rows.len(), 2, "refresh picks up post-freeze rows");
        });
    }

    #[gpui::test]
    fn the_read_is_off_thread_with_a_loading_state(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let store = Arc::new(MemorySink::default());
        cx.update(|cx| init_with(cx, store.clone() as Arc<dyn UsageStore>));
        let now = usage_records::now_epoch_ms();
        cx.update(|cx| {
            let s = sink(cx);
            s.record(snap(now - MIN, Transport::Graphql, 500, now + 40 * MIN));
            s.record(snap(now - MIN, Transport::Rest, 4_000, now + 40 * MIN));
        });
        let (page, _window_ctx) = cx.add_window_view(UsagePage::new);
        // Inside one update the background read cannot have landed.
        page.update(cx, |p, cx| {
            p.refresh(cx);
            assert_eq!(
                p.read,
                ReadState::Loading,
                "D18: loading until the rows land"
            );
            assert!(p.honesty().line().starts_with("Reading history · as of "));
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let p = page.read(cx);
            assert_eq!(p.read, ReadState::Ready);
            assert_eq!(p.rows.len(), 2);
            assert!(!p.degraded_shown());
            assert_eq!(p.budget, Budget::Graphql);
        });
        // A superseded read never lands: the newest refresh wins.
        page.update(cx, |p, cx| {
            p.refresh(cx);
            p.refresh(cx);
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let p = page.read(cx);
            assert_eq!((p.read_gen, p.read), (4, ReadState::Ready));
        });
    }

    #[gpui::test]
    fn a_degraded_store_reaches_the_honesty_line(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        struct Degraded(MemorySink);
        impl crate::usage_records::UsageSink for Degraded {
            fn record(&self, rec: UsageRecord) {
                self.0.record(rec)
            }
        }
        impl crate::usage_records::UsageSource for Degraded {
            fn records_between(&self, from: u64, to: u64) -> Vec<UsageRecord> {
                self.0.records_between(from, to)
            }
            fn high_water(&self) -> u64 {
                self.0.high_water()
            }
            fn is_degraded(&self) -> bool {
                true
            }
        }
        impl UsageStore for Degraded {}
        let store = Arc::new(Degraded(MemorySink::default()));
        cx.update(|cx| init_with(cx, store.clone() as Arc<dyn UsageStore>));
        cx.update(|cx| sink(cx).record(rec(usage_records::now_epoch_ms() - MIN)));
        let (page, _window_ctx) = cx.add_window_view(UsagePage::new);
        cx.run_until_parked();
        cx.update(|cx| assert!(page.read(cx).degraded_shown()));
    }

    #[gpui::test]
    fn only_first_open_and_refresh_touch_the_store(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        struct CountingStore {
            reads: AtomicUsize,
        }
        impl crate::usage_records::UsageSink for CountingStore {
            fn record(&self, _rec: UsageRecord) {}
        }
        impl crate::usage_records::UsageSource for CountingStore {
            fn records_between(&self, _from: u64, _to: u64) -> Vec<UsageRecord> {
                self.reads.fetch_add(1, Ordering::Relaxed);
                Vec::new()
            }
            fn high_water(&self) -> u64 {
                0
            }
            // Overridden so the default (records_since(0)) cannot
            // masquerade as a page read.
            fn snapshot(&self) -> Vec<UsageRecord> {
                Vec::new()
            }
        }
        impl UsageStore for CountingStore {}
        let store = Arc::new(CountingStore {
            reads: AtomicUsize::new(0),
        });
        cx.update(|cx| init_with(cx, store.clone() as Arc<dyn UsageStore>));

        let (page, _window_ctx) = cx.add_window_view(UsagePage::new);
        cx.run_until_parked();
        assert_eq!(
            store.reads.load(Ordering::Relaxed),
            1,
            "one read on first open"
        );

        // Control changes (D10): pure recomputation, zero reads.
        page.update(cx, |p, cx| {
            p.set_preset(Preset::Trailing(Span::D7), cx);
            p.set_view(UsageView::Requests, cx);
            p.set_preset(Preset::CurrentWindow, cx);
        });
        assert_eq!(store.reads.load(Ordering::Relaxed), 1);

        // Refresh: exactly one more bounded read — memory only, never
        // a fetch (no Http, no specs).
        page.update(cx, |p, cx| p.refresh(cx));
        cx.run_until_parked();
        assert_eq!(store.reads.load(Ordering::Relaxed), 2);
    }
}
