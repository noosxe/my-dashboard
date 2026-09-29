# 0058 — Toast Notifications (Shell Alert Surface)

Issue: #376. Follows docs/0057 (usage SQLite sink), whose degrade path is the
motivating first consumer.

## 1. Context & problem

The app has no shell-level mechanism to tell the user "something important
failed". High-level failures die in `warn!` logs (post-hoc, nobody watches) or
the ambient status bar (easy to miss). Core services and modules have no API to
raise user-visible alerts at all.

The concrete trigger: docs/0057's SQLite sink degrades to in-memory fallback
when `data_dir` is unwritable or the DB corrupts — the right behavior is to
tell the user *their history is not being saved*, and today there is nothing to
tell them with.

## 2. Scope

- A **toast stack** as shell chrome: severity levels, auto-dismiss (errors
  sticky), bounded stack, dedupe.
- A **core raise API** usable from services and modules, including from
  non-UI threads (the usage writer thread).
- The **first consumer**: usage-sink degradation (docs/0057 D6).
- Bridge-verifiable rendering (docs/0010 D10 culture).

Out of scope (see §10): OS notification-center delivery, per-module styling,
sounds, persistence, a settings UI.

## 3. Verified upstream facts (gpui-component @ `94a313a`, v0.6.0)

All checked against the pinned checkout — `crates/component` unless noted.

- **F1** — `Root` (the view `main.rs` mounts via `Root::new(shell, window, cx)`)
  owns `pub notification: Entity<NotificationList>` and renders the layer
  (`root.rs:42`, `root.rs:182 render_notification_layer`), alongside the
  dialog/sheet layers it already z-orders and focus-restores.
- **F2** — `Notification` builder (`notification.rs:107–350`):
  `info/success/warning/error(message)` constructors, `.title`, `.icon`,
  `.placement(Anchor)`, `.delivery`, `.autohide(bool)`, `.on_click`,
  `.on_close`, `.action(F)`, `.id::<T>()` / `.id1(key)`.
  `NotificationType` = `Info | Success | Warning | Error`; severity icons use
  theme tokens (`info`, `success`, `warning`, `danger`) — consistent with our
  styling-token discipline (docs/0002 D5/D9/D10).
- **F3** — sanctioned raise path from any windowed context:
  `Root::update(window, cx, |root, window, cx|
    root.push_notification(note, window, cx))` (`root.rs:153`, `root.rs:422`).
  Dismissal by source type: `Root::remove_notification::<T>()`.
- **F4** — `NotificationDelivery` defaults to **`InApp`**; OS notification
  center delivery is strictly opt-in per notification (`notification.rs:53–70`).
  docs/0060 opts in behind the window-activation gate (macOS only).
- **F5** — lifecycle: autohide → `ToastOptions { timeout: 5 s }`, otherwise the
  toast is sticky until dismissed; a wall-clock advancing loop drives
  enter/exit transitions (`notification.rs:798–860`, shell `root.rs:47`).
- **F6** — dedupe is built in: gpui-base `ToastManager::push` **removes and
  replaces** an existing entry with the same id (`base/src/toast.rs:178`,
  returns the replaced value). Same-id pushes refresh instead of stacking.
- **F7** — geometry lives in theme `NotificationSettings` (`notification.rs:530–560`):
  placement default **`Anchor::TopRight`**, top margin = `TITLE_BAR_HEIGHT +
  16 px` (title-bar-aware), `max_items: 10`, `width: 382 px`. All `pub`, all
  theme-overridable.
- **F8** — a11y gap: the notification render path contains **no** accessibility
  role/label wiring (grep-verified absence). Toasts risk being role-less nodes
  in the docs/0010 tree. Addressed in D8 and §7.
- **F9** — gpui-base also exports raw `Toast`/`ToastManager`/`ToastStack`
  parts; `NotificationList` already wraps them. We never touch the raw layer.

## 4. Decisions

### D1 — Engine: upstream `NotificationList`; we build no toast widget

`Root` already owns the layer, the stacking order against dialogs/sheets, the
lifecycle clock, and the geometry. The design adds a raise path, not a
presentation. Custom placement/styling re-implements what F2/F7 already give.

### D2 — Raise API: core-global `Entity<ToastHub>` (the docs/0013 D2 pattern)

New `crates/core/src/toasts.rs`, mirroring `status::InFlight`:

```rust
pub struct ToastHub { /* pending: VecDeque<Toast>, next ticket */ }
struct ToastsGlobal(Entity<ToastHub>);
impl gpui::Global for ToastsGlobal {}

pub fn init(cx: &mut App);                       // beside status::init
pub fn raise(cx: &mut App, toast: Toast);        // free-fn API for &mut App callers
pub fn info(cx, message) / success / warning / error   // convenience ctors
```

Rationale (issue open question 1): raisers exist *before and without* the
window — early `main` wiring, module init, background threads, tests. A handle
on the shell entity makes raising impossible in exactly the cases the issue
asks for. The shell subscribes to the hub and forwards (§5); the hub itself
touches no UI.

### D3 — `Toast` value type; severity and stickiness

```rust
pub enum Level { Info, Success, Warning, Error }   // 1:1 with NotificationType
pub struct Toast {
    pub level: Level,
    pub title: Option<SharedString>,
    pub message: SharedString,
    pub dedupe_key: Option<SharedString>,
    pub action: Option<ToastAction>,               // label + gpui action; later use
}
```

Stickiness (issue requirement): `Error` raises with `.autohide(false)` (manual
dismiss); `Info`/`Success`/`Warning` use the upstream 5 s autohide (F5).

### D4 — Dedupe and bounding

`dedupe_key`, when present, maps to `.id1(key)` — upstream's replace semantics
(F6) then *refresh* the toast instead of stacking duplicates. The hub queue is
capped (32, drop-oldest, `warn!` on drop) so a raiser storm can't grow without
bound even if the window never opens.

### D5 — Placement and geometry: upstream theme defaults, `max_items` tightened

Answers issue open question 2: **not** the status-bar edge. We keep the
TopRight default (F7) — its top margin already clears the client-drawn title
bar (docs/0002 D11 chrome on macOS/Windows; server-decorated Linux has no strip
and the margin is simply generous), and toasts are non-modal overlays:
keyboard focus and content remain reachable, click or timeout dismisses. The
panel stays untouched; no layout math changes (contrast with docs/0013 D1,
which deliberately did *not* overlay).

One theme override in `main.rs`'s theme setup: `max_items` 10 → **3** (issue's
bound; upstream's visible-mount limit handles overflow). Width stays 382 px.

### D6 — Cross-thread raises: bounded spill channel

Non-UI threads (the docs/0057 writer thread) cannot touch gpui state — and the
entity itself is `!Send`. The hub therefore hands out a clonable spill handle:

```rust
#[derive(Clone, Default)]
pub struct ToastSpill(Arc<Mutex<Vec<Toast>>>);  // Send + Sync; cap 16, drop-oldest + warn!
impl ToastSpill {
    pub fn push(&self, toast: Toast);  // from any thread; mirrored to the log (D8)
    fn take(&self) -> Vec<Toast>;      // hub thread only
}
```

The shell drains the spill on every hub notification and via a 1 s timer pump
(`cx.spawn` loop, window lifetime) so thread-raised toasts appear without any
raiser-side UI coupling. The spill handle is `Send + Sync`; all other hub
state lives on the UI thread.

### D7 — First consumer: usage-sink degradation (docs/0057 D6)

The SQLite store stays UI-free: `main.rs` hands `SQLiteStore` an optional
spill handle at `init_with` time. On the **3-strike degrade flip**
(`note_flush_failure`) and on startup **open-failure fallback**, the store
raises once per transition (not per record):

> ⚠ "Usage history can't be saved this session — check data-dir
> permissions." (`dedupe_key: "usage-store-degraded"`, `Warning`)

`is_degraded()` remains the programmatic API; the toast is a wired side effect.
Startup fallback hits the spill → drained as soon as the shell mounts (D6).

### D8 — A11y and the bridge (docs/0010 D10)

- Every raise is mirrored to the log (`info!`/`warn!`) — the information never
  exists only inside a possibly role-less overlay (F8).
- Implementation **must** exercise toasts through `dashboard-debug`:
  `tree` must show the toast node(s) with stable ids (`dedupe_key`-derived) and
  the dismissal must work via the bridge `click` verb (§7). If toasts surface
  role-less/label-less in the dump, that is an upstream bug to file (rev bump
  or upstream PR), not a bridge problem — and D8's log mirror is the interim
  guarantee.
- No focus stealing: toasts never take focus; dismissal via click/timeout only.

### D9 — Ephemeral session state (issue open question 3: lean confirmed "no")

Nothing is persisted: the hub queue and the spill die with the process; queued-
but-unshown toasts are dropped on quit. No settings UI — geometry is theme-
level only. Toasts are present-tense alerts; docs/0057 is the durable record.

### D10 — Adoption boundary

This design ships: `toasts.rs` hub + shell forwarding + theme override + the
usage-sink consumer. Module raise sites (e.g. github service fetch-failure
paths, `notification_page.rs:1065`'s silent degrade) are **follow-up issues**,
not this work — and stay on each module's own schedule. Per-module custom
styling is a non-goal (§10).

## 5. File layout & shape

```
crates/core/src/toasts.rs        # Toast, Level, ToastHub, global, raise/spill (~150 lines)
crates/core/src/lib.rs           # pub mod toasts;
crates/core/src/shell.rs         # hub → Root forwarding: subscribe + drain pump, in DashboardShell::new
crates/core/src/usage_sqlite.rs  # Option<ToastSpill> field set at init_with; raise on degrade transitions
crates/app/src/main.rs           # toasts::init beside status::init; theme max_items = 3; spill handle wiring
docs/README.md                   # index row
```

Forwarding sketch (shell side):

```rust
// DashboardShell::new — one subscription, window lifetime.
cx.subscribe(&toasts::hub(cx), window, |this, _, _, window, cx| {
    Root::update(window, cx, |root, window, cx| {
        for toast in this.drain() {                    // hub → Notification
            root.push_notification(toast.into_notification(), window, cx);
        }
    });
});
```

`Into<Notification>` on `Toast` (shell-side impl, keeps gpui-component out of
the hub's type) maps level → severity ctor, `dedupe_key` → `.id1`, error →
`.autohide(false)`.

## 6. Testing

- **Core unit tests** (`toasts.rs`): queue cap + drop-oldest order; spill
  bounds under overflow; dedupe_key → id mapping; Level ↔ stickiness mapping;
  raise-before-window (headless `TestAppContext`) lands in queue, not panic.
- **usage_sqlite tests**: degrade transition raises exactly one spill entry
  (3-strike test extends; open-failure test extends).
- **CI**: no new platform constraints; gpui-component already in the tree.

## 7. Verification through the debug bridge (implementation PR)

1. `cargo run -p dashboard-debug -- session start --name toasts --size 1280x800`
2. Launch the app with `DASHBOARD_STATE_DIR` pointed at an unwritable path
   (docs/0057 §6 recipe) → degrade fires at startup.
3. `dashboard-debug tree` → toast node present, `#usage-store-degraded` id,
   warning icon + label readable; `logs -f` shows the mirrored `warn!`.
4. `click <toast-close-id>` → node leaves the tree (sticky dismissal works).
5. Auto-dismiss case: raise an `info` (debug verb or test hook) → node gone
   within ~5 s + exit transition.
6. Repeat-raise case: trigger degrade twice → still one toast (F6 refresh).
7. `session stop`.

## 8. Acceptance criteria

- `toasts::raise` works from `main`, a module, and a background thread; each
  lands as a toast once the window exists.
- Degrade (startup-fallback and 3-strike) produces exactly one warning toast
  per transition, sticky, with the dedupe_key id visible to the bridge.
- Max 3 visible; identical messages refresh, never stack; errors sticky,
  everything else auto-dismisses in 5 s.
- No persisted toast state; nothing survives restart.
- fmt/clippy/tests green; bridge script (§7) passes on the owner's machine.

## 9. Human checks (owner)

- [ ] Toast appears top-right, severity icon themed, never over the title bar.
- [ ] Error/warning degrade toast: sticky, dismisses on click, doesn't steal focus.
- [ ] Info toast auto-dismisses in ~5 s.
- [ ] Repeated identical alerts don't stack.
- [ ] macOS and Linux (server decorations) both place correctly.
- [ ] Log mirror present for each raise.

## 10. Non-goals

From the issue: OS/system notification integration (DBus
`org.freedesktop.Notifications`, `NSUserNotification`) — upstream supports it
(F4); docs/0060 opts macOS in behind the activation gate — Windows/Linux
stay out (0060 D1); per-module custom toast styling; sounds.
Added here: toast persistence or restart replay; a toast settings UI; toast
actions wiring beyond the `ToastAction` type (no first consumer yet).

## 11. Follow-ups (GitHub issues, not this PR)

- Module raise-site adoption (github service failures, silent degrade at
  `notification_page.rs:1065`) — one batch issue, picked up incrementally.
- Upstream a11y roles for notifications (file after §7 step 3 confirms F8;
  may resolve as a rev bump).
- First real `ToastAction` consumer (e.g. "Open settings" on persistence
  failures) once one exists.

## 12. Errata (docs/0059)

The notification hub (docs/0059) adds a bounded unread log on the `ToastHub`
entity: every raise lands there, the status-bar bell + badge surfaces the
unread count, and a popover panel lists the unread entries. That supersedes
"gone when hidden" for *unread tracking* only — D9's restart-ephemerality is
unchanged (the log still dies with the process). Upstream `on_close` fires on
the autohide path too (docs/0059 D2, canary test in `shell.rs`), so toast
dismissal is never treated as a read signal.

## 13. Raise-site adoption (2026-09-27, #382)

The §11 follow-up landed as the github module's first adoption batch. The
contract per site follows the severity guidance settled on #382:
user-action failures raise sticky Errors, background transitions raise one
Warning per incident (transition-only), everything else stays log-only.

**Adopted (raise sites):**

- Thread mutations (`notification_page.rs`): a failed mark-read / mark-done
  raises a sticky `Error` ("Couldn't mark the notification as read/done —
  try again."), deduped per thread + action (`thread-mark-{read,done}-<id>`)
  so retrying the same action refreshes instead of stacking (D4).
- Auth terminal transitions (`service.rs`): the first transition to
  `TokenRejected` / `KeychainError` in an incident raises a sticky `Error`
  ("GitHub session expired — reconnect required.", key
  `github-auth-expired`); repeats (drained waiters' follow-up 401s, a
  second `Surface`) stay log-only. `install_credential` re-arms; `sign_out`
  resets.
- Transport health (`service.rs`): the first network-level failure on a
  service lane (`Kind::is_service_lane` — inventory, notifications,
  snapshot, card totals, home PRs, the rate-limit probe) raises a `Warning`
  ("GitHub is unreachable — retrying in the background.", key
  `github-poll-offline`); repeats stay log-only; any landed response heals
  the incident **silently** (recovery needs no action — D9).

Mechanics: `apply`/`apply_refresh` are cx-free, so they only *stage* alerts
(`GithubState::pending_alerts`, bounded by the transition edges); the tick
loop and the refresh lead drain and raise on the main thread via the
free-fn API (`toasts::raise`). Worst-case raise latency is one tick
(60 s) for alerts staged by page-driven `apply` calls.

**Consciously log-only (the §audit outcome):**

- `notification_page.rs` thread-GET degrade — superseded: the page renders
  the load error inline (`notification-load-error`, since docs/0026); the
  issue's "silent degrade" line ref predates that.
- PR/issue create + PR mutations — pages render `mutation_error` inline on
  the form the user is acting on.
- Page-owned lane fetch failures (pr-detail, files, threads, pickers…) —
  page-local error + Retry UI per the detail-page convention.
- `DraftStore::discard` failure — self-healing on the next overwrite, and
  the store has no `App` access to raise from.
- Infra warnings (etag/last-known writes, settings/theme fallbacks, store
  flushes) — not user-actionable in-session.

Non-goals riding #382's close: per-module toast styling (docs/0058 §10),
the first real `ToastAction` consumer (§11), and a `ToastAction`
("Reconnect") on the auth toast — filed with the follow-ups.
