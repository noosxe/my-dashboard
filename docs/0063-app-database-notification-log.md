# 0063 — The app database (generalizing `usage.db`) + notification log persistence

Status: design for review. Implements #387 — the storage round deferred by
docs/0059 §8 — and generalizes the docs/0057 sink so any persistent data has
one home. Tracking: implementation issue filed on merge of this doc.

## 1. Problem

Two problems, one PR's worth of work:

1. **#387**: the notification log lives on the `ToastHub` entity and dies
   with the process (docs/0058 D9, docs/0059 D5). If the app restarts —
   update, crash, reboot — every unread notification is silently gone: the
   badge reboots at zero and "what did I miss" has no answer. The issue
   names four policy holes: storage decision, retention/TTL, read-state
   semantics, and the degrade-latch interaction.
2. **Owner direction**: the only durable SQLite store we have is
   analytics-scoped (docs/0057: `usage.db`, one table, one consumer).
   Rather than inventing a second engine, file, writer thread, and degrade
   story per future need, the DB becomes **the app database**: shared
   migrations, shared writer, shared failure handling — the notification
   log is its second table and the proof of the general shape.

## 2. Verified facts (current tree, `crates/core/src/usage_sqlite.rs`, 985 lines)

- **F1 — store shape (docs/0057 D3–D9, all shipped)**: `SQLiteStore` =
  mutex-guarded buffer + one dedicated `std::thread` (1 s condvar wake,
  drain-by-swap, transaction outside the lock) — `record()` never waits on
  I/O. WAL + `synchronous=NORMAL`, one writer connection, one lazy reader,
  2 s busy timeout. Retention: 90 days, pruned hourly on the writer thread
  only. Quit-time `flush_blocking` beside the drafts flush in `main`.
- **F2 — migrations (D4)**: `PRAGMA user_version` gates additive steps;
  v0/v1 → `SCHEMA_V1`. `user_version` is **per database file**, so a file
  shared by multiple concerns needs one centrally-ordered chain.
- **F3 — the §9 errata precedent**: durable keys are engine-assigned
  rowids (`INSERT` omits `seq`), globally monotonic across restarts,
  arrival-ordered by the single writer. The same rule applies to any new
  table — never trust caller-stamped keys.
- **F4 — degrade (D6)**: open failure, 3 consecutive failed batches, or a
  failed read flip to a `MemorySink` + `is_degraded()`; the transition
  raises exactly one toast through the hub spill (`ToastSlot` latch,
  dedupe key `usage-store-degraded`, docs/0058 D7).
- **F5 — seams**: `UsageSink`/`UsageSource`/`UsageStore` traits
  (`usage_records.rs`); core's `init` stays `MemorySink`, the app opts in
  (`main.rs:98–119`: `open_or_fallback`, `attach_toast_spill`,
  quit-time `flush_blocking`); analytics reads `records_between` directly
  on the UI thread (docs/0054 §8).
- **F6 — the hub side**: entries append at exactly two ingest choke points
  (post-0062: after the mute filter), `LOG_CAP` 100 drop-oldest; read
  flips are human actions only (panel open, per-row ✕, clear-all,
  `mark_read_by_key`); the panel shows the open-time **unread** snapshot —
  read entries have no UI surface after their panel closes; `raised_at` is
  an `Instant`; re-raises under the same dedupe key append as separate
  entries and flip together on key-mark.
- **F7 — persistence seams in the tree**: settings/drafts/cache are JSON
  files under `state/` (docs/0004); nothing else uses SQLite. docs/0057 §7
  explicitly deferred "adopting the sink in other modules" — this design
  lifts that non-goal by owner direction.

## 3. Decisions

- **D1 — one database, same file: `usage.db` is the app database.** New
  tables live beside `usage_records`. No rename to `app.db`: the filename
  is install-visible and renaming buys a file-move migration for zero
  function. The module renames to `app_db.rs` (the name tells the truth);
  the `SQLiteStore` type keeps its name. "Usage DB" in older docs means
  this file — a pointer note says so.
- **D2 — migrations: one ordered chain, additive-only.** `migrate()` walks
  centrally-registered steps: v1 (historical, `usage_records`), v2
  (`notification_log`, this design). Each concern contributes its step;
  no concern ever bumps `user_version` alone. A v1 install migrates to v2
  in place — zero data movement, `usage_records` untouched.
- **D3 — the `notification_log` table (v2): a pending-unread queue.**
  Columns: `id INTEGER PRIMARY KEY` (engine-assigned rowid, F3),
  `raised_at_ms INTEGER NOT NULL`, `level TEXT NOT NULL`,
  `source TEXT NOT NULL` (effective id), `title TEXT NULL`,
  `message TEXT NOT NULL`, `dedupe_key TEXT NULL`. **Only unread entries
  are persisted**: the panel's read rows have no surface after close
  (F6), so "read" is **delete** — `UPDATE`s and a read-state column don't
  exist. Human actions map to three statements: panel open → `DELETE
  ALL`; per-key mark → `DELETE WHERE dedupe_key = ?`; (per-row ✕ /
  clear-all operate on already-read rows — no-ops here). Muted sources
  never insert (0062 D3 drops them before `append_log`, which is the
  single hook that also feeds this table).
- **D4 — writes ride the existing writer thread.** The buffer becomes a
  command enum — `WriteUsage(Vec<UsageRecord>)` |
  `WriteNotification(row)` | `DeleteNotifications{by_key, all}` — drained
  by the same 1 s wake; usage batches keep their single-transaction
  commit, notification commands batch into their own transaction per
  wake. The `record()`-style latency contract holds: hub-side calls are a
  lock + push, never I/O. **Failure isolation**: usage keeps its
  3-strike→degrade rule untouched; notification write failures are
  log-only, best-effort — the in-memory hub stays the session's source of
  truth, and a notification failure must never pause usage persistence
  (or toast — the store-level degrade toast already covers a dead
  database).
- **D5 — boot restore: one indexed query, best-effort.** `main` opens the
  store (as today), then `hub.restore_unread(store.load_unread())`
  **before the window exists**: `SELECT … ORDER BY id DESC LIMIT 100`
  (newest first, reversed into the log — `LOG_CAP` semantics on restore).
  The badge's first frame reflects restored unread. Query failure → start
  empty (exactly today's behavior); restore never blocks boot beyond the
  one sub-ms indexed read (the docs/0054 §8 read precedent).
- **D6 — retention and TTL (the #387 policy answer).** Unread rows have
  **no time-based expiry** — "you missed it" is the feature; they die by
  being read (deleted). Absolute cap: **500 rows**, drop-oldest inside the
  existing hourly prune pass (a never-opened panel can't grow the file
  forever; drop-oldest losing an unread mirrors today's in-memory
  `LOG_CAP` semantics, just at a larger bound). No read-TTL exists because
  no read rows exist (D3).
- **D7 — read-state semantics (the #387 question): read/unread survives
  restarts.** Restarting must not re-nag what was dismissed: the persisted
  set *is* the unread set. `mark_read_by_key` (docs/0060 D3, OS-banner and
  toast-click marks) persists by the same key-delete. Re-raise of a
  previously-read incident after restart is a new row — the same
  one-entry-per-raise, key-marks-flip-together semantics as in-session
  (F6), no special casing.
- **D8 — degrade-latch interaction (the #387 guardrail).** An unwritable
  dir at boot fails `open_or_fallback` exactly as today: one degrade
  toast (the condition *re-occurred* — correct per #387's "unless it
  re-occurs"), hub runs ephemeral, nothing restored, nothing extra
  raised. The notification-persistence layer adds **no toasts of its
  own** in any failure mode (D4). A degrade that heals (dir becomes
  writable) needs no persisted latch: the next boot restores whatever the
  log holds.
- **D9 — `Instant` vs wall clock.** Rows persist `raised_at_ms`
  (epoch millis). On restore, `raised_at = Instant::now() −
  `Duration::from_millis(elapsed)` — clamped so a clock skew backwards
  can't panic; `relative_time` keeps operating on `Instant` unchanged.
- **D10 — the seam: a small trait, defaulting to no-op.**
  `NotificationLog` (in `toasts.rs`, `Send + Sync`, no gpui types — the
  docs/0057 seam discipline): `insert(row)`, `delete_by_key(key)`,
  `delete_all()`, `load_unread()`. The hub holds an `Arc<dyn
  NotificationLog>` defaulting to `NoopLog` (tests and core `init`
  unchanged in semantics); `main` attaches the store's impl. Row type is
  plain data (`PendingNotification`: level, source, title, message,
  dedupe_key, raised_at_ms).

## 4. File layout

- `crates/core/src/usage_sqlite.rs` → **`crates/core/src/app_db.rs`**
  (git mv; type names unchanged): buffer → command enum; v2 migration +
  `notification_log` DDL + the three statements; `load_unread` restore
  query; the 500-row prune step riding the hourly pass;
  `NotificationLog` impl for `SQLiteStore`; failure isolation per D4.
- `crates/core/src/toasts.rs` — `NotificationLog` trait + `NoopLog` +
  `PendingNotification`; hub: `attach_log`, insert-at-`append_log`,
  key-delete in `mark_read_by_key`, `delete_all` in `open_panel`,
  `restore_unread` (D5/D9 mapping).
- `crates/app/src/main.rs` — attach the log seam beside
  `attach_toast_spill`; restore before the window.
- Docs amendments riding the **implementation** PR (0057 §8 precedent):
  0057 D2/D4 amendments + §7 non-goal lifted; 0059 D5 superseded; 0054
  §8 note (read contract unchanged); README rows.

## 5. Tests

- Store: fresh DB lands at v2 with both tables; a v1 install migrates
  (usage rows survive, `user_version=2`); notification round-trip
  preserves every field (incl. `source`, `title`/`dedupe_key` NULLs);
  engine-assigned ids stay monotonic across restarts; `delete_all` /
  key-delete; the 500 cap drops oldest in the prune pass; notification
  write failures never flip `is_degraded` and never strike out usage.
- Hub: default `NoopLog` keeps every existing test's semantics; insert
  fires at both ingest paths (and never for muted sources); panel open /
  key-mark reach the seam; restore seeds entries newest-last, badge
  counts restored unread, clamped `raised_at` yields sane
  `relative_time`; failed restore (unusable store) → empty log, no panic.
- Integration (store + hub): raise → flush → reopen → restored; read →
  flush → reopen → gone; re-raise after read → one new row.

## 6. Verification

- Bridge (Linux box): session with an unwritable state dir → the one
  degrade toast, badge from in-session raises only; healthy session →
  raise 2 (one dismissed via panel) → quit → relaunch → badge 1 with the
  survivor listed, ages sane; mute a source → its raises never reappear
  after restart.
- Human checks (owner): restart during everyday use never loses unread;
  clock-skew scenario (change system clock back a day) → ages clamp, no
  panic; `usage.db` inspected after a week stays small (prune works).

## 7. Acceptance criteria

1. Restart preserves unread notifications — badge and panel — with ages
   intact; read/dismissed ones stay gone (#387 headline).
2. `usage.db` is the app's single database; usage analytics behavior is
   byte-identical (v1 data intact; the `ORDER BY seq` read contract,
   docs/0054 §9, unchanged).
3. Existing v1 installs migrate in place; zero data movement.
4. Only human actions clear unread — now across sessions (0059 D2
   extended); muted sources never persist (0062 D3).
5. Failure isolation: notification persistence can't degrade, toast about
   itself, or pause usage capture; a dead DB boots exactly like today
   (ephemeral hub + the one store degrade toast).
6. Bridge script §6 passes on Linux.

## 8. Non-goals (filed as follow-ups unless noted)

- **Migrating existing JSON stores into the DB** (settings, drafts,
  GithubCache) — they work; this design builds the rail, not the
  migration. Candidates file issues when a store actually outgrows its
  file.
- **Visit analytics table** (#334) — separate record type per docs/0053
  §7; it will adopt this rail when designed.
- DB rename to `app.db`, encryption, VACUUM scheduling, multi-process
  access — unchanged from docs/0057 §7.
- Persisting the visible toast queue or OS-notification state — only the
  log persists.
- Cross-device sync of any kind.

## 9. Review resolutions (owner)

1. Numbers stand (D5/D6): 500-row persisted cap; restore limited to the
   newest 100 (`LOG_CAP` semantics).
2. Unread has no time TTL (D6) — an old unread error keeps badging until
   read.
3. Module rename approved (D1): `usage_sqlite.rs` → `app_db.rs`; type
   names unchanged.
4. Existing JSON stores stay put (§8): this design builds the rail, not
   the migrations.
