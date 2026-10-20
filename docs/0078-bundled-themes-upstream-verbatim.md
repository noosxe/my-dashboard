# 0078 — Bundled themes: upstream verbatim, zero app-owned palettes

- **Date:** 2026-10-20
- **Amends:** docs/0025 D3 (again), docs/0056 (superseded), docs/0065 D4, docs/0074 D1/D2/D4/D7/D8
- **Disposition:** #524 closed as moot-by-design; #254 unaffected (already closed on the gallery)

## 1 — Decision

Owner verdict (2026-10-20): maintaining app-owned theme content is a hassle we
stop taking on. The bundled set becomes **exactly the pinned gpui-component
gallery — every file, including `aurora.json`** — and the factory defaults
become **the kit's own `Default Light` / `Default Dark` pair**. The app-owned
Aurora pair (docs/0056) is deleted: **`Aurora Dark` is retired**; any theme we
might want in the future ships as a **user-installed theme file**
(docs/0025 §9 path), never as bundled content. Zero app-owned palette bytes
remain in the tree.

This is the same trust stance docs/0074 already wrote down ("upstream
curation is trusted — same trust we already extend to every component
default"), taken to its conclusion: we no longer curate, exclude, or override
any of it.

## 2 — Upstream facts (live introspection 2026-10-20, pin `1676826`)

- The kit's `default-theme.json` holds exactly **`Default Light`** (light,
  `is_default: true`, 99 color keys, 27-entry syntax map) and **`Default
  Dark`** (dark, `is_default: true`, 94 keys, 26-entry syntax map).
  `init_default_themes` registers them into `themes` **and** the mode-keyed
  fallback map — but the method is **private** (docs/0025 §2); the public
  path is `gpui_component::init` → `theme::init`, which our app calls at boot
  (main.rs) *before* settings rebuilds the registry.
- Upstream's gallery `themes/aurora.json` is **`Aurora Light` only** — there
  is no upstream Aurora Dark. Its colors are a divergent port of ours: **81
  of 82 shared color keys differ** (slate gradient `primary.background` vs
  our flat violet, different neutrals). Its `highlight.syntax` sub-map is
  the classic Xcode light palette (`keyword = #0433ff`, `comment =
  #007fff`) — inspected on #524 and rejected there as seed material; under
  this doc it ships as-is because it is upstream's, and upstream curation is
  the point.
- `load_themes_from_str` is insert-only (first registration wins) — the
  replay order remains the authority rule (docs/0025 D5/D6).

## 3 — Design

**D1 — Bundled set: the kit gallery, verbatim.** `assets/themes/presets/`
gains upstream's `aurora.json` → **21 files / 36 themes**, no exclusions.
The docs/0074 D2 exclusion (and its "factory pair must not depend on load
order" rationale) is obsolete: there is no factory pair to protect. The
parse-and-shape smoke test pins the count.

**D2 — Shipped defaults: the kit's own pair, vendored verbatim.** The
`BUNDLED_DEFAULTS` slot switches from our aurora.json to **`assets/themes/
kit-defaults.json`** — a byte-for-byte copy of the kit's
`crates/component/src/theme/default-theme.json` at the pin, **with
`is_default: true` kept** (unlike presets, which strip it): the pair
registers first in every rebuild, floats to the top of the pickers via
`sorted_themes()`, and re-points the fallback-map semantics at the library's
own names. docs/0056's reasoning for a checked-in file over a runtime
snapshot ("reviewable and diffable") applies unchanged — the file is
upstream's now, synced by the same bump ritual (D8). `DEFAULT_LIGHT_NAME` /
`DEFAULT_DARK_NAME` become `"Default Light"` / `"Default Dark"`.

**D3 — Boot and stale-name semantics.** Replay order: kit defaults → user
dir → presets (docs/0025 D5/D6, unchanged). Factory boots and stale-name
boots land on the kit pair via the existing install-by-name fallthrough.
A persisted `"Aurora Dark"` is unknown after this ships: the documented
degradation applies (docs/0025 D8 — warn once per boot, persisted value
untouched, palette falls back). Recovery for Aurora Dark fans: the file
lives in git history (`docs/0056`'s asset) and installs as an ordinary user
theme file — the sanctioned path for every non-bundled theme from here on.

**D4 — Accepted visual changes.** (a) Dark default: `Aurora Dark` →
`Default Dark` (softer blues, gray comments — a *less* piercing first boot
than the one #524 was filed about). (b) Light "Aurora Light" keepers: same
name, upstream's divergent port — visibly different (81/82 keys). Both are
the decision working as intended: first-boot look is upstream's call now.

**D5 — The contrast gate retires with the app-owned theme.** `contrast_gate.rs`
vouches for WCAG AA on the factory palette — content we chose. Under 1:1 we
vouch for nothing: gating upstream's defaults would re-litigate their
curation every bump (exactly the hassle this doc removes), and docs/0074 D4
already ruled we don't AA-vouch for upstream editor themes. The gate's file
is deleted; docs/0065 D4 is amended to scope AA gating to *app-owned* theme
content — of which there is now none. The threshold table stays in 0065 for
any future app-owned surface.

**D6 — #524 disposition.** The retune targeted the 0025→component mapping of
the factory pair. The factory pair no longer exists; nothing app-owned
remains to retune. #524 closes as moot-by-design, with the upstream-aurora
inspection (Light-only, Xcode syntax, tints not in `highlight`) recorded on
the issue as the trail.

**D7 — Docs reconciliation (the amendments).**
- **docs/0025 D3**: amended again — bundled set = the kit gallery verbatim;
  the library's upstream defaults ship (via the vendored pair file); replay
  order kit defaults → user dir → presets.
- **docs/0056**: superseded — the factory pair is retired; `Aurora Dark` is
  recoverable from this doc's git history as a user-installed file.
- **docs/0065 D4**: AA gate scoped to app-owned theme content (now none).
- **docs/0074**: D1/D2 (no exclusion), D4 (gate scope), D7 (#524 resolved),
  D8 (sync list gains `aurora.json` + `kit-defaults.json`) amended in place.
- **docs/README**: 0056 row marked superseded; 0074 row updated; 0078 row
  added.

**D8 — Bump ritual.** The gpui-component bump checklist re-copies
`themes/*.json` (all of it, no exclusions) into `presets/` **and**
`crates/component/src/theme/default-theme.json` into
`assets/themes/kit-defaults.json`. The smoke test and the defaults-name
test catch shape drift.

## 4 — Tests

1. `presets_parse_and_carry_highlight`: count 35 → **36**; all prior
   per-theme shape asserts hold (upstream `aurora.json` carries a non-empty
   `highlight` — the section the Diff story depends on).
2. `rebuild_replays_the_bundled_defaults_and_all_presets`: reworked — the
   registry holds `Default Light` / `Default Dark` **plus** 36 presets
   (38 names); the pair carries `is_default: true`; upstream's
   `Aurora Light` registers with upstream's values (no value asserts
   against our old file).
3. Factory-boot slot install: a no-saved-names boot installs the kit pair by
   name (replaces the contrast gate as the "what ships is what boots" check).
4. Stale-name degrade and user-dir shadowing tests: unchanged semantics;
   fixtures/comments re-pointed at the new names.

## 5 — Verification

- Gates: fmt, clippy (zero warnings), workspace tests.
- Receipts (docs/0071): the settings surface golden **changes** (picker
  content; default names) — recaptured in the containerized env in the
  implementation PR; audit chain + `tree --tab-stops` re-run; the
  Appearance picker entries are interactive and must appear in the tab-stop
  cycle. `golden.txt`'s Aurora-theme capture notes are re-pointed at the
  new defaults.

## 6 — Non-goals

- **Picker grouping/search** — unchanged from docs/0074 §6.
- **Any new bundled theme, ever** — future themes are user-dir files
  (docs/0025 §9); this doc sets that policy, not just this snapshot.
- **AA gating of upstream content** — trusted, not vouched (D5).
- **Publishing release artifacts of the retired Aurora pair** — git history
  is the archive; the user-file path (D3) is the recovery.
