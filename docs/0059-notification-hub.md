# 0059 — Notification hub (unread log, status-bar badge, panel)

Status: design for review. Implements the durable side of docs/0058: toasts are
ephemeral; this adds the record. Tracking: (issue filed on merge of this doc).

## 1. Problem

docs/0058 shipped toasts: transient, top-right, 5 s autohide (errors sticky).
If the user is not looking at that corner when a failure is raised, the
information is gone — only the log mirror (D8) survives. There is no place to
see "what did I miss", and no surface count of outstanding alerts.

Goal: every raised toast also lands in an in-app **notification log**; the
status bar shows an **unread badge** at the rightmost position; clicking it
opens a **panel** listing unread notifications, where they can be cleared.

## 2. Verified upstream facts (rev `94a313a`, `crates/component/src/`)

- **F1 — Badge** (`badge.rs:31`): `Badge::new().icon(IconName)` with
  `.count(usize)`, `.max(usize)` (renders `max+` beyond), `.dot()` mode and
  `.color(Hsla)`. Already in app use (`issue_page.rs` imports `badge::Badge`).
- **F2 — Popover** (`popover.rs:106`): `Popover::new(id)` with `.anchor(Anchor)`,
  `.trigger(el)`, `.content(closure)`, `.open(bool)` / `.default_open(bool)`,
  `.on_open_change(F)`, `.overlay_closable(bool)`, `.track_focus(&FocusHandle)`.
  `RenderOnce`; the trigger stays in place, the surface floats anchored.
- **F3 — dismissal callbacks** (`notification.rs:320/332`): `Notification` has
  `.on_click(...)` (body click) and `.on_close(...)` (close button,
  middle-click, programmatic close). `on_close` is *documented* to fire on
  **all** close paths including autohide (`notification.rs:331`), and the
  single firing point is `complete_close` (`notification.rs:371`).
- **F4 — but the internals distinguish** (`notification.rs:1658`): test
  `explicit_close_retracts_system_notification_but_autohide_does_not` proves
  the close machinery knows explicit-close from autohide (only the former
  retracts the OS notification). Whether `on_close` *actually* fires on the
  autohide path must be verified empirically during implementation (§6 spike
  T1) — doc comment and observable behavior may diverge.
- **F5 — timing is not a discriminator** (`notification.rs:1341`): window
  focus pauses autohide; a wall-clock "closed before/after 5 s" heuristic
  cannot classify a dismissal. Classification must come from upstream
  callbacks, not clocks.
- **F6 — our status bar** (`crates/core/src/status.rs`, `pub fn bar`): builds
  `StatusBar` via `.right(element)` — in-flight module spinners, then
  module-contributed items (docs/0050 D5). The **last** `.right(...)` is the
  rightmost position; appending there satisfies the placement requirement.
  `bar` already renders upstream components (`ProgressCircle`) with
  `accessibility_label`s.
- **F7 — fallback overlay precedent** (`shell.rs:692`): the About dialog is a
  plain `absolute().inset_0()` backdrop + card with `.id(...)`, backdrop-click
  dismiss and stop-propagation on the card. Available if `Popover` anchoring
  misbehaves at the bottom-right corner.

## 3. Decisions

- **D1 — the raise path is the integration point; no new plumbing.** The
  existing `Entity<ToastHub>` (docs/0058 D2) gains a bounded entry log:
  `HubEntry { toast: Toast, raised_at: Instant, read: bool }`, appended on
  every raise — direct (`&mut App` free fns) and spill-drained (the shell
  already funnels both through the hub entity on the UI thread). Cap 100,
  drop-oldest. Module adoption (#382) feeds the hub for free.
- **D2 — read/unread semantics: only human actions mark read.** Unread =
  `read == false`; the badge counts unread entries. A read transition is
  caused exclusively by a human action: the toast's ✕, opening the panel,
  a per-row ✕, or "Clear all". Nothing else flips state — an autohide
  timeout leaves the entry unread (the user never saw it act), and
  programmatic closes are equally inert. Explicit dismissal rides upstream
  `on_close` (F3); §6 spike T1 decides the wiring: if autohide provably
  skips `on_close`, human-✕→read maps exactly; if it fires on both paths,
  v1 falls back to panel-managed-only read state (toast dismissal does not
  touch the log) and an upstream issue asks for a dismissal-reason enum —
  the human-actions-only principle holds either way.
- **D3 — indicator: `Popover` + `Badge` in the rightmost bar slot.** The last
  `.right(...)` item in `status::bar` (F6) is a `Popover` whose trigger is a
  bell `Button` carrying `Badge::count(unread).max(99)` (F1); no badge at
  zero. `on_open_change` keeps the open flag on the hub entity so all
  readers stay consistent. `Anchor` variant and edge behavior verified in §6
  spike T2; fallback is the F7 overlay pattern.
- **D4 — panel: our own list, rendered as `Popover` content.** A `v_flex`
  (~24rem wide, capped height, scrollable — upstream `scroll`/`list`): header
  row ("Notifications", "Clear all" button), then one row per unread entry —
  level icon + title + message (single line truncated) + relative time + a
  per-row dismiss button. Empty state: "No unread notifications". Opening
  the panel marks every unread entry read — the badge clears at open, not
  at close (owner ruling) — while the displayed list is the snapshot
  captured at open, so it doesn't vanish mid-read; rows stay individually
  dismissible until the panel closes, and the next open re-snapshots
  (previously read entries don't reappear).
- **D5 — still ephemeral across restarts (docs/0058 D9 unchanged).** The log
  lives in the hub entity; process exit discards it. Cross-session
  persistence needs a storage decision (the usage DB is analytics-scoped) —
  follow-up issue, not this design.
- **D6 — bridge ids and roles.** Trigger `status-notifications`, panel
  `notification-panel`, clear-all `notification-clear-all`, rows
  `notification-row-<n>` (n = entry index), per-row dismiss
  `notification-dismiss-<n>`. Rows carry `Role::ListItem`; the trigger is a
  labeled button ("Notifications, N unread") — visible to `tree`/`click`
  (docs/0010 D10).
- **D7 — no per-entry actions yet.** ToastAction stays unwired (docs/0058
  §10); rows are informational. Deep links ("open the page that failed") come
  with #382 adoption once raisers carry actions.

## 4. File layout

- `crates/core/src/toasts.rs` — `HubEntry`, log + cap on `ToastHub`
  (`entries`, `unread_count`, `mark_all_read` on open, `clear_entries`,
  panel snapshot state), `panel_open` flag; unit tests
  (append/cap/mark/count/snapshot).
- `crates/core/src/status.rs` — rightmost indicator: `Popover` + bell +
  `Badge`, reading `toasts::hub(cx)`; render tests for badge count states.
- `crates/core/src/shell.rs` — unchanged except whatever D2's T1 outcome
  requires in `forward_toasts` (`on_close` wiring when exact mapping holds).
- `docs/0058-toasts.md` — pointer note in D-sections: the hub log supersedes
  "ephemeral, gone when hidden" for unread tracking (D9's restart-ephemeral
  scope unchanged).

## 5. Tests

- Hub: append on raise (direct + spill path), cap-100 drop-oldest, unread
  count, mark-read on explicit ✕ / panel open / per-row ✕ / clear-all only.
- Status bar: zero unread renders bare bell; N unread renders `Badge` count
  (`max` capping at 99+).
- Panel: opening marks all unread read (badge clears at open) while the
  open-time snapshot stays listed; clear-all empties the view; empty state
  renders; non-human paths (autohide, programmatic close) never flip read.
- Spike tests (§6 T1) live behind `#[ignore]`-free unit tests on the upstream
  entity so CI flags any upstream behavior drift on rev bumps.

## 6. Verification

- **T1 (spike, decides D2 wiring)**: push an autohide notification and a
  sticky one with `on_close` counters; advance past autohide; assert which
  counter fired. Documents F3 vs F4.
- **T2 (spike)**: render the `Popover` anchored to the status-bar slot at
  window bottom-right; confirm the surface opens upward inside the window
  and `Anchor` variant choice; else switch to F7.
- **Bridge script** (`dashboard-debug`, docs/0010 §3):
  1. Session start; healthy boot → bell present, no badge, `tree` clean.
  2. Degrade path (unwritable `DASHBOARD_STATE_DIR`) → toast appears; badge
     count 1 at `status-notifications`; toast autohides → badge stays 1.
  3. `click status-notifications` → `notification-panel` in tree with the
     degrade row (`Role::ListItem`) — badge clears to 0 at open (rows stay
     listed, the open-time snapshot) — plus the a11y-role observation that
     feeds the upstream issue from docs/0058 F8.
  4. Close and reopen → empty state; `Clear all` on a second entry → view
     empties, badge 0; a fresh raise after reopen → badge 1 again.
- **Human checks (owner)**: badge sits rightmost next to module items on both
  platforms; panel opens upward, dismisses on outside click/Esc; sticky
  error toast X marks its entry read; appearance flip doesn't restyle the
  badge; VoiceOver/NVDA announces the trigger label with count.

## 7. Acceptance criteria

1. Every raised toast (all current + future raise sites) appears in the log
   without per-site work.
2. Unread badge reflects reality in all paths: raise → +1; explicit toast X →
   read (or documented fallback per D2); panel open → all unread read and
   the badge clears at open; clear-all → 0; autohide/programmatic close → +0.
3. Panel lists unread entries with level, title, message, time; scrollable at
   cap; empty state reachable.
4. No regressions: docs/0058 toast behavior unchanged (severity, sticky,
   dedupe, caps); status bar height rule (docs/0013 D5) intact.
5. Bridge script §6 passes on Linux; T1/T2 recorded in the PR.

## 8. Non-goals (filed as follow-ups, `deferred`)

- Cross-session persistence of the log (needs storage decision) — D5 —
  resolved: docs/0063 (design round; implementation tracked there).
- Per-entry actions/deep links — D7, rides #382.
- Notification preferences/settings UI — resolved: docs/0062 (design round;
  implementation tracked there).
- OS-notification delivery bridging — resolved: docs/0060 ships the
  macOS-only window-activation gate (banner click marks read via the
  key); Windows/Linux remain deferred there (0060 §7).

## 9. Review resolutions (owner, 2026-09-27)

1. Read on **panel open** — the badge clears the moment the panel opens;
   the displayed list is the open-time snapshot so it doesn't empty
   mid-read (D4).
2. Explicit ✕ on a sticky error toast **marks the entry read** — and the
   general rule: only human actions mark read; timers and programmatic
   closes never do (D2).
3. Badge: **numeric count**, capped at 99+ (D3).
