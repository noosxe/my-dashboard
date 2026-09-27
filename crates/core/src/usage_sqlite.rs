//! SQLite persistence for usage records (docs/0057): the [`UsageStore`]
//! impl that becomes the app's startup default.
//!
//! Shape (docs/0057 D3): `record()` appends to a mutex-guarded buffer —
//! the same microsecond-scale lock shape as [`MemorySink`] — and one
//! dedicated `std::thread` commits batches to SQLite. The thread wakes
//! on a 1 s condvar timeout, drains by swap, and runs its transaction
//! *outside* the buffer lock, so `record()` never waits on I/O. A plain
//! thread is correct precisely because the sink seam is `Send + Sync`
//! with no gpui types — no executor, no waker, no entity access.
//!
//! Durability (D5): WAL journal, `synchronous = NORMAL`, one writer
//! connection owned by the writer thread and one lazy reader
//! connection. Reads are direct indexed queries on the calling thread
//! (D8) — the owner accepted UI-thread blocking at this app's volume.
//!
//! Failure handling (D6): an unusable data dir or database flips the
//! store into degraded mode — records route into an internal
//! [`MemorySink`] and analytics stay session-local. [`is_degraded`]
//! exposes the state; the degrade transition also raises the docs/0058
//! D7 toast through the hub's spill.
//! [`flush_blocking`] is the quit-time flush (D9): drain + commit
//! before the process exits, beside `flush_pending_on_quit` in `main`.
//!
//! Retention (D7): 90 days, pruned inside the writer thread at most
//! once per hour — never the startup path, never the UI thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use gpui::SharedString;
use log::warn;
use rusqlite::{Connection, params};

use crate::paths::AppPaths;
use crate::toasts::{Level, Toast, ToastSpill};
use crate::usage_records::{
    MemorySink, Transport, UsageRecord, UsageSink, UsageSource, UsageStore, now_epoch_ms,
};

/// Writer cadence: the condvar timeout the flush loop parks on (D3).
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);
/// Buffer bound (D3): a pathology guard for a dead writer thread, not a
/// working-set cap — retention (D7) is what bounds the table.
const BUFFER_CAP: usize = 100_000;
/// Consecutive failed batches before write-time degrade (D6).
const FLUSH_FAILURE_LIMIT: u32 = 3;
/// Retention window (D7): 90 days in milliseconds.
const RETENTION_MS: u64 = 90 * 24 * 60 * 60 * 1000;
/// Prune cadence (D7): at most one check per hour inside the writer loop.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// `busy_timeout` for both connections (D5): a brief wait outlasts any
/// sane commit; contention beyond it is not worth stalling reads for.
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);
/// The database file name under [`AppPaths::data_dir`] (D2).
const DB_FILE: &str = "usage.db";
/// The only schema version; future steps are additive (D4).
const SCHEMA_VERSION: i64 = 1;

/// Schema v1 (docs/0057 D4): column order mirrors [`UsageRecord`];
/// Option fields are nullable columns, bools are 0/1. `trigger_kind` is
/// the Rust `trigger` field — `TRIGGER` is a reserved word in SQL.
/// Migrations are additive only: future versions add columns via
/// `ALTER TABLE`, never reshape.
const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS usage_records (
    seq               INTEGER PRIMARY KEY,  -- rowid alias: the pre-stamped key
    ts_epoch_ms       INTEGER NOT NULL,
    module            TEXT NOT NULL,
    transport         TEXT NOT NULL,        -- 'rest' | 'graphql' | 'auth'
    op                TEXT NOT NULL,
    surface           TEXT NOT NULL,
    trigger_kind      TEXT NOT NULL,
    ident             TEXT,                 -- NULL = unscoped op
    status            INTEGER NOT NULL,     -- 0 on network error
    etag_304          INTEGER NOT NULL,     -- 0/1
    cost              INTEGER,              -- NULL = not attributable
    remaining_after   INTEGER,
    limit_after       INTEGER,
    cycle_reset_epoch INTEGER,              -- NULL until first snapshot
    duration_ms       INTEGER NOT NULL,
    queue_ms          INTEGER,
    resp_bytes        INTEGER,
    network_error     INTEGER NOT NULL      -- 0/1
);
CREATE INDEX IF NOT EXISTS idx_usage_records_ts
    ON usage_records(ts_epoch_ms);
PRAGMA user_version = 1;
";

const INSERT_SQL: &str = "INSERT INTO usage_records (
    seq, ts_epoch_ms, module, transport, op, surface, trigger_kind, ident,
    status, etag_304, cost, remaining_after, limit_after, cycle_reset_epoch,
    duration_ms, queue_ms, resp_bytes, network_error
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)";

/// The D8 read: half-open `[from, to)` on the ts index, arrival
/// (`seq`) order — the documented [`UsageSource`] contract.
const SELECT_SQL: &str = "SELECT seq, ts_epoch_ms, module, transport, op, surface, \
trigger_kind, ident, status, etag_304, cost, remaining_after, limit_after, \
cycle_reset_epoch, duration_ms, queue_ms, resp_bytes, network_error \
FROM usage_records WHERE ts_epoch_ms >= ?1 AND ts_epoch_ms < ?2 ORDER BY seq";

/// Buffer state shared between `record()` and the writer thread.
struct WriterState {
    buffer: Vec<UsageRecord>,
    quit: bool,
    /// Bumps once per writer-loop iteration — [`flush_blocking`] waits
    /// on this to know its final flush (and prune) has run.
    iterations: u64,
    thread_done: bool,
    /// Drop-oldest counter for the pathology path (D3).
    dropped: u64,
    /// Set by the writer when an iteration *begins* with quit already
    /// set — the drain of that iteration is the quit flush proper.
    quit_observed: bool,
}

/// Degrade-toast plumbing (docs/0058 D7): the spill handle arrives after
/// construction (the hub lives on the UI thread), so a raise before it
/// attaches latches in `pending` instead of being lost.
#[derive(Default)]
struct ToastSlot {
    spill: Option<ToastSpill>,
    pending: bool,
}

/// The D7 alert: one message, one dedupe key, so upstream's replace-on-id
/// refreshes instead of stacking.
fn degrade_toast() -> Toast {
    Toast::new(
        Level::Warning,
        "Usage history can't be saved this session — check data-dir permissions.",
    )
    .dedupe_key("usage-store-degraded")
}

/// The SQLite-backed usage store (docs/0057). Implements the same
/// [`UsageStore`] seam as [`MemorySink`]; swapped in at `init_with` by
/// `main` (D9). All fallible paths degrade to in-memory capture rather
/// than surfacing — analytics can never break fetching (docs/0053 D3c).
pub struct SQLiteStore {
    db_path: PathBuf,
    state: Mutex<WriterState>,
    cond: Condvar,
    /// The D5 read connection: opened lazily on first read, kept, and
    /// (per WAL) usable while the writer commits.
    reader: Mutex<Option<Connection>>,
    /// The D10 token: a commit counter. Bumps once per durable batch
    /// and once per prune; `record()` never moves it.
    high_water: AtomicU64,
    /// Consecutive failed flush batches (D6).
    failures: AtomicU32,
    degraded: AtomicBool,
    /// Where records go once degraded (D6): the memory impl, unchanged.
    fallback: MemorySink,
    /// D7: degrade toasts via the hub's spill; see [`ToastSlot`].
    toast_slot: Mutex<ToastSlot>,
}

impl SQLiteStore {
    /// The app's store (D2): `<data>/usage.db` under the platform data
    /// root (docs/0004 §4.1). Degrades (D6) when the path or database
    /// is unusable. `main` owns the returned handle so the quit hook
    /// can call [`flush_blocking`](Self::flush_blocking).
    pub fn open_or_fallback(paths: &AppPaths) -> Arc<Self> {
        Self::open_at(paths.data_dir().join(DB_FILE))
    }

    /// Open (creating as needed) at an explicit path — the test seam.
    /// Always returns a store; failures degrade to memory instead of
    /// surfacing (docs/0057 D6, docs/0053 D3 boundary (c)).
    pub(crate) fn open_at(db_path: PathBuf) -> Arc<Self> {
        let store = Arc::new(Self {
            db_path: db_path.clone(),
            state: Mutex::new(WriterState {
                buffer: Vec::new(),
                quit: false,
                iterations: 0,
                thread_done: false,
                dropped: 0,
                quit_observed: false,
            }),
            cond: Condvar::new(),
            reader: Mutex::new(None),
            high_water: AtomicU64::new(0),
            failures: AtomicU32::new(0),
            degraded: AtomicBool::new(false),
            fallback: MemorySink::default(),
            toast_slot: Mutex::new(ToastSlot::default()),
        });

        match open_writer(&db_path) {
            Ok(conn) => {
                let worker = store.clone();
                // Core's first dedicated thread (docs/0057 §2): a plain
                // std::thread — the seam has no gpui types to service.
                // It holds the store's Arc while parked; process exit
                // reaps it on any path that skips quit-time flush.
                let spawned = std::thread::Builder::new()
                    .name("usage-sqlite".into())
                    .spawn(move || writer_loop(worker, conn));
                if let Err(e) = spawned {
                    warn!("usage store: writer thread failed to spawn ({e}); staying in memory");
                    lock(&store.state).thread_done = true;
                    store.degraded.store(true, Ordering::Relaxed);
                    store.raise_degrade_toast();
                }
            }
            Err(e) => {
                warn!(
                    "usage store: sqlite unavailable at {} ({e}); records stay in memory",
                    db_path.display()
                );
                lock(&store.state).thread_done = true;
                store.degraded.store(true, Ordering::Relaxed);
                store.raise_degrade_toast();
            }
        }
        store
    }

    /// Whether persistence is paused and records are memory-only (D6).
    pub fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::Relaxed)
    }

    /// Hand the store the toast spill (docs/0058 D7). Called once from
    /// `main` after the hub exists; a raise that happened earlier (the
    /// startup fallback) surfaces here instead of being lost.
    pub fn attach_toast_spill(&self, spill: ToastSpill) {
        let mut slot = lock(&self.toast_slot);
        slot.spill = Some(spill.clone());
        if slot.pending {
            slot.pending = false;
            spill.push(degrade_toast());
        }
    }

    /// The degrade toast, exactly once per transition (docs/0058 D7).
    /// With no spill attached yet the raise latches — the hub lives on
    /// the UI thread and `main` attaches after the store is built.
    fn raise_degrade_toast(&self) {
        let mut slot = lock(&self.toast_slot);
        match &slot.spill {
            Some(spill) => spill.push(degrade_toast()),
            None => {
                if !slot.pending {
                    slot.pending = true;
                    warn!("usage store: degrade toast latched; hub not attached yet");
                }
            }
        }
    }

    /// Quit-time flush (D9): wake the writer, let it run one final
    /// iteration (drain → commit → prune check), return only after.
    /// Bounded wait — the app is quitting; a stuck store must not hang
    /// shutdown (the docs/0004 §4.7 log-and-swallow rule).
    pub fn flush_blocking(&self) {
        let mut st = lock(&self.state);
        if st.thread_done {
            return;
        }
        st.quit = true;
        self.cond.notify_all();
        let before = st.iterations;
        // Wait for the iteration that OBSERVES quit (not merely any
        // iteration: one that started before quit may have drained only
        // a partial buffer) to drain + commit and bump its counter.
        while !st.thread_done {
            if st.quit_observed && st.iterations > before {
                break;
            }
            let (guard, _) = self
                .cond
                .wait_timeout(st, Duration::from_secs(5))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            st = guard;
        }
    }

    /// Flip to degraded (D6): everything buffered and every future
    /// record lands in the internal [`MemorySink`]. Idempotent.
    fn degrade(&self) {
        if !self.degraded.swap(true, Ordering::Relaxed) {
            let buffered = std::mem::take(&mut lock(&self.state).buffer);
            for rec in buffered {
                self.fallback.record(rec);
            }
            warn!("usage store: degraded to in-memory capture; persistence paused");
            self.raise_degrade_toast();
        }
    }

    /// One durable batch. Runs on the writer thread with the buffer
    /// lock *released* — `record()` can never wait on a commit (D3).
    fn commit_batch(&self, conn: &Connection, batch: Vec<UsageRecord>) {
        let res = (|| -> rusqlite::Result<()> {
            let tx = conn.unchecked_transaction()?;
            {
                let mut stmt = tx.prepare_cached(INSERT_SQL)?;
                for rec in &batch {
                    stmt.execute(params![
                        enc_ms(rec.seq),
                        enc_ms(rec.ts_epoch_ms),
                        rec.module.as_ref(),
                        rec.transport.as_str(),
                        rec.op.as_ref(),
                        rec.surface.as_ref(),
                        rec.trigger.as_ref(),
                        rec.ident.as_ref().map(|s| s.as_ref()),
                        i64::from(rec.status),
                        i64::from(rec.etag_304),
                        rec.cost.map(i64::from),
                        rec.remaining_after.map(i64::from),
                        rec.limit_after.map(i64::from),
                        rec.cycle_reset_epoch.map(enc_ms),
                        i64::from(rec.duration_ms),
                        rec.queue_ms.map(i64::from),
                        rec.resp_bytes.map(i64::from),
                        i64::from(rec.network_error),
                    ])?;
                }
            }
            tx.commit()
        })();
        match res {
            Ok(()) => {
                self.failures.store(0, Ordering::Relaxed);
                // D10: one bump per durable batch, not per record.
                self.high_water.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => self.note_flush_failure(e),
        }
    }

    /// D6's write-time path: count consecutive failures; at the limit,
    /// flip to memory so records keep accumulating session-local.
    fn note_flush_failure(&self, err: rusqlite::Error) {
        let n = self.failures.fetch_add(1, Ordering::Relaxed) + 1;
        if n == 1 {
            warn!("usage store: flush failed ({err})");
        }
        if n >= FLUSH_FAILURE_LIMIT {
            self.degrade();
        }
    }
}

impl UsageSink for SQLiteStore {
    fn record(&self, rec: UsageRecord) {
        if self.degraded.load(Ordering::Relaxed) {
            self.fallback.record(rec);
            return;
        }
        let mut st = lock(&self.state);
        if st.buffer.len() >= BUFFER_CAP {
            // Pathology only (dead writer); O(n) is fine off any hot path.
            st.buffer.remove(0);
            st.dropped += 1;
            if st.dropped == 1 || st.dropped % 10_000 == 0 {
                warn!(
                    "usage store: writer stalled, dropped {} oldest unflushed records",
                    st.dropped
                );
            }
        }
        st.buffer.push(rec);
    }
}

impl UsageSource for SQLiteStore {
    fn records_between(&self, from: u64, to: u64) -> Vec<UsageRecord> {
        if self.degraded.load(Ordering::Relaxed) {
            return self.fallback.records_between(from, to);
        }
        // u64::MAX (the `records_since` open bound) saturates to
        // i64::MAX — still above any stored ts (D8). Bind the values;
        // `params!` borrows temporaries, so it must sit at the call.
        let from_i = enc_ms(from);
        let to_i = enc_ms(to);
        let mut guard = lock(&self.reader);
        if guard.is_none() {
            match Connection::open(&self.db_path).and_then(|c| {
                configure(&c)?;
                Ok(c)
            }) {
                Ok(conn) => *guard = Some(conn),
                Err(e) => {
                    warn!("usage store: read connection failed ({e}); degrading to memory");
                    drop(guard);
                    self.degrade();
                    return self.fallback.records_between(from, to);
                }
            }
        }
        let Some(conn) = guard.as_ref() else {
            return self.fallback.records_between(from, to);
        };
        let read = (|| -> rusqlite::Result<Vec<UsageRecord>> {
            let mut stmt = conn.prepare_cached(SELECT_SQL)?;
            let rows = stmt.query_map(params![from_i, to_i], dec_row)?;
            rows.collect()
        })();
        match read {
            Ok(rows) => rows,
            Err(e) => {
                warn!("usage store: read failed ({e})");
                Vec::new()
            }
        }
    }

    fn high_water(&self) -> u64 {
        if self.degraded.load(Ordering::Relaxed) {
            self.fallback.high_water()
        } else {
            self.high_water.load(Ordering::Relaxed)
        }
    }
}

impl UsageStore for SQLiteStore {}

/// The writer loop (D3): park up to 1 s, drain by swap, commit outside
/// the lock, check retention (D7) at most hourly. One final full
/// iteration runs after `quit` is set, which is what makes
/// [`flush_blocking`] deterministic.
fn writer_loop(store: Arc<SQLiteStore>, conn: Connection) {
    let mut last_prune: Option<Instant> = None;
    loop {
        let (batch, quitting) = {
            let mut st = lock(&store.state);
            let batch = std::mem::take(&mut st.buffer);
            let quitting = st.quit;
            if quitting {
                st.quit_observed = true;
            }
            (batch, quitting)
        };

        if !batch.is_empty() {
            if store.is_degraded() {
                // Flipped mid-flight (D6): finish the drain into memory.
                for rec in batch {
                    store.fallback.record(rec);
                }
            } else {
                store.commit_batch(&conn, batch);
            }
        }
        // Retention check (D7): hourly, and always on the quitting
        // iteration — that is what makes flush_blocking's final prune
        // deterministic.
        let prune_due = quitting || last_prune.is_none_or(|at| at.elapsed() >= PRUNE_INTERVAL);
        if prune_due {
            match prune_once(&conn, now_epoch_ms()) {
                // Eviction alters the retained set — the token moves
                // (docs/0057 D7, docs/0054 D9's contract).
                Ok(n) if n > 0 => {
                    store.high_water.fetch_add(1, Ordering::Relaxed);
                }
                Ok(_) => {}
                Err(e) => warn!("usage store: prune failed ({e})"),
            }
            last_prune = Some(Instant::now());
        }

        let mut st = lock(&store.state);
        st.iterations += 1;
        store.cond.notify_all();
        if quitting {
            // The guard is still held here — drop it before the loop's
            // exit lock or the writer deadlocks on itself (std Mutex is
            // not reentrant; caught by sampling the hung test binary).
            drop(st);
            break;
        }
        let (guard, _) = store
            .cond
            .wait_timeout(st, FLUSH_INTERVAL)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        st = guard;
    }
    lock(&store.state).thread_done = true;
}

/// D7: one indexed DELETE past the retention window.
fn prune_once(conn: &Connection, now_ms: u64) -> rusqlite::Result<usize> {
    let cutoff = now_ms.saturating_sub(RETENTION_MS);
    conn.execute(
        "DELETE FROM usage_records WHERE ts_epoch_ms < ?1",
        params![enc_ms(cutoff)],
    )
}

/// Writer connection setup: create the data dir (D2 — the one
/// filesystem touch on the open path, a local mkdir), then pragmas
/// (D5) and schema migration (D4) before the thread starts. Errors
/// collapse to their message: the only consumer logs and degrades (D6).
fn open_writer(db_path: &Path) -> Result<Connection, String> {
    if let Some(dir) = db_path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
    configure(&conn).map_err(|e| e.to_string())?;
    migrate(&conn).map_err(|e| e.to_string())?;
    Ok(conn)
}

fn configure(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    // journal_mode reports the new mode, so set it via query.
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    debug_assert_eq!(mode, "wal");
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

/// D4: `user_version` gates additive steps; v0/v1 → create. Future
/// steps follow the same pattern (`if v < 2 { … ALTER TABLE … }`).
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if v < SCHEMA_VERSION {
        conn.execute_batch(SCHEMA_V1)?;
    }
    Ok(())
}

/// u64 → i64 for SQLite INTEGER: saturating, never panics — an absurd
/// value must not be able to abort a request (docs/0053 D3). Values
/// past i64::MAX ms cannot occur in this universe; sane rows are exact.
fn enc_ms(v: u64) -> i64 {
    v.min(i64::MAX as u64) as i64
}

fn dec_ms(v: i64) -> u64 {
    v.max(0) as u64
}

fn transport_of(s: &str) -> Transport {
    match s {
        "graphql" => Transport::Graphql,
        "auth" => Transport::Auth,
        _ => Transport::Rest,
    }
}

fn dec_row(row: &rusqlite::Row) -> rusqlite::Result<UsageRecord> {
    Ok(UsageRecord {
        seq: dec_ms(row.get(0)?),
        ts_epoch_ms: dec_ms(row.get(1)?),
        module: SharedString::from(row.get::<_, String>(2)?),
        transport: transport_of(row.get::<_, String>(3)?.as_str()),
        op: SharedString::from(row.get::<_, String>(4)?),
        surface: SharedString::from(row.get::<_, String>(5)?),
        trigger: SharedString::from(row.get::<_, String>(6)?),
        ident: row.get::<_, Option<String>>(7)?.map(SharedString::from),
        status: row.get::<_, i64>(8)? as u16,
        etag_304: row.get::<_, i64>(9)? != 0,
        cost: row.get::<_, Option<i64>>(10)?.map(|v| v as u32),
        remaining_after: row.get::<_, Option<i64>>(11)?.map(|v| v as u32),
        limit_after: row.get::<_, Option<i64>>(12)?.map(|v| v as u32),
        cycle_reset_epoch: row.get::<_, Option<i64>>(13)?.map(dec_ms),
        duration_ms: row.get::<_, i64>(14)? as u32,
        queue_ms: row.get::<_, Option<i64>>(15)?.map(|v| v as u32),
        resp_bytes: row.get::<_, Option<i64>>(16)?.map(|v| v as u32),
        network_error: row.get::<_, i64>(17)? != 0,
    })
}

/// Poison-tolerant locking: a panicked writer must not turn `record`
/// into a panic at the call site (docs/0053 D3 — the sink owns failure).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    /// A unique temp file path; `open_at` creates the parent dir (D2).
    fn temp_db(tag: &str) -> PathBuf {
        let n = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("usage-sqlite-{}-{}-{n}", tag, std::process::id()))
    }

    /// A minimal row (docs/0053 §9's helper shape): controlled
    /// ts/seq, everything else empty.
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

    /// A row stamped near the real clock. Retention (D7) prunes
    /// anything older than 90 days on the quit-style flush, so tests
    /// must write rows the prune is meant to keep — toy epoch values
    /// (100, 200, …) are 56 years expired and get eaten, correctly.
    fn near_now(delta_ms: u64, seq: u64) -> UsageRecord {
        row(now_epoch_ms() + delta_ms, seq)
    }

    /// Every field populated — the roundtrip probe.
    fn full_row(ts: u64, seq: u64) -> UsageRecord {
        UsageRecord {
            module: SharedString::from("github"),
            seq,
            ts_epoch_ms: ts,
            transport: Transport::Graphql,
            op: SharedString::from("pr-detail"),
            surface: SharedString::from("page"),
            trigger: SharedString::from("shown"),
            ident: Some(SharedString::from("noosxe/my-dashboard#333")),
            status: 201,
            etag_304: true,
            cost: Some(u32::MAX),
            remaining_after: Some(4_999),
            limit_after: Some(5_000),
            cycle_reset_epoch: Some(1_700_000_000_000),
            duration_ms: u32::MAX,
            queue_ms: Some(12),
            resp_bytes: Some(65_536),
            network_error: true,
        }
    }

    #[test]
    fn roundtrip_preserves_every_field() {
        let store = SQLiteStore::open_at(temp_db("roundtrip"));
        assert!(!store.is_degraded());
        let ts = now_epoch_ms();
        store.record(full_row(ts, 7));
        store.flush_blocking();
        let rows = store.records_since(0);
        assert_eq!(rows.len(), 1);
        assert_eq!(format!("{:?}", rows[0]), format!("{:?}", full_row(ts, 7)));
    }

    #[test]
    fn half_open_window_and_arrival_order() {
        let store = SQLiteStore::open_at(temp_db("window"));
        let t0 = now_epoch_ms();
        for (off, seq) in [(100, 1), (200, 2), (300, 3)] {
            store.record(row(t0 + off, seq));
        }
        store.flush_blocking();
        let ts = |rows: &[UsageRecord]| rows.iter().map(|r| r.ts_epoch_ms).collect::<Vec<_>>();
        assert_eq!(
            ts(&store.records_between(t0 + 100, t0 + 300)),
            vec![t0 + 100, t0 + 200]
        );
        assert_eq!(
            ts(&store.records_between(t0 + 150, t0 + 300)),
            vec![t0 + 200]
        );
        assert_eq!(
            ts(&store.records_between(t0 + 150, t0 + 350)),
            vec![t0 + 200, t0 + 300]
        );
        assert!(store.records_between(t0 + 301, u64::MAX).is_empty());
        // Arrival order = seq order (the documented contract).
        let seqs: Vec<u64> = store.records_since(0).iter().map(|r| r.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3]);
    }

    #[test]
    fn boundary_values_round_trip() {
        let store = SQLiteStore::open_at(temp_db("boundary"));
        // i64::MAX as u64 itself is unreachable by reads (the saturated
        // open bound excludes it) and cannot occur in reality; one below
        // probes the same cast path.
        let mut rec = full_row(i64::MAX as u64 - 1, 1);
        rec.ident = None;
        rec.cost = None;
        store.record(rec);
        store.flush_blocking();
        let back = store.records_between(i64::MAX as u64 - 1, u64::MAX);
        assert_eq!(back[0].ts_epoch_ms, i64::MAX as u64 - 1);
        assert_eq!(back[0].duration_ms, u32::MAX);
    }

    #[test]
    fn swap_contract_memory_and_sqlite_agree() {
        // docs/0053 §5: the same flights must land identically in both
        // backends — the seam is the contract, storage is a detail.
        let memory = MemorySink::default();
        let store = SQLiteStore::open_at(temp_db("swap"));
        let t0 = now_epoch_ms();
        for (off, seq) in [(1_000, 1), (2_000, 2), (3_000, 3)] {
            let r = full_row(t0 + off, seq);
            memory.record(r.clone());
            store.record(r);
        }
        store.flush_blocking();
        assert_eq!(
            format!("{:?}", store.records_since(0)),
            format!("{:?}", memory.records_since(0)),
        );
    }

    #[test]
    fn migration_creates_v1_and_rows_survive_reopen() {
        let path = temp_db("migrate");
        let t0 = now_epoch_ms();
        {
            let store = SQLiteStore::open_at(path.clone());
            store.record(row(t0 + 500, 1));
            store.flush_blocking();
        }
        // Fresh connection sees the schema at v1.
        let v: i64 = Connection::open(&path)
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        // A "restart": reopen the same file, old row still there.
        let store = SQLiteStore::open_at(path);
        assert_eq!(store.records_since(0).len(), 1);
        store.record(row(t0 + 600, 2));
        store.flush_blocking();
        assert_eq!(store.records_since(0).len(), 2);
    }

    #[test]
    fn hand_built_v0_db_is_migrated() {
        let path = temp_db("v0");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        {
            let conn = Connection::open(&path).unwrap();
            // A v0-shaped past: the crash-between-CREATE-and-PRAGMA
            // window — full v1 table, version stamp never set.
            conn.execute_batch(&SCHEMA_V1.replace("PRAGMA user_version = 1;", ""))
                .unwrap();
        }
        let store = SQLiteStore::open_at(path);
        assert!(!store.is_degraded());
        store.record(near_now(0, 1));
        store.flush_blocking();
        assert_eq!(store.records_since(0).len(), 1);
    }

    #[test]
    fn prune_drops_expired_keeps_recent_and_moves_token() {
        let store = SQLiteStore::open_at(temp_db("prune"));
        let now = now_epoch_ms();
        store.record(row(now - RETENTION_MS - 3_600_000, 1));
        store.record(row(now, 2));
        // The quit-style flush runs drain → commit → prune in one
        // iteration, so the prune is deterministic on return.
        store.flush_blocking();
        let seqs: Vec<u64> = store.records_since(0).iter().map(|r| r.seq).collect();
        assert_eq!(seqs, vec![2], "expired row pruned, recent kept");
        // One commit bump + one prune bump (eviction moves the token).
        assert_eq!(store.high_water(), 2);
    }

    #[test]
    fn flush_makes_buffered_rows_durable() {
        let store = SQLiteStore::open_at(temp_db("precommit"));
        store.record(near_now(100, 1));
        store.record(near_now(200, 2));
        // The writer may legitimately commit within microseconds of the
        // record (its first iteration races the test) — the guarantee
        // under test is the flush, not buffered-row invisibility.
        store.flush_blocking();
        assert_eq!(store.records_since(0).len(), 2);
        assert!(store.high_water() > 0);
    }

    #[test]
    fn rapid_records_all_survive_one_flush() {
        let store = SQLiteStore::open_at(temp_db("burst"));
        let t0 = now_epoch_ms();
        for seq in 1..=50u64 {
            store.record(row(t0 + seq, seq));
        }
        store.flush_blocking();
        assert_eq!(store.records_since(0).len(), 50);
    }

    #[test]
    fn open_failure_degrades_to_memory() {
        // A regular file where a directory must be created (D2's mkdir
        // is the failure point) → degrade, never surface.
        let dir = temp_db("unwritable-parent");
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        std::fs::write(&dir, b"not a directory").unwrap();
        let store = SQLiteStore::open_at(dir.join("usage.db"));
        assert!(store.is_degraded());
        store.record(row(100, 1));
        assert_eq!(store.records_since(0).len(), 1, "memory capture works");
        assert!(store.high_water() > 0, "token moves in degraded mode too");
    }

    #[test]
    fn explicit_degrade_routes_records_to_memory() {
        let store = SQLiteStore::open_at(temp_db("flip"));
        store.record(row(100, 1));
        store.flush_blocking();
        store.degrade();
        assert!(store.is_degraded());
        store.record(row(200, 2));
        let seqs: Vec<u64> = store.records_since(0).iter().map(|r| r.seq).collect();
        assert_eq!(seqs, vec![2], "post-flip reads come from the memory sink");
    }

    #[test]
    fn three_consecutive_failures_flip_degraded() {
        let store = SQLiteStore::open_at(temp_db("failures"));
        let err = || rusqlite::Error::InvalidParameterName("injected".into());
        store.note_flush_failure(err());
        store.note_flush_failure(err());
        assert!(!store.is_degraded());
        store.note_flush_failure(err());
        assert!(store.is_degraded(), "the limit flips to memory capture");
        store.record(row(100, 1));
        assert_eq!(store.records_since(0).len(), 1);
    }

    #[test]
    fn degrade_toast_latches_until_the_spill_attaches() {
        // The startup fallback raises before `main` can attach the spill
        // (the hub lives on the UI thread) — the raise must survive the
        // gap (docs/0058 D7).
        let dir = temp_db("toast-latch");
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        std::fs::write(&dir, b"not a directory").unwrap();
        let store = SQLiteStore::open_at(dir.join("usage.db"));
        assert!(store.is_degraded());

        let spill = ToastSpill::default();
        assert!(
            spill.take().is_empty(),
            "not attached yet — nothing surfaces"
        );
        store.attach_toast_spill(spill.clone());
        let drained = spill.take();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].level, Level::Warning);
        assert_eq!(
            drained[0].dedupe_key.as_deref(),
            Some("usage-store-degraded")
        );
        // Re-attaching must not replay the latch.
        store.attach_toast_spill(spill.clone());
        assert!(spill.take().is_empty());
    }

    #[test]
    fn three_strikes_raise_exactly_one_degrade_toast() {
        let store = SQLiteStore::open_at(temp_db("toast-3strike"));
        let spill = ToastSpill::default();
        store.attach_toast_spill(spill.clone());
        let err = || rusqlite::Error::InvalidParameterName("injected".into());
        for _ in 0..3 {
            store.note_flush_failure(err());
        }
        assert!(store.is_degraded());
        let drained = spill.take();
        assert_eq!(
            drained.len(),
            1,
            "one toast per transition, not per failure"
        );
        // Post-transition failures stay silent — the flip is one-way.
        store.note_flush_failure(err());
        assert!(spill.take().is_empty());
    }

    #[test]
    fn flush_blocking_is_idempotent_and_quit_safe() {
        let store = SQLiteStore::open_at(temp_db("quit"));
        store.record(near_now(0, 1));
        store.flush_blocking();
        store.flush_blocking(); // second call: thread already done
        assert_eq!(store.records_since(0).len(), 1);
    }
}
