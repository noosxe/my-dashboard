//! The app database (docs/0063 D1): `usage.db` under the data root is
//! the single SQLite file for all persistent data. `usage_records`
//! (docs/0057) was its first table; `notification_log` (docs/0063) the
//! second. The store generalizes the original sink's machinery — one
//! file, one writer thread, one additive `user_version` chain shared by
//! every table's migrations — instead of per-concern engines.
//!
//! Shape (docs/0057 D3, generalized by docs/0063 D4): callers push a
//! [`WriteCmd`] onto a mutex-guarded buffer — the same microsecond-scale
//! lock shape as [`MemorySink`] — and one dedicated `std::thread` commits
//! batches to SQLite. The thread wakes on a 1 s condvar timeout, drains
//! by swap, and runs its transaction *outside* the buffer lock, so no
//! caller ever waits on I/O. A plain thread is correct precisely because
//! the seams are `Send + Sync` with no gpui types — no executor, no
//! waker, no entity access.
//!
//! Durability (0057 D5): WAL journal, `synchronous = NORMAL`, one writer
//! connection owned by the writer thread and one lazy reader connection.
//! Reads are direct indexed queries on the calling thread (D8) — the
//! owner accepted UI-thread blocking at this app's volume.
//!
//! Failure handling (0057 D6 + 0063 D4): an unusable data dir or
//! database flips the store into degraded mode — *usage* records route
//! into an internal [`MemorySink`] and analytics stay session-local.
//! [`is_degraded`] exposes the state; the degrade transition also raises
//! the docs/0058 D7 toast through the hub's spill. **Failure isolation**
//! (0063 D4): notification-log writes are best-effort and log-only — a
//! failed notification batch never strikes out usage, never flips
//! [`is_degraded`], and never toasts (the in-memory hub stays the
//! session's source of truth; a dead database is already covered by the
//! store-level degrade toast).
//!
//! [`flush_blocking`] is the quit-time flush (0057 D9): drain + commit
//! before the process exits, beside `flush_pending_on_quit` in `main`.
//!
//! Retention (0057 D7 + 0063 D6): usage rows prune past 90 days; the
//! notification log keeps at most 500 unread rows (drop-oldest — unread
//! has no time TTL, it dies by being read). Both prune inside the writer
//! thread at most once per hour — never the startup path, never the UI
//! thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use gpui::{App, Global, SharedString};
use log::warn;
use rusqlite::{Connection, params};

use crate::paths::AppPaths;
use crate::toasts::{Level, NotificationLog, PendingNotification, Toast, ToastSpill};
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
/// The chain head (D4 + docs/0063 D2): one centrally-ordered,
/// additive-only migration chain per database file — no concern bumps
/// `user_version` alone. v1 = `usage_records` (docs/0057), v2 =
/// `notification_log` (docs/0063), v3 = `github_documents`
/// (docs/0072 D8).
const SCHEMA_VERSION: i64 = 3;

/// Schema v1 (docs/0057 D4): column order mirrors [`UsageRecord`];
/// Option fields are nullable columns, bools are 0/1. `trigger_kind` is
/// the Rust `trigger` field — `TRIGGER` is a reserved word in SQL.
/// Migrations are additive only: future versions add columns via
/// `ALTER TABLE`, never reshape.
const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS usage_records (
    seq               INTEGER PRIMARY KEY,  -- rowid alias: engine-assigned at commit (§9)
    ts_epoch_ms       INTEGER NOT NULL,
    module            TEXT NOT NULL,
    transport         TEXT NOT NULL,        -- 'rest' | 'graphql' | 'auth' | 'local' (0064 D9)
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

/// The flush write. The durable key is NOT the in-memory `seq`: as a
/// rowid alias, `seq` is engine-assigned (`max(rowid)+1`) at commit —
/// globally monotonic across restarts and modules, and arrival-ordered
/// because the single writer thread inserts batches in drain order
/// (docs/0057 §9 errata). The in-memory `seq` stays the session-local
/// identity docs/0053 D3 defines; it is simply not persisted.
const INSERT_SQL: &str = "INSERT INTO usage_records (
    ts_epoch_ms, module, transport, op, surface, trigger_kind, ident,
    status, etag_304, cost, remaining_after, limit_after, cycle_reset_epoch,
    duration_ms, queue_ms, resp_bytes, network_error
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)";

/// The D8 read: half-open `[from, to)` on the ts index, arrival
/// (`seq`) order — the documented [`UsageSource`] contract.
const SELECT_SQL: &str = "SELECT seq, ts_epoch_ms, module, transport, op, surface, \
trigger_kind, ident, status, etag_304, cost, remaining_after, limit_after, \
cycle_reset_epoch, duration_ms, queue_ms, resp_bytes, network_error \
FROM usage_records WHERE ts_epoch_ms >= ?1 AND ts_epoch_ms < ?2 ORDER BY seq";

/// Schema v2 (docs/0063 D2/D3): the notification log is a
/// pending-unread queue — only unread rows persist; "read" is DELETE.
/// `id` is engine-assigned (rowid, the §9 errata rule) and
/// arrival-ordered by the single writer. No TTL column exists (D6):
/// unread rows die by being read, bounded by the row cap in the prune.
const SCHEMA_V2: &str = "
CREATE TABLE IF NOT EXISTS notification_log (
    id           INTEGER PRIMARY KEY,  -- rowid alias: engine-assigned (§9)
    raised_at_ms INTEGER NOT NULL,
    level        TEXT NOT NULL,        -- Level::name()
    source       TEXT NOT NULL,        -- effective source id
    title        TEXT,                 -- NULL = no title
    message      TEXT NOT NULL,
    dedupe_key   TEXT                  -- NULL = unkeyed
);
CREATE INDEX IF NOT EXISTS idx_notification_log_key
    ON notification_log(dedupe_key);
PRAGMA user_version = 2;
";

/// Schema v3 (docs/0072 D8): the PR/issue document store's table — one
/// row per document, natural key `(kind, repo, number)` under UNIQUE
/// (the §9 rule applies to the `id`: engine-assigned rowid, never
/// caller-stamped; the natural key only enforces one row per
/// document). Layers are opaque JSON — core has no github types —
/// stamped with wall-clock epoch ms so a restore can reconstruct ages.
/// `touched_ms` drives the drop-oldest prune.
const SCHEMA_V3: &str = "
CREATE TABLE IF NOT EXISTS github_documents (
    id            INTEGER PRIMARY KEY,  -- rowid alias: engine-assigned (§9)
    kind          TEXT NOT NULL,        -- 'pr' | 'issue'
    repo          TEXT NOT NULL,        -- 'owner/name' (the store's key)
    number        INTEGER NOT NULL,
    summary_at_ms INTEGER NOT NULL,
    detail_at_ms  INTEGER,              -- NULL = no cached detail
    summary_json  TEXT NOT NULL,
    detail_json   TEXT,                 -- NULL = no cached detail
    touched_ms    INTEGER NOT NULL,
    UNIQUE(kind, repo, number)
);
PRAGMA user_version = 3;
";

/// The notification statements (docs/0063 D3): insert on append,
/// key-delete on mark-read, all-delete on panel open.
const INSERT_NOTIF_SQL: &str = "INSERT INTO notification_log (\
     raised_at_ms, level, source, title, message, dedupe_key) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
const DELETE_NOTIF_KEY_SQL: &str = "DELETE FROM notification_log WHERE dedupe_key = ?1";
const DELETE_NOTIF_ALL_SQL: &str = "DELETE FROM notification_log";
/// D5's boot restore: newest-first (the caller reverses into log
/// order), bounded to the hub's restore window.
const SELECT_NOTIF_SQL: &str = "SELECT raised_at_ms, level, source, title, \
     message, dedupe_key FROM notification_log ORDER BY id DESC LIMIT ?1";
/// The persisted-unread cap (D6): drop-oldest, inside the hourly prune.
const NOTIF_CAP: usize = 500;
/// The restore window (D5): the newest unread rows the hub re-seeds its
/// log with (`LOG_CAP` semantics on restore).
const NOTIF_RESTORE_LIMIT: usize = 100;

/// The document-store row (docs/0072 D8): one github PR/issue document,
/// its two layers as opaque JSON plus wall-clock landing stamps. Core is
/// type-blind (no github dependency) — the module owns parse/serialize.
#[derive(Clone, Debug)]
pub struct StoredDocument {
    pub kind: String,
    pub repo: String,
    pub number: i64,
    pub summary_at_ms: u64,
    pub detail_at_ms: Option<u64>,
    pub summary_json: String,
    pub detail_json: Option<String>,
}

/// v3 statements (docs/0072 D8): upsert on the natural key, key/all
/// deletes (sign-out), newest-first restore bounded to the table cap.
const UPSERT_DOC_SQL: &str = "INSERT INTO github_documents (\
     kind, repo, number, summary_at_ms, detail_at_ms, summary_json, detail_json, touched_ms) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
     ON CONFLICT(kind, repo, number) DO UPDATE SET \
     summary_at_ms = excluded.summary_at_ms, detail_at_ms = excluded.detail_at_ms, \
     summary_json = excluded.summary_json, detail_json = excluded.detail_json, \
     touched_ms = excluded.touched_ms";
const DELETE_DOC_KEY_SQL: &str =
    "DELETE FROM github_documents WHERE kind = ?1 AND repo = ?2 AND number = ?3";
const DELETE_DOC_ALL_SQL: &str = "DELETE FROM github_documents";
const SELECT_DOC_SQL: &str = "SELECT kind, repo, number, summary_at_ms, detail_at_ms, \
     summary_json, detail_json FROM github_documents \
     ORDER BY touched_ms DESC, id DESC LIMIT ?1";
/// The persisted cap (docs/0072 D8): drop-oldest-touched in the hourly
/// prune — the table is the store's working set, not an archive.
pub const GITHUB_DOC_CAP: usize = 1024;

/// One buffered write (docs/0063 D4): usage records and notification
/// commands share the writer thread; each kind commits in its own
/// transaction so a failure in one never strikes out the other.
enum WriteCmd {
    Usage(UsageRecord),
    Notification(PendingNotification),
    /// docs/0063 D3: `mark_read_by_key` — the incident deletes.
    NotifDeleteKey(String),
    /// docs/0063 D3: panel open — everything acknowledges.
    NotifDeleteAll,
    /// docs/0072 D8: document write-through (upsert on the natural key).
    DocPut(StoredDocument),
    /// docs/0072 D8: one document deleted (currently unused by callers;
    /// the sign-out path is the all-delete below — kept for symmetry
    /// with the notification pair and future lane deletes).
    DocDeleteKey {
        kind: String,
        repo: String,
        number: i64,
    },
    /// docs/0072 D8: sign-out wipes every persisted document.
    DocDeleteAll,
}

/// Buffer state shared between callers and the writer thread.
struct WriterState {
    buffer: Vec<WriteCmd>,
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
        "Usage history can't be saved this session — persistence paused.",
    )
    .dedupe_key("usage-store-degraded")
}

/// The app-wide DB handle (docs/0072 D8): `main` sets it right after
/// opening the store; module inits that need persistence read it — the
/// [`crate::usage_records::init_with`] precedent, without a trait
/// object (the document paths are concrete `SQLiteStore` methods).
struct DbGlobal(Arc<SQLiteStore>);

impl Global for DbGlobal {}

/// Publish the handle (called once from `main`, before module init).
pub fn set_global(cx: &mut App, store: Arc<SQLiteStore>) {
    cx.set_global(DbGlobal(store));
}

/// The handle, if published. `None` before `main` sets it (and in
/// tests that never set it) — callers then run unpersisted.
pub fn global(cx: &App) -> Option<Arc<SQLiteStore>> {
    cx.try_global::<DbGlobal>().map(|g| g.0.clone())
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

    /// Open (creating as needed) at an explicit path — the test seam
    /// (in-crate and module tests; production goes through
    /// [`Self::open_or_fallback`]). Always returns a store; failures
    /// degrade to memory instead of surfacing (docs/0057 D6,
    /// docs/0053 D3 boundary (c)).
    pub fn open_at(db_path: PathBuf) -> Arc<Self> {
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
                    .name("app-db".into())
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
            for cmd in buffered {
                if let WriteCmd::Usage(rec) = cmd {
                    self.fallback.record(rec);
                }
                // Notification commands drop: best-effort by design
                // (docs/0063 D4).
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

impl SQLiteStore {
    /// The one buffered-push path (D3/D4): lock, cap-guard, push. Never
    /// I/O, never a notify — the writer's 1 s wake commits it. A
    /// degraded store routes usage into the memory fallback and drops
    /// notification commands (D4: best-effort, log-only).
    fn push_cmd(&self, cmd: WriteCmd) {
        if self.degraded.load(Ordering::Relaxed) {
            if let WriteCmd::Usage(rec) = cmd {
                self.fallback.record(rec);
            }
            return;
        }
        let mut st = lock(&self.state);
        if st.buffer.len() >= BUFFER_CAP {
            // Pathology only (dead writer); O(n) is fine off any hot path.
            st.buffer.remove(0);
            st.dropped += 1;
            if st.dropped == 1 || st.dropped.is_multiple_of(10_000) {
                warn!(
                    "app db: writer stalled, dropped {} oldest unflushed commands",
                    st.dropped
                );
            }
        }
        st.buffer.push(cmd);
    }

    /// D4: one wake's notification commands, replayed in exact drain
    /// order (an insert-before-delete must not be reordered away) in a
    /// single transaction. Log-only failure — no strike counter, no
    /// degrade, no toast (0063 D4): the in-memory hub stays the
    /// session's source of truth.
    fn commit_notifications(&self, conn: &Connection, cmds: Vec<WriteCmd>) {
        let res = (|| -> rusqlite::Result<()> {
            let tx = conn.unchecked_transaction()?;
            for cmd in &cmds {
                match cmd {
                    WriteCmd::Notification(row) => {
                        let mut stmt = tx.prepare_cached(INSERT_NOTIF_SQL)?;
                        stmt.execute(params![
                            enc_ms(row.raised_at_ms),
                            row.level.name(),
                            row.source,
                            row.title,
                            row.message,
                            row.dedupe_key,
                        ])?;
                    }
                    WriteCmd::NotifDeleteKey(key) => {
                        let mut stmt = tx.prepare_cached(DELETE_NOTIF_KEY_SQL)?;
                        stmt.execute(params![key.as_str()])?;
                    }
                    WriteCmd::NotifDeleteAll => {
                        tx.execute(DELETE_NOTIF_ALL_SQL, [])?;
                    }
                    // Usage and document commands take their own batch
                    // paths; a mixed call would only reorder rows
                    // within a kind, never destroy them.
                    WriteCmd::Usage(_)
                    | WriteCmd::DocPut(_)
                    | WriteCmd::DocDeleteKey { .. }
                    | WriteCmd::DocDeleteAll => {}
                }
            }
            tx.commit()
        })();
        if let Err(e) = res {
            warn!("app db: notification batch failed ({e}); best-effort, session state unaffected");
        }
    }
}

impl SQLiteStore {
    /// docs/0072 D8: one wake's document commands, replayed in drain
    /// order in a single transaction, same best-effort contract as the
    /// notification path (log-only failure — the in-memory store stays
    /// the session's source of truth; a lost write only costs warmth on
    /// the next launch).
    fn commit_documents(&self, conn: &Connection, cmds: Vec<WriteCmd>) {
        let res = (|| -> rusqlite::Result<()> {
            let tx = conn.unchecked_transaction()?;
            for cmd in &cmds {
                match cmd {
                    WriteCmd::DocPut(doc) => {
                        let mut stmt = tx.prepare_cached(UPSERT_DOC_SQL)?;
                        stmt.execute(params![
                            doc.kind,
                            doc.repo,
                            doc.number,
                            enc_ms(doc.summary_at_ms),
                            doc.detail_at_ms.map(enc_ms),
                            doc.summary_json,
                            doc.detail_json,
                            enc_ms(now_epoch_ms()),
                        ])?;
                    }
                    WriteCmd::DocDeleteKey { kind, repo, number } => {
                        let mut stmt = tx.prepare_cached(DELETE_DOC_KEY_SQL)?;
                        stmt.execute(params![kind, repo, number])?;
                    }
                    WriteCmd::DocDeleteAll => {
                        tx.execute(DELETE_DOC_ALL_SQL, [])?;
                    }
                    _ => {}
                }
            }
            tx.commit()
        })();
        if let Err(e) = res {
            warn!("app db: document batch failed ({e}); best-effort, session state unaffected");
        }
    }
}

impl UsageSink for SQLiteStore {
    fn record(&self, rec: UsageRecord) {
        self.push_cmd(WriteCmd::Usage(rec));
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

impl SQLiteStore {
    /// docs/0072 D8 write-through: buffer one document upsert (never
    /// I/O — the writer thread commits it). Degraded stores drop the
    /// command; the session's memory store is unaffected either way.
    pub fn put_document(&self, doc: StoredDocument) {
        self.push_cmd(WriteCmd::DocPut(doc));
    }

    /// docs/0072 D8: buffer one document delete (key-scoped lane
    /// cleanup).
    pub fn delete_document(&self, kind: &str, repo: &str, number: i64) {
        self.push_cmd(WriteCmd::DocDeleteKey {
            kind: kind.to_string(),
            repo: repo.to_string(),
            number,
        });
    }

    /// docs/0072 D8: buffer the all-delete (sign-out wipes persisted
    /// documents the way `GithubCache::clear` wipes the JSON lanes).
    pub fn delete_documents(&self) {
        self.push_cmd(WriteCmd::DocDeleteAll);
    }

    /// docs/0072 D8 boot restore: newest-first (by touch), bounded to
    /// [`GITHUB_DOC_CAP`], on the lazy reader. Best-effort by the
    /// notification contract — a failure reads as empty and must NOT
    /// flip the store degraded; the module then runs session-scoped,
    /// exactly pre-#556.
    pub fn load_documents(&self) -> Vec<StoredDocument> {
        if self.degraded.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let mut guard = lock(&self.reader);
        let Some(conn) = self.reader_conn(&mut guard) else {
            return Vec::new();
        };
        let read = (|| -> rusqlite::Result<Vec<StoredDocument>> {
            let mut stmt = conn.prepare_cached(SELECT_DOC_SQL)?;
            let rows = stmt.query_map(
                params![i64::try_from(GITHUB_DOC_CAP).unwrap_or(i64::MAX)],
                |r| {
                    Ok(StoredDocument {
                        kind: r.get(0)?,
                        repo: r.get(1)?,
                        number: r.get(2)?,
                        summary_at_ms: dec_ms(r.get::<_, i64>(3)?),
                        detail_at_ms: r.get::<_, Option<i64>>(4)?.map(dec_ms),
                        summary_json: r.get(5)?,
                        detail_json: r.get(6)?,
                    })
                },
            )?;
            rows.collect()
        })();
        match read {
            Ok(rows) => rows,
            Err(e) => {
                warn!("app db: document read failed ({e})");
                Vec::new()
            }
        }
    }

    /// The lazy read connection, shared by the reader paths (the
    /// notification `load_unread` shape). `None` = degrade to empty.
    fn reader_conn<'a>(&'a self, guard: &'a mut Option<Connection>) -> Option<&'a Connection> {
        if guard.is_none() {
            match Connection::open(&self.db_path).and_then(|c| {
                configure(&c)?;
                Ok(c)
            }) {
                Ok(conn) => *guard = Some(conn),
                Err(e) => {
                    warn!("app db: read connection failed ({e})");
                    return None;
                }
            }
        }
        guard.as_ref()
    }
}

impl NotificationLog for SQLiteStore {
    fn insert(&self, row: PendingNotification) {
        self.push_cmd(WriteCmd::Notification(row));
    }

    fn delete_by_key(&self, key: &str) {
        self.push_cmd(WriteCmd::NotifDeleteKey(key.to_string()));
    }

    fn delete_all(&self) {
        self.push_cmd(WriteCmd::NotifDeleteAll);
    }

    /// The D5 boot read: direct query on the lazy reader, newest-first,
    /// bounded by the restore window. Best-effort by contract — a
    /// failure reads as empty and must NOT flip the store degraded
    /// (0063 D4/D8; the hub then runs ephemeral, exactly pre-0063).
    fn load_unread(&self) -> Vec<PendingNotification> {
        if self.degraded.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let mut guard = lock(&self.reader);
        if guard.is_none() {
            match Connection::open(&self.db_path).and_then(|c| {
                configure(&c)?;
                Ok(c)
            }) {
                Ok(conn) => *guard = Some(conn),
                Err(e) => {
                    warn!("app db: notification read connection failed ({e})");
                    return Vec::new();
                }
            }
        }
        let Some(conn) = guard.as_ref() else {
            return Vec::new();
        };
        let read = (|| -> rusqlite::Result<Vec<PendingNotification>> {
            let mut stmt = conn.prepare_cached(SELECT_NOTIF_SQL)?;
            let rows = stmt.query_map(
                params![i64::try_from(NOTIF_RESTORE_LIMIT).unwrap_or(i64::MAX)],
                dec_notif_row,
            )?;
            rows.collect()
        })();
        match read {
            Ok(rows) => rows,
            Err(e) => {
                warn!("app db: notification read failed ({e})");
                Vec::new()
            }
        }
    }
}

/// The writer loop (D3): park up to 1 s, drain by swap, commit outside
/// the lock, check retention (D7) at most hourly. One final full
/// iteration runs after `quit` is set, which is what makes
/// [`flush_blocking`] deterministic.
fn writer_loop(store: Arc<SQLiteStore>, conn: Connection) {
    let mut last_prune: Option<Instant> = None;
    loop {
        let (cmds, quitting) = {
            let mut st = lock(&store.state);
            let cmds = std::mem::take(&mut st.buffer);
            let quitting = st.quit;
            if quitting {
                st.quit_observed = true;
            }
            (cmds, quitting)
        };

        if !cmds.is_empty() {
            if store.is_degraded() {
                // Flipped mid-flight (D6): finish the drain into memory;
                // notification commands drop (best-effort, 0063 D4).
                for cmd in cmds {
                    if let WriteCmd::Usage(rec) = cmd {
                        store.fallback.record(rec);
                    }
                }
            } else {
                // D4: one drain, two transactions — usage batches keep
                // their commit path (and strike counter) untouched;
                // notification commands replay in their own log-only
                // transaction, preserving drain order within each kind.
                let mut usage: Vec<UsageRecord> = Vec::new();
                let mut notifs: Vec<WriteCmd> = Vec::new();
                let mut docs: Vec<WriteCmd> = Vec::new();
                for cmd in cmds {
                    match cmd {
                        WriteCmd::Usage(rec) => usage.push(rec),
                        WriteCmd::DocPut(_)
                        | WriteCmd::DocDeleteKey { .. }
                        | WriteCmd::DocDeleteAll => docs.push(cmd),
                        cmd => notifs.push(cmd),
                    }
                }
                if !usage.is_empty() {
                    store.commit_batch(&conn, usage);
                }
                if !notifs.is_empty() {
                    store.commit_notifications(&conn, notifs);
                }
                if !docs.is_empty() {
                    store.commit_documents(&conn, docs);
                }
            }
        }
        // Retention check (D7): hourly, and always on the quitting
        // iteration — that is what makes flush_blocking's final prune
        // deterministic.
        let prune_due = quitting || last_prune.is_none_or(|at| at.elapsed() >= PRUNE_INTERVAL);
        if prune_due {
            // The capture clock (docs/0081 §12.1): a seeded ledger
            // anchored to DASHBOARD_FIXED_NOW must not age out of the
            // window on a later calendar day; the wall clock otherwise.
            match prune_once(&conn, crate::freshness::clock_epoch_ms()) {
                // Eviction alters the retained set — the token moves
                // (docs/0057 D7, docs/0054 D9's contract).
                Ok(n) if n > 0 => {
                    store.high_water.fetch_add(1, Ordering::Relaxed);
                }
                Ok(_) => {}
                Err(e) => warn!("usage store: prune failed ({e})"),
            }
            // 0063 D6: the notification cap rides the same pass —
            // drop-oldest past 500. Log-only failures, and eviction
            // here never moves the usage mutation token (docs/0054 §9's
            // contract is usage-scoped).
            if let Err(e) = prune_notifications(&conn) {
                warn!("app db: notification prune failed ({e})");
            }
            if let Err(e) = prune_documents(&conn) {
                warn!("app db: document prune failed ({e})");
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

/// docs/0063 D6: keep the newest [`NOTIF_CAP`] unread rows. Unread has
/// no time TTL — it dies by being read — so only the absolute cap
/// bounds the table.
fn prune_notifications(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM notification_log WHERE id NOT IN \
         (SELECT id FROM notification_log ORDER BY id DESC LIMIT ?1)",
        params![i64::try_from(NOTIF_CAP).unwrap_or(i64::MAX)],
    )
}

/// docs/0072 D8: keep the newest [`GITHUB_DOC_CAP`] rows by touch —
/// the table mirrors the store's working-set cap (docs/0072 D9), not
/// an archive. Rides the hourly pass like the notification cap.
fn prune_documents(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM github_documents WHERE id NOT IN \
         (SELECT id FROM github_documents ORDER BY touched_ms DESC, id DESC LIMIT ?1)",
        params![i64::try_from(GITHUB_DOC_CAP).unwrap_or(i64::MAX)],
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

/// D4 + docs/0063 D2: `user_version` gates one centrally-ordered,
/// additive-only chain. Every step is idempotent (`IF NOT EXISTS`), so
/// a crash mid-migration re-runs cleanly; a v1 install migrates to v2
/// in place — zero data movement, `usage_records` untouched.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if v < 1 {
        conn.execute_batch(SCHEMA_V1)?;
    }
    if v < 2 {
        conn.execute_batch(SCHEMA_V2)?;
    }
    if v < 3 {
        conn.execute_batch(SCHEMA_V3)?;
    }
    debug_assert_eq!(v.max(3), SCHEMA_VERSION);
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
        "local" => Transport::Local, // docs/0064 D9
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

/// A persisted notification row (docs/0063 D3). Unknown level names
/// read as Info — we only ever write `Level::name()` values, so this is
/// defensive: severity degrades, the unread alert never drops.
fn dec_notif_row(row: &rusqlite::Row) -> rusqlite::Result<PendingNotification> {
    Ok(PendingNotification {
        raised_at_ms: dec_ms(row.get(0)?),
        level: Level::from_name(&row.get::<_, String>(1)?).unwrap_or(Level::Info),
        source: row.get(2)?,
        title: row.get(3)?,
        message: row.get(4)?,
        dedupe_key: row.get(5)?,
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

    /// A minimal document row: optional detail layer (`with_detail`)
    /// carrying a recognizable marker.
    fn doc(kind: &str, repo: &str, number: i64, with_detail: bool) -> StoredDocument {
        StoredDocument {
            kind: kind.into(),
            repo: repo.into(),
            number,
            summary_at_ms: now_epoch_ms(),
            detail_at_ms: with_detail.then(now_epoch_ms),
            summary_json: format!("\"{kind}-{repo}#{number}\""),
            detail_json: with_detail.then(|| format!("\"detail-{repo}#{number}\"")),
        }
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
        // The durable key is engine-assigned at commit (docs/0057 §9):
        // first arrival in a fresh table gets 1, whatever the session's
        // in-memory stamp was. Every other field must roundtrip exactly.
        assert_eq!(rows[0].seq, 1);
        let mut expected = full_row(ts, 7);
        expected.seq = rows[0].seq;
        assert_eq!(format!("{:?}", rows[0]), format!("{:?}", expected));
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
    fn migration_creates_schema_and_rows_survive_reopen() {
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
    fn v2_install_migrates_and_keeps_notification_rows() {
        let path = temp_db("v2-migrate");
        {
            // A genuine v2 install: schemas 1+2, stamped v2.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute_batch(SCHEMA_V2).unwrap();
            conn.execute(
                INSERT_NOTIF_SQL,
                params![
                    enc_ms(now_epoch_ms()),
                    "warning",
                    "test",
                    None::<&str>,
                    "m",
                    None::<&str>
                ],
            )
            .unwrap();
        }
        let store = SQLiteStore::open_at(path.clone());
        assert_eq!(user_version(&path), SCHEMA_VERSION);
        // The new table is live. One flush per store lifetime (the
        // quit-flush is the only deterministic drain) — every buffered
        // command rides it, and reads after it are the durable truth.
        store.put_document(doc("pr", "acme/one", 1, true));
        store.flush_blocking();
        assert_eq!(store.load_unread().len(), 1, "v2 row survives v3");
        assert_eq!(store.load_documents().len(), 1);
    }

    #[test]
    fn document_round_trip_upsert_and_layers() {
        let store = SQLiteStore::open_at(temp_db("doc-roundtrip"));
        // Drain order is commit order (D3): the upsert's second put
        // lands after the first, so the final row is the stamp-less
        // one — a re-land that voided the detail.
        store.put_document(doc("pr", "acme/one", 7, true));
        store.put_document(doc("issue", "acme/one", 12, true));
        store.put_document(doc("pr", "acme/one", 7, false));
        store.flush_blocking();
        let rows = store.load_documents();
        assert_eq!(rows.len(), 2);
        let pr = rows.iter().find(|r| r.kind == "pr").unwrap();
        assert_eq!(pr.repo, "acme/one");
        assert_eq!(pr.number, 7);
        assert_eq!(pr.summary_json, "\"pr-acme/one#7\"");
        assert!(
            pr.detail_json.is_none(),
            "stamp-less detail persists summary-only"
        );
        assert!(pr.detail_at_ms.is_none());
        let issue = rows.iter().find(|r| r.kind == "issue").unwrap();
        assert_eq!(issue.number, 12);
        assert!(issue.detail_json.is_some());
        assert!(issue.detail_at_ms.is_some());
    }

    #[test]
    fn document_deletes_key_then_all() {
        let store = SQLiteStore::open_at(temp_db("doc-delete"));
        store.put_document(doc("pr", "acme/one", 7, true));
        store.put_document(doc("issue", "acme/one", 12, true));
        store.delete_document("pr", "acme/one", 7);
        store.delete_documents();
        // All three commands replay in drain order: the puts insert,
        // the key-delete removes one, the all-delete clears the rest.
        store.flush_blocking();
        assert!(store.load_documents().is_empty());
    }

    #[test]
    fn document_table_prunes_to_the_cap_oldest_arrival_first() {
        let store = SQLiteStore::open_at(temp_db("doc-prune"));
        for n in 0..(GITHUB_DOC_CAP as i64 + 1) {
            store.put_document(doc("issue", "acme/many", n, false));
        }
        // The quitting flush runs drain -> commit -> prune in one
        // iteration, so the prune is deterministic on return.
        store.flush_blocking();
        let rows = store.load_documents();
        assert_eq!(rows.len(), GITHUB_DOC_CAP);
        // Arrival order breaks equal-touch ties: row 0 arrived first
        // and is the one that went.
        assert!(rows.iter().all(|r| r.number != 0));
        assert!(rows.iter().any(|r| r.number == GITHUB_DOC_CAP as i64));
    }

    #[test]
    fn document_reads_empty_when_degraded() {
        let store = SQLiteStore::open_at(temp_db("doc-degraded"));
        store.degrade();
        assert!(store.load_documents().is_empty(), "degraded reads empty");
        store.put_document(doc("pr", "acme/two", 8, true));
        store.flush_blocking();
        assert!(store.load_documents().is_empty(), "degraded writes drop");
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

    /// The shipped-bug regression (docs/0057 §9): a fresh session's
    /// Recorders stamp `seq` from 1 again while the persisted table
    /// already holds those keys — every boot after the first used to
    /// trip `UNIQUE constraint failed: usage_records.seq` on flush and
    /// degrade to memory. The durable key is engine-assigned, so the
    /// reopen must flush cleanly.
    #[test]
    fn reopen_reuses_in_memory_seqs_without_collision() {
        let path = temp_db("reopen-collision");
        let t0 = now_epoch_ms();
        {
            let store = SQLiteStore::open_at(path.clone());
            for (off, seq) in [(100, 1), (200, 2), (300, 3)] {
                store.record(row(t0 + off, seq));
            }
            store.flush_blocking();
        }
        // A "second session": the same stamps a fresh Recorder emits.
        let store = SQLiteStore::open_at(path);
        for (off, seq) in [(400, 1), (500, 2), (600, 3)] {
            store.record(row(t0 + off, seq));
        }
        store.flush_blocking();
        assert!(!store.is_degraded(), "no flush failures on reopen");
        assert_eq!(store.records_since(0).len(), 6);
    }

    /// `ORDER BY seq` is the documented arrival-order contract
    /// (docs/0054 §9) — including across restarts, where per-session
    /// in-memory stamps restart at 1. The engine-assigned durable key
    /// keeps a global arrival order.
    #[test]
    fn seq_order_matches_arrival_order_across_restart() {
        let path = temp_db("restart-order");
        let t0 = now_epoch_ms();
        {
            let store = SQLiteStore::open_at(path.clone());
            store.record(row(t0 + 100, 1));
            store.record(row(t0 + 200, 2));
            store.flush_blocking();
        }
        let store = SQLiteStore::open_at(path);
        store.record(row(t0 + 300, 1));
        store.flush_blocking();
        let rows = store.records_since(0);
        let ts: Vec<u64> = rows.iter().map(|r| r.ts_epoch_ms).collect();
        assert_eq!(ts, vec![t0 + 100, t0 + 200, t0 + 300]);
        let seqs: Vec<u64> = rows.iter().map(|r| r.seq).collect();
        assert!(
            seqs.windows(2).all(|w| w[0] < w[1]),
            "durable keys strictly increase in arrival order: {seqs:?}"
        );
    }

    /// In-memory `seq` is session-local identity (docs/0053 D3):
    /// multiple Recorders (today's module plus any future one) can
    /// stamp the same value within one session. The durable key must
    /// not care.
    #[test]
    fn duplicate_in_memory_seqs_do_not_collide() {
        let store = SQLiteStore::open_at(temp_db("dup-seqs"));
        let t0 = now_epoch_ms();
        store.record(row(t0 + 100, 1));
        store.record(row(t0 + 200, 1));
        store.flush_blocking();
        assert!(!store.is_degraded());
        assert_eq!(store.records_since(0).len(), 2);
    }

    // --- notification log (docs/0063) --------------------------------

    fn notif(tag: &str, key: Option<&str>) -> PendingNotification {
        PendingNotification {
            level: Level::Warning,
            source: "github".into(),
            title: Some(format!("t-{tag}")),
            message: format!("m-{tag}"),
            dedupe_key: key.map(|k| k.to_string()),
            raised_at_ms: now_epoch_ms(),
        }
    }

    fn user_version(path: &Path) -> i64 {
        Connection::open(path)
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn fresh_db_lands_at_head_with_every_table() {
        let path = temp_db("fresh-head");
        let store = SQLiteStore::open_at(path.clone());
        assert!(!store.is_degraded());
        store.flush_blocking();
        assert_eq!(user_version(&path), SCHEMA_VERSION);
        let tables: i64 = Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' \
                 AND name IN ('usage_records','notification_log','github_documents')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 3);
    }

    #[test]
    fn v1_install_migrates_in_place_usage_rows_survive() {
        let path = temp_db("v1-migrate");
        let t0 = now_epoch_ms();
        {
            // A genuine v1 install: only the v1 schema, stamped v1.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute(
                INSERT_SQL,
                params![
                    enc_ms(t0),
                    "github",
                    "rest",
                    "op",
                    "surface",
                    "manual",
                    None::<&str>,
                    200,
                    0,
                    None::<i64>,
                    None::<i64>,
                    None::<i64>,
                    None::<i64>,
                    5,
                    None::<i64>,
                    None::<i64>,
                    0
                ],
            )
            .unwrap();
            assert_eq!(user_version(&path), 1);
        }
        // Open with the current store: in-place bump to v2, usage rows
        // untouched (D2 — zero data movement), notification table live.
        let store = SQLiteStore::open_at(path.clone());
        assert!(!store.is_degraded());
        store.insert(notif("migrated", None));
        store.flush_blocking();
        assert_eq!(user_version(&path), SCHEMA_VERSION);
        assert_eq!(store.records_since(0).len(), 1, "v1 usage row survived");
        assert_eq!(store.load_unread().len(), 1);
    }

    #[test]
    fn notification_roundtrip_preserves_every_field() {
        let store = SQLiteStore::open_at(temp_db("notif-roundtrip"));
        let ts = now_epoch_ms() - 1234;
        let row = PendingNotification {
            level: Level::Error,
            source: "github".into(),
            title: None,
            message: "m-full".into(),
            dedupe_key: Some("incident-1".into()),
            raised_at_ms: ts,
        };
        store.insert(row.clone());
        store.insert(PendingNotification {
            level: Level::Success,
            source: "app".into(),
            title: Some("t".into()),
            message: "m2".into(),
            dedupe_key: None,
            raised_at_ms: ts + 1,
        });
        store.flush_blocking();
        let back = store.load_unread();
        assert_eq!(back.len(), 2);
        // Newest first (D5's contract) — the caller reverses.
        assert_eq!(back[0].message, "m2");
        assert_eq!(back[1], row, "every field round-trips, NULLs included");
    }

    #[test]
    fn notification_delete_all_and_by_key() {
        let path = temp_db("notif-delete");
        // One flush per store lifetime (quit semantics) — a second
        // delete round reopens the file, which is exactly the restart
        // shape D7 describes.
        let store = SQLiteStore::open_at(path.clone());
        store.insert(notif("a", Some("k1")));
        store.insert(notif("b", Some("k1")));
        store.insert(notif("c", Some("k2")));
        // D7: key-mark deletes the whole incident — insert and delete
        // replay in drain order within one wake.
        store.delete_by_key("k1");
        store.flush_blocking();
        drop(store);
        let store = SQLiteStore::open_at(path.clone());
        let back = store.load_unread();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].dedupe_key.as_deref(), Some("k2"));
        // D3: panel open deletes everything.
        store.delete_all();
        store.flush_blocking();
        drop(store);
        assert!(SQLiteStore::open_at(path).load_unread().is_empty());
    }

    #[test]
    fn notification_commands_commit_in_drain_order() {
        let store = SQLiteStore::open_at(temp_db("notif-order"));
        // Insert, acknowledge (delete-all), insert again: the second
        // raise happened *after* the panel opened and must survive —
        // the writer replays commands in exact order (D4).
        store.insert(notif("before", None));
        store.delete_all();
        store.insert(notif("after", None));
        store.flush_blocking();
        let back = store.load_unread();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].message, "m-after");
    }

    #[test]
    fn notification_cap_keeps_newest_500() {
        let path = temp_db("notif-cap");
        let store = SQLiteStore::open_at(path.clone());
        let t0 = now_epoch_ms();
        for i in 0..=NOTIF_CAP + 4 {
            store.insert(PendingNotification {
                level: Level::Info,
                source: "app".into(),
                title: None,
                message: format!("m{i}"),
                dedupe_key: None,
                raised_at_ms: t0 + i as u64,
            });
        }
        // The quit-style flush drains → commits → prunes deterministically.
        store.flush_blocking();
        let count = |sql: &str| -> i64 {
            Connection::open(&path)
                .unwrap()
                .query_row(sql, [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(
            count("SELECT COUNT(*) FROM notification_log"),
            NOTIF_CAP as i64,
            "the cap bounds the table (D6)"
        );
        assert_eq!(
            count(
                "SELECT COUNT(*) FROM notification_log WHERE message IN ('m0','m1','m2','m3','m4')"
            ),
            0,
            "drop-oldest past the cap"
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM notification_log WHERE message = 'm504'"),
            1,
            "the newest rows stay"
        );
    }

    #[test]
    fn notification_failures_never_degrade_or_pause_usage() {
        let path = temp_db("notif-fail");
        let store = SQLiteStore::open_at(path.clone());
        store.insert(notif("x", None));
        store.flush_blocking();
        assert_eq!(store.load_unread().len(), 1);
        drop(store);
        // Kill the table out from under a fresh store (migrate sees
        // user_version = 2 and does not recreate it): every
        // notification batch and read now fails while usage commits
        // keep succeeding — both in the same failed wake (D4).
        Connection::open(&path)
            .unwrap()
            .execute("DROP TABLE notification_log", [])
            .unwrap();
        let store = SQLiteStore::open_at(path.clone());
        store.insert(notif("y", None));
        store.record(near_now(50, 1));
        let before = store.high_water();
        store.flush_blocking();
        assert!(
            !store.is_degraded(),
            "notification failures are log-only (D4)"
        );
        assert_eq!(store.records_since(0).len(), 1, "usage unaffected");
        assert!(
            store.high_water() > before,
            "usage commits keep moving the token"
        );
        assert!(
            store.load_unread().is_empty(),
            "failed read = empty, no panic"
        );
    }

    #[test]
    fn load_unread_restores_at_most_100_newest_first() {
        let store = SQLiteStore::open_at(temp_db("notif-limit"));
        let t0 = now_epoch_ms();
        for i in 0..NOTIF_RESTORE_LIMIT + 20 {
            store.insert(PendingNotification {
                level: Level::Info,
                source: "app".into(),
                title: None,
                message: format!("m{i}"),
                dedupe_key: None,
                raised_at_ms: t0 + i as u64,
            });
        }
        store.flush_blocking();
        let back = store.load_unread();
        assert_eq!(back.len(), NOTIF_RESTORE_LIMIT, "the restore window (D5)");
        assert_eq!(
            back[0].message,
            format!("m{}", NOTIF_RESTORE_LIMIT + 19),
            "newest first"
        );
        assert_eq!(
            back[NOTIF_RESTORE_LIMIT - 1].message,
            "m20",
            "oldest kept is #20"
        );
    }
}
