# 0060 — OS notification delivery on macOS (upstream `NotificationDelivery`)

Date: 2026-09-28 · Evaluates #389 (docs/0059 §8 deferral) · Builds on
docs/0058 (toasts), docs/0059 (hub), docs/0058 §13 (raise-site adoption)

## 1. Problem

The hub (docs/0059) surfaces every alert in-app: toast + bell badge + log.
When the window is unfocused or hidden, nothing reaches the owner until they
look at the app again — an expired GitHub token or a poll-offline incident
can sit unnoticed for the whole session. macOS offers exactly one sanctioned
channel for that: the system notification center.

Upstream gpui-component already implements the bridge
(`NotificationDelivery`, rev `94a313a`); docs/0058 §10 and docs/0059 §8
deferred opting in "separate evaluation". This doc is that evaluation.

## 2. Verified upstream facts (F)

All at pinned revs `gpui-component 94a313a` / `gpui-pre-0.3.3`.

- **F1 API.** `NotificationDelivery { InApp (default), System,
  InAppAndSystem }`; per-notification `.delivery()` / `.in_app_and_system()`,
  or global via `NotificationSettings::delivery` (theme settings). We use the
  per-notification path — our policy is per-raise, not global.
- **F2 System semantics.** Title+message become the system banner's
  title/body (a notification with neither is not posted). **Re-pushing with
  the same `Notification::id` replaces the previous system post.**
- **F3 Click.** Clicking the system banner activates the window, closes the
  in-app toast, fires `on_close` on all close paths (verified during
  docs/0058 work) and `on_click`.
- **F4 Retraction.** An explicit close **retracts** the system post;
  autohide does **not** (upstream test
  `explicit_close_retracts_system_notification_but_autohide_does_not`).
- **F5 macOS requirements.** The app must run from a bundled, trusted `.app`
  (e.g. `~/Applications`, `/Applications`); posts are **silently dropped**
  under plain `cargo run`. First post triggers the authorization prompt; a
  denial drops posts silently thereafter.
- **F6 Windows/Linux.** Windows needs `App::set_app_identity` early; Linux
  needs a notification daemon and **cannot retract** (posts age out).
- **F7 Response plumbing.** `gpui_component::init` (our `main.rs:85`)
  registers gpui's single `on_system_notification_response` handler —
  click/dismissal responses already have a route back; apps must not
  re-register.
- **F8 Activation.** `Window::is_active(&mut App) -> Option<bool>`
  (gpui-pre-0.3.3 `window.rs:7002`). `None` = unknown/headless.
- **F9 Our id wiring already keys OS posts.** `into_notification`
  (shell.rs) sets `.id1::<HubToast>(key)` for keyed raises (docs/0058 D3/D4)
  → F2's replace semantics apply to OS posts with zero new code: a re-armed
  `github-auth-expired` incident replaces its stale banner instead of
  stacking.

## 3. Decisions (D)

### D1 — Scope: macOS only, via a cfg-branched policy helper

The delivery gate itself is platform-neutral code, but it compiles to
"always `InApp`" off macOS (`#[cfg(target_os = "macos")]` wrapper around a
pure decision fn). Rationale: Windows needs identity plumbing we don't run
(F6); Linux's no-retraction gap (F6) directly conflicts with our ✕-retracts
rule (docs/0059 D2) and stays deferred. Behavior off macOS is byte-identical
to today.

### D2 — Policy: one activation gate at the shell drain, all hub raises

`forward_toasts` (shell.rs) reads `window.is_active(cx)` once per drain and
passes it through `into_notification`:

- focused (`Some(true)`) → `InApp` — the owner is looking at the app; the
  toast is the right surface;
- unfocused or unknown (`Some(false)` / `None`) → `InAppAndSystem` — banner
  now, toast still queued in-app for when they return.

No per-site cherry-picking: the hub is already the bounded severity contract
(docs/0058 §13 — user-action failures sticky Error, background transitions
one-shot Warning/Error); that contract carries over unchanged. Mark-failure
feedback (sticky Error) posts to the OS only if it fires while unfocused,
which in practice it doesn't — no special-casing needed.

The decision fn is pure and unit-tested:

```rust
fn deliver_to_os(active: Option<bool>) -> bool { active != Some(true) }
// macOS wrapper: if deliver_to_os(active) { InAppAndSystem } else { InApp }
```

### D3 — Read-state on OS click: mark read, no deep link

An OS-banner click is a human action: it must mark the hub entry read
(docs/0059 D2 — "only human actions mark read"), same as the in-app ✕.
Mechanically: OS click activates the window and closes the toast (F3), so
the existing on-close → mark-read wiring covers it; implementation adds an
acceptance test for the click path specifically. Deep-linking the click into
the panel/thread (issue's open question) is **v1-out**: it needs per-entry
nav targets and rides #387's per-entry actions.

An OS-side dismissal *without* click (swipe away in Notification Center)
produces no callback through gpui → the entry stays unread and the badge
stays until the panel is opened. Accepted and documented; it cannot desync
the badge (badge counts hub reads, which only human in-app/OS-click actions
change).

### D4 — Bundled dev posture: `scripts/macos-app.sh`

macOS delivery requires a trusted `.app` (F5), so the deliverable includes a
small bundling script (dev-only; no DMG/notarization/distribution):

1. `cargo build --release -p dashboard-app` (binary `my-dashboard`);
2. assemble `My Dashboard.app` (`Contents/MacOS/my-dashboard` +
   `Info.plist`: `CFBundleIdentifier com.noosxe.my-dashboard`,
   `CFBundleName My Dashboard`, `CFBundleShortVersionString 0.1.0`,
   `LSMinimumSystemVersion 11.0`, `NSHighResolutionCapable`);
3. `codesign --force -s -` (ad-hoc), copy to `~/Applications` (trusted
   without admin), `open`.

**Known papercut, accepted for dev:** ad-hoc signatures change per build, and
macOS keys notification authorization off the signature, so a rebuilt bundle
may re-prompt. The script prints a note; if that grates, `SIGN_IDENTITY=<id>`
re-signs with a real identity to stabilize it. Plain `cargo run` remains the
everyday loop — it just never posts to the OS (F5), which the README will
state.

### D5 — Observability: one log line per system post

Each OS-delivered raise logs key + level (our existing `log`-mirror
channel, docs/0058 §6). If macOS denies authorization (F5) posts vanish
silently; the log line is the only trace that a post was attempted.
Querying/surfacing authorization state is not exposed by gpui → non-goal;
the doc note "System Settings → Notifications → My Dashboard" covers it.

### D6 — No new setting in v1

Always-on when unfocused + bundled. A preferences toggle (and per-level
opt-outs, if ever wanted) rides #388's notification settings UI, which
already has a home for it.

## 4. Implementation sketch

- `crates/core/src/shell.rs`
  - `fn deliver_to_os(active: Option<bool>) -> bool` + thin cfg wrapper
    `fn os_delivery(active: Option<bool>) -> NotificationDelivery`;
  - `forward_toasts`: read `window.is_active(cx)` once, thread through;
  - `into_notification(active)`: append `.delivery(os_delivery(active))`;
  - test: policy fn table (focused/unfocused/None), plus the OS-click
    marks-read acceptance test (follows upstream's TestAppContext pattern:
    `cx.set_app_identity("com.noosxe.my-dashboard", "My Dashboard")`).
- `scripts/macos-app.sh` (new, executable).
- `docs/0058` §10/§13 + `docs/0059` §8: amend the "never opt in" deferral
  wording to point here; `README.md`: bundled-run paragraph.
- No `main.rs` changes (F7 — handler already registered; `set_app_identity`
  is the Windows path only).

## 5. Testing

- Unit: `deliver_to_os` truth table; `into_notification` maps
  delivery per policy and keeps `.id1::<HubToast>` keying (F9 regression
  guard).
- Existing hub/shell tests run with `is_active() == None` → macOS test
  builds would request system delivery; in `TestAppContext` without identity
  the system post is a no-op and in-app behavior is unchanged — full gate
  run must stay green unmodified (any assertion drift here is a bug in the
  change, not the tests).
- macOS human checks (below) are the real delivery verification; the bridge
  cannot drive macOS notification center.

## 6. Human checks (owner, on the bundled app)

- [ ] First unfocused raise → authorization prompt; allow → banner appears.
- [ ] Focused raise → toast only, no banner. Unfocused raise → banner **and**
      toast-on-return.
- [ ] Banner click → window activates, entry marked read, badge count drops.
- [ ] Re-armed incident (e.g. second auth expiry after reconnect) → banner
      **replaces**, doesn't stack (F2/F9).
- [ ] In-app ✕ on the sticky error → OS banner retracts (F4).
- [ ] Rebuild + re-open the bundle → note whether the authorization prompt
      returns (D4 papercut) and whether `SIGN_IDENTITY` fixes it.

## 7. Non-goals (filed or parked, not done here)

- Windows enablement (`set_app_identity`) and Linux enablement (retraction
  gap, D1) — separate issues if ever wanted.
- Deep links / per-entry actions on OS click — #387.
- Any settings UI / delivery toggles — #388.
- Sounds, app icon, dock badge count, notification-center widget.
- Release packaging: DMG, notarization, signed distribution builds.
- Querying or surfacing authorization state in-app (not exposed by gpui).
