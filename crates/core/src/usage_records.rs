//! Usage records (docs/0053): the module-global, per-flight cost log.
//!
//! One immutable [`UsageRecord`] per real HTTP flight — cost-first
//! (nominal points per the module's booking rules, never a guessed
//! number), origin-attributed (`surface` × `trigger`, stamped from the
//! dispatching module's own closed enums), held by an injectable
//! [`UsageStore`] behind a `gpui::Global`. The v1 store is the capped
//! in-memory [`MemorySink`]; a future SQLite backend is a new
//! in-memory [`MemorySink`]; the SQLite backend (`crate::app_db`,
//! docs/0057) is the `UsageStore` swapped at [`init_with`] — the row is
//! pre-stamped and storage-independent, so nothing else changes (D3).
//!
//! The write side is the [`Recorder`] (one per module): it stamps
//! `seq`/timestamp/`cycle_reset_epoch` *before* the sink sees the row,
//! keeping identity storage-independent and the sink a dumb writer.
//! `record` is best-effort by contract — a sink must never fail or
//! stall a request (D3); analytics can never break fetching.
//!
//! The read side is [`UsageSource`] (docs/0054 D9): a writer/reader
//! split so the analytics page programs against rows, not storage.
//! `records_between` is half-open `[from, to)` — the upper bound is
//! what makes a frozen `as_of` view real. `high_water` is a monotonic
//! mutation token (moves on record *and* eviction) for staleness
//! checks that must not confuse "nothing happened" with "the oldest
//! row fell off the cap".

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use gpui::{App, Global, SharedString};
use serde::{Deserialize, Serialize};

/// The v1 retention: one FIFO across all modules (D3). A heavy day is
/// thousands of flights; the cap is the analytics working set — totals
/// live in each module's own counters.
pub const RECORD_CAP: usize = 10_000;

/// Which wire a flight went out on. Generic across modules (docs/0053
/// D6): REST, GraphQL, or the provider's auth endpoints — plus `Local`
/// for non-network records (docs/0064 D9: the favicon generator's
/// export runs). Additive value; storage is a plain TEXT column, so old
/// rows and readers are unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transport {
    Rest,
    Graphql,
    Auth,
    Local,
}

impl Transport {
    pub fn as_str(self) -> &'static str {
        match self {
            Transport::Rest => "rest",
            Transport::Graphql => "graphql",
            Transport::Auth => "auth",
            Transport::Local => "local",
        }
    }
}

/// One HTTP flight, sqlite-ready (docs/0053 D1): flat, owned, no
/// entity handles — this struct *is* the future table row. `surface`
/// and `trigger` arrive as strings stamped from the dispatching
/// module's closed enums (D6): the shared schema stays stable while
/// each module keeps compiler-enforced origin vocabularies.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct UsageRecord {
    /// "github" today; "forgejo"/"gitea" when those modules exist (D6).
    pub module: SharedString,
    /// Monotonic, log-local, never resets — the table key.
    pub seq: u64,
    pub ts_epoch_ms: u64,
    pub transport: Transport,
    /// The module's op name — GitHub's `kind_name()` ("pr-detail", …).
    pub op: SharedString,
    /// Stamped from the module's `Surface` enum (D2).
    pub surface: SharedString,
    /// Stamped from the module's `Trigger` enum (D2) — docs/0052's set.
    pub trigger: SharedString,
    /// `owner/repo`, `repo#n`, … when the op is scoped.
    pub ident: Option<SharedString>,
    /// 0 on network error.
    pub status: u16,
    pub etag_304: bool,
    /// Nominal points per the module's booking rules; `None` = not
    /// attributable (GQL without the cost field, network errors).
    pub cost: Option<u32>,
    /// Real budget snapshot from the flight, when the provider sent one.
    pub remaining_after: Option<u32>,
    /// The quota ceiling the response itself reported (docs/0054 D14):
    /// REST `x-ratelimit-limit`, GQL body `rateLimit.limit`. `None`
    /// when the response carried none (network errors, auth flights).
    pub limit_after: Option<u32>,
    /// Denormalized quota-cycle key (the window's reset epoch) so
    /// per-cycle rollups are a plain GROUP BY later (D3).
    pub cycle_reset_epoch: Option<u64>,
    pub duration_ms: u32,
    /// Wait from spec dispatch to flight start (docs/0054 D14) —
    /// self-inflicted contention, separable from network latency.
    /// `None` when the dispatch instant wasn't captured.
    pub queue_ms: Option<u32>,
    /// Response body length (docs/0054 D14) — payload size as the
    /// other half of "why was this slow". `None` when there is no
    /// body (304 revalidations, network errors).
    pub resp_bytes: Option<u32>,
    pub network_error: bool,
}

impl UsageRecord {
    /// The write-time baseline (#484): who flew, what, why — stamped
    /// now; the flight's outcome rides on the returned value's `pub`
    /// fields (`seq`/identity are the recorder's to stamp).
    pub fn new(module: &str, op: &str, surface: &str, trigger: &str) -> Self {
        Self {
            module: module.into(),
            seq: 0,
            ts_epoch_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            transport: Transport::Rest,
            op: op.into(),
            surface: surface.into(),
            trigger: trigger.into(),
            ident: None,
            status: 0,
            etag_304: false,
            cost: None,
            remaining_after: None,
            limit_after: None,
            cycle_reset_epoch: None,
            duration_ms: 0,
            queue_ms: None,
            resp_bytes: None,
            network_error: false,
        }
    }
}

/// The write half (docs/0053 D3, docs/0054 D9): fire-and-forget by
/// contract — impls own their errors and must not stall callers.
pub trait UsageSink: Send + Sync {
    fn record(&self, rec: UsageRecord);
}

/// The read half (docs/0054 D9): what the analytics page (and, later,
/// any programmatic consumer) programs against — rows, never storage.
pub trait UsageSource: Send + Sync {
    /// Rows with `from <= ts_epoch_ms < to` (half-open — the upper
    /// bound keeps post-freeze flights out of a frozen `as_of` view,
    /// docs/0054 D10), in arrival order.
    fn records_between(&self, from: u64, to: u64) -> Vec<UsageRecord>;
    /// Sugar: everything retained from `from` on.
    fn records_since(&self, from: u64) -> Vec<UsageRecord> {
        self.records_between(from, u64::MAX)
    }
    /// Monotonic mutation token — moves on `record` *and* on
    /// eviction (eviction alters the retained set without advancing
    /// seq). "Nothing happened" and "the oldest row fell off" must
    /// not look alike to a staleness check.
    fn high_water(&self) -> u64;
    /// The full retained set — tests and the debug bridge today.
    fn snapshot(&self) -> Vec<UsageRecord> {
        self.records_since(0)
    }
}

/// Writer and reader in one object (docs/0054 D9): what [`init_with`]
/// injects. The v1 [`MemorySink`] implements it; the future SQLite
/// backend is a swapped impl of this same trait.
pub trait UsageStore: UsageSink + UsageSource {}

/// The v1 store: one capped FIFO across all modules. The cap is this
/// impl's concern (D3) — a SQLite store simply wouldn't have one.
pub struct MemorySink {
    inner: Mutex<VecDeque<UsageRecord>>,
    cap: usize,
    high_water: AtomicU64,
}

impl MemorySink {
    pub fn new(cap: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::new()),
            cap: cap.max(1),
            high_water: AtomicU64::new(0),
        }
    }
}

impl UsageSink for MemorySink {
    fn record(&self, rec: UsageRecord) {
        let mut q = self.inner.lock().unwrap();
        if q.len() >= self.cap {
            q.pop_front();
        }
        q.push_back(rec);
        // One bump per mutation, eviction included (D9).
        self.high_water.fetch_add(1, Ordering::Relaxed);
    }
}

impl UsageSource for MemorySink {
    fn records_between(&self, from: u64, to: u64) -> Vec<UsageRecord> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.ts_epoch_ms >= from && r.ts_epoch_ms < to)
            .cloned()
            .collect()
    }

    fn high_water(&self) -> u64 {
        self.high_water.load(Ordering::Relaxed)
    }
}

impl UsageStore for MemorySink {}

impl Default for MemorySink {
    fn default() -> Self {
        Self::new(RECORD_CAP)
    }
}

/// The mutable half of a record, before the [`Recorder`] stamps
/// identity — what a flight actually learned.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RecordDraft {
    pub transport: Transport,
    pub op: SharedString,
    pub surface: SharedString,
    pub trigger: SharedString,
    pub ident: Option<SharedString>,
    pub status: u16,
    pub etag_304: bool,
    pub cost: Option<u32>,
    pub remaining_after: Option<u32>,
    /// The response's own quota ceiling (docs/0054 D14).
    pub limit_after: Option<u32>,
    /// The flight's own reset epoch when its response carried one;
    /// `None` falls back to the recorder's last-known cycle (D3).
    pub cycle_reset_epoch: Option<u64>,
    pub duration_ms: u32,
    /// Dispatch→flight-start wait (docs/0054 D14).
    pub queue_ms: Option<u32>,
    /// Response body length (docs/0054 D14).
    pub resp_bytes: Option<u32>,
    pub network_error: bool,
}

impl RecordDraft {
    /// The flight's write-time baseline (#484): what flew, why, and
    /// the scope — the measured outcome rides on the returned value's
    /// `pub` fields.
    pub fn new(
        transport: Transport,
        op: impl Into<SharedString>,
        surface: impl Into<SharedString>,
        trigger: impl Into<SharedString>,
        ident: Option<SharedString>,
    ) -> Self {
        Self {
            transport,
            op: op.into(),
            surface: surface.into(),
            trigger: trigger.into(),
            ident,
            status: 0,
            etag_304: false,
            cost: None,
            remaining_after: None,
            limit_after: None,
            cycle_reset_epoch: None,
            duration_ms: 0,
            queue_ms: None,
            resp_bytes: None,
            network_error: false,
        }
    }
}

/// The per-module writer (docs/0053 D3/D4): stamps `seq`, the
/// timestamp, and the cycle carry-forward, then hands the finished
/// row to the sink. Cloneable handle — `GithubState` holds one beside
/// its ledger; a future module holds its own with its module id.
#[derive(Clone)]
pub struct Recorder {
    module: SharedString,
    seq: Arc<AtomicU64>,
    last_cycle: Arc<Mutex<Option<u64>>>,
    sink: Arc<dyn UsageSink>,
}

impl Recorder {
    pub fn new(module: &str, sink: Arc<dyn UsageSink>) -> Self {
        Self {
            module: SharedString::from(module),
            seq: Arc::new(AtomicU64::new(1)),
            last_cycle: Arc::new(Mutex::new(None)),
            sink,
        }
    }

    /// The sink behind this recorder (tests read via `snapshot`).
    pub fn sink(&self) -> Arc<dyn UsageSink> {
        self.sink.clone()
    }

    /// Stamp and store one flight. Best-effort: the sink owns failure.
    pub fn emit(&self, draft: RecordDraft) {
        let cycle = match draft.cycle_reset_epoch {
            Some(c) => {
                *self.last_cycle.lock().unwrap() = Some(c);
                Some(c)
            }
            None => *self.last_cycle.lock().unwrap(),
        };
        self.sink.record(UsageRecord {
            module: self.module.clone(),
            seq: self.seq.fetch_add(1, Ordering::Relaxed),
            ts_epoch_ms: now_epoch_ms(),
            transport: draft.transport,
            op: draft.op,
            surface: draft.surface,
            trigger: draft.trigger,
            ident: draft.ident,
            status: draft.status,
            etag_304: draft.etag_304,
            cost: draft.cost,
            remaining_after: draft.remaining_after,
            limit_after: draft.limit_after,
            cycle_reset_epoch: cycle,
            duration_ms: draft.duration_ms,
            queue_ms: draft.queue_ms,
            resp_bytes: draft.resp_bytes,
            network_error: draft.network_error,
        });
    }
}

pub(crate) fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// The global (D3/D6) — the `status::InFlight` pattern: one app-wide
// store, created in `main`, reached through `cx`.
// ---------------------------------------------------------------------------

struct UsageRecordsGlobal(Arc<dyn UsageStore>);

impl Global for UsageRecordsGlobal {}

/// Create the records store with the default [`MemorySink`]. Once, in
/// `main`, beside `status::init` and before any module `init` runs.
pub fn init(cx: &mut App) {
    init_with(cx, Arc::new(MemorySink::default()));
}

/// Create the records store with an injected store — the swap point
/// for tests today and the SQLite backend later (docs/0053 D3,
/// docs/0054 D9).
pub fn init_with(cx: &mut App, store: Arc<dyn UsageStore>) {
    cx.set_global(UsageRecordsGlobal(store));
}

/// The app-wide writer view. Panics if [`init`] was never called —
/// the same programming-error contract as `status::tracker`.
pub fn sink(cx: &App) -> Arc<dyn UsageSink> {
    cx.global::<UsageRecordsGlobal>().0.clone()
}

/// The app-wide reader view (docs/0054 D9) — same store, read half.
pub fn source(cx: &App) -> Arc<dyn UsageSource> {
    cx.global::<UsageRecordsGlobal>().0.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(status: u16, cost: Option<u32>, cycle: Option<u64>) -> RecordDraft {
        RecordDraft {
            transport: Transport::Rest,
            op: SharedString::from("test-op"),
            surface: SharedString::from("test-surface"),
            trigger: SharedString::from("test-trigger"),
            ident: None,
            status,
            etag_304: status == 304,
            cost,
            remaining_after: None,
            limit_after: None,
            cycle_reset_epoch: cycle,
            duration_ms: 5,
            queue_ms: None,
            resp_bytes: None,
            network_error: false,
        }
    }

    /// A hand-stamped row for read-contract tests (docs/0054 §9):
    /// controlled `ts_epoch_ms`/`seq`, recorded directly.
    fn row(ts: u64, seq: u64) -> UsageRecord {
        UsageRecord {
            module: SharedString::from("github"),
            seq,
            ts_epoch_ms: ts,
            transport: Transport::Rest,
            op: SharedString::from("test-op"),
            surface: SharedString::from("test-surface"),
            trigger: SharedString::from("test-trigger"),
            ident: None,
            status: 200,
            etag_304: false,
            cost: Some(1),
            remaining_after: None,
            limit_after: None,
            cycle_reset_epoch: None,
            duration_ms: 5,
            queue_ms: None,
            resp_bytes: None,
            network_error: false,
        }
    }

    #[test]
    fn memory_sink_evicts_at_cap_and_seq_stays_monotonic() {
        let sink = Arc::new(MemorySink::new(3));
        let rec = Recorder::new("github", sink.clone());
        for i in 0..5 {
            rec.emit(draft(200, Some(1), Some(1_000 + i)));
        }
        let rows = sink.snapshot();
        assert_eq!(rows.len(), 3, "cap evicts oldest");
        assert_eq!(
            rows.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![3, 4, 5],
            "seq is monotonic across eviction"
        );
        assert_eq!(rows.last().unwrap().cycle_reset_epoch, Some(1_004));
    }

    #[test]
    fn cycle_carries_forward_from_last_known() {
        let sink = Arc::new(MemorySink::new(8));
        let rec = Recorder::new("github", sink.clone());
        rec.emit(draft(200, Some(1), Some(7_200)));
        rec.emit(draft(200, Some(1), None));
        let rows = sink.snapshot();
        assert_eq!(rows[0].cycle_reset_epoch, Some(7_200));
        assert_eq!(
            rows[1].cycle_reset_epoch,
            Some(7_200),
            "no snapshot → last-known cycle"
        );
    }

    #[test]
    fn records_between_is_half_open_on_both_ends() {
        // docs/0054 D9/D10: the window is [from, to) — the upper bound
        // is what keeps post-freeze flights out of a frozen view.
        let sink = MemorySink::new(8);
        sink.record(row(100, 1));
        sink.record(row(200, 2));
        sink.record(row(300, 3));
        let ts = |rows: &[UsageRecord]| rows.iter().map(|r| r.ts_epoch_ms).collect::<Vec<_>>();
        assert_eq!(ts(&sink.records_between(100, 300)), vec![100, 200]);
        assert_eq!(ts(&sink.records_between(150, 300)), vec![200]);
        assert_eq!(ts(&sink.records_between(150, 350)), vec![200, 300]);
        assert!(sink.records_between(301, u64::MAX).is_empty());
        // Sugar and snapshot ride the same bounds (D9).
        assert_eq!(ts(&sink.records_since(150)), vec![200, 300]);
        assert_eq!(ts(&sink.snapshot()), vec![100, 200, 300]);
    }

    #[test]
    fn records_between_preserves_arrival_order() {
        // Arrival order is the contract (per-recorder seq order).
        let sink = MemorySink::new(8);
        for seq in 1..=4 {
            sink.record(row(1_000 + seq, seq));
        }
        let seqs: Vec<u64> = sink.records_since(0).iter().map(|r| r.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4]);
    }

    #[test]
    fn high_water_moves_on_record_and_eviction() {
        // docs/0054 D9: eviction alters the retained set without
        // advancing seq — the token must move anyway.
        let sink = MemorySink::new(2);
        let hw0 = sink.high_water();
        sink.record(row(100, 1));
        let hw1 = sink.high_water();
        assert!(hw1 > hw0, "a record moves the token");
        sink.record(row(200, 2));
        let hw2 = sink.high_water();
        sink.record(row(300, 3)); // evicts row 100
        let hw3 = sink.high_water();
        assert!(hw3 > hw2, "an evicting record moves the token too");
        assert_eq!(sink.records_since(0).len(), 2);
        assert_eq!(sink.records_since(0)[0].ts_epoch_ms, 200);
    }

    /// docs/0053 §5 (swap contract) / docs/0054 D9: `init_with`
    /// injects a custom store; the recorder built from the app-global
    /// sink writes identical pre-stamped rows there, and the same
    /// object serves both views. The future SQLite backend rides the
    /// same seam, and this is the test that will prove both agree.
    #[gpui::test]
    fn init_with_swaps_the_app_wide_store(cx: &mut gpui::TestAppContext) {
        struct CountingStore(std::sync::Mutex<Vec<UsageRecord>>);
        impl UsageSink for CountingStore {
            fn record(&self, rec: UsageRecord) {
                self.0.lock().unwrap().push(rec);
            }
        }
        impl UsageSource for CountingStore {
            fn records_between(&self, from: u64, to: u64) -> Vec<UsageRecord> {
                self.0
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|r| r.ts_epoch_ms >= from && r.ts_epoch_ms < to)
                    .cloned()
                    .collect()
            }
            fn high_water(&self) -> u64 {
                self.0.lock().unwrap().len() as u64
            }
        }
        impl UsageStore for CountingStore {}
        let injected: Arc<CountingStore> =
            Arc::new(CountingStore(std::sync::Mutex::new(Vec::new())));
        cx.update(|cx| init_with(cx, injected.clone()));
        let rec = cx.update(|cx| Recorder::new("github", sink(cx)));
        rec.emit(draft(200, Some(1), Some(3_600)));
        let rows = injected.snapshot();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].module.as_ref(), "github");
        assert_eq!(rows[0].cost, Some(1));
        assert_eq!(rows[0].seq, 1, "identity is stamped before the sink");
        // The reader view reaches the same object (D9).
        let read_back = cx.update(|cx| source(cx).records_since(0));
        assert_eq!(read_back.len(), 1);
        assert_eq!(read_back[0].seq, rows[0].seq);
    }

    #[test]
    fn two_recorders_share_one_sink_and_keep_module_identity() {
        let sink = Arc::new(MemorySink::new(8));
        let gh = Recorder::new("github", sink.clone());
        let fj = Recorder::new("forgejo", sink.clone());
        gh.emit(draft(200, Some(1), None));
        fj.emit(draft(200, Some(1), None));
        let rows = sink.snapshot();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].module.as_ref(), "github");
        assert_eq!(rows[1].module.as_ref(), "forgejo");
        // Independent seq streams — per-recorder counters.
        assert_eq!(rows[0].seq, rows[1].seq);
    }
}
