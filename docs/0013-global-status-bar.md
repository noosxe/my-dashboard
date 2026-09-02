# 0013 — Global Status Bar (in-flight request indicators)

- **Date:** 2026-09-03
- **Depends on:** 0003 (module architecture, shell layout), 0005 (GitHub fetch model),
  0007 (fetch specs, tick cadence), 0010 (debug bridge, D10 id/role rule)
- **Closes the design need of:** owner request — "global app statusbar … goes right
  below both main columns … show in-flight requests on its right side (e.g. a GitHub
  REST/GraphQL request shows a loading indicator in the right section)"

## 1. Context & problem

The app's shell today is: optional client `TitleBar` → window padding (`p_4`) → one
framed panel (`border_1` + `rounded` + `overflow_hidden`) holding the two main columns
(`Sidebar` | content column). Below that panel there is nothing — the bottom of the
window is dead chrome.

Meanwhile the GitHub module does real network work at unpredictable times: a 60 s tick
loop (notifications every tick, the GraphQL snapshot every 5th), repo-scoped fetches on
view open, and manual refresh. From the outside the app gives no live signal that work
is happening — only log lines and the eventual data change.

Owner request (this design):

1. **Global status bar** — one bar for the whole app, rendered **right below both main
   columns**, built on gpui-component's `StatusBar`.
2. **In-flight requests on the right** — while a module (GitHub today) has REST or
   GraphQL requests in flight, a loading indicator shows in the bar's right section.

## 2. Scope

In:

- One always-visible status bar in the shell, below both columns, inside the framed
  panel.
- A core-owned, app-global in-flight request tracker (`Entity` in a gpui global).
- Instrumentation of the GitHub module's two fetch runners (tick loop, page-run specs)
  so every `FetchSpec` run is one tracked unit.
- Right-region rendering: per-module item = spinner + module title, `×n` when a module
  has more than one concurrent run.
- Tests (tracker unit tests), bridge verification steps, this doc + README index row.

Out (non-goals):

- No per-HTTP-request granularity: one multi-page `FetchSpec` walk (e.g. 10-page
  notifications stitching) is **one** indicator, not N.
- No device-flow / token-refresh OAuth POSTs in the indicator — the connect flow has
  its own rich state UI (`ConnectPhase`), and refresh is a 0011 internal.
- No avatar image loads (gpui `HttpClient` asset pipeline — not the module data plane).
- No error/rate-limit surface in the bar (§10), no progress *percentages* (the fetch
  model has no total to divide by), no left/center region content in v1.
- No persistence: the bar is pure chrome, nothing enters `shell.json`.

## 3. Verified upstream facts

All verified against the pinned revs (`gpui-component` @ `1081d2b`, `gpui` from the
same lockfile):

- `gpui_component::status_bar::StatusBar` (`crates/ui/src/status_bar.rs` @ `1081d2b`):
  `RenderOnce` component with three regions — `.left(child)` and `.right(child)`
  append to the pinned end regions, `child`/`children` fill the center. It styles
  itself: `py_1`/`px_2`, `border_t_1` with `cx.theme().status_bar_border`, background
  `cx.theme().tokens.status_bar`, `text_xs` in `muted_foreground`. Both tokens exist in
  the theme schema (`status_bar.background` / `status_bar.border`, falling back to the
  title-bar tokens when a custom theme omits them). It also implements `Styled`, so
  `.flex_shrink_0()` can be chained. Usage pattern: `status_bar_story.rs` @ `1081d2b`.
- `gpui_component::progress::ProgressCircle`: `.loading(true)` gives an indeterminate
  rotating arc; `Sizable` (`.small()` → 12 px, matching `text_xs` cap height);
  `.accessibility_label(...)` flows into `gpui_base::Progress`, which renders with
  `.role(Role::ProgressIndicator)` — i.e. it is a first-class a11y node and satisfies
  the docs/0010 D10 id+role rule for bridge visibility.
- gpui globals (`zed` `crates/gpui/src/app.rs`, `global.rs`): `App::set_global` /
  `App::global` with the empty marker `trait Global: 'static`; storing an `Entity` in a
  global wrapper is the documented pattern for app-lifetime shared state.

Everything about the fetch model (spec kinds, cadence, page caps, timeouts) is per
docs/0005 §7 / 0007 D6 and is not re-derived here.

## 4. Decisions

### D1 — Placement: inside the framed panel, below both columns

The framed panel becomes a column: `[both-columns row, StatusBar]`.

```rust
v_flex().flex_1().min_h_0().p_4().child(
    // Framed panel — now a column so the bar spans below BOTH columns.
    v_flex()
        .relative().flex_1().min_h_0().w_full()
        .border_1().border_color(cx.theme().border)
        .rounded(cx.theme().radius)
        .overflow_hidden()
        .child(
            h_flex().flex_1().min_h_0()   // the two columns, unchanged
                .child(/* Sidebar */)
                .child(/* content column */),
        )
        .child(status::bar(&tracker, &registry, cx).flex_shrink_0()),
)
```

- The bar is **always visible** on every page (Home, modules, Settings) — it is shell
  chrome, not page content.
- `StatusBar` ships its own `border_t_1` + distinct background, so it reads as a bar
  with no extra styling; the panel's `overflow_hidden` clips its background to the
  rounded bottom corners exactly like the columns' backgrounds are clipped today.
- The columns row keeps `flex_1() + min_h_0()`; the bar is a fixed-height shrink-0
  footer. Vertical layout math for the pages does not change (the page host already
  absorbs flex, `min_h_0` everywhere).
- Client-decorated windows: the `TitleBar` stays full-width at the top; the bar is the
  bottom edge of the framed panel. Server-decorated windows: same, minus the title bar.

Rejected: below the framed panel (outside `p_4`) — the bar would float on the window
background, detached from the columns it reports on.

### D2 — In-flight tracking is a core global: `Entity<InFlight>` in a gpui global

New `crates/core/src/status.rs`:

```rust
pub struct InFlight { /* entries: BTreeMap<u64, Entry>, next_id: u64 */ }

/// Global wrapper — the documented gpui pattern for app-lifetime shared state.
struct InFlightGlobal(Entity<InFlight>);
impl gpui::Global for InFlightGlobal {}

/// Called once in main(), right after `gpui_component::init(cx)`.
pub fn init(cx: &mut App) { /* set_global(cx.new(|_| InFlight::default())) */ }

/// Modules and the shell fetch the tracker entity through this.
pub fn tracker(cx: &App) -> Entity<InFlight> { cx.global::<InFlightGlobal>().0.clone() }

/// Main-thread begin/end — what async fetch loops call.
pub fn begin(cx: &mut App, module: SharedString) -> u64;
pub fn end(cx: &mut App, id: u64);
```

Rationale:

- **Cross-module by nature.** The bar is global; today only GitHub reports, but the
  contract must not assume one reporter. Any module grabs the same global in its
  `init` or fetch loops — no per-module wiring.
- **No `Module` trait churn.** Threading an `Entity<InFlight>` through
  `Module::init(...)` would change the 0003 §4.1 contract for a peripheral concern and
  touch every module crate; the global is fetched, not passed.
- **Entity, not bare global, for the UI.** The shell observes the entity
  (`cx.observe`) and re-renders on every mutation — the codebase's standard
  entity-driven refresh (0003 §4.5), no `observe_global` wiring to keep straight.

### D3 — Tracker semantics: begin/end with unique ids, aggregate by module, stale sweep

- `begin(module_id) -> u64` allocates a monotonic id and records
  `Entry { module: SharedString, started: Instant }`. Ids are unique per call, so two
  concurrent runs of the *same* kind (tick-loop notifications + manual-refresh
  notifications overlap) are two entries, never confused.
- `end(id)` removes the entry; an unknown id is a no-op (end already applied or app
  restarted — never a panic).
- `by_module() -> BTreeMap<SharedString, usize>` aggregates for rendering;
  `BTreeMap` ordering (module id lexicographic) keeps the item order deterministic
  for free.
- **Stale sweep — the stuck-spinner backstop.** If a fetch loop ever loses its `end`
  (a dropped/duplicated future, a future bug), the spinner must not live forever.
  Every mutation sweeps entries older than `STALE_AFTER = 10 min` and logs
  `warn!("in-flight entry {module} expired after {age}s — end() was never called")`.
  10 min covers the worst legal run with headroom: the longest single spec is the
  notifications walk, ≤10 sequential requests × 30 s ureq total timeout = 300 s; the
  snapshot walk is 3 pages (0007 §5). When idle nothing sweeps — harmless, the next
  connected tick mutates within 60 s.
- The bookkeeping (`entries`, sweep, aggregation) is a plain inner struct so it is
  unit-tested without gpui (same pure-machinery/thin-orchestration split as the
  GitHub service layer, 0005 §7).
- `end` is best-effort from async contexts: wrapped in `cx.update`, whose failure
  means the app is tearing down — no UI left to stick.

### D4 — Unit of tracking = one `FetchSpec` run; both runners instrumented via one helper

One `run_spec(...)` execution — one `begin`/`end` pair. That is the unit the service
already plans, executes and applies (0007 D6), and it maps to what a user perceives as
"the app is fetching GitHub".

The GitHub crate gets one private helper, used by **both** existing runners (the tick
loop in `service.rs::spawn_tick_loop` and the page runner in `page.rs::run_specs` —
repo-open and manual refresh), so the two call sites cannot drift:

```rust
// crates/modules/github/src/service.rs
pub(crate) async fn run_spec_tracked(
    client: Arc<dyn Http>,
    spec: FetchSpec,
    cx: &mut AsyncApp,
) -> SpecResult {
    let id = cx.update(|cx| status::begin(cx, "github".into())).ok();
    let result = cx.background_executor()
        .spawn(async move { run_spec(&*client, &spec) })
        .await;
    if let Some(id) = id {
        let _ = cx.update(|cx| status::end(cx, id));
    }
    result
}
```

- Sequential loops stay sequential: a tick with two specs shows `×2` only if the runs
  actually overlap (tick + page-run concurrently); a sequential tick shows one live
  indicator at a time. No behavior change to either loop — only the wrapper call.
- `begin` before the background spawn, `end` after the await, both on the main thread
  (entities are main-thread-only; `AsyncApp::update` marshals correctly — the existing
  loops already use this shape for `state.update`).
- Excluded (unchanged, §2): device-flow OAuth POSTs, token-refresh POST, avatar
  image loads.

### D5 — Right-region item: spinner + module title (+ `×n`); idle = empty

For each module with ≥1 in-flight run, the bar's **right** region gains:

```
[◐ GitHub]        ◐ = ProgressCircle, indeterminate  (×2 → [◐ GitHub ×2])
```

```rust
h_flex()
    .gap_1()
    .child(
        ProgressCircle::new(SharedString::from(format!("status-spinner-{module_id}")))
            .small()
            .loading(true)
            .accessibility_label(format!("{title} syncing")),
    )
    .child(title)                                   // registry title, not the raw id
    .when(count > 1, |this| this.child(format!("×{count}"))),
```

- `ProgressCircle` (§3-verified) is the sanctioned loading element — indeterminate arc,
  `Size::Small` tier, theme-provided progress color. No bespoke animation.
- `title` resolves through the `ModuleRegistry` the shell already owns (same lookup
  the breadcrumb uses); unknown ids fall back to the raw module id (defensive — every
  reporter is a registered module).
- The `×n` suffix inherits the bar's `muted_foreground`; the spinner id pattern
  `status-spinner-{module_id}` keeps bridge nodes stable and namespaced.
- **Idle bar:** right region renders nothing — no spinner, no text. The bar itself
  stays (it is app chrome) at a **stable minimum height** (the `h_6` scale step, ≈ the
  component's active geometry of `py_1` + `text_xs` line): without it the component
  collapses to bare padding when empty and the columns above would resize on the
  first fetch — exactly the shift this design promised not to make.
- **Left/center regions:** empty in v1, reserved (§10). The upstream component
  centers children only when both ends are pinned, so an empty left region with right
  items is the well-defined "start-aligned center" case — nothing to fill today.

### D6 — Shell wiring: observe once, render from the snapshot

`DashboardShell::new` stores `status::tracker(cx)` and does
`cx.observe(&tracker, |_, _, cx| cx.notify())` (same pattern as its `nav` observer).
The bar renders per frame from `tracker.read(cx).by_module()` — no derived state, no
caches, nothing to invalidate. `ShellState` and persistence are untouched.

### D7 — [DG] and token-discipline compliance

- Semantic colors only: the bar's chrome comes from `StatusBar`'s own theme tokens;
  no literal color constructors, no ad-hoc `text_size`/`line_height` (the bar's
  `text_xs` is the component's, not ours) — the `clippy.toml` gates hold as-is.
- Component size tier (`Size::Small`) instead of a one-off pixel height for the
  spinner.
- Designed loading state per [DG]: the in-flight spinner *is* the module's global
  loading affordance; empty state = nothing to show = nothing rendered.
- Bridge (0010 D10): the spinner is an interactive-eligible node with id + role
  (`Role::ProgressIndicator`) + accessibility label — visible and addressable in
  `dashboard-debug tree` with no extra work.

## 5. File layout & shape

```
crates/core/src/status.rs     new — InFlight entity, global, init/tracker/begin/end,
                              bar() render helper, STALE_AFTER, unit tests
crates/core/src/lib.rs        pub mod status
crates/core/src/shell.rs      panel restructure (D1), tracker field + observe (D6),
                              bar into the panel
crates/app/src/main.rs        status::init(cx) right after gpui_component::init(cx)
crates/modules/github/src/
  service.rs                  run_spec_tracked (D4); tick loop calls it
  page.rs                     run_specs calls it
docs/
  README.md                   index row for 0013 (this PR)
  0002, 0003, 0005, 0007      untouched — shipped behavior unchanged by this feature;
                              implementation PR touches them only if something diverges
```

## 6. Testing

- `status.rs` unit tests (pure inner, no gpui):
  - begin/end pairing empties the tracker; unknown `end` id is a no-op.
  - two concurrent begins, same module → `by_module() == {"github": 2}`; second module
    gets its own entry.
  - stale sweep: entries older than `STALE_AFTER` are dropped on the next mutation,
    younger ones survive (test hook ages entries; no clock mocking needed).
- `service.rs`: the existing pure machinery tests are untouched — `run_spec_tracked`
  wraps orchestration only (the same `run_spec` runs underneath). The mock-transport
  tests keep covering the fetch semantics.
- Gates before push: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets` (zero warnings — token-discipline lints
  apply), `cargo test --workspace`.

## 7. Verification through the debug bridge (implementation PR)

1. `cargo run` with a connected token — the first tick fires immediately (0007 D6):
   `dashboard-debug tree` shows the `status-spinner-github` ProgressIndicator node
   during the bootstrap run; `dashboard-debug logs` correlates with the
   `fetch snapshot:` / `fetch notifications:` debug lines.
2. Repo view open + manual **Refresh** (the densest overlap): tree shows the spinner
   node and, when tick and page-run coincide, the `×2` text; after the run completes,
   the node disappears (re-dump the tree).
3. Idle window (no tick in flight): no `status-spinner-*` nodes; the bar strip is
   still present at the bottom of the framed panel (screenshot-visual, §9).
4. Disconnected: no spinner nodes ever — no fetches are planned, none tracked.

## 8. Acceptance criteria

1. The status bar renders on every page, directly below both columns, inside the
   framed panel; rounded-corner clipping and top border come from the component.
2. While any module has a `FetchSpec` in flight (bootstrap, 60 s tick, repo open,
   manual refresh), the right region shows spinner + module title, `×n` when >1
   concurrent run for that module.
3. Idle → right region empty; bar remains. Disconnected → never any indicator.
4. Completed runs clear their indicators; a lost `end` clears via the 10-minute stale
   sweep with a `warn` log (unit-tested).
5. No layout regressions: sidebar collapse, page scrolling, and min-window 900×600
   behave exactly as before (columns row keeps `flex_1 + min_h_0`).
6. Spinner node addressable in `dashboard-debug tree` as `status-spinner-github`.
7. Gates green; this doc + README index merged with the implementation PR.

## 9. Human checks (owner)

- Bar placement: a strip under both columns, inside the frame, with its own top
  border and background; looks native next to the columns at both sidebar states.
- Light/dark theme switch: bar tokens follow (the component maps them; no app-side
  colors).
- Spinner: reads clearly at `text_xs` density (12 px, `Size::Small`); title and `×n`
  aligned; nothing shifts when the indicator appears/disappears.
- Open a repo view and hit Refresh: indicator appears for the run's duration and
  clears — no stuck spinner after a manual refresh storm.

## 10. Follow-ups (GitHub issues, not this PR)

- Per-kind detail (tooltip "GitHub — snapshot, notifications"): needs a `detail` field
  in the tracker entry; API extension, no redesign.
- Left-region content (app version, active page, sync clock) once a consumer exists.
- Error/rate-limit surface in the bar (e.g. paused-until chip fed from
  `rate_limited_until`) — pairs with the 0005 Q9 states matrix.
- Other modules adopt the tracker as they gain network work (no core change needed).
