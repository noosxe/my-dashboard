# 0062 — Notification hub settings (badge, levels, per-source mute)

Status: implemented (#416, landed via PR #422). Implements #388 —
the settings round deferred by docs/0059 §8.

## 1. Problem

The notification hub (docs/0059) ships with fixed product policy: the badge is
always numeric with a 99+ cap, always counts every unread level, every raise
from every source becomes a toast, and errors are always sticky. With module
adoption landed (#382 — GitHub raises thread-mutation errors and service
alerts; core raises sink-degrade notices), volume grows with every module, and
the user has no say: a noisy source nags forever, and someone who only cares
about errors still sees the badge climb on info toasts.

#388 names four candidates in expected-value order: per-source mute, per-level
badge filtering, badge style, autohide duration. This design takes the first
three and defers the fourth (§3 D6 — upstream-hardcoded).

## 2. Verified facts (pin: gpui-component `0c830f4`, v0.7.0)

- **F1 — settings controls exist**: `SettingField::{switch, checkbox,
  dropdown, scrollable_dropdown, element}` (`crates/component/src/setting/
  fields/mod.rs:156/165/188/210/232`) — the docs/0006 framework already used
  for Appearance. Custom rows escape via `SettingField::element` (About-page
  precedent).
- **F2 — badge modes**: `Badge::dot()` and `.count(n).max(99)`
  (`badge.rs:56/64`); docs/0059 F1 already verified both. The trigger's
  accessibility label carries the count independently of the visual mode.
- **F3 — autohide is upstream-hardcoded**: `notification.rs:872`:
  `timeout: autohide.then_some(Duration::from_secs(5))` — no builder knob, no
  theme field at this rev. Tuning it means patching upstream or re-implementing
  the dismissal timer; not worth it for the lowest-value candidate.
- **F4 — our settings persistence pattern** (`crates/core/src/settings.rs`):
  `AppSettings` is a plain `Global`, all-`Option` serde fields, saved to
  `<state>/settings.json` via `DebouncedSave` (500 ms + quit flush); unknown
  fields degrade silently. Setters mutate the global, `persist`, then
  `cx.refresh_windows()` (settings.rs:300/309) — live apply without observers.
- **F5 — the hub has exactly two ingest choke points**: `ToastHub::push_local`
  and `ToastHub::drain_spill` (`toasts.rs`) — both hold `&mut Context<Self>`
  (which derefs to `App`), so both can read settings globals. The shell's
  forward path (`shell.rs:835-838`) maps `Level` → upstream builders and needs
  no change.
- **F6 — raise sites today**: GitHub `notification_page.rs:243/304`
  (mark-read / mark-done errors, dedupe-keyed per thread), `service.rs:3204/
  3354` (refresh alerts), and core's sink-degrade raise (docs/0057 D6). `Toast`
  carries no source field — nothing today distinguishes them.
- **F7 — the indicator is one function**: `status.rs:
  notifications_indicator` builds bell `Button` + `Badge` + `Popover` panel;
  the badge branch and the panel header are the only render changes needed.

## 3. Decisions

- **D1 — one home: a core-owned "Notifications" subpage in Settings.** New
  `notifications_page` beside `appearance_page`/`about_page` (docs/0006
  pattern; core-owned because the hub is core). The panel header gains a gear
  button that *navigates* to Settings → Notifications — no toggles inside the
  popover: settings are durable, the popover is transient, and duplicated
  state invites drift.
- **D2 — source attribution: `Toast.source` + a registry.** `Toast` gains
  `source: Option<SharedString>` (builder `.source(...)`, like `dedupe_key`).
  A registry in `toasts.rs` — `register_source(id, label)` called from `init`
  (`"app"` = "App") and module init (`"github"` = "GitHub") — supplies display
  labels; raises without `.source` attribute to `"app"`. The settings page
  lists the union: registered sources, then any raw source id seen in the log
  (an unregistered raiser degrades to its id as label). Sources are stable
  string ids (the module registry ids), not new types.
- **D3 — mute is absolute, at ingest.** `muted: Vec<String>` in settings.
  `push_local` and `drain_spill` drop raises whose source is muted: no pending
  toast, no log entry, no badge. No error exemption — mute means mute; the
  toast, no log entry, no badge. No error exemption — mute means mute; the
  failures still surface inline at their raise sites (the toast is the alert
  layer, not the only feedback) and docs/0058 D8's log-file mirror still fires
  raise call (suppression happens after `mirror_to_log`), so the paper trail
  survives. The settings row copy says this explicitly: "Muting hides all
  notifications from this source, including errors. Failures still show inline
  and in the log."
- **D4 — per-level badge filtering counts only; the log is unfiltered.**
  `badge_levels: Vec<String>` (default all four). The badge counts unread
  entries whose level is in the set; the panel still lists every unread entry
  regardless of level — the filter tunes nag intensity, not content. An empty
  set is legal and means the badge never renders (a legitimate "badge off"
  preference; the panel remains the full record). Round-trips as strings for
  serde tolerance; unknown level names drop on load.
- **D5 — badge style: count or dot.** `badge_style: String` — `"count"`
  (default; today's numeric with 99+ cap) or `"dot"` (`Badge::dot()`, hides
  the number — unread presence only). The trigger's a11y label keeps the exact
  count in both modes ("Notifications, 3 unread"), so screen readers lose
  nothing in dot mode.
- **D6 — autohide duration: deferred.** F3: the 5 s is hardcoded upstream at
  our pin. File an upstream issue asking for a `timeout` builder (or theme
  field), revisit at the next rev bump. Non-goal here (§8).
- **D7 — model, persistence, live apply.** `AppSettings` gains
  `notifications: Option<NotificationsSettings>` with all-`Option` fields
  (`badge_style`, `badge_levels`, `muted`) — missing file, missing section, or
  unknown fields all degrade to today's behavior (numeric, all levels, nothing
  muted). Setters follow F4 exactly: mutate, `persist`, `cx.refresh_windows()`
  — the badge, panel, and mute checks all read the global live; nothing caches
  a copy. Default `None` = factory behavior, so no migration.
- **D8 — bridge ids and roles.** Panel gear `notification-settings` (a
  labeled button that navigates). The subpage's controls render through the
  docs/0006 framework, whose generated ids we do not control — implementation
  includes a tree spike recording what the framework emits (item title → node
  label) and, if a control is missing from the dump, an `element`-based row
  with an explicit `.id(...)` per docs/0010 D10. Rows in the panel gain a
  dim source tag (e.g. "GitHub") before the relative time — the user needs
  to see *which* source a row came from to make an informed mute.
  *(Implementation note: upstream's `Settings` keeps the subnav selection
  in per-window keyed state that cannot be set externally at our pin, so
  the gear's one-shot request preselects Notifications via
  `default_selected_index` — effective while the subnav state is fresh; a
  remembered selection wins once the user has browsed.)*

## 4. File layout

- `crates/core/src/toasts.rs` — `Toast.source` + builder; source registry
  (`register_source`, `sources()` returning registered + log-seen ids);
  mute-filter helper (pure: `fn muted_for(settings) -> bool` given a source)
  applied in `push_local` and `drain_spill` after `mirror_to_log`;
  `unread_count_for(levels: &LevelSet)` (pure, test-pinable; `unread_count()`
  keeps its meaning: all unread). Unit tests.
- `crates/core/src/settings.rs` — `NotificationsSettings` (serde model +
  defaulted getters `badge_style()`, `badge_level_set()`, `muted_sources()`),
  setters (persist + `refresh_windows`), `notifications_page(cx)` building
  the subpage: Style dropdown (Count/Dot), Levels checkbox group (four
  switches or checkboxes — implementation picks per F1 ergonomics), Sources
  group (one Muted switch per source, ordered: registered first, log-seen
  after, `"app"` pinned first). Round-trip + defaults tests.
- `crates/core/src/status.rs` — badge branch (`count(n).max(99)` vs `dot()`,
  skip render when the filtered count is 0 or the level set is empty), the
  filtered count read, panel-row source tag, panel-header gear navigating to
  Settings → Notifications. Render tests for count/dot/hidden states.
- `crates/core/src/shell.rs` — register the subpage beside Appearance/About;
  `toasts::init` registers the `"app"` source.
- `crates/modules/github/src/{notification_page,service}.rs` — stamp
  `.source("github")` at the four raise sites; github init registers the
  source label.
- `docs/0006`, `docs/0059` — pointer notes (§6 here).

## 5. Tests

- Model: `NotificationsSettings` serde round-trip; missing/empty/unknown
  values degrade to factory defaults (numeric, all levels, nothing muted);
  persisted file from a pre-0062 install loads unchanged.
- Hub: muted source suppressed on both ingest paths (direct + spill), nothing
  in pending/log/badge; the log-file mirror still fires (assert via a test
  log capture or the existing pattern); unmuted sources unaffected; source
  fallback to `"app"`; registry union for the page list (registered + seen).
- Badge: `unread_count_for` respects the level set (empty set → 0); render
  test — count badge at N unread, dot badge, no badge at 0 or empty set; the
  trigger label carries the exact count in dot mode.
- Panel: source tag renders; gear navigates (route assertion).
- Bridge (Linux box): script in §6.

## 6. Verification

- Tree spike first (D8): open Settings → Notifications via the bridge, record
  the framework's emitted ids/labels for the three groups; note any control
  invisible to `tree` and its `element` fallback.
- Bridge script:
  1. Session start; Settings → Notifications renders; defaults match factory
     (Style = Count, all four levels checked, no source muted).
  2. Toggle Style to Dot → `status-notifications` badge flips to dot (tree
     label still carries the count); toggle back.
  3. Uncheck Info/Success → raise an info toast → badge stays 0, panel open
     still lists the entry; re-check → new info raise badges 1.
  4. Mute GitHub → trigger a thread-mutation failure (or a probe raise with
     `.source("github")`) → no toast, no badge, no panel row; the app log
     still shows the mirror line; unmute → the next raise appears again.
  5. Panel gear navigates to the subpage; restart the app → all three
     settings survived.
- Human checks (owner): subpage reads well in both themes; dot badge legible;
  mute copy honest; panel row source tags don't crowd the layout; VoiceOver
  reads the trigger count in dot mode.

## 7. Acceptance criteria

1. Factory default = today's behavior exactly (numeric 99+ badge, all levels
   counted, nothing muted) with no settings file present.
2. Mute suppresses a source's raises everywhere user-visible (toast, panel,
   badge) on both ingest paths, while the log-file mirror survives.
3. Badge respects level filter and style live — no restart, no re-navigation.
4. Settings persist across restart; hand-edited unknown values degrade to
   defaults, never error.
5. No regressions: docs/0058 toast behavior (severity, sticky errors, dedupe,
   caps), docs/0059 read semantics (only human actions mark read — a muted
   raise never lands in the log, so it can never flip read), docs/0060 keyed
   marks, status-bar height rule (docs/0013 D5).
6. Bridge script §6 passes on Linux; the D8 tree spike is recorded in the PR.

## 8. Non-goals (filed as follow-ups, `deferred` unless noted)

- Autohide duration tuning — D6: upstream-hardcoded 5 s at our pin; an
  upstream issue is filed and the setting lands when the knob exists.
- Per-source *level* overrides (mute-all in v1; finer grain if volume demands).
- Quiet hours / schedules.
- OS-notification delivery preferences — docs/0060 scope, macOS-gated there.
- Cross-session log persistence — docs/0059 D5.
- Sounds, per-entry actions / deep links — docs/0059 D7.
