# 0074 — Built-in theme presets: the kit's theme gallery, bundled

Date: 2026-10-14
Status: design (docs-only PR)
Follow-ups: implementation issue(s) to be filed after merge; upstream
accessor contribution filed `deferred`
Amends: docs/0025 D3 (bundled set = factory pair only), D10 (no bundled
Catppuccin), §2 D2 note (`highlight` sections unused); docs/README index

## 1 — Problem

The factory install ships exactly two themes — the Aurora pair
(docs/0056). Everything else requires the user to find, download, and
drop `ThemeSet` files into `~/.config/my-dashboard/themes/` (docs/0025).
The #524 human check made the cost visible: the Diff's default syntax
palette (the kit's built-in fallback, a wall of blues) "pierces," and
retuning the 0025→component mapping is real design work we deferred.

Meanwhile the pinned upstream ships a curated theme gallery one
dependency away: **20 preset files / 35 themes** (10 light, 25 dark) in
`themes/*.json` at the `gpui-component` checkout — catppuccin (all four
flavors), tokyonight (three variants), gruvbox, ayu, solarized, flexoki,
everforest, and more. **Every one carries a complete `highlight` syntax
section** — pick any preset and the Diff gets a designed palette for
free. The #524 verdict ("colors pierce") and this request have the same
shape: the app should not make users hand-install what its own UI kit
already carries.

This doc bundles the gallery as built-in themes: pickable from the
Appearance picker on first boot, no files, no Whiskers, no restart. The
#524 retune stays deferred — Aurora is untouched; the presets simply
give every user an immediate non-piercing alternative.

## 2 — Upstream facts (live introspection 2026-10-14, `gpui-component`
@ `67265a3` = v0.7.3)

- `themes/` holds **21 files / 36 themes**; we exclude the kit's own
  `aurora.json` (see D2), leaving **20 files / 35 themes — 10 light,
  25 dark**. Largest file ~13 KB; the gallery totals ~172 KB.
- **No preset sets `is_default`** — registering them cannot hijack the
  mode-keyed fallback slots (docs/0025 §2).
- **Every preset carries a `highlight` section** (7–49 keys) with the
  Zed-compatible `syntax` map the Diff/highlighter resolves through
  `HighlightStyleResolver` (docs/0070 D3).
- The only native embed of `themes/` in the component crate is
  **`aurora.json` itself — a stale, partial copy** (theme count
  asserted as exactly 1: `Aurora Light` only, no `Aurora Dark`) in a
  test at `crates/component/src/theme/schema.rs:1274`. The full gallery
  is embedded only by the **story** crate's `embedded_themes()`, which
  is `#[cfg(target_family = "wasm")]` — **not reachable from a native
  build**, and the story crate is not (and should not become) a
  dependency of the app.
- Registry mechanics (docs/0025 §2, unchanged at this pin):
  name-keyed `HashMap`; `load_themes_from_str` parses a `ThemeSet` and
  **skips names already registered**; nothing evicts; `sorted_themes()`
  floats `is_default` first, light before dark, then name; the user-dir
  watcher reloads add-only.
- Our install path (docs/0025 D5, docs/0056 D1): `rebuild_themes`
  replays `BUNDLED_DEFAULTS` (our `aurora.json`) first, then loads the
  user dir; the registry observer re-applies slots by name on any
  change.

## 3 — Design

**D1 — Vendored copies, not an upstream accessor.** The gallery ships
as checked-in copies under `crates/core/assets/themes/presets/*.json`,
one `include_str!` list in a small `presets` module of
`dashboard-core`. The alternative — contributing a native
`theme_presets()` accessor to the component crate (moving the list out
of the wasm-gated story module) — is the cleaner end state but buys us
upstream review latency on longbridge/gpui-component for zero
functional difference: our dep is **rev-pinned**, so the vendored files
cannot drift without a deliberate bump PR anyway. We contribute the
accessor upstream as a `deferred` issue; if it lands, a later bump PR
deletes the vendored copies and switches to the crate API. Until then,
syncing presets is one `cp` inside the existing bump ritual (D8).

**D2 — Preset set excludes the kit's `aurora.json`; factory and user
themes outrank presets by name.** The kit's vendored Aurora is a stale
subset of ours (one theme vs our pair) — shipping it would rely on
load-order luck to keep our factory pair winning. So the preset bundle
is the kit gallery **minus `aurora.json`** (20 files / 35 themes), and
the replay order is: factory pair (`BUNDLED_DEFAULTS`) → user dir →
presets. Insert-only semantics then make the order the authority rule
with no extra machinery: the factory pair always registers first, and —
since `load_themes_from_str` *skips* names already present — a user
file with a preset's name (e.g. a hand-tuned Catppuccin flavor)
must load **before** the presets to win. (The implementation PR
corrected an earlier draft of this paragraph, which put presets before
the user dir; the registry tests caught that order handing preset names
to the built-ins.) A test pins the shadowing.


Amended by docs/0078: the exclusion is lifted — the bundled gallery is
verbatim (21 files / 36 themes, `aurora.json` included) and the app-owned
factory pair is retired; the shipped defaults are the kit's own pair
(vendored verbatim, docs/0078 D2).
**D3 — `is_default` is stripped from presets at load.** No preset
carries the flag today (§2), but the strip is one line and makes the
invariant structural: the fallback slots stay factory-owned regardless
of what upstream ships later. Mirrors docs/0025 D2's stance on user
files, moved from "documented as unsupported" to "structurally
enforced" for everything the app itself loads.

**D4 — Gate scope: factory AA gate unchanged; presets get a parse-and-
shape smoke test.** `contrast_gate.rs` keeps vouching for the Aurora
pair only — that gate is our design guarantee, and extending AA to 35
upstream editor themes would fail on their aggressive syntax colors and
vouch for nothing we chose. The preset test asserts, per file: parses
as a `ThemeSet`, every theme has a mode, a non-empty colors map, and a
non-empty `highlight` section (the property the Diff story depends on),
and no `is_default` (belt for D3). Upstream curation is trusted for the
rest — same trust we already extend to every component default.

Amended by docs/0078: the factory AA gate retires with the factory pair —
docs/0065 D4 scopes AA gating to app-owned theme content, of which none
remains. Upstream curation is trusted, not vouched.

**D5 — The picker needs no change.** `sorted_themes()` already orders
defaults first, light before dark, then name; persistence and the
light/dark slot pickers are name-keyed and work for any registered
theme. 35 extra entries in a flat, sorted list is acceptable for this
round (Non-goal: grouping).

**D6 — Docs reconciliation (the amendments).**
- **0025 D3** (bundled set = library defaults): the bundled set becomes
  the factory pair **plus the kit presets**; user-dir authority and
  hot-reload semantics unchanged.
- **0025 D10** (no bundled Catppuccin): superseded — Latte, Frappé,
  Macchiato and Mocha ship built-in. A user-dir file with a preset's
  name remains the **override** path: same names, user dir wins (D2).
  The Nix-side Whiskers builder (template, theme package, HM options,
  install script) has since been removed as redundant — docs/0025 §9.
- **0025 §2 D2 note** ("`highlight` sections parse but are unused — no
  code editor in-app"): stale since docs/0070 adopted the Diff; the
  note is corrected to point at the Diff/highlighter consumption.
- **docs/README**: index row for 0074; the README's theme § describes
  the built-in presets and the user-dir override path.

**D7 — #524 interplay: no Aurora change in this scope.** The retune of
the 0025→component mapping (both Aurora themes' `danger`/`success`
washes and the missing `highlight` section) stays deferred on #524.
This doc ships alternatives, not the fix. A deliberate non-goal-adjacent
note: the kit's stale `aurora.json` carries a 7-key `highlight` section
we could lift into ours in a one-line follow-up — filed `deferred`, not
bundled here, because any Aurora `highlight` addition is a #524 design
decision (which palette family matches Aurora), not a vendoring step.


Resolved by docs/0078: the factory pair is retired and #524 closes as
moot-by-design. The candidate seed was inspected on the issue first (the
kit's `aurora.json` highlight section is the Xcode light palette,
Light-only) and rejected there before the retirement decision.
**D8 — Bump ritual: re-sync presets on every gpui-component bump.** The
Cargo.toml bump checklist (exact-pin comment) gains one line: re-copy
`themes/*.json` (minus `aurora.json`) from the new checkout into
`crates/core/assets/themes/presets/` and let the smoke test (D4) catch
shape drift. Presets change only when we bump, so the copies cannot rot

Amended by docs/0078: the sync copies the whole gallery (`aurora.json`
included) plus the kit's `default-theme.json` into
`crates/core/assets/themes/kit-defaults.json` (the shipped defaults).
between bumps.

## 4 — Tests

1. `presets_parse_and_carry_highlight` (D4): every vendored file parses
   as a `ThemeSet`; 35 themes; per-theme mode + colors + highlight
   present; no `is_default` anywhere.
2. `rebuild_registers_all_presets` (D2): after `rebuild_themes`, all 35
   preset names are registered alongside `Aurora Light/Dark`; the
   factory pair is untouched (our Aurora values, not the kit's).
3. `user_dir_preset_names_win` (D2): a user file naming `Catppuccin
   Mocha` (a one-color sentinel) leaves the registry with the user's
   copy, not the built-in.
4. `preset_defaults_cannot_hijack_slots` (D3): a hand-edited preset
   with `is_default: true` still lands with the flag stripped and the
   fallback slots unchanged.


Amended by docs/0078: the smoke count is 36; the rebuild test pins the kit
pair plus all presets (38 names) instead of the factory pair.
## 5 — Verification

- Gates: `cargo fmt --all --check`, `cargo clippy --workspace
  --all-targets` (zero warnings), `cargo test --workspace`.
- Receipts (docs/0071): the settings surface's structural golden
  **changes** (the picker list grows) — new golden captured in the
  containerized env in the implementation PR; full audit chain +
  `tree --tab-stops` re-run; the new picker entries are interactive
  elements and must appear in the tab-stop cycle.
- CI: docs/0027 unchanged (no new jobs; the presets ride the existing
  test build).

## 6 — Non-goals

- **Picker grouping/search** (by mode, family, or search box) — 40 flat
  sorted entries ship first; a grouping pass is its own UI change with
  its own 0065 ritual, filed if the list hurts.
- **Contrast-gating the presets** (D4) — we don't AA-vouch for upstream
  curation.
- **Aurora retune / `highlight` for the factory pair** — #524's scope,
  still deferred; this doc neither fixes nor worsens it.
- **Upstream accessor adoption** — filed `deferred`; lands via a future
  bump PR (D1/D8).

Amended by docs/0078: the #524 non-goal is resolved by retirement — the
factory pair no longer exists to retune.
- **Theme authoring UX, live preview, per-surface overrides** — out of
  scope; docs/0025's file-based flow remains the authoring story.
- **wasm** — the app is desktop; the wasm-gated upstream accessor is
  irrelevant to this design.

## 7 — Shipped record

Shipped in one PR (issue #568):

- D1/D2: 20 files vendored under `crates/core/assets/themes/presets/`;
  `theme_presets::load` replayed from `rebuild_themes` after the user
  dir. **D2's replay order was corrected during implementation** — the
  original draft (factory → presets → user) handed preset names to the
  built-ins; `user_dir_preset_names_win` caught it, and this doc's D2
  now states factory → user dir → presets.
- D3: `theme_presets::sanitize` strips `is_default` before the registry
  ever sees a preset.
- D4: `presets_parse_and_carry_highlight` (35 themes, mode/colors/
  highlight, no `is_default`, names unique) + `contrast_gate.rs`
  untouched.
- Tests: the §4 list, all green (`presets_parse_and_carry_highlight`,
  `preset_defaults_cannot_hijack_slots`,
  `rebuild_replays_the_bundled_defaults_and_all_presets` — factory pair
  pinned to our values, 37 names —, `user_dir_preset_names_win`).
- D6: 0025 D2-note/D3/D10 amended; README row + theme section
  describe the presets and the user-dir override path; this §7.
- D8: Cargo.toml bump checklist gained the re-sync line.
- Receipts: see the PR body (settings golden recaptured in the
  containerized env; audit chain + tab-stops re-run).
