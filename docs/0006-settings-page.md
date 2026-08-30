# 0006 — Settings (app settings shell, module-provided subpages)

- **Status:** Accepted (owner merged PR [#20](https://github.com/noosxe/my-dashboard/pull/20) `08315af` 2026-08-29; open questions Q1–Q6 resolved at implementation review — proposals stand unless overridden there)
- **Date:** 2026-08-29
- **Depends on:** 0003 (module contract, registry, navigator), 0004 (paths,
  persistence primitives, debounced saves), 0002 (shell chrome)

## 1. Summary

An idiomatic settings surface for the app:

- A top-level **Settings** destination (`Page::Settings`) opened from a gear
  button in a new **sidebar footer** (and later, if wanted, a keybinding).
- The page itself is a **two-column layout** (owner requirement): left — a
  scrollable subnav listing the settings subpages; right — the active
  subpage's controls. This mirrors the GitHub module page's 2-column shape,
  and — decisively — **gpui-component already ships exactly this component**
  (`Settings` in `crates/ui/src/setting/`): sidebar subnav with search,
  virtualized page body, per-page "reset" machinery, resizable split, and a
  stacked fallback at narrow widths. We adopt it rather than hand-roll.
- Subpages come from two sources (owner requirement): a few **app-specific
  pages** (core-owned) and one **per-module page for modules that opt in**.
- v1 app pages: **Appearance** (theme mode + theme) and **About**. v1 module
  page: **GitHub** (account state + disconnect).
- Persistence: app-level settings in a new `state/settings.json`; module
  settings stay inside each module's own storage (`state/modules/<id>/`).

Non-goals (v1): user-reorderable subpages, settings import/export, custom
user themes (registry supports watching a themes dir — parked), per-window
appearance overrides.

## 2. Open questions — NEEDS YOUR INPUT

### Q1 — Entry points
Sidebar-footer gear button is the baseline. Add a keybinding (`ctrl-,` /
`cmd-,`, requires the app's first gpui `actions!` + `KeyBinding` registration)?
- **Proposal:** yes, both — one action, registered on the window; it's the
  canonical desktop shortcut and costs ~20 lines.
- **Your call:** keybinding in v1, or footer-only?

### Q2 — App-page scope for v1
- **Proposal:** `Appearance` + `About` only. A `General` page (startup
  destination: Home vs last page) is real but thin — defer until there are 2+
  General-worthy settings.
- **Your call:** agree, or pull `General` in now?

### Q3 — Where module settings live
Settings are user state, not data. `0004` puts module *data* under
`<data>/modules/<id>/`.
- **Proposal:** module *settings* go to `<state>/modules/<id>/settings.json`
  (new `paths.module_state_dir(id)`, symmetric with `module_data_dir`,
  sanitized + created on demand). Modules load it in `init` and save through
  the existing `DebouncedSave` primitive — identical pattern to GitHub's
  etag file, just in the state root.
- **Your call:** separate state file per module (proposed), or fold settings
  into the existing `data/modules/<id>/` dir to keep one directory per
  module?

### Q4 — How modules expose their page
- **Proposal:** no `Module`-trait change. `ModuleEntry` gains
  `settings: Option<SettingsProvider>` where
  `pub type SettingsProvider = Rc<dyn Fn(&mut Window, &mut App) -> Option<SettingPage>>`.
  Modules that want a page build the closure inside their `init` (where the
  state entity exists — the closure reads the entity on every call, so values
  are always live), then set `entry.settings = Some(...)`. Existing modules
  only add `settings: None` to their `ModuleEntry` literal. The registry
  collects pages in registration order for the settings shell.
- Alternative rejected: a `Module::settings_page()` trait method — the
  registry holds built entries, not types, so static dispatch is gone after
  `init`; the closure keeps entity access without spawning wrapper types.
- **Your call:** OK with the `ModuleEntry` field?

### Q5 — Appearance page contents (v1)
`gpui_component::init` already registers the default theme set
(`theme::init` → `ThemeRegistry::init_default_themes()`), and `Theme` exposes
`change(mode)` (light/dark/system) plus `theme_name()`.
- **Proposal:** two controls — **mode** (System/Light/Dark, `Theme::change`)
  and **theme** (picker over `ThemeRegistry::themes()` names, persisted and
  applied on boot). Font scale etc. out of scope.
- **Your call:** theme picker in v1, or mode-only until we have a wallpaper/
  density story?

### Q6 — About page contents
- **Proposal:** app name + `CARGO_PKG_VERSION`, module list (from the
  registry, with icons), "Open state folder" / "Open data folder" buttons
  (`SettingFieldElement`-style custom fields calling `cx.open_url` on the
  folder paths — same launcher as PR links; exact `file://` handling on
  Linux verified at implementation time), repo link.
  License texts NOT bundled in v1 (link instead).
- **Your call:** anything else you want on it?

## 3. Verified facts (sources at pinned rev: gpui-component `1081d2b2`)

All paths relative to the gpui-component checkout.

### 3.1 The `Settings` framework (`crates/ui/src/setting/`)
- `Settings` (`setting/settings.rs`): `RenderOnce`; constructor takes an
  element id and a list of `SettingPage`s (`.page()` / `.pages()`).
  Rendering:
  - **Left:** a `Sidebar` (non-collapsible) whose header is a **search
    `Input`** (placeholder localized); its `SidebarMenu` lists pages; a page
    with **more than one titled group** gets its groups nested as child menu
    items; clicking a group sets `deferred_scroll_group_ix` and the page body
    scrolls to it (`list_state.scroll_to_reveal_item`).
  - **Right:** the active page. The split is **`h_resizable`** with
    `resizable_panel`s; sidebar default width 250 px, range 160..360 px
    (`.sidebar_width()`, `.sidebar_size_range()`).
  - `container_query` flips item layout to stacked (label above control)
    when the content pane is ≤ 480 px wide (`STACKED_LAYOUT_MAX_WIDTH`).
  - Search filters **items** (title + description + keywords) across pages;
    pages whose groups all filter out disappear from the subnav.
  - Internal selection state (`SettingsState { selected_index,
    search_input, ... }`) is `window.use_keyed_state` under the constructor
    id — each `Settings::new("...")` id keeps its own selection.
- `SettingPage` (`setting/page.rs`): title, optional icon, optional
  description, `title_suffix` (custom element after the title), groups;
  `resettable` (default true) renders a small ghost **Undo "Reset All"**
  button in the page header when any item is dirty. Body: virtualized
  `list()` of groups + `vertical_scrollbar`.
- `SettingGroup` (`setting/group.rs`): optional title/description + items,
  rendered as `GroupBox` (variant from `Settings::with_group_variant` or
  per-group style).
- `SettingItem` (`setting/item.rs`), an enum:
  - `Item { title, description, keywords, layout, disabled, field }` — the
    standard row: title (+ muted description) on the left, field control on
    the right (`layout: Axis::Horizontal` default) or stacked.
    `disabled` → 0.5 opacity + non-interactive field.
  - `Element { render, reset_handler, keywords }` — full custom item via
    `SettingItem::render(|options, window, cx| ...)`; supports custom dirty/
    reset via `.on_reset(is_dirty, reset)`.
  - `keywords(...)` exist purely for search matching (e.g. "MFA" for a
    2FA toggle); custom items are always shown when the query is empty and
    only keyword-matched otherwise.

### 3.2 Fields and binding (`setting/fields/`)
- `SettingField<T>` = `{ field_type, value: Rc<Fn(&App) -> T>, set_value:
  Rc<Fn(T, &mut App)>, default_value, reset_handler }` — **getter/setter
  closures over `&App`**, not owned values. This is the whole binding story:
  setters write app state and notify; getters read it. Pages can be rebuilt
  every render with live values (the story does exactly this).
- Constructors by type (dispatch in `item.rs::render_field` is by `TypeId`):
  - `SettingField::switch(value, set_value)` / `::checkbox(...)` — `bool`
  - `::input(value, set_value)` — `SharedString`/`String`
  - `::dropdown(options, value, set_value)` /
    `::scrollable_dropdown(...)` — options are `(value, label)` pairs of
    `SharedString`
  - `::number_input(NumberFieldOptions, value, set_value)` — `f64`
  - `::element(impl SettingFieldElement)` — custom control type; the story
    implements `SettingFieldElement` for an "Open URL" `Button`
  - `::render(closure)` — arbitrary element field
- Reset machinery: `default_value(v)` marks a field resettable (page's Reset
  All + per-field reset); `on_reset(is_dirty, reset)` covers element fields.
- Field rendering respects `RenderOptions` (size, disabled, layout) — e.g.
  `options.size()` flows to the custom element in the story.

### 3.3 Theme infrastructure (`crates/ui/src/theme/`, `lib.rs`)
- `gpui_component::init` → `theme::init` (`lib.rs:124` → `theme/mod.rs:29`)
  → registers default themes into the global `ThemeRegistry`
  (`registry.rs:43` `init_default_themes()`). **Our app gets this today**
  (we call `gpui_component::init`); no extra setup needed.
- `Theme` (the `ActiveTheme` global we already read everywhere):
  - `Theme::change(mode, window, cx)` (`theme/mod.rs:232`) — switch
    light/dark/system (system follows OS via `sync_system_appearance`).
  - `ThemeRegistry::global(cx).themes()` (`registry.rs:121`) — all
    registered theme names → `ThemeConfig`; `Theme::theme_name()`
    (`theme/mod.rs:190`) — current selection.
  - Applying a named theme + boot-time restore: set `theme_name` on the
    `Theme` global and re-derive (exact call sequence — `sync_base`,
    scrollbar resync, window refresh — verified at implementation time;
    the story/theme picker code paths are the reference).

### 3.4 Sidebar footer (`crates/ui/src/sidebar/`)
- `Sidebar::footer(impl IntoElement)` (`sidebar/mod.rs:282`) — mount point
  we currently don't use.
- `SidebarFooter` (`sidebar/footer.rs`) — `Selectable::selected(bool)` for
  active styling, `.collapsed(bool)` for the icon-rail mode, renders its
  children in a padded `h_flex`. A gear `Button` (ghost, `IconName::Settings`)
  inside it gives the entry point in both expanded and collapsed sidebars.

### 3.5 What our app already has that this builds on
- `Page` enum (`core/nav.rs`): `Home | Module(SharedString)`; `Navigator::go`
  clears crumbs. Settings becomes a third variant — shell chrome (breadcrumb
  "Settings") and sidebar active-state just work.
- `ModuleEntry { meta, overview, main_page }` (`core/module.rs`) + registry
  in registration order — the collector iterates entries, so subpage order
  follows sidebar order deterministically.
- Persistence primitives (`core/store.rs`): `load_json`, `save_json_atomic`,
  `DebouncedSave::schedule` — proven by ShellState + GitHub cache.
- `paths.module_data_dir(id)` (`core/paths.rs:114`) — sanitize + create-on-
  demand pattern to mirror for `module_state_dir(id)`.
- GitHub module state entity (auth status, account login) — the v1 module
  page reads it; `AuthStatus` is already an enum with `Connected{..}`.

## 4. Design

### 4.1 Entry point
- `Page::Settings` variant added to `nav::Page`.
- Shell sidebar gains `.footer(...)` with a `SidebarFooter` containing one
  ghost gear button:
  - expanded: gear + "Settings" label (label hides in icon rail);
  - `.collapsed(nav.sidebar_collapsed)` and `.selected(page ==
    Page::Settings)`;
  - on_click → `nav.go(Page::Settings, cx)`.
- Breadcrumb for the page: single non-clickable "Settings" crumb (subpage
  selection is internal to the `Settings` component — it is not nav state;
  see §4.5).
- (Pending Q1) `actions!(dashboard, [OpenSettings])` + window keybinding
  `ctrl-,`/`cmd-,` dispatched on the shell's focus handle.

### 4.2 The settings shell view
New `crates/core/src/settings.rs`:

```rust
/// The app's Settings destination. Hosts gpui-component's `Settings`
/// component with: app pages (core-owned) + pages contributed by modules
/// (registration order). Rebuilt each render from live state, per the
/// library's intended usage (settings_story.rs).
pub struct SettingsPage { /* nothing stateful of its own in v1 */ }

impl Render for SettingsPage {
    fn render(&mut self, window, cx) -> impl IntoElement {
        let mut pages = vec![appearance_page(cx), about_page(&self.registry, cx)];
        pages.extend(self.registry.settings_pages(window, cx));
        div().size_full().child(
            Settings::new("app-settings").pages(pages),
        )
    }
}
```

- Sizing: the component manages its own two-pane split; the shell content
  column already enforces `min_w_0`/`overflow_hidden`, and the component's
  `container_query` handles the ≤480 px stacked fallback. No shell changes
  beyond hosting it.
- If a query is active and a module page has no matches, the component hides
  it from the subnav — module pages need nothing extra for search.

### 4.3 Module opt-in (pending Q4)
- `ModuleEntry` gains:
  ```rust
  pub type SettingsProvider =
      Rc<dyn Fn(&mut Window, &mut App) -> Option<SettingPage>>;
  pub struct ModuleEntry {
      pub meta: ModuleMeta,
      pub overview: AnyView,
      pub main_page: AnyView,
      /// None = module ships no settings page.
      pub settings: Option<SettingsProvider>,
  }
  ```
- `ModuleRegistry::settings_pages(window, cx) -> Vec<SettingPage>` — folds
  over entries calling each provider, dropping `None`s (registration order).
- Module duty cycle (GitHub example):
  1. in `init`, after building the state entity:
     ```rust
     let state = state.clone();
     entry.settings = Some(Rc::new(move |window, cx| {
         Some(github::settings::page(&state, window, cx))
     }));
     ```
  2. `github/src/settings.rs` builds `SettingPage`s from the entity
     (getters read `state.read(cx)`; setters update + `cx.notify()`, which
     also re-renders the settings shell next frame).
- Existing modules (`clock`, `notes`) add `settings: None`. No trait
  signature changes.

### 4.4 App pages (v1, pending Q2/Q5/Q6)
- **Appearance** (`AppearanceSettings` global, core-owned):
  - Group "Theme": `switch`-free — one `dropdown` (System/Light/Dark →
    `Theme::change`), one `scrollable_dropdown` of theme names from
    `ThemeRegistry::themes()` (sorted); setters write `AppSettings` +
    apply immediately. Getters read live `Theme` state so the UI reflects
    external changes (e.g. system flip).
  - Boot restore: after `gpui_component::init` in `main.rs`, load
    `AppSettings` and apply mode + theme before the window opens content.
- **About** (static-ish, rebuilt per render):
  - Group "Application": version row (`SettingItem::render` — name +
    `CARGO_PKG_VERSION`), module list row (icons + titles from the
    registry), links row (`SettingFieldElement`-style buttons: repository
    URL, license).
  - Group "Storage": "Open state folder" / "Open data folder" /
    "Open cache folder" — same `cx.open_url` mechanism as PR links.
- App-level persistence (pending Q3 shape, app-owned part): `AppSettings`
  global + `state/settings.json` via `DebouncedSave`; loaded in
  `main.rs` beside `ShellState`:

  ```rust
  #[derive(Serialize, Deserialize, Default)]
  pub struct AppSettings {
      pub theme_mode: Option<String>,  // "system" | "light" | "dark"
      pub theme_name: Option<String>,
  }
  ```
  (serde defaults everywhere → forward-compatible; unknown fields ignored.)

### 4.5 State & selection semantics
- The `Settings` component keeps selected subpage + search text in keyed
  window state (per `Settings::new("app-settings")` id) — **not** persisted.
  Leaving the page and returning resets to the first subpage; that matches
  every native settings surface and avoids inventing persistence for
  transient UI state. (If it annoys, a later iteration can persist
  `selected_index` — the component's state is opaque to us, so this would
  mean forking the component; deliberately out of scope.)
- Settings *values* persist immediately on change (debounced write per
  owning store) — no "apply/cancel" buttons anywhere; every control is
  live. Reset buttons restore defaults per the component's machinery.
- Module settings apply semantics are module-owned: GitHub's disconnect
  takes effect immediately (existing service shutdown path); appearance
  applies instantly via `Theme::change`.

### 4.6 GitHub module page sketch (v1)
- Group **Account**:
  - `SettingItem::render` row: avatar + login + `AuthStatus` line when
    connected; "Disconnect" `Button::danger` (reuses existing auth teardown;
    disabled/not-shown when disconnected); "Sign in" row when disconnected.
  - Kept deliberately thin: token/keychain UX already lives in the module's
    own page; settings links to it rather than duplicating the connect flow.
- Group **Notifications** (stretch, only if owner wants): poll interval
  `number_input` — currently a compile-time const (`TICK = 60s`, §10 of
  0005); making it runtime means threading a setting through the service
  loop. Default: defer to a follow-up iteration.
- File: `crates/modules/github/src/settings.rs`; page built from the state
  entity (live `AuthStatus`), search keywords ("github", "account",
  "disconnect", "token").

## 5. UI structure (per [DG])

- All surfaces use library components (`Settings`/`SettingPage`/`SettingGroup`
  /`SettingItem`/fields) → semantic tokens and popover/surface styling come
  from the framework, same as the sidebar and GitHub page today. No
  hand-rolled backgrounds.
- Empty/loading/error states: settings is local-first — no async states in
  v1 except GitHub's account row, which renders its `AuthStatus` variants
  exactly like the module page does (Connected / Disconnected / keychain
  error line, muted foreground).
- Icons: subpage icons from the shipped `IconName` set (sliders/palette/info
  analogues exist) — no new bundled assets needed for settings itself;
  module pages reuse `ModuleIcon` mapping where an icon is wanted.
- Tooltips: Reset All button already has one from the library; footer gear
  gets `Button::tooltip("Settings")`.

## 6. Implementation plan

1. **core**: `Page::Settings`; sidebar footer + gear; `SettingsPage` view +
   shell routing; `ModuleEntry.settings` + `registry.settings_pages`;
   `paths.module_state_dir(id)`; `AppSettings` global + `state/settings.json`
   wiring in `main.rs`; Appearance + About pages.
2. **github**: `settings.rs` page (Account group) + provider closure in
   `init`.
3. **clock/notes**: `settings: None`.
4. Doc §3 facts re-verified at implementation time where marked (theme-name
   apply sequence; `SidebarFooter` collapsed styling).

Crates touched: `dashboard-core`, `dashboard-module-github`, `dashboard-
module-clock`, `dashboard-module-notes` (one-line), `app` (boot wiring).

## 7. Testing & verification

- Unit: `AppSettings` serde round-trip + defaults (unknown-field tolerance);
  `module_state_dir` sanitize/create (mirrors `module_data_dir` tests);
  `settings_pages` ordering + `None`-dropping.
- Manual checks (this list doubles as the PR's human-check list): footer gear
  in expanded + collapsed rails; `Page::Settings` breadcrumb; search filters
  app + module pages; theme mode + name apply instantly and survive restart;
  About folder-open buttons;
  GitHub account row reflects connect/disconnect done from the module page
  (two-way); settings values survive restart; no layout break at min window
  size (stacked fallback).

## 8. Limitations / non-goals (v1)

- Subpage selection + search text are per-session (component-internal), not
  persisted.
- Settings pages are rebuilt every render from live state — by design
  (cheap: pages are small value trees); a module should not do heavy work in
  its provider closure (GitHub's is one entity read).
- No settings import/export/sync; no per-module enable/disable toggles (the
  module set is compile-time, 0003).
- `h_resizable` drag gives users a wider/narrower subnav — split position is
  component-internal per id, not persisted.
- Module settings storage layout (Q3) is the only new on-disk contract;
  everything else stays within 0004's roots.

## 9. References

- gpui-component `1081d2b2`: `crates/ui/src/setting/` (settings.rs, page.rs,
  group.rs, item.rs, fields/*), `crates/story/src/stories/settings_story.rs`
  (integration pattern), `crates/ui/src/sidebar/{mod,footer}.rs`,
  `crates/ui/src/theme/{mod,registry}.rs`
- Our docs: 0003 §4 (module contract, registry), 0004 §4.3/4.4 (state vs
  data roots, debounce), 0005 §3 (GitHub page anatomy, AuthStatus)
