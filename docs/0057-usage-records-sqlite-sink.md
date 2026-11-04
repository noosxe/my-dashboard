# 0057 — Usage records: the SQLite persistence sink (`UsageStore` impl)

*Implements the deferred docs/0053 §7 non-goal; tracks #333. Pre-design decisions
were recorded on #333 (2026-09-27): crate choice
([comment](https://github.com/noosxe/my-dashboard/issues/333#issuecomment-5855840105)),
behavior decisions
([comment](https://github.com/noosxe/my-dashboard/issues/333#issuecomment-5855880028)).
This doc formalizes them and fills in the mechanics.*

## 1. Context & problem

docs/0053 shipped the usage-records pipeline with `MemorySink` — a capped
(`RECORD_CAP = 10_000`, `usage_records.rs:36`) in-memory FIFO — explicitly
deferring persistence: *"the `UsageSink` trait is the seam (a new impl swapped
at `init_with`), and the record shape + `cycle_reset_epoch` denormalization
exist so the migration is a table, not a redesign."*

Consequences today: every restart wipes the log, so the analytics page
(docs/0054) loses all history, and 90 days of quota-cycle context can never
accumulate. This doc designs the SQLite-backed `UsageStore` that becomes the
**default sink at startup**, with retention replacing the memory cap.

Owner decisions (2026-09-27, #333): SQLite is the default sink; retention is
90 days with pruning that never touches the startup path; reads are direct
indexed queries on the UI thread; open/write failures degrade to in-process
memory with the app fully working.

## 2. Facts (receipts)

- **rusqlite 0.40.2 / libsqlite3-sys 0.38.2** (`cargo info`, 2026-09-27).
  The SQLite amalgamation ships inside `libsqlite3-sys`, so the C source is
  pinned by our `Cargo.lock` and identical on every build host.
- **rusqlite ≥0.40 default features changed** (docs.rs feature manifest,
  fetched 2026-09-27): `default = [cache, ffi-sqlite-wasm-rs]` — the default
  FFI backend is now a WASIX-compiled SQLite (`sqlite-wasm-rs` 0.5.1). We opt
  out explicitly.
- **No SQLite dependency exists in the workspace** (Cargo.toml/Cargo.lock
  grep, 2026-09-27) — this is the first one.
- **Nix build**: `flake.nix:91` builds via `rustPlatform.buildRustPackage` +
  `cargoLock`; the amalgamation's C compile runs inside the crate build (cc is
  already required by the gpui stack) — no new Nix inputs.
- **Read side**: the only `UsageSource` consumer is
  `usage_analytics.rs:1066` (`records_between(from, as_of)`), called on the
  UI thread at the analytics page's *explicit* refresh/range change — the
  frozen-`as_of` memoization contract is docs/0054 D10. `snapshot()` has no
  external callers (tests/inspection only).
- **`UsageSource` also carries a monotonic mutation token** ("moves on
  `record` *and* on eviction", `usage_records.rs:122`) that the analytics
  memoization consumes — the SQLite impl must move it too.
- **Quit-flush precedent**: `store.rs:207` `flush_pending_on_quit()`, invoked
  once from `main.rs:102` (docs/0045 §9, #283).
- **No background-thread precedent in core** (`thread::spawn` appears only in
  tests) — this introduces the first one; deliberately a plain
  `std::thread`, not a gpui executor task (see D3).
- **Paths**: `AppPaths::data_dir()` (docs/0004 §4.1): Linux
  `$XDG_DATA_HOME` else `~/.local/share`, macOS `~/Library/Application
  Support`; it resolves but does **not** create the directory
  (`module_data_dir` is the creating variant). docs/0008 D4 is the precedent
  for app-level durable data under the data root.
- **Row shape** = docs/0053 D1 + docs/0054 D14 additive fields
  (`limit_after`, `queue_ms`, `resp_bytes`) — 18 columns, keyed on
  pre-stamped monotonic `seq`.

## 3. Decisions

**D1 — `rusqlite` with `default-features = false, features = ["bundled",
"cache"]`, added to `[workspace.dependencies]`.** `bundled` compiles the
amalgamation (hermetic: same SQLite on macOS dev, Nix, ubuntu CI — no ancient
macOS system sqlite, no pkg-config); `cache` keeps the statement cache so the
flush loop reuses one prepared INSERT. The `sqlite-wasm-rs` default backend
and everything else in the feature matrix stay off. Rejected: sqlx 0.9
(async machinery + MSRV 1.94 for one blocking writer thread), redb 4.3 (KV —
no SQL, voids the 0053 D1 "sqlite-ready row" contract), diesel/sea-orm (ORM
for one INSERT + one range SELECT), system sqlite (version drift), rusqlite
defaults (drags in the WASIX runtime). Version floors ride `Cargo.lock` in
the repo's normal style; bumps are ordinary PRs.

**D2 — Database at `AppPaths::data_dir()/usage.db`** *(amended by
docs/0063: this file is the app database — `notification_log` is its
second table; the module lives at `core/src/app_db.rs`)*.** Linux
`~/.local/share/my-dashboard/usage.db`, macOS
`~/Library/Application Support/my-dashboard/usage.db`. Usage records are
durable app data — not user state (`state/` holds shell.json/settings.json)
and not re-derivable (`cache/`); docs/0008 D4 set the same
durable-under-data precedent for logs. `DASHBOARD_STATE_DIR` redirects the
whole root, so tests, dev runs, and docs/0055 sessions isolate the DB for
free. The sink `create_dir_all`s the data dir on open (the one place it may
touch the filesystem at startup — a local mkdir, not a prune).

**D3 — `SQLiteStore`: mutex buffer + one dedicated `std::thread`.**
`record()` locks a short-lived mutex and appends to a `Vec<UsageRecord>`
buffer — same shape as `MemorySink`'s lock, microsecond-scale, never
blocking on I/O. The writer thread wakes on a 1 s condvar timeout (an
explicit notify covers quit), **drains the buffer by swap**, and commits the
batch in one transaction *outside* the lock — `record()` can never wait on a
commit. A plain `std::thread` is correct here precisely because the seam is
`Send + Sync` with no gpui types: the thread needs no executor, no waker,
and no entity access; a gpui background task would add coupling for nothing.
Buffer bound: 100 000 records with drop-oldest + a `warn!`-counted drop
counter — pathology guard for a dead writer thread, not a working-set cap
(the *retention* cap is D7).

**D4 — Schema v1, additive migrations via `PRAGMA user_version`**
*(amended by docs/0063 D2: the chain is centrally-ordered and shared —
one `user_version` per file; v2 adds `notification_log`).*

```sql
CREATE TABLE IF NOT EXISTS usage_records (
    seq               INTEGER PRIMARY KEY,  -- rowid alias: the pre-stamped key
    ts_epoch_ms       INTEGER NOT NULL,
    module            TEXT NOT NULL,
    transport         TEXT NOT NULL,        -- 'rest' | 'graphql' | 'auth' | 'local' (0064 D9)
    op                TEXT NOT NULL,
    surface           TEXT NOT NULL,
    trigger_kind      TEXT NOT NULL,        -- Rust field `trigger`; TRIGGER is
                                            -- a reserved word in SQL
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
```

Column order mirrors the struct for reviewability; all Option fields are
nullable columns; bools are 0/1. `u64 → i64` casts are safe for every sane
timestamp/seq (post-2262 is someone else's bug) and asserted in the row
codec. Migrations are **additive only** (docs/0054's field additions are the
precedent): `ALTER TABLE ADD COLUMN` + `user_version` bump on open, before
the writer thread starts. A future schema change that cannot be additive is
rejected at design time, not solved here.

**D5 — Connection topology: one writer, one lazy reader, WAL.** The writer
thread owns the only write connection, opened with
`journal_mode = WAL`, `synchronous = NORMAL` (the standard WAL pairing; the
usage log tolerates losing the last transactions to a power cut),
`busy_timeout = 2000`. Reads (`records_between`/`snapshot`) use a second
connection, opened lazily on first read and kept; WAL allows it to read
while the writer commits. No third connection ever exists; there is no
pool.

**D6 — Failure handling: degrade to in-memory, never break fetching.**
Open-time failure (`create_dir_all`, `Connection::open`, pragma/schema
failure) logs `warn!` and returns a `SQLiteStore` in **degraded** mode: the
buffer feeds an internal `MemorySink`, so records and analytics keep working
with persistence paused; `is_degraded()` exposes the state for the future
toast surface (#376). Write-time failure on the flush thread: three
consecutive failed batches flip the same degraded switch (in-memory capture
continues; one rate-limited `warn!` per flip). Consistent with docs/0053 D3
boundary (c) — storage failures are the sink's own.

**D7 — Retention: 90 days, pruned on the writer thread only.**
`DELETE FROM usage_records WHERE ts_epoch_ms < ?` (now − 90 d), checked at
most once per hour *inside the writer thread's loop* — never at startup
(owner constraint: a slow launch is a bug), never on the UI thread. A missed
prune is harmless; the check is one indexed `DELETE` on a table sized in the
hundreds of thousands of rows at worst. The mutation token moves on prune
(eviction alters the retained set — the existing `UsageSource` contract).
*(Capture clock, docs/0081 §12.1: "now" is `freshness::clock_epoch_ms`.
That is the wall clock, pinned under `DASHBOARD_FIXED_NOW` in debug
builds, so a seeded fixture ledger can't age out of the window as the
calendar moves past the pinned instant.)*

**D8 — Read path: direct indexed query on the UI thread.**
`records_between(from, to)` = `SELECT … WHERE ts_epoch_ms >= ? AND
ts_epoch_ms < ? ORDER BY seq` on the lazy reader connection (idx from D4;
`ORDER BY seq` preserves arrival order — the documented contract). The
owner accepted UI-thread blocking here explicitly: at this app's flight
volume the window query is millisecond-scale; "if things ever get slow we
will address it." `to = u64::MAX` (`records_since`) maps to an open upper
bound. `snapshot()` = `records_since(0)`. `u64` boundary values round-trip
through the i64 cast in the row codec.

**D9 — Wiring: the app opts in; core's `init` stays MemorySink.**
`usage_records::init(cx)` is unchanged (tests and non-app binaries keep the
in-memory default — nothing in `cargo test` ever creates a real DB file).
`main.rs` calls `init_with(cx, usage_sqlite::SQLiteStore::open_or_fallback())`
beside the existing `init` at `main.rs:94`. The impl lives in
`crates/core/src/usage_sqlite.rs` (core owns storage, symmetric with
`MemorySink`; `rusqlite` is a `dashboard-core` dependency). Clean-quit
flushing: `main.rs:102`, where `flush_pending_on_quit()` already runs, gains
the one-line `store.flush_blocking()` — the buffer is drained and committed
before the process exits, so a normal quit loses nothing (docs/0045 §9's
quit-time-flush principle, applied to the new store).

**D10 — The mutation token is a commit counter.** `SQLiteStore`'s
`UsageSource` token is an `AtomicU64` bumped by the writer thread after each
successful commit and after each prune; `record()` does not bump it (the
record is not yet durable — the analytics view may legitimately lag the
buffer by the flush interval, and the token moving only on commit keeps the
memoization honest).

## 4. State & plumbing changes

- **core**: new `usage_sqlite.rs` (`SQLiteStore`, row codec, DDL +
  migrations, prune); `rusqlite` in `[workspace.dependencies]` +
  `dashboard-core`; no changes to `usage_records.rs` traits or `MemorySink`.
- **app**: `main.rs` two-line change (D9: `init_with` swap + quit flush).
- **Nothing else**: `Recorder`, the github module, `usage_analytics`, the
  status bar, and the chip are untouched (docs/0053 D5's additivity holds —
  this doc swaps what sits *behind* the seam 0053 designed).

## 5. Testing

- **Roundtrip units** (temp-dir DB via `DASHBOARD_STATE_DIR`-style temp
  root): insert N rows → `records_between` returns them exactly
  (Option/None fields, bool encoding, `trigger_kind`, u64/i64 boundaries at
  `u64::MAX`), half-open window honored both ends, arrival order = `seq`
  order (mirrors `records_between_is_half_open_on_both_ends` /
  `records_between_preserves_arrival_order`).
- **Swap-contract test, real backend** (docs/0053 §5's "storage injection"
  test, now pointed at `SQLiteStore`): same flights → `MemorySink` and
  `SQLiteStore` agree row for row.
- **Migration**: fresh open creates v1 and sets `user_version = 1`; a v1 DB
  reopened is a no-op; the additive `ALTER TABLE` path is exercised by
  opening a hand-built older-version DB (fixture created with raw SQL in the
  test).
- **Prune**: old + recent rows inserted; prune keeps the recent, drops the
  expired, bumps the token; the hourly throttle is a pure function test.
- **Flush/quit**: buffered records survive `flush_blocking`; a batch commit
  happens outside the buffer lock (a slow commit never blocks `record`).
- **Degradation**: unwritable data dir → `is_degraded()`, records land in
  the internal memory sink, app APIs unaffected; three-failure flip covered.
- **Concurrency**: records while the writer sleeps → all arrive within one
  flush; WAL read during an open transaction returns the pre-commit snapshot.
- **CI proves D1's hermeticity**: the ubuntu job compiles and runs the
  bundled C build with no system sqlite and no extra Nix/CI inputs.

## 6. Acceptance

- Launch → `usage.db` exists under the platform data root; the analytics
  page shows history from before a restart (the user-visible point of the
  whole feature).
- Clean quit never loses buffered records (flush-on-quit; test + a kill-free
  manual pass).
- Startup latency unchanged — no prune, no query, no schema work beyond
  `user_version` read on the launch path (owner constraint).
- Unwritable data dir → `warn!` in logs, app fully functional, analytics
  session-local (degraded mode), zero user-facing breakage.
- Gates: `cargo fmt --all --check`, `cargo clippy --workspace
  --all-targets` zero warnings, `cargo test --workspace` green.

## 7. Non-goals

- **Analytics UI changes** — the page reads the same trait; no query-surface
  work here (#324 views, deferred analytics tabs stay theirs).
- **Visit analytics** (#334) — separate record type/table, per docs/0053 §7.
- **Toast integration** (#376) — `is_degraded()` is the hook; wiring the
  alert UI is that issue's work.
- **Encryption (SQLCipher), compaction/VACUUM scheduling, multi-process
  access, Windows** — not needed at this scale; VACUUM can be revisited if
  freelist growth after prunes ever shows up in practice.
- **Adopting the sink in other modules** — Forgejo/Gitea later; D6 of 0053
  keeps the schema module-agnostic already. *(Lifted by docs/0063: the file
  is the general app database — `notification_log` is its second table.)*

## 8. Docs amendments (ride with the implementation PR)

- `docs/README.md`: add the 0057 row (this doc — rides with the design PR).
- docs/0053 §7: the "SQLite persistence" non-goal gains a one-line pointer —
  *designed in docs/0057* — historical text preserved (docs/0027 convention).
- docs/0008 D4 gains no edit (sibling precedent only); docs/0004 §4.1 gains
  no edit (0057 *consumes* the root choice).

## 9. Errata (post-ship fix, 2026-09-27)

**The durable key is engine-assigned at commit, not the pre-stamped in-memory `seq`.**
D3/D4 of docs/0053 make `seq` a *per-Recorder, per-session* stamp (each
Recorder's counter starts at 1 on every boot), but §3's schema made that same
`seq` the persistent `INTEGER PRIMARY KEY` and the flush a plain `INSERT`. Any
launch after the first therefore collided with the previous session's keys —
`UNIQUE constraint failed: usage_records.seq` — and three failed batches
flipped the store to degraded mode with persistence paused. Persistence could
only ever work in the DB's first session; no test caught it because every
test opens a fresh database.

The fix: `INSERT` no longer lists `seq`. As a rowid alias it is assigned by
the engine (`max(rowid)+1`) at commit — globally monotonic across restarts
*and* across modules (the per-session stamps of two Recorders could already
collide inside one session), and arrival-ordered because the single writer
thread inserts batches in drain order. The documented read contract
(`ORDER BY seq` = arrival order, docs/0054 §9) now holds globally instead of
per-session. The in-memory `seq` remains the session-local identity docs/0053
D3 defines — nothing at the `UsageSink` seam changes; the schema comment in
§3 and the roundtrip test's key-identity expectation are corrected by this
errata. Regression tests: reopen with restarted stamps survives, cross-restart
seq order matches arrival order, duplicate in-memory stamps don't collide.

The degrade toast likewise dropped its "check data-dir permissions" hint:
it was written for the open-failure class (unwritable dir), but flush
failures — disk full, constraint errors — surface through the same toast and
the hint sent users looking at the wrong thing. The message is now
class-neutral: *"Usage history can't be saved this session — persistence
paused."*
