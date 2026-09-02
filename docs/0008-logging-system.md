# 0008 — Logging System

- **Date:** 2026-09-01
- **Depends on:** 0004 (paths/persistence primitives), 0007 (snapshot
  service whose events need wiring)

## 1. Context & problem

The app is a black box between launches. Diagnostics today are 28 ad-hoc
`eprintln!` call sites (`dashboard: …`, `[github] …` prefixes) that only
appear when the binary happens to be run from a terminal, carry no
timestamps or levels, vanish on close, and say nothing about the healthy
path: what launched, what connected, what fetched, what applied. The
owner's report format has already been "I see no data in the app" —
answering that today means re-instrumenting by hand.

What's needed: real levels, a persisted log with rotation, everything
important wired through one facade — and gpui's own diagnostics, which
we currently cannot see at all.

## 2. Scope

In scope: logging facade + backend choice, sinks (file + stderr),
levels, default/override filtering, the event catalog (what gets logged
where), privacy rules, init/fallback sequence, migration of all
existing `eprintln!`s.

Out of scope: in-app log viewer UI, log shipping/upload, structured
(span-based) tracing, log-based metrics.

## 3. Verified upstream facts

- **gpui emits through the `log` facade** (`log::error!/warn!/debug!/
  trace!` throughout `crates/gpui/src`; `log` is a regular dependency).
  `env_logger` appears only in gpui's *dev*-dependencies. gpui-component
  declares `log` + `tracing` but emits from neither (0 call sites in
  src). Pinned revs: zed `8cfa848c`, gpui-component `1081d2b2`.
- `log` 0.4.34 is already in our lockfile (transitive); adopting the
  facade adds **no new dependency edges** for emitting crates.
- **flexi_logger 0.31.10** (docs.rs, verified): `log`-facade backend;
  env-var-style spec syntax (env_logger-compatible, `RUST_LOG=
  [target][=][level][,...]`); `Logger::log_to_file(FileSpec)` with
  rotation `Criterion::Size(u64)` (bytes) / `Age` / `AgeOrSize`,
  `Cleanup::KeepLogFiles(usize)`; `duplicate_to_stderr(Duplicate)`
  (`None/Error/Warn/Info/Debug/Trace/All`); `WriteMode::Direct` is the
  default (no buffering, no extra thread, real-time, crash-safe);
  `start() -> LoggerHandle` — keep alive, dropping flushes + shuts down.
  **Trap:** `Logger::try_with_env()` logs *nothing* if `RUST_LOG` is
  unset — a default spec must be supplied explicitly.
- **env_logger 0.11.11**: stderr-oriented; file output only via a
  custom `Target::Pipe` writer; **no rotation** (long-open bikeshed,
  rust-cli/env_logger#178).
- **tracing-appender** (tokio-rs): rotation by time only, **no
  size-based rotation** (open issue tokio-rs/tracing#1940); capturing
  `log` records additionally needs the tracing-log bridge.

## 4. Decisions

### D1 — Facade: `log`

All crates emit via `log` macros with default targets (crate/module
path — replaces today's manual `[github]`/`dashboard:` prefixes).
Rationale: gpui's records flow through our subscriber for free —
window, renderer and asset failures become visible without touching
upstream code. `tracing` rejected: our events are point events, not
spans; capturing gpui would need the `tracing-log` bridge; heavier
subscriber tree for no current need. Revisit only if span correlation
becomes a real requirement.

### D2 — Backend: flexi_logger 0.31, file + stderr duplicate

One writer setup, no async:

- `log_to_file(FileSpec::default().directory(paths.logs_dir()).
  basename("dashboard"))` → `<data_dir>/logs/dashboard.log`
- `rotate((Criterion::Size(1 MiB), Naming::Numbers,
  Cleanup::KeepLogFiles(5)))` → hard disk cap ≈ 6 MiB
- `duplicate_to_stderr(Duplicate::Info)` → error/warn/info visible in
  a console when run from one; debug/trace file-only (they'd flood a
  terminal)
- `WriteMode::Direct` (default): crash-safe, real-time, no thread

env_logger rejected (no file path, no rotation — §3). tracing-appender
rejected (no size rotation; bridge needed for gpui records — §3).

### D3 — Default spec + `RUST_LOG` override

Built-in default spec string:

```
warn,dashboard_app=info,dashboard_core=info,dashboard_module_github=info
```

Global floor is `warn` so gpui/gpui-component warnings land without
their debug noise flooding the file. Our three crates log `info` by
default. `RUST_LOG` (flexi/env_logger syntax) overrides the whole spec
when set; a parse failure falls back to the default with a stderr note
— **logging misconfiguration never blocks boot**. Because
`try_with_env()` alone logs nothing when the variable is unset (§3
trap), init reads the variable itself and dispatches to
`Logger::try_with_str` (pinned: spec-string constructor,
`FlexiLoggerError::Parse` on malformed input).

### D4 — Storage location: `AppPaths::logs_dir()` = `data_dir()/logs`

Logs are app-level diagnostics, not module state and not re-derivable
cache — `data_dir()/logs` (the same convention Zed uses) with the 6 MiB
cap from D2 bounding growth. `paths.rs` gains the accessor; module
sandboxes are untouched.

### D5 — Init order and failure policy

`main()` sequence: `AppPaths::init()` → **`logging::init(paths)`** →
`gpui_component::init` → settings → window/modules. Everything after
logging init emits through the facade; `AppPaths::init`'s own failure
notes stay on stderr (pre-logger). The `LoggerHandle` lives in `main`
for the process lifetime (drop = flush + shutdown, §3).

Fallback ladder: logs-dir creation or logger start fails → retry as
stderr-only logger (`log_to_stderr`); that fails too → eprintln note
and run unlogged. The app must always start.

### D6 — Hot-path rule

No log macros in gpui render functions or anything invoked per-frame.
Logging lives in service/state-mutation paths (which already run off
the render path). Disabled levels still cost a branch — the rule keeps
even that out of layout/paint.

### D7 — Privacy: what never gets logged

- **Token material** — never, at any level (keychain-only policy,
  0005 §3.2 corollary)
- **Device-flow user code** — only the fact that it was displayed
  (interval, expiry); the code itself stays out
- **Content fields** (PR/issue titles, notification subjects/bodies) —
  never; identifiers are enough to debug: repo slug, number, counts

## 5. Event catalog (the wiring)

| Area | Event | Level |
| --- | --- | --- |
| boot | app start: version, data dir, logs dir, RUST_LOG set? | info |
| boot | settings load fallback (missing/corrupt → default) | warn |
| boot | settings applied: theme name + mode | debug |
| boot | window opened; module installed (id) | info / debug |
| persistence | cache load: key, hit / miss / quarantined | debug / warn on quarantine |
| persistence | cache save: key, bytes, atomic-rename result | debug / warn on failure |
| github auth | device flow started (interval, expires_in) | info |
| github auth | token acquired (login); connected | info |
| github auth | token rejected / disconnect | warn |
| github auth | token refresh due (expiry, reason proactive/reactive-401) | debug |
| github auth | token refreshed (new expiry) | info |
| github auth | token refresh failed (reason) — reconnect required | warn |
| github auth | legacy non-expiring token loaded (no refresh) | debug |
| github fetch | GraphQL request: kind, cursor?, rate-limit budget before | debug |
| github fetch | GraphQL response: status, ms, cost, remaining, items-in-page | debug |
| github fetch | REST request/response: path, page, etag?, 304 vs 200, count | debug |
| github fetch | network error (kind, message) | warn |
| github apply | snapshot applied: N repos, M open PRs (today's landing note) | info |
| github apply | repo-snapshot applied: repo, prs | info |
| github apply | notifications applied: N unread | debug |
| github apply | per-node [DG] skip (malformed node, key) | warn |
| github apply | envelope errors[] entries (message) | warn |
| github apply | rate-limit gate engaged (until ISO) / resumed | warn / info |
| github svc | repo selected (key); manual refresh; bootstrap run | debug |
| github svc | tick plan (tick, kinds) | trace |
| third-party | gpui / gpui-component records | pass-through at global floor |

Every existing `eprintln!` site maps to a row above (degradation notes
→ warn; load-failure notes → warn; settings fallback → warn). The 28
sites migrate in the implementation PR; no `eprintln!` remains outside
the pre-logger boot path and the fallback ladder.

## 6. Testing

- Unit: default-spec string parses; `RUST_LOG` dispatch (set/unset/
  malformed → expected spec choice); `logs_dir()` layout mirrors 0004
  tests
- Behavior tests are unchanged — they assert state, not stderr; the
  [DG] degradation tests keep asserting stale-but-present
- Asserting emitted lines (test `LogWriter`) is a follow-up if a test
  ever needs message-level guarantees

## 7. Acceptance criteria

- Fresh launch writes `<data_dir>/logs/dashboard_rCURRENT.log` (the live
  file under `Naming::Numbers`; rotated files are `dashboard_r0000N.log`)
  with the boot info lines; closing the app leaves a complete file
- Run from a terminal: error/warn/info mirror to stderr; `RUST_LOG=
  dashboard_module_github=debug` shows fetch request/response lines
- A gpui warning (e.g. forced by a broken asset path) appears in the
  log
- Log dir grows to the rotation cap then holds steady (≈6 MiB, 5
  rotated files + current)
- No token material, user codes, or PR/issue titles appear anywhere in
  the log
- `grep eprintln` in crates returns only the pre-logger boot path and
  the fallback ladder
- Gates green; no behavior change in any existing test

## 8. Human checks

- [ ] Launch from terminal → boot lines visible, log file exists and
      timestamps look sane
- [ ] Let the GitHub module tick → `snapshot applied: N repos, M open
      PRs` lands in file (info, so also on stderr)
- [ ] `RUST_LOG=dashboard_module_github=trace cargo run` → tick plans
      visible; plain `cargo run` → they aren't
- [ ] Kill -9 mid-write, relaunch → file readable, no corruption
      (Direct write mode)
- [ ] `ls` the logs dir after a long session → rotation held the cap

## 9. Follow-ups (GitHub issues, not this PR)

- Message-level log assertions in tests via a test `LogWriter`
- In-app log viewer (settings page surface) — only if debugging
  without a terminal becomes a real need
- Runtime spec change via `LoggerHandle` (e.g. a debug toggle) — defer
  until asked for
