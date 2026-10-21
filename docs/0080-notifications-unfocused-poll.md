# 0080 — Notifications: the unfocused-poll carve-out

*Amends docs/0052 (D1/D5). Tracked by: design-doc PR (docs-only); the
implementation rides a follow-up after this merges.*

## 1. Context & problem

The owner's report (2026-10-21): "PR merged notifications arrive 10–15
minutes after the merge." Investigation verified the full path:

- GitHub creates a notifications thread on merge; the dashboard learns
  of it only via `GET /notifications` on the notifications lane.
- That lane is **trigger-only** (docs/0052 D5): session bootstrap,
  module view-shown (`page.rs:628,640`), card view-shown (`card.rs:171`),
  app refocus (15 s `FOCUS_MIN_AGE` floor), manual refresh.
  `service.rs:2622–2627`: "docs/0052 D1: the tick plans no content
  lanes." The docs/0052-era idle cadences (10 s engaged / 60 s idle)
  were deleted.
- **No landing ever raises a toast for new threads**: the staged-alert
  inventory (`service.rs` `pending_alerts`) is auth, offline/recovery,
  and the user's own mutation completions only. The only OS-banner path
  is the hub raise gated on window-inactive (`shell.rs:957–969`,
  docs/0060 D2) — and new threads never reach it.

So the observed latency is **merge time → the next view-shown /
refocus / manual trigger** — the owner's alt-tab rhythm. The app
sitting focused-but-untouched (or in the background) never learns a
merge happened.

The owner picked the fix direction: poll, but only when it cannot
churn — **while the window is inactive**, mirroring docs/0060's
banner-when-unfocused gate (fetch exactly when the user can't see the
app, which is exactly when a banner is useful).

## 2. Verified facts (F)

- **F1 — the lane gate is reusable.** `notifications_spec_if_stale`
  (`service.rs:2633`) already composes the docs/0036 backoff, the
  rate-limit pause, and an age floor; callers pass the floor. The poll
  arm needs no new gating logic.
- **F2 — the tick loop survives docs/0052 D1** for non-content duties
  (TICK = 10 s, `service.rs:59`); adding one focus-gated content arm
  is a scoped exception, not a resurrection of the cadence tables.
- **F3 — both activation observers exist.** `page.rs:403` and
  `card.rs:101` observe window activation for trigger purposes today;
  they are the natural writers of a focus flag.
- **F4 — the OS gate is exactly the poll's complement.**
  `deliver_to_os(!window_active)` (`shell.rs:958`): a toast raised
  while unfocused becomes a banner; while focused it stays in-app.
- **F5 — toast levels** are `Info | Success | Warning | Error`
  (`toasts.rs:41`); the "github" source label is registered
  (`lib.rs:153`, docs/0062 D2).
- **F6 — the Origin vocabulary is closed and receipt-bearing.**
  `usage.rs:449/483`: every fetch carries Surface × Trigger and shows
  on the Usage page (docs/0054) — poll fetches will be individually
  auditable.
- **F7 — `GET /notifications` does not mark threads read** (that is
  the thread `PATCH`); polling is observationally side-effect-free.
- **F8 — quota headroom.** At a 60 s poll floor: ≤60 requests/hour
  against the 5,000/h core limit, plus the existing per-fire dedup.

## 3. Goals and non-goals

**Goals**

- G1: A PR merged (or any GitHub notification thread created) while
  the owner is away from the app surfaces as an OS banner within
  ~a poll period.
- G2: Zero timer-driven fetches while the owner is actively using the
  app — docs/0052's quiet-while-focused guarantee holds verbatim.
- G3: Every poll fetch is receipted (Usage page) with a trigger that
  names it as a poll.

**Non-goals** (§8): webhooks/push infrastructure; per-reason level
mapping; persisted seen-state; polling any lane other than
notifications; ETag conditional GETs (a follow-up trim if quota ever
matters).

## 4. Decisions (D)

- **D1 — the carve-out, worded narrowly.** docs/0052 D1 gains exactly
  one exception: *"No lane may produce a network request because a
  timer elapsed — except the notifications lane while the window is
  inactive, at the docs/0080 floor."* No other lane regains a timer;
  the docs/0052 §6 acceptance ("idle app ⇒ zero content fetches") is
  amended to "zero while the window is active; the notifications lane
  polls only while inactive."
- **D2 — the focus signal is one flag, not a cadence tier.**
  `GithubState.window_active: bool` (default `true`: a fresh session
  assumes the user is present until an observer says otherwise). The
  two existing activation observers write it: `true` on
  `window.is_window_active()`, `false` on deactivation. This is not
  the deleted docs/0052 `focus_engaged` — that priced cadence tiers
  for every lane; this gates a single arm and prices nothing.
- **D3 — the poll arm rides the tick through the existing gate.**
  `plan_tick` gains: if `!window_active`, offer
  `notifications_spec_if_stale(UNFOCUSED_POLL_MIN_AGE, Origin::new(
  Surface::ServiceTick, Trigger::Poll))`. `UNFOCUSED_POLL_MIN_AGE =
  Duration::from_secs(60)` — worst-case detection ≈ 70 s (floor +
  tick), 60 fetches/hour worst case (F8). Backoff after failures
  still applies via the gate (F1): a failing lane backs off 60 s →
  120 s → 240 s… exactly like every other trigger. New
  `Trigger::Poll` variant (`as_str() = "poll"`) so Usage receipts name
  the arm (F6).
- **D4 — the announce: new threads raise per-thread Info toasts**
- **D4 — the announce: new threads raise per-thread Info toasts**
  (source `github`, `Level::Info`, dedupe key = thread id, click
  opens the notification detail page, ✕ per docs/0058/0059). The
  docs/0060 gate (F4) does the rest: a raise while the window is
  inactive becomes an OS banner; while focused it stays an in-app
  toast. In practice banners come from poll landings — the poll arm
  (D3) is the only trigger that fires while the window is inactive —
  and focused landings (refocus/view-shown/manual) surface as quiet
  in-app toasts. Cap: the first **3** new threads per landing raise
  individually; the remainder collapse into one summary toast
  ("+N more GitHub notifications", key `github-notif-summary`) so a
  long-absence backlog can never stack dozens of banners. The cap
  applies per landing regardless of origin.
- **D5 — the seen-set is in-memory.** `GithubState` holds
  `notification_seen: HashSet<thread-id>` for the current unread
  inbox; the **first landing after launch seeds it silently** (no
  announcement storm for threads that arrived while the app was
  closed — they are in the inbox), and every later landing announces
  the diff. Restart forgets: accepted, documented in §8; persistence
  via the docs/0063 log is a deliberate non-goal.
- **D6 — announce on every landing's diff, any origin.** The seen-set
  diff (D5) runs in the shared apply path, not behind a trigger
  check: refocus, view-shown, manual, and poll landings all announce
  their new threads identically. Rationale: "new" should not depend
  on *why* the fetch happened, the docs/0060 gate already prices the
  channel (banner when away, toast when present), and a manual
  refresh mid-session deserves the same news as the poll.
- **D7 — receipts.** Poll fetches appear on the Usage page as
  `service-tick × poll` (F6); announced toasts carry the `github`
  source and obey the docs/0062 per-source mute — muting `github`
  silences the announce path too (rows still land in the inbox).

## 5. State & plumbing changes

- `service.rs`: `window_active`, `notification_seen`,
  `UNFOCUSED_POLL_MIN_AGE`; the `plan_tick` arm (D3); announce
  staging in the notifications apply path (D4/D6) via the existing
  `pending_alerts` (drained by the shell exactly like today's
  alerts).
- `usage.rs`: `Trigger::Poll` (+ `as_str`).
- `page.rs` / `card.rs`: observers write `window_active` (D2).
- `shell.rs`: no change (F4 gate as-is).

## 6. Testing

- State-level (the `make_state` pattern): tick arm emits the spec iff
  unfocused ∧ floor elapsed ∧ not paused ∧ not backed off; focused ⇒
  no spec from the arm; the 15 s refocus path unchanged.
- Announce: first landing after launch seeds silently; second landing
  with 2 new threads raises 2 keyed toasts; 5 new threads raise 3 +
  1 summary; already-seen threads stay silent; muted source stages
  nothing.
- docs/0052's "idle tick produces no content specs" test is amended
  to focused-scoped (focused tick ⇒ probe/bootstrap only; unfocused
  tick ⇒ notifications when stale).
- Origin receipts: poll dispatches stamp `service-tick × poll`.

## 7. Acceptance (human checks, on the containerized env + local)

- [ ] Merge a PR on github.com with the dashboard unfocused: OS
      banner within ~70 s; click opens the notification page.
- [ ] Dashboard focused and idle 10 min on the home page: Usage page
      shows zero `poll`-trigger rows (G2); alt-tab away → polls begin;
      back → they stop.
- [ ] Failing backend (pause the lane): backoff delays visible in the
      receipts, no request storm.
- [ ] Mute `github` in settings: no announce toasts; inbox rows still
      land.
- [ ] Gates: fmt, clippy zero, workspace tests.

## 8. Non-goals

- **Webhooks / push**: needs a server; a local app polls. Revisit if
  a companion service ever exists.
- **Persisted seen-state** (docs/0063 log as the seed): restart
  forgets what was announced; threads that arrived while the app was
  closed announce nothing (they're in the inbox). Cheap follow-up if
  the owner wants cross-restart correctness.
- **Per-reason level mapping** (e.g. `ci_activity` vs `merge`):
  everything announces as `Info`; a mapping table is a follow-up.
- **Any other lane on a timer**: totals, repo rows, detail pages stay
  trigger-only (docs/0052 D1 holds for them, unchanged).
- **ETag conditional GETs**: the poll is cheap enough (F8); if quota
  ever shows up in Usage receipts, 304-based polling is the trim.

## 9. Docs amendments (ride with the implementation PR)

- **0052**: amendment banner under the title (D1's exception pointer).
- **README**: 0052 row gains "Amended by docs/0080"; new 0080 row.
