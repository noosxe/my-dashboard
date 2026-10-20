# 0025 — File-based color themes (user themes dir + hot reload)

- **Date:** 2026-09-07
- **Issue:** #23
- **Amends:** docs/0006 §3.6 (bundled-themes story), docs/README.md roadmap

## 1. Summary

Users can drop `ThemeSet` JSON files into `~/.config/my-dashboard/themes/`
(`AppPaths::config_dir()`, docs/0004) and pick them in Appearance → Theme.
Files are watched: new, edited, and removed themes take effect on the
running app without a restart. The bundled set shrinks to the library's
built-in **`Default Light` / `Default Dark`** — the embedded Catppuccin
Mocha is removed (dark mode visibly changes; accepted). Catppuccin flavors
return later as generated output via Whiskers (#143), not hand-maintained
files.

On a machine where the app runs from a dev checkout (no Home Manager),
`scripts/install-catppuccin-theme.sh [flavors…]` does the same
installation by hand: it renders `nix/catppuccin.tera` with Whiskers
(available in the dev shell), applies the same gates the nix package
applies, and writes the files into the platform's user themes dir —
`~/Library/Application Support/my-dashboard/themes/` on macOS,
`~/.config/my-dashboard/themes/` under XDG. The watcher picks the
files up live; no restart.

## 2. Upstream facts (live introspection 2026-09-07, pin `0c746df`, `crates/ui/src/theme/`)

- `ThemeRegistry.themes: HashMap<SharedString, Rc<ThemeConfig>>` — **keyed
  by name only**; two entries cannot share a name.
- `init_default_themes()` (runs inside `gpui_component::init` →
  `theme::init`) registers exactly two themes from `default-theme.json`:
  **`Default Light`** (light, `is_default: true`, 99 color keys) and
  **`Default Dark`** (dark, `is_default: true`, 94 keys). They populate
  both `themes` and the mode-keyed `default_themes` fallback map. There is
  no other stock theme set at this pin.
- `load_themes_from_str(&str)` parses a `ThemeSet`; per-theme color keys
  are individually optional (defaults fill the rest). Inserts only names
  not already present (`has_custom_themes` flag set).
- `watch_dir(PathBuf, &App, on_load)` (`#[cfg(not(target_family =
  "wasm"))]`): creates the dir if missing, spawns a `notify` recursive
  watcher, then `reload_themes()` + `on_load` — once up front and again on
  every Create/Modify/Remove event.
- `reload()` reads **flat** `*.json` in the dir (the watcher is recursive,
  the loader is not — upstream inconsistency), parses per file
  (invalid file → `tracing::error!`, others still load), and **skips names
  already registered** — so its hot reload is add-only: edits to an
  already-loaded theme do not update it, removed files leave stale
  entries. A user theme with `is_default: true` replaces the
  `default_themes[mode]` fallback pair.
- `cx.observe_global::<ThemeRegistry>` (installed by `theme::init`)
  re-fetches the active light/dark slot configs by name on any registry
  change, re-applies `Theme::change`, and refreshes windows. A name that
  vanished leaves the previously installed config in place.
- `sorted_themes()`: `is_default` first, light before dark, then name —
  the two defaults float to the top of any picker.
- **Found during implementation:** `init_default_themes` is **private**,
  and `ThemeConfig`/`ThemeSet` are `Serialize + Clone` — so a rebuild
  restores the defaults by replaying a serialized snapshot of the
  pristine pair through `load_themes_from_str` (see D5). The private
  `default_themes` fallback map is not repopulated by that path; its
  only read outside `Theme::change`'s never-taken fresh-bootstrap
  branch was this crate's Appearance page, which now uses fixed-name
  constants (D9).

## 3. Design

**D1 — Location: `<config>/themes/`, flat.** User-authored content is
config, not state (docs/0004 roots table). The dir is created on demand
by the watcher. Flat only — upstream's loader ignores subdirectories even
though its watcher sees them; documented as flat until upstream grows
recursive loading.

**D2 — Format: `ThemeSet` JSON** (same shape as the removed Mocha file,
docs/0006 §3.6). Color keys optional; upstream publishes
`.theme-schema.json` for editor validation — linked from this doc and the
Appearance description, not bundled. `highlight` sections parse; since
docs/0070 they are consumed — the Diff/highlighter resolves syntax
tokens through them. `is_default` in user files is
unsupported: it would silently swap the library's fallback pair. Say so
in the docs; do not special-case it.

**D3 — Bundled set: the upstream defaults, nothing else.** Delete
`CATPPUCCIN_MOCHA` / `CATPPUCCIN_MOCHA_NAME` and
`crates/core/assets/themes/catppuccin-mocha.json`; delete
`install_bundled_theme`. The factory dark slot becomes `Default Dark`
(the registry's `default_dark_theme()`); the Appearance dark palette
field's `default_value` follows it. Acknowledged visual change: dark mode
no longer looks like Mocha. Naming note: the registry is name-keyed, so
two bundled themes cannot literally both be called "default" — the
upstream names `Default Light` / `Default Dark` are kept verbatim; each
slot picker is mode-filtered, so only one default is ever offered per
slot.

Amended by docs/0074: the bundled set grows — the factory Aurora pair
plus the vendored gpui-component presets (the library's upstream
defaults stay unshipped); replay order factory → user dir → presets.


Amended by docs/0078: the bundled set is exactly the kit gallery verbatim
(`aurora.json` included) and the library's upstream defaults ship again via
the vendored pair file (`kit-defaults.json`, `is_default` kept); replay
order kit defaults → user dir → presets.
**D4 — Boot order (synchronous first load).** `settings::init` (runs
after `gpui_component::init`, before any window exists): load persisted
values → walk the themes dir once synchronously (same loader as below)
→ `apply_saved_theme` → spawn the watcher. The synchronous walk is what
makes a persisted *user* theme resolve on the very first frame.

**D5 — Hot reload: rebuild, don't merge.** Upstream `reload()` is
add-only (§2), which would make edits/removals need a restart. Instead,
the `watch_dir` `on_load` callback runs our own rebuild:
`cx.set_global(ThemeRegistry::default())` → replay the bundled defaults
→ load every dir file (per-file tolerance, D7) → let the library's
`observe_global` re-resolve the active slots by name and refresh windows.
Fresh-global rebuild gives full add/edit/remove semantics; the library
observer does the re-styling. The initial `on_load` (watcher spawn
re-loads once) is an idempotent duplicate of D4's walk — accepted, files
are tiny. As shipped, the defaults are restored from a boot-time
As shipped, the defaults are restored from a checked-in, verbatim copy of
the upstream pair (docs/0078 D2; docs/0056 introduced the checked-in-file
approach for its own — since-retired — pair); the fallback map stays empty,
which nothing reads at runtime (D9).

**D6 — Collision precedence: first registration wins.** `Default Light` /
`Default Dark` are registered before any user file, and both our loader
and upstream's skip existing names — user files re-using those names are
skipped with a stderr note. Bundled names cannot be shadowed. Document:
don't name user themes like the defaults.

**D7 — Error handling per [DG]: per-file degrade, stderr only.** One
invalid file logs `path: error` at warn/error level and is skipped;
siblings still load; the app never dialogs about themes. Removed-name
behavior: a theme that disappears while active keeps its last-installed
config until the mode changes or the app restarts (library observer
cannot re-fetch a missing name) — then falls back to that mode's
`default_themes` entry.

**D8 — Migration: stale persisted names degrade, are not rewritten.**
Installs that persisted `theme_name_dark: "Catppuccin Mocha"` boot with
`install_slot_theme` failing → warn → `Default Dark`. The stale value
stays in `settings.json` (harmless, warned once per boot); the picker
always shows the live slot config. No settings migration code.

**D9 — Reset semantics unchanged (docs/0006).** Appearance Reset All
restores `System` mode; the palette fields' `default_value`s become the
bundled default names. No new UI: the picker rebuilds per render from
`sorted_themes()`, so reloaded themes appear (and disappear) without
Appearance-specific code. As shipped, the defaults are name constants
(`DEFAULT_LIGHT_NAME`/`DEFAULT_DARK_NAME`) rather than
`default_light_theme()`/`default_dark_theme()` lookups — the fallback
map those getters read is not repopulated after a rebuild (§2, D5).

**D10 — Whiskers out of scope (#143).** No Catppuccin flavor is bundled
or shipped with this design; the `.tera` template, the four generated
flavor files, and the drop-in workflow land via #143. The removed Mocha
JSON is kept in git history as the semantic-key → Catppuccin-role
reference for that work.

Amended by docs/0074: the four flavors ship built-in (vendored kit
presets). The Whiskers flow stays as the **override** path — user-dir
files load before the presets, so a Whiskers install shadows the
built-in flavors by name, unchanged.

## 4. Implementation plan (one PR, after this doc merges)

`crates/core/src/settings.rs` is the only code site (plus deletions):

1. Remove `CATPPUCCIN_MOCHA*`, `install_bundled_theme`, and the asset.
2. Add `AppPaths::themes_dir()` (config dir + `themes`), a
   `load_themes_dir` walk (read flat `*.json`, parse `ThemeSet`, insert
   with first-registration semantics, log per-file failures), the
   `snapshot_bundled_defaults`/`rebuild_themes` pair per D5, and
   `DEFAULT_*_NAME` constants per D9.
3. `init`: values → `load_dir` → `apply_saved_theme` → spawn
   `ThemeRegistry::watch_dir(themes_dir, cx, on_load = rebuild)` — the
   spawn must tolerate a missing config path (tests run without
   `AppPaths`; fall back to no watcher, as the save handle already
   does).
4. Appearance page: dark field `default_value` from
   `default_dark_theme().name`; description gains the themes-dir path.
5. Tests: defaults still parse and slot-apply (replaces
   `bundled_catppuccin_mocha_is_valid_dark_theme`); `load_dir` handles a
   valid file, an invalid file beside it, and a default-name collision;
   stale persisted Mocha name degrades to `Default Dark`; rebuild
   re-registers defaults + dir content from a scratch global.

## 5. Testing

- Unit (no window): the five cases in §4.5, with inline `ThemeSet`
  fixtures written to a temp dir; assertions on registry contents and
  slot names, log assertions kept loose (warn emitted).
- Probe: not needed — no interactive surface changes; the picker is
  upstream-rendered and already covered by docs/0006 tests.

## 6. Human checks (owner)

- [ ] Drop a valid theme file into `~/.config/my-dashboard/themes/` → it
      appears in the matching slot's picker without restart; select it;
      survives restart.
- [ ] Edit a color hex in the active theme's file → app restyles within a
      beat, no restart.
- [ ] Delete the file → gone from the picker after the change; the app
      stays styled until mode switch/restart, then falls back to the
      default.
- [ ] Broken JSON in the dir → boot fine, file skipped, stderr note,
      other themes intact.
- [ ] Fresh boot dark mode now renders `Default Dark` (not Mocha);
      `Reset All` in Appearance returns to the defaults.

## 7. Follow-ups (GitHub issues, not this PR)

- ~~#143 — Whiskers-generated Catppuccin flavors (template + drop-in
  workflow + optional release artifacts).~~ → shipped: nix/catppuccin.tera
  + flake package + HM module (see §9); release artifacts not needed
  (the flake generates on demand).
- Upstream report: watcher is recursive, loader is flat
  (`themes_dir` subdirectories silently inert) — file against
  gpui-component when we next touch themes.

## 8. Docs housekeeping

- This design-doc PR: adds 0025 to the docs/README.md index and resolves the
  stale roadmap line that still labels "Build, packaging & distribution per
  OS" as 0006 (the number was reused by the settings doc — it takes the next
  free number when written), per #23's housekeeping note.
- Implementation PR: docs/0006 §3.6's bundled-Mocha paragraphs are replaced
  by a pointer here, in the same PR that removes the embedding.

## 9. Shipped record — Catppuccin via Whiskers (#143, flake-integrated)

**Authoring.** `nix/catppuccin.tera` is the one-time semantic-key →
Catppuccin-role mapping (100 keys, transcribed from the removed bundled
mocha — see D3), parameterized per flavor: theme name
`Catppuccin {{flavor.name}}`, `mode` from `flavor.dark` (Latte → light,
rest → dark), all hexes as `#{{<color>.hex}}` plus the constant alpha
suffixes where the old mapping had them. Sapphire stays the accent.
Whiskers facts (docs, v2.9): `matrix: [flavor]` + `filename` (required
with matrix) writes one file per flavor relative to CWD;
`flavor.dark`/`flavor.name`/`<color>.hex` are single-flavor context;
the default `hex_format` yields 6-digit hexes for the fully-opaque
palette.

**Nix path (primary).** The flake builds
`packages.my-dashboard-catppuccin-themes` by running
`nixpkgs#catppuccin-whiskers` over the template (hermetic — the palette
is built into whiskers; no network). jq gates in the derivation assert
each file parses with a `Catppuccin ` name, a light/dark mode and 100
color keys, and that there are exactly 4 — the build fails rather than
half-shipping. `homeManagerModules.default` gains
`programs.my-dashboard.catppuccin.{enable,flavors}` (independent of
`programs.my-dashboard.enable`); enabling links the chosen flavor files
into `~/.config/my-dashboard/themes/` **file-by-file**, so hand-dropped
themes coexist. `home-manager` must not manage that directory as a
whole (standard `xdg.configFile` conflict rules apply).

**Manual path (no Nix).** `whiskers nix/catppuccin.tera` (or
`cargo install catppuccin-whiskers` first) renders the four files into
`themes/` next to the CWD; copy the ones you want into
`~/.config/my-dashboard/themes/`. They hot-reload like any other theme
file (D5).

**Name auto-heal.** The generated names are the pre-0025 ones
(`Catppuccin Mocha`, …), so an install that persisted the old bundled
name and then enables catppuccin resolves its D8 fallback silently on
the next boot — no settings edit needed.
