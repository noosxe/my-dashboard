# 0003 — Phase 2: Multi-module Architecture (Module Trait, Registry, Navigation)

- **Implementation record:** rebase-merged as 514f59a (7 commits); all §6 gates
  passed; UI corrections from owner review (breadcrumb pinned, card
  alignment/packing, sidebar brand + header spacing) and the §3.12 Wayland
  unfocused-update pattern shipped in the same PR.
- **Date:** 2026-08-28
- **Phase:** 2 (Framework)
- **Depends on:** 0001 (PR #1), 0002 (PR #4)
- **Design authority:** gpui-component Design Guides — cited as **[DG]** (see
  `docs/README.md` → Conventions). Requirements from owner recorded in
  `docs/README.md` → Planned docs → 0003.

## 1. Scope & goals

Deliver the module framework promised by the roadmap requirements:

- **Module = domain unit**: service layer + one state entity + two presentation
  faces (overview card on home, main page).
- **Two-level navigation**: sidebar lists Home + module main pages; sub-pages
  are module-internal and surface as breadcrumb items.
- **Responsive home grid**: fixed v1 layout that adapts columns to window width
  (tiling WMs).
- **Compile-time registry**: modules registered statically in `main.rs`.

Out of scope (deferred): real modules (github etc. get their own docs),
persistence/settings (0004), runtime enable/disable of modules, drag-reorder
of home cards, custom per-module icon assets, dynamic/plugin loading.

Non-goals from [DG]: no hover-only actions, no raw colors, no one-off control
sizes — see §4.7.

## 2. Open questions — NEEDS YOUR INPUT

Recommendations in *italics*; silence = follow recommendation.

**Q1 — Sample modules.** The framework ships with two throwaway modules to
prove the contract, replaced by real ones later:
- **Clock** — state entity ticked by a 1 s background timer; overview shows
  compact time, main page shows large time. Proves: async service → one entity
  → two live views. Needs `chrono` (clock feature only).
- **Notes** — static welcome text; main page has a "Details" sub-page.
  Proves: sub-page navigation + breadcrumb reporting + crumb-click back.

*Recommendation: yes, exactly these two. GitHub module follows in its own doc
(0004+) — note the `github` icon already ships in the default icon set.*

**Q2 — Overview card size.** gpui has no `min_w`, so responsiveness =
`flex_wrap` + fixed-width cards. V1: **320 × 200 px** cards, 16 px gap, 16 px
page padding. At 1280 content width ≈ 3 columns; narrow tiles collapse to 1.
Per-module spans (2×1 etc.) deferred. *Recommendation: accept.*

**Q3 — Sidebar composition.** V1: `SidebarHeader` = "Dashboard" wordmark;
single `SidebarGroup("Modules")` containing module items; **no footer**.
Home is the first menu item above the group. *Recommendation: accept;
categorized groups + footer come with real modules.*

**Q4 — Sidebar default state.** Expanded by default, `SidebarCollapsible::Icon`
(collapses to icon rail), state not persisted (0004). *Recommendation: accept.*

**Q5 — Breadcrumb shape.** `Home` (always, click → home) + current module
title (click → module main page) + module-reported sub-page crumbs
(click → module handles back-nav). Home page shows just `Home`. *Recommendation:
accept.*

**Q6 — Background refresh policy.** Core does not enforce pause/resume;
modules own their timers (Clock ticks even when its page is hidden — its
overview needs it anyway). A window-hidden pause helper can come with the
first networked module. *Recommendation: accept.*

**Q7 — Close-button tooltip.** [DG] requires tooltips on icon-only buttons;
our titlebar-less ghost close button predates this rule. *Recommendation: fix
in this phase — `Button::tooltip("Close")` (verified: `button.rs:341`).*

## 3. Verified facts (sources at pinned revs: zed `8cfa848c`, gpui-component `1081d2b2`)

1. **`AnyView`** (`gpui/src/view.rs:19`): type-erased view = entity + render
   fn pointer. `impl<V: Render> From<Entity<V>> for AnyView` (so
   `entity.into()`), `impl IntoElement for AnyView` (`view.rs:103`) —
   renderable directly as a child; `AnyView::cached(style)` recycles frames.
   The story app hosts arbitrary story views this way inside `Root`
   (`crates/story/src/lib.rs:121,133,165-175`).
2. **`flex_wrap()`** (`gpui/src/styled.rs:266-281`): `FlexWrap::Wrap` /
   `WrapReverse` / `NoWrap` — CSS-like wrapping flex; the responsive-grid
   primitive.
3. **No `min_w`/`min_h`** on `Styled` (grepped `styled.rs`) — intrinsic
   min-width sizing is unavailable; fixed-width cards + `flex_wrap` is the
   supported pattern.
4. **`Sidebar`** (`crates/ui/src/sidebar/mod.rs:238-294`):
   `new(id).side(Side).collapsible(SidebarCollapsible).collapsed(bool)`
   `.header(el).footer(el).child(el).children(iter)`.
   `SidebarCollapsible::{Icon, Offcanvas, None}` (sidebar_story.rs).
5. **`SidebarMenu`/`SidebarMenuItem`** (`sidebar/menu.rs:27-200`):
   `MenuItem::new(label).icon(...)` **icon optional**, `.active(bool)`,
   `.on_click(...)`, `.children(sub_items)` (native sub-items),
   `.suffix(builder)`, `.disable(bool)`.
6. **`Breadcrumb`/`BreadcrumbItem`** (`crates/ui/src/breadcrumb.rs:31-133`):
   `Breadcrumb::new().child(item).children(items)`;
   `BreadcrumbItem::new(label).disabled(bool).on_click(...)`.
7. **`SidebarToggleButton::new().collapsed(bool).on_click(...)`** — the
   layout we mirror: story places it left of the breadcrumb in the content
   column's top row (`sidebar_story.rs:661-690`; `last_active_item` drives
   both menu active state and breadcrumb).
8. **Nav-state precedent**: the story gallery keeps
   `active_group_index/active_index` on one entity and switches views
   (`crates/story/src/gallery.rs:38-39,155,211`) — same shape as our
   `Navigator.page`.
9. **`Button::tooltip(impl Into<SharedString>)`** exists
   (`crates/ui/src/button/button.rs:341`).
10. **Default icon set** (99 SVGs, `crates/assets/assets/icons/`): includes
    `github`, `layout-dashboard`, `book-open`, `bell`, `calendar`, `settings`,
    `cpu`…; **no** house/clock/note. `IconNamed` trait (`icon.rs:13`) is the
    custom-icon path for module-owned assets (deferred).
11. **[DG] obligations** (design guide, indexed): semantic theme colors only;
    shared spacing (~2/4/8/12/16/24/32 px) and type scales; component
    `Size`/density tiers over one-off heights; tooltips + accessible names on
    icon-only buttons; menus/keys over hover-only actions; designed
    empty/loading/error states; "summary region or card that exposes the
    count, representative results, meaningful state, and a clear next action"
    — our overview-card pattern is this, verbatim.
12. **API corrections found at implementation** (doc listings updated where
    affected): `Sidebar` children are **typed** — `content: Vec<E: SidebarItem>`
    — so a sidebar holds one homogeneous child type; our v1 sidebar is a flat
    `Sidebar<SidebarMenuItem>` (Home first, then modules) instead of the §2 Q3
    menu+`SidebarGroup` mix (group headers return with real modules).
    `Breadcrumb`/`BreadcrumbItem` live under `gpui_component::breadcrumb::`,
    not the crate root. `Crumb.on_click` is `Arc` (not `Box`) — the shell clones
    it into render closures each frame (`CrumbAction` alias keeps clippy's
    `type_complexity` happy). `IconName` is not `Copy` (clone it).
    `AsyncApp::update` returns `()` (no shutdown Result to check). `App::new`
    requires `FnOnce(&mut Context<T>)`. `Div` has no `z_index`.
    **Unfocused-window updates on Wayland** (empirically isolated on Hyprland):
    compositors can withhold `wl_surface` frame callbacks from unfocused-
    but-visible windows, which parks gpui's Wayland frame loop in
    `AwaitingCallback` — entity `notify()` marks the window dirty but no draw
    is delivered until refocus. `request_animation_frame` is render-phase-
    only (panics from a service task). The working pattern: **modules
    capture `window.window_handle()` in `init` and call
    `window_handle.update(cx, |_, window, _| window.refresh()).ok()` after
    each state write** — the force-render path presents without compositor
    callbacks. Verified by render-tracing: zero unfocused renders before,
    ~1/s after.

## 4. Design

### 4.1 Module model

A module is a **crate** implementing `Module`. Its `init` runs once at
startup: builds the state entity, spawns services (timers/network), and
returns two type-erased views sharing that entity.

```rust
// dashboard-core::module
pub struct ModuleMeta {
    pub id: SharedString,    // "clock" — stable, used as nav key
    pub title: SharedString, // "Clock" — sidebar + breadcrumb
    pub icon: Option<IconName>, // sidebar icon, None OK (F10)
}

pub struct ModuleEntry {
    pub meta: ModuleMeta,
    pub overview: AnyView,  // home card content (reads same state entity)
    pub main_page: AnyView, // nav destination
}

pub trait Module: 'static {
    fn meta() -> ModuleMeta;
    fn init(nav: &Entity<Navigator>, window: &mut Window, cx: &mut App) -> ModuleEntry;
}
```

Rules (enforced by review, not types):
- The **state entity is single**; both views `cx.observe` it. Views never own
  the data.
- Services talk **only** to the state entity (`entity.update`); views are dumb.
- Views stay alive for the app's lifetime (registry holds the `AnyView`s), so
  page switches preserve UI state. Rendering a not-currently-visible `AnyView`
  is skipped naturally because it simply isn't in the tree.

### 4.2 Registry (compile-time)

```rust
// dashboard-core::registry
pub struct ModuleRegistry {
    entries: Vec<ModuleEntry>,           // order = sidebar/home order
    index: HashMap<SharedString, usize>, // id → entries index
}
impl ModuleRegistry {
    pub fn new() -> Self;
    pub fn register<M: Module>(&mut self, nav: &Entity<Navigator>,
                               window: &mut Window, cx: &mut App);
    pub fn entries(&self) -> &[ModuleEntry];
    pub fn get(&self, id: &SharedString) -> Option<&ModuleEntry>;
}
```

`main.rs` lists modules explicitly — the registry is data, the crate graph is
the plugin system.

### 4.3 Navigation

`Navigator` is a plain gpui entity owned by the shell; modules receive a
clone in `init`. It is the single source of truth for chrome state:

```rust
// dashboard-core::nav
pub enum Page { Home, Module(SharedString) }

pub struct Crumb {
    pub label: SharedString,
    // Module-provided back-nav; None = non-clickable crumb.
    pub on_click: Option<Box<dyn Fn(&mut Window, &mut App)>>,
}

pub struct Navigator {
    pub sidebar_collapsed: bool,
    pub page: Page,
    crumbs: Vec<Crumb>,           // cleared on every page switch
}
impl Navigator {
    pub fn go(&mut self, page: Page, cx: &mut Context<Self>);
    pub fn set_crumbs(&mut self, crumbs: Vec<Crumb>, cx: &mut Context<Self>);
    pub fn crumbs(&self) -> &[Crumb];
    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>);
}
```

- **Main-page nav**: sidebar item click → `nav.go(Page::Module(id))`; home
  card click → same.
- **Sub-page nav**: module-internal. The module's main page view keeps its
  own state (e.g. `enum Screen { Main, Details }`) and calls
  `nav.set_crumbs(...)` when entering/leaving a sub-page. Crumb `on_click`
  closures capture the module's view entity and drive it back — no route
  resolver in core, no core↔module coupling beyond `Navigator`.
- Note: crumb closures that capture entities form strong reference cycles;
  acceptable for app-lifetime views (entities are GC'd by reachability from
  the window — cycles live exactly as long as the app).

### 4.4 Home page (responsive overview grid)

```
v_flex (page)
├── optional page header row (deferred; modules are self-titling)
└── h_flex().flex_wrap().gap_4().p_4()        // F2: wrap = responsiveness
    └── per module: ModuleCard
        div().w(px(320.)).h(px(200.))          // fixed card frame (Q2)
        ├── header row: icon? + title (muted_foreground) + open affordance
        └── content: module's overview AnyView, flex_1 + overflow hidden
```

- Card is clickable → `nav.go(Page::Module(id))`; hover shows theme
  `border` emphasis (state visibility per [DG]).
- No card-level min-width tricks (F3): wrapping handles narrow widths; at
  900 px content width ≈ 2 columns, at 1280 ≈ 3, at 1920 ≈ 5.
- The grid host renders **all** registered modules' overviews, in registry
  order (fixed layout, per roadmap requirement).

### 4.5 Shell integration

`DashboardShell` (moved from `crates/app` to `dashboard-core`) renders:

```
v_flex (existing key_context("Dashboard") + ToggleTitleBar wiring — unchanged)
├── TitleBar (when show_titlebar) / ghost close (else)   [0002, plus Q7 tooltip]
└── h_flex (body, flex_1)
    ├── Sidebar::new("app-sidebar")                      (F4)
    │   .collapsible(SidebarCollapsible::Icon).collapsed(nav.sidebar_collapsed)
    │   .header(wordmark "Dashboard")
    │   .child(SidebarGroup::new("Modules")
    │       .child(SidebarMenu::new()
    │           .child(SidebarMenuItem::new("Home").icon(layout-dashboard)
    │               .active(matches!(page, Page::Home)).on_click(go_home))
    │           .children(module items (meta.title, meta.icon?, active,
    │               on_click → open_module)))))
    └── v_flex (content column, flex_1, min_w via… n/a — plain flex_1)
        ├── h_flex (chrome row: p_2, gap_2, border_b)
        │   ├── SidebarToggleButton (left of breadcrumb — owner-verified
        │   │    sidebar_story.rs layout) → nav.toggle_sidebar
        │   └── Breadcrumb: [Home] + [module title] + [crumbs…]
        └── page content (flex_1, relative):
            Page::Home → home grid (§4.4)
            Page::Module(id) → registry entry's main_page AnyView (F1)
```

The shell `cx.observe`s the `Navigator`; all nav actions flow through it.

### 4.6 Crate layout & dependency graph

```
crates/
├── app/                    dashboard-app (bin) → core, module-clock, module-notes
├── core/                   dashboard-core    → gpui, gpui-component
└── modules/
    ├── clock/              dashboard-module-clock  → core, chrono (clock)
    └── notes/              dashboard-module-notes  → core
```

- `dashboard-core` owns: `module` (trait/entry), `registry`, `nav`, `home`,
  `shell`. Modules depend on core, never on each other or on the app.
- The app crate keeps only identity/geometry constants, window options, and
  the registration list (0002's `main.rs` slims further).

### 4.7 [DG] compliance checklist (implementation gate)

- [ ] All colors via `cx.theme()` semantic roles (`sidebar`, `border`,
      `muted_foreground`, `background`…); zero raw hex/rgb.
- [ ] Spacing from the shared scale only (p_2/p_4, gap_2/gap_4 …).
- [ ] No one-off control heights: stock components + `Size` variants.
- [ ] Ghost close button gains `.tooltip("Close")` (Q7, F9).
- [ ] Card hover/active states distinct; no hover-only commands.
- [ ] Clock overview/page designed for loading & error (timer can't fail, but
      the card reserves space — no layout jump between states).
- [ ] One icon family: default gpui-component set only (F10).

## 5. Implementation plan (files + listings)

Branch `feat/0003-module-framework`, stacked on this doc PR.

### 5.1 Workspace manifest additions

```toml
# root Cargo.toml [workspace.dependencies] additions
dashboard-core = { path = "crates/core" }
dashboard-module-clock = { path = "crates/modules/clock" }
dashboard-module-notes = { path = "crates/modules/notes" }
chrono = { version = "0.4", default-features = false, features = ["clock"] }
# members += crates/core, crates/modules/clock, crates/modules/notes
```

### 5.2 `crates/core/src/module.rs` — as §4.1 (full listing at implementation;
skipped here to avoid duplication).

### 5.3 `crates/core/src/nav.rs` — as §4.3.

### 5.4 `crates/core/src/home.rs` (shape)

```rust
use gpui::{prelude::FluentBuilder as _, *};
use gpui_component::{h_flex, v_flex};

use crate::{ModuleEntry, Navigator, Page};

pub const CARD_SIZE: (f32, f32) = (320., 200.); // Q2

pub fn home_grid(registry: &ModuleRegistry, nav: &Entity<Navigator>)
    -> impl IntoElement {
    h_flex()
        .id("home-grid")
        .flex_wrap()
        .gap_4()
        .p_4()
        .children(registry.entries().iter().map(|entry| {
            module_card(entry, nav.clone())
        }))
}

fn module_card(entry: &ModuleEntry, nav: Entity<Navigator>) -> impl IntoElement {
    let id = entry.meta.id.clone();
    v_flex()
        .id(ElementId::Name(id.clone()))
        .w(px(CARD_SIZE.0))
        .h(px(CARD_SIZE.1))
        .border_1()
        .border_color(|_, _, cx| cx.theme().border.into()) // impl-verify
        .rounded_lg()
        .p_3()
        .gap_2()
        .hover(|this| this.border_color(/* theme emphasis */))
        .child(/* header: title row, muted_foreground */)
        .child(/* content: entry.overview.clone(), flex_1 */)
        .on_click(move |_, window, cx| {
            nav.update(cx, |nav, cx| nav.go(Page::Module(id.clone()), cx))
        })
}
```

### 5.5 `crates/modules/clock/src/lib.rs` (shape — proves async + two views)

```rust
pub struct ClockModule;

impl Module for ClockModule {
    fn meta() -> ModuleMeta { ModuleMeta {
        id: "clock".into(), title: "Clock".into(), icon: None } }
    fn init(_nav: &Entity<Navigator>, _window: &mut Window, cx: &mut App)
        -> ModuleEntry {
        // 1. state entity
        let state = cx.new(|_| ClockState { now: chrono::Local::now() });
        // 2. service: 1s timer writes to state (BackgroundExecutor)
        cx.spawn(async move |cx| loop {
            cx.update(|cx| state.update(cx, |s, _| s.now = chrono::Local::now()))
                .ok();
            cx.background_executor()
                .timer(std::time::Duration::from_secs(1)).await;
        }).detach();
        // 3. two views over the same entity
        ModuleEntry {
            meta: Self::meta(),
            overview: cx.new(|cx| ClockOverview::new(state, cx)).into(),
            main_page: cx.new(|cx| ClockPage::new(state, cx)).into(),
        }
    }
}
// ClockOverview/ClockPage: hold Entity<ClockState>, cx.observe(&state) in
// constructors, render reads s.now (small HH:MM / large HH:MM:SS, tabular
// numerals per [DG]).
```

### 5.6 `crates/modules/notes/src/lib.rs` (shape — proves sub-pages)

Main page view holds `enum Screen { Main, Details }`; "Details" button →
`screen = Details` + `nav.set_crumbs(vec![Crumb { label: "Details".into(),
on_click: Some(Box::new(move |window, cx| /* set Main + clear crumbs */)) }])`;
crumb click reverses it. Breadcrumb row shows `Home / Notes / Details`.

### 5.7 `crates/app/src/main.rs` (diff from 0002)

```rust
// shell module moves to dashboard-core; imports adjust
use dashboard_core::{Navigator, ModuleRegistry, DashboardShell};
use dashboard_module_clock::ClockModule;
use dashboard_module_notes::NotesModule;

// inside open_window closure:
let nav = cx.new(|_| Navigator::default());
let registry = ModuleRegistry::new()
    .with::<ClockModule>(&nav, window, cx)   // register in sidebar order
    .with::<NotesModule>(&nav, window, cx);
cx.new(|cx| DashboardShell::new(registry, nav, cx))
```

### 5.8 Gates

`cargo build` / `fmt` / `clippy -D warnings` / smoke run, plus §6 manual
checks.

## 6. Acceptance criteria

- [ ] Window opens on Home: sidebar (Header "Dashboard", Modules group:
      Home + Clock + Notes) + breadcrumb row (`Home` only) + overview grid.
- [ ] Grid is responsive: resize from 900 px to full-screen — cards wrap
      1→N columns, no horizontal scrollbar, no clipped cards.
- [ ] Clock overview ticks every second on Home **and** on its page;
      switching pages never resets or freezes it (state survives switches).
- [ ] Notes: main page → "Details" sub-page; breadcrumb shows
      `Home / Notes / Details`; clicking `Notes` or `Details` crumbs behaves
      (back-nav); Home crumb returns to grid.
- [ ] Sidebar: active item tracks the current page; collapse toggle (left of
      breadcrumb) collapses to icon rail and back; collapsed icons still
      navigate.
- [ ] Overview card click opens that module's page.
- [ ] `ctrl-shift-t` still toggles the 0002 titlebar modes; ghost close
      button now shows a "Close" tooltip and quits.
- [ ] Zero raw colors/one-off sizes in diff (§4.7 gate).
- [ ] fmt/clippy/build clean; smoke run stable.

## 7. Risks & deferred

- **`AnyView` focus/keyboard**: type-erased views still participate in focus
  (focus handles are per-entity); sidebar/keyboard nav is deferred — v1 is
  mouse + existing bindings.
- **Card content overflow**: fixed 200 px height clips long overview content;
  modules must design for the frame (documented module contract), truncation
  rules land with real modules.
- **Crumb closures** capture entities → reference cycles that live for the
  app's lifetime; acceptable (entities are GC'd; app-lifetime views anyway).
- **Custom module icons** (Clock has none): `IconNamed` custom-asset path or
  default-set curation when real modules land.
- **Per-module overview spans** (2×1 etc.), reorder/hide, runtime enable,
  persistence: 0004+.
- **Window-hidden service pause**: first networked module introduces the
  policy (Q6).
