# 0056 — Factory color theme: Aurora (dark + light)

- **Date:** 2026-09-22
- **Issue:** none — owner pick from live candidate previews (session
  receipts below)
- **Amends:** docs/0025 D3 (bundled set = the upstream defaults), D5
  (defaults restored from a runtime snapshot), D9 (default-name
  constants)

## 1. Summary

The factory palettes — what a fresh install sees with no settings and
nothing in the themes dir — become **`Aurora Light` / `Aurora Dark`**,
a multi-hue pair designed for this app. The upstream gpui-component
pair (`Default Light` / `Default Dark`, the shadcn *neutral* palette:
pure grayscale, a five-shades-of-one-blue chart ramp) no longer ships:
it read as flat and monochrome, which is exactly what it is. The pair
is embedded at compile time (`crates/core/assets/themes/aurora.json`)
and replaces docs/0025 D5's runtime snapshot as the replay source for
every registry rebuild.

Persisted installs that name the removed upstream pair degrade per
docs/0025 D8 (warn once, land on the factory palette, persisted value
untouched) — the acknowledged visual change of this doc, same
precedent as 0025's own Catppuccin Mocha removal.

## 2. Provenance

- **Derivation:** neutrals are a single cool slate family (OKLCH hue
  280, chroma 0.004–0.020 by step); the accent set is eight hues fixed
  at mode-appropriate lightness/chroma (dark: L 0.65–0.79, C 0.11–0.19;
  light: L 0.48–0.64, C 0.08–0.20). Violet is primary. Charts order
  five of the eight hues for adjacent contrast:
  violet → cyan → yellow → magenta → green; bullish/bearish are the
  green/red pair.
- **Tooling survey (2026-09-22):** no CLI tool emits desktop-app theme
  JSON — the CLI-grade tools (`flavours`, tinted-theming/base16, Gogh)
  are terminal-scheme managers. Web tools: tweakcn (visual shadcn
  editor; covers only the ~26 core vars, not this registry's ~98 keys),
  Adobe Leonardo (contrast-targeted generation, programmatic lib),
  Atmos/incluud (OKLCH ramps + WCAG/APCA checks). None maps to this
  token set, so the pair was derived directly in OKLCH (reference
  Ottosson matrices) and authored as `ThemeSet` JSON — the same format
  user themes use (docs/0025 D2), hand-tweakable from here on.
- **Selection:** three multi-hue candidate pairs (Ember Ink — Flexoki-
  adapted; Aurora; Sage Rose — warm-green) were rendered live at
  1280×800 in both modes over the home and GitHub pages via the
  docs/0025 themes-dir watcher + the docs/0010 bridge, and the owner
  picked Aurora. The other two candidates were never committed.

## 3. Design

**D1 — Embedded asset, not a runtime snapshot.** `BUNDLED_DEFAULTS`
becomes `include_str!("../assets/themes/aurora.json")`;
`snapshot_bundled_defaults` and its `OnceLock` are deleted. The
snapshot existed only because upstream's `init_default_themes` is
private (docs/0025 §2) — moot once the shipped defaults are ours. A
checked-in file is reviewable, diffable, and revertible; the JSON is
the single source of truth for the palette.

**D2 — Names and flags.** `Aurora Light` / `Aurora Dark`, `is_default:
true` on both. `is_default` is honored by `sorted_themes()` only (the
load path ignores it — verified at the current pin, `registry.rs`
`load_themes_from_str` never touches the fallback map), so it floats
the pair to the top of the Appearance pickers and nothing else.
`DEFAULT_LIGHT_NAME` / `DEFAULT_DARK_NAME` (docs/0025 D9) point at the
new names; the Appearance fields' `default_value`s follow.

**D3 — The factory fallback is installed by us, not bootstrapped by
the library.** On any boot, `apply_saved_theme` now installs a palette
into **both** slots: the persisted name, or — when absent *or* failed
(unknown/stale, docs/0025 D8) — the embedded factory name for that
mode. This matters because the registry's private `default_themes`
fallback map still holds the *upstream* pair (`gpui_component::init`
populates it before our rebuild runs): a fresh `Theme` global
bootstraps its slots from that map, so without an explicit install a
factory boot would render shadcn neutral, not Aurora. The install
order is safe: `rebuild_themes` registers the pair before
`apply_saved_theme` runs (docs/0025 D4), and first-registration-wins
(docs/0025 D6) keeps user files from shadowing the names.

**D4 — Palette reference.** Core values (full set in the asset; the
`colors` maps carry 98 (dark) / 97 (light) keys):

| token | Aurora Dark | Aurora Light |
| --- | --- | --- |
| `background` | `#0e0f15` | `#f8f9fc` |
| `foreground` | `#dedfe8` | `#1e1f26` |
| `border` | slate step L 0.29 | slate step L 0.905 |
| `primary.background` | `#9c76f3` | `#7447c8` |
| `chart_1..5` | `#9c76f3` `#3ebfc6` `#ddb63e` `#da5ab6` `#53be70` | `#7447c8` `#028a9b` `#ab8704` `#b42c92` `#17843f` |
| `chart_bullish` / `chart_bearish` | `#53be70` / `#f0555b` | `#17843f` / `#c2272d` |
| sidebar / title bar | `#08090e` / `#14141b` | `#f1f1f7` / `#eff0f4` |

Pill tokens (`success`/`danger`/`warning`/`info`) are solid hue
backgrounds with ink-on-dark / white-on-light foregrounds; list/table
active states tint the primary; selection, caret, ring, drag border,
slider and progress bar all ride the primary.

**D5 — No `highlight` section.** The asset ships `colors` only. The
Zed-style syntax `highlight` maps are unused in-app (docs/0025 D2):
diff token colors derive from `ThemeColor` tokens
(`syntax.rs` `token_style`), which the palette sets.

**D6 — Stale persisted names degrade to Aurora.** Installs that
persisted `Default Light` / `Default Dark` (or any other vanished
name) warn once at boot and land on the factory pair; the stale value
stays in `settings.json` (docs/0025 D8, unchanged). The
`stale_persisted_dark_name_falls_back_to_default` test now asserts the
Aurora landing.

## 4. Implementation (one PR with this doc)

- `crates/core/assets/themes/aurora.json` — the pair (new).
- `crates/core/src/settings.rs` — embedded asset replaces the snapshot
  machinery; constants follow the new names; `install_slot_or_factory`
  + the two-slot install in `apply_saved_theme` (D3); tests updated
  (default pair, collision name, degrade target).
- docs/0025 — no text edits; amended by this doc per its Amends line.

## 5. Receipts

- Boot log (fresh session, no settings, headless dark fallback):
  `Reload active theme: "Aurora Dark"...`; with
  `theme_mode: "light"` only: `Reload active theme: "Aurora Light"...`.
- Registry after boot contains exactly the factory pair plus user-dir
  themes (`rebuild_replays_the_bundled_defaults`).
- Live renders at 1280×800 over Home + GitHub (PR body): multi-hue
  charts and pills, violet primary, slate sidebar distinct from
  content in both modes.
