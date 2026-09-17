# 0036 — Refresh backoff (+ why pause-while-minimized is rejected) (#225)

| | |
|---|---|
| Issue | #225 (deferred from docs/0032 §2 non-goals, §7c–d) |
| Status | design |
| Depends on | 0031/0032/0035 (merged: `Fetch<T>` records, heartbeat host + focus trigger, repo-rows beat) |

## 1. Context

The v1 heartbeat (docs/0032) was shipped with two accepted defaults,
parked as #225 for a cost-driven revisit:

- **Fixed 60 s cadence, no adaptive backoff** — a `Failed` lane
  re-qualifies on every beat (`needs_refresh`: failed lanes qualify at
  any age), so a down transport, an expired token or a rate-limited
  session costs one failing request **per open lane per minute,
  indefinitely**. With three detail lanes per page plus the 0035
  repo-rows pair, a dead transport while a repo page and a PR page are
  open burns ~5 requests/minute forever, and every retry lands after
  the transport died — none of them can succeed until it's back.
- **Polling continues while the window is minimized** — deferred with
  "the activation trigger catches up on return anyway; pausing adds a
  state machine for a ~1 request/60 s saving".

The owner has asked to pick #225 up. This doc revisits both halves and
takes one and deliberately rejects the other — with the evidence
in-tree.

## 2. Goals and non-goals

**Goals**

- G1: A persistently failing lane backs off exponentially instead of
  retrying on every beat; success resets it. The transport gets time to
  recover; request logs stop showing Sisyphus.
- G2: The heartbeat honors the **rate-limit pause** the tick loop
  already obeys (`rate_limited_until`, docs/0028 §3.4) — today the beat
  happily fires into an exhausted point budget.
- G3: No new timers, no settings, no user-visible UI — policy only
  (docs/0034 D6 reasoning).

**Non-goals**

- N1: **Pause-while-minimized — rejected as specified** (§4 D1). Not a
  deferral this time: the pinned gpui cannot express it, and its only
  substitutable signal is the wrong predicate for this app.
- N2: The state-level tick loop (notifications every 60 s, snapshot
  every 5th) is untouched — it already gates on the rate-limit pause,
  and its cadence is the module's backbone (docs/0029 §2).
- N3: The focus trigger (docs/0032 D3) is untouched — it fires once on
  activation and is not part of the cost problem.

## 3. Current mechanics (verified in-tree 2026-09-17)

- **The record** (`Fetch<T>`, service.rs:502): `status: Idle | Ready {
  data, fetched_at } | Failed { error }`, `in_flight`, `last_error`.
  There is **no last-attempt time and no failure count**: `fail()` over
  landed data keeps the payload and sets `last_error`; `fetched_at`
  stays at the last *successful* apply. `begin()`/`land()`/`fail()` are
  the only mutators, and page-owned lanes (docs/0031 convention) call
  them from the pages.
- **The beat** (page.rs `refresh_open_page`): nav gate → slot
  mutual-exclusion → per-page `auto_refresh(min_age)` → per lane
  `needs_refresh(min_age)` → dispatch. Failure today only *adds* the
  error to the record; the next beat re-qualifies the lane.
- **The repo-rows beat** (docs/0035): `repo_rows_refresh_specs` gates
  on `rows_landed` age — but failures never touch `rows_landed` (they
  don't apply), so a failing repo lane also re-fires every beat.
- **The rate-limit pause exists** — `rate_limited_until: Option<i64>`
  (epoch seconds), set by `apply` whenever a landed result reports
  `remaining == 0`; `plan_tick` returns an empty plan while it is in
  the future (tested: `apply_snapshot_rate_limit_zero_gates`). The
  heartbeat never reads it.
- **gpui-pre-0.3.3 exposes no minimized state** (pinned registry source
  verified): no `is_minimized` on `Window` — `is_window_active()` (line
  2664) and `is_maximized()` (2281) are all there is; `is_minimizable`
  in `WindowOptions` is a write-side capability flag; there are no
  occlusion events in this snapshot.

## 4. Design decisions

**D1 — Pause-while-minimized: rejected, with evidence.** The pinned
gpui cannot report minimized state at all (§3). The only proxy,
`is_window_active()`, measures *focus* — and minimizing is not the only
way to lose focus. This is a **monitoring dashboard**: its core
scenario is visible-but-unfocused on a second screen, where pausing
would break the product. Focus-deactivation conflates "minimized"
(a real saving) with "watching from another window" (the product
working); there is no signal that separates them. The cost side of #225
is real but lives in the failure paths (D2/D3) — a *successful* poll
costs 1 request/60 s and is the point of the app. If upstream gpui ever
grows a minimized/occlusion query, a one-beat check slots in behind the
nav gate; until then this stays rejected rather than approximated.
**Owner sign-off requested on this rejection (Q1).**

**D2 — Per-lane exponential backoff on the `Fetch` record.** The record
grows two fields:

```rust
attempted_at: Option<Instant>, // set in begin() — every dispatch
fail_streak: u32,              // bumped in fail(), zeroed in land()
```

and one gate beside the age policy:

```rust
/// #225 D2: a lane whose last dispatch failed waits
/// min(BACKOFF_BASE * 2^(streak-1), BACKOFF_CAP) before the next
/// attempt. Success zeroes the streak, so healthy lanes never wait.
pub(crate) fn backoff_elapsed(&self) -> bool
```

Pages compose, they don't entangle: `needs_refresh(min_age)` stays the
pure age/failure policy (docs/0032 D2, unchanged — its policy-table
test stays green); the dispatch condition becomes
`needs_refresh(min_age) && backoff_elapsed()`. `begin()` stamps
`attempted_at` on every dispatch (including successful ones), so the
clock is honest even when the guard skips a beat.

Constants beside the record: `BACKOFF_BASE = 60 s` (the beat — first
retry behaves exactly like today), `BACKOFF_CAP = 10 min`. Streak →
delay: 1 → 60 s, 2 → 2 min, 3 → 4 min, 4 → 8 min, ≥5 → 10 min. A
downed transport therefore costs 1 + 1 + 1 + 1 + 1 requests across the
first ~16 minutes and then settles at ~6/hour per lane instead of
60/hour, and `land()` on any success (even over previously landed data)
restores the old cadence immediately.

**D3 — The repo-rows beat mirrors it, state-side.** The 0035 lane has
no `Fetch` record (results land through `apply`), so the state grows
`rows_fail_streak: u32` + `rows_attempted_at: Option<Instant>`, bumped
in `run_specs`'s landing arm — it already computes settled-ness the
same way for thread mutations (`network.is_none() && 2xx`).
`repo_rows_refresh_specs` returns `None` while the backoff is running.
Same constants, same reset-on-success.

**D4 — The heartbeat honors the rate-limit pause.** `refresh_open_page`
early-returns while `state.rate_limited_until` is in the future (epoch
vs `SystemTime::now()`), the same gate `plan_tick` applies. Landed
results set the pause; the beat stops feeding a exhausted budget until
the reset epoch passes. This also covers the detail lanes' own
landings — a rate-limited lane result applies and sets the pause like
any other.

**D5 — No UI.** Backoff is invisible by construction: failed lanes
already show their error (docs/0031), recovery is automatic, and the
rate-limit pause already has its surface (docs/0028 §3.4). No new
selectors, no labels.

## 5. Testing

- **Record**: `backoff_elapsed` policy table — streak 0 → always true;
  streaks 1–5 → boundary at `>=` delay (docs/0032 semantics);
  `land()` resets; `begin()` stamps `attempted_at`.
- **Page**: the 0032 gating test gains a phase — a failed lane with a
  fresh attempt does not re-dispatch on the beat; with `attempted_at`
  backdated past its delay it does. A rate-limited state silences the
  whole beat (new phase).
- **Repo-rows**: a failed pair bumps the state streak; a fresh attempt
  makes `repo_rows_refresh_specs` `None` until the backoff elapses.

## 6. Implementation plan

1. service.rs: `attempted_at`/`fail_streak` on `Fetch` +
   `backoff_elapsed` + constants; `rows_fail_streak`/`rows_attempted_at`
   on the state, wired into `repo_rows_refresh_specs`; policy-table
   tests.
2. page.rs: the composed dispatch condition in the three detail pages'
   `auto_refresh`; the rate-limit early-return in `refresh_open_page`;
   the run_specs landing-arm bump; test extensions.
3. docs/0036 §8 Shipped + docs/README row (row already present from
   this PR) with the implementation PR.

## 7. Open questions (owner)

- **Q1**: D1 rejects pause-while-minimized outright — the pinned gpui
  has no minimized/occlusion signal (verified in the registry source),
  and its focus proxy would pause exactly the second-monitor monitoring
  this app exists for. Accept backoff + rate-limit gate as the whole of
  #225, with the minimize half documented as rejected-pending-upstream?
- **Q2**: D2's cap — 10 minutes per lane. With the observed ephemeral
  costs this makes a dead transport nearly free; too loose? (The reset
  on success is immediate, so the cap only bounds the wait to *retry*,
  not the recovery time once the transport is back.)
