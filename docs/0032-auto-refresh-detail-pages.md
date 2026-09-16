# 0032 — Auto-refresh detail pages (heartbeat + focus, #188)

| | |
|---|---|
| Issue | #188 (consumers of 0031's `Fetch` record: also closes #194, refresh-on-focus) |
| Status | design |
| Depends on | 0031 (merged: `Fetch<T>` records on the PR/issue pages, per-key in-flight dedup) |

## 1. Context

Detail pages fetch once on open and never refresh (docs/0029 §2/§4). A PR
left open while a review moves shows frozen comments, checks and state;
the user verifies in the browser, defeating the app. #188 lists three
options — periodic re-fetch, refresh-on-focus, manual refresh. The third
already shipped (`pr-refresh`, `issue-refresh`, `notification-refresh`
buttons). This design adds the first two on top of the 0031 substrate.

Everything needed landed with 0031: each lane has a `Fetch<T>` record
with an apply-time `fetched_at`, an orthogonal `in_flight` flag, and
stale-on-error semantics (fail over landed data keeps the payload and
sets `last_error`). Refreshing is therefore a pure policy question —
*when* to re-dispatch — with no new fetching machinery.

Verified in-tree 2026-09-16:

- **Pages live while open**: the module root holds `self.pr` /
  `self.issue` / `self.notification` as `Option<Entity<…>>` (page.rs:444,
  677; mutual exclusion docs/0020 D1). Closing drops the entity; re-opening
  constructs a fresh one whose `new()` fetches — so "refetch on re-open"
  already exists and needs nothing.
- **The module root outlives visibility**: a page stays alive behind
  other dashboard modules; the nav-active check (`Page::Module("github")`,
  page.rs:248 precedent) is how the root knows it is on screen.
- **Timers**: `BackgroundExecutor::timer(Duration) -> Task<()>`
  (gpui-pre-0.3.3 executor.rs:197); the tick loop already sleeps this way
  (service.rs:2271, `TICK` = 60s).
- **Focus**: `Context::observe_window_activation(window, cb) ->
  Subscription` (gpui-pre-0.3.3 context.rs:444) plus
  `Window::is_window_active()` (window.rs:2664).
- **Manual refresh buttons** exist on all three pages and re-dispatch
  through the same records.

## 2. Goals and non-goals

**Goals**

1. While a detail page is open and its module visible, its lanes
   re-fetch on a steady interval — the page converges to current truth
   without user action.
2. Returning to the window (alt-tab back) refreshes promptly instead of
   waiting out the interval (#194, folded in).
3. A lane that failed comes back on its own: `Failed` records retry on
   the next trigger — stale-on-error becomes self-healing.
4. Zero new fetching state: policy reads only what 0031 shipped
   (`is_loading`, `age`, `last_error`) and re-dispatches through the
   existing page-owned fetch paths.

**Non-goals**

- **A visible staleness indicator / last-synced line** — #192's UI, and
  its absence must not shape this design (the record already exposes
  everything #192 will need).
- **TTL or persisted staleness** — #193; cold-start staleness marking
  stays out.
- **List surfaces** (repo rows, notification rows) — they ride the
  existing snapshot tick; no per-page list refresh here.
- **Backoff / adaptive intervals / pause-while-minimized** — the v1
  heartbeat keeps a fixed cadence (§7d); refinements are deferred until
  the cost is observed, not imagined.
- **Realtime (websockets, polling the check-runs API)** — out of scope
  for a GitHub-app-scale refresh.

## 3. Current mechanics (verified in-tree 2026-09-16)

- PR page lanes: `detail`, `files`, `threads` — three independent
  `Fetch` records; each fetch arms the shared guard synchronously,
  dispatches on the raw lane, clears its own guard (0031 §8, the #222
  correction). Issue page: `detail`. Notification page: **not yet
  adopted** — `NotificationDetailPage` still carries the pre-0031
  `loading: bool` + `load_error: Option<String>` pair and a one-shot
  `run_spec_tracked` GET (notification_page.rs:112, :313); its
  mark/done writes are mutations on the raw lane and stay that way.
- The in-flight guard is skip-never-queue: a second dispatch for a key
  already in flight is a no-op, so any number of refresh triggers can
  race without duplicated requests.
- Rendering during a re-fetch keeps landed data visible
  (stale-while-revalidate falls out of the record's orthogonality) — a
  background refresh must not skeleton-flash the page.

## 4. Design decisions

### D1 — The module root owns one heartbeat; pages expose a refresh entry point

`GithubPage` spawns a single detached loop at construction
(`cx.spawn` + `timer(AUTO_REFRESH_INTERVAL)` per beat, the plan_tick
pattern). Each beat resolves to `auto_refresh_beat(&mut self, cx)`: if
the github module is nav-active **and** a detail page is open, call that
page's `auto_refresh(cx)`; otherwise do nothing.

Per-page loops were rejected: pages stay alive behind other modules, so
a page-owned timer would poll invisibly for as long as the page entity
exists. The root already holds both inputs the gate needs (nav state,
open-page slots) and the `run_specs` precedent already has the root
driving lanes. The loop body is glue; everything testable lives in
`auto_refresh_beat` and the pages' `auto_refresh`.

### D2 — Policy: refresh a lane iff it is idle and (failed or old)

Each page implements `auto_refresh(&mut self, min_age: Duration, cx)`;
per lane it re-dispatches iff

```
!record.is_loading() && (record.load_error().is_some() || record.age() >= min_age)
```

- **Fresh data is never re-fetched** — a trigger that lands on a
  just-loaded lane is a no-op.
- **`Failed` lanes retry** — this is the auto-recovery win: the
  files-pane error row and threads hint row heal themselves on the next
  beat without the user pressing retry.
- The lanes are independent: a PR page refreshes detail, files and
  threads each by their own record; a page showing one fresh lane and
  one failed lane refreshes only the latter.
- The in-flight guard makes any residual race (manual click mid-beat)
  a skip, never a queue.

The dispatches go through the pages' **existing** fetch methods
(page-owned: sync arm, raw lane, own guard clear — 0031 §8); nothing
new is added to the transport path.

### D3 — Focus trigger (#194, folded in)

`GithubPage` subscribes with `observe_window_activation`: on activation
(`window.is_window_active()`), if the module is active and a page is
open, call `auto_refresh` with the smaller `FOCUS_MIN_AGE` threshold.
Rapid alt-tab churn is filtered by the threshold; a page left for more
than `FOCUS_MIN_AGE` is current within one activation. The heartbeat
and focus differ only in the threshold — one policy function, two
constants (D7).

### D4 — Notification page adopts the record (the last 0031 deferral)

`NotificationDetailPage` replaces `loading: bool` +
`load_error: Option<String>` with `Fetch<ThreadDetail>`: its one-shot
GET becomes a page-owned record dispatch (sync arm, raw lane, own guard
clear — identical to the other pages), its error row renders from
`last_error` with the existing retry button re-dispatching, and it
gains the same `auto_refresh` entry point. Mark/done writes stay
mutations on the raw lane (their `MarkState` lifecycles are untouched).

This is the surface 0031 §8 explicitly left for #188; with it, all
three detail pages share one refresh mechanism.

### D5 — Re-open behavior stays as is

Opening a page constructs a fresh entity whose `new()` fetches; closing
and re-opening is already the freshest possible path. No
"page-became-active" navigation signal is added — the window-focus
trigger (D3) covers returning to the app, and re-opening covers
returning to the page.

### D6 — Rendering: stale-while-revalidate, no new UI

Background refreshes keep landed data on screen (0031 D3 fallout) —
the only visible change during a refresh is the existing header
spinner / button spin states. **No new interactive elements**: no
indicator, no toggle (that is #192). The bridge contract is unchanged;
existing selectors (`pr-refresh`, `issue-refresh`,
`notification-refresh`, error rows) keep their ids and behavior.

### D7 — Constants

`AUTO_REFRESH_INTERVAL = 60s` (aligned with the snapshot `TICK` — one
mental model for "how fresh is this app"), `FOCUS_MIN_AGE = 15s`.
Defined once in the page.rs host, documented, referenced by the tests.
At one open page the steady-state cost is ~60 GraphQL reads/hour —
noise against the 5000/h REST+GraphQL budget.

## 5. Testing

Scripted-transport pages (the `ProbeHttp` pattern — always-erroring for
land-lane tests, counting scripted responses for dispatch tests):

1. **Policy table** (`auto_refresh` on each page): idle+stale →
   dispatches; idle+fresh → no request; `Failed` → re-dispatches (and
   the error clears on land); in-flight → no request.
2. **Host beat gating**: module inactive → no page update; page closed
   → no-op; module active + page open → the page's stale lanes get one
   dispatch per beat.
3. **Focus threshold**: age < `FOCUS_MIN_AGE` → no dispatch; age ≥ →
   dispatch; deactivation (blur) events trigger nothing.
4. **Notification adoption round-trip**: scripted thread GET lands into
   the record (`loading`/`load_error` gone from the struct), refresh
   entry point dispatches, and the existing page tests survive the
   rename.
5. Whole-workspace gates stay green; the 0031 suite (transition table,
   dedup, raw-lane) passes untouched.

## 6. Implementation plan

Single issue, ordered so each step ships green:

1. Notification page record adoption (D4) + its tests.
2. `auto_refresh(min_age, cx)` on PR/issue/notification pages + the
   policy table tests (1).
3. Host heartbeat + focus observer + beat/focus gating tests (2, 3).
4. Docs: this file gains the shipped section; README row updates.

## 7. Open questions (owner)

- **(a) 60s heartbeat** — right cadence for "live enough"? Default: yes.
- **(b) 15s focus threshold** — filters alt-tab churn; too aggressive?
  Default: keep.
- **(c) Poll while the window is minimized** — v1 keeps polling (the
  activation trigger would catch up on return anyway; pausing adds a
  state machine for a cost that is ~1 request/60s). Default: keep
  polling.
- **(d) Backoff on repeated failures** — a `Failed` lane retries each
  beat; with the transport down that is one failing request/60s. Default:
  accept for v1; file as deferred if it ever shows in logs.
