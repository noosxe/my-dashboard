# 0064 — Favicon generator module

Status: design for review. Implements the requirements in #427 (reference
audit condensed there); cut scope lives in #428–#432 (+ #433, §7). Owner
rulings from the requirements pass are encoded below and called out in §9.

## 1. Context & problem

The dashboard's modules so far consume data (GitHub). #427 adds a
producer: an offline tool that turns a glyph, emoji, image, or SVG into a
platform-correct favicon/app-icon set, with **per-platform output
selection** (web / iOS / Android-PWA; Windows tiles and adaptive layers
are filed away). Everything the user needs today lives in web tools like
favicongenerator.io; we ship the same capability locally, riding the
module conventions, state store, toast hub, usage rail, and debug bridge.

## 2. Facts (receipts)

- Modules register at startup: `ModuleRegistry::new().with::<M: Module>(
  &nav, window, paths, cx)` — `crates/app/src/main.rs:190` registers
  `GithubModule`; `Module::init` returns a `ModuleEntry { meta, overview,
  main_page, settings: Option<_>, nav: Option<_> }`
  (`crates/core/src/module.rs:67–90`); nav addresses modules as
  `Page::Module(SharedString)` (`crates/core/src/nav.rs:33–43`).
- Module crate shape: `dashboard-module-github` deps = core, gpui,
  gpui-component, serde(+json), … (`crates/modules/github/Cargo.toml`);
  `lib.rs` is a flat `pub mod` list.
- State store helpers exist in core: `DebouncedSave`, `load_json`,
  `save_json_atomic` (`crates/core/src/lib.rs:25`); the drafts store is
  the worked example (`drafts.rs:59,109`).
- Usage records have a fixed schema (`usage_records.rs:64–85`) with
  `Transport = Rest | Graphql | Auth` (`:42–46`); `module` is free-form
  (`"github" today; … when those modules exist (D6)`).
- Toast raise API: `Toast::new(level, message).title().dedupe_key()
  .source()` + hub `push` (`toasts.rs:135–197`); sources register per
  0062 D3.
- Upstream pins: gpui-component v0.7.0 (git rev `0c830f4`) ships
  `ColorPicker` (`color_picker.rs:53`), `Slider` (`slider.rs:86`),
  `Switch`, `Checkbox`, `Radio`, `Popover`, `Tab`, searchable list —
  the whole UI vocabulary this page needs. gpui-pre `=0.3.7` pulls
  `resvg 0.46.0` + `image 0.25.9` + `fontdb 0.23` + `tiny-skia` into the
  lockfile already (the 0.45.1 resvg copy is platform-gated dead weight;
  `cargo tree -i resvg@0.46.0` → gpui-pre only).
- `rfd 0.17.2` (docs.rs, verified): Linux backends `gtk3` (default) or
  `xdg-portal`; macOS is native cocoa. Not currently a dependency.
- resvg renders text with its own shaper + fontdb (no native text
  rendering — upstream README "Limitations"); color-emoji raster support
  is not documented → spike S2.
- Bridge rule (0010 D10): every interactive element needs `.id(...)` +
  role (`status.rs:330,383`, `usage_analytics.rs:1658`).

## 3. Decisions

### D1 — Module crate and registration

New crate `crates/modules/favicon` (`dashboard-module-favicon`), deps:
core, gpui, gpui-component, serde, serde_json, resvg, image, rfd. It
implements `Module` with meta id `"favicon"`, a home-grid overview card
(the live preview mini-render + "Generate favicons" action), and a main
page; **no settings subpage and no nav-trail places in v1** (single-page
surface). Registration goes right after github in `main.rs` — sidebar
and home-grid order follow registration order (registry.rs).

### D2 — Single source of truth: emit, then raster

`DesignSpec` (D3) is the model; `render.rs` emits the canonical SVG
string from it, and **every raster frame is resvg's rendering of that
exact emitted SVG** (`resvg::Tree::from_str` → `render` → pixmap →
`image` PNG encode). The on-screen preview shows the raster frames, not
a parallel gpui draw path — preview and export can never drift. Raster
work runs off the UI thread (background executor), debounced ~150 ms on
spec edits; a 512 px raster of this scene is sub-10 ms.

### D3 — `DesignSpec` (model + serde)

```
source:   Text { chars: String (1–2), family: String, weight: u32, scale: f32 }
        | Emoji { glyph: String }
        | Image { png_base64 / jpeg bytes, fit }
        | Svg   { markup: String }          // pass-through, normalized (D4)
style:    fg + bg_start + bg_end per theme (light & dark), shape { radius_frac, circle }
platforms: { web: bool, ios: bool, pwa: bool }   // D5
export:   { dir: Option<PathBuf>, app_name: String }  // D7
```

One flat struct, all-`Option` serde fields defaulting on missing keys
(the 0062 `AppSettings` compat pattern) — a state file from an older v1
patch never fails to load.

### D4 — Emitted SVG contract

- Canonical coordinate space: `viewBox="0 0 512 512"`; every raster size
  is a scale of it.
- Background: two-stop linear gradient (`bg_start → bg_end`); a
  shape-radius path clips both gradient and glyph (circle = radius ½).
- Dark palette (when enabled): the light tree carries the design, plus a
  `<style>@media (prefers-color-scheme: dark) { … }</style>` block that
  swaps the two gradient stops and the glyph fill — **one file, two
  looks**, the reference tool's mechanism.
- Text source: `<text font-family="…" font-weight="…">` — the family
  name travels with the file; machines without the font fall back (the
  page states this next to the font field). Rasters resolve families via
  fontdb system fonts.
- Image source: bytes embedded as a base64 `<image>` — the favicon.svg
  is self-contained and the emit-then-raster rule keeps previews honest.
- Svg source: parsed with `usvg` (same tree resvg builds), re-emitted
  normalized — viewBox scaled into 512, aspect preserved, letterboxed on
  the styled background. No editing (§7).

### D5 — Platform/component matrix

| Platform | Components | Sizes |
|---|---|---|
| Web | `favicon.svg`, `favicon.ico`, `favicon-96x96.png`, `favicon.html` snippet | —, 16+32, 96, — |
| iOS | `apple-touch-icon.png`, `icon-1024.png` (optional within the row) | 180, 1024 |
| Android/PWA | `icon-192.png`, `icon-512.png`, `site.webmanifest` | 192, 512, — |

Three checkboxes (plus the optional 1024 toggle inside iOS). The
manifest lists exactly the selected icons; the HTML snippet lists
exactly the selected files (+ per-theme `theme-color` metas when the
dark palette is on). Components are **descriptors** (id → filename,
size, encoder), so #429/#430 later grow the matrix without redesign.
The manifest gains `"name": <app_name>` (export field, default
"My App").

### D6 — ICO writer, hand-rolled

`favicon.ico` = ICONDIR (6 B) + one ICONDIRENTRY (16 B) per frame +
embedded PNGs (16, 32). PNG-in-ICO is what modern browsers want; the
writer is ~40 lines and needs no dependency. Byte-tested against a
pinned fixture (§5).

### D7 — Export flow

Target = **text field with path + "Browse…" (`rfd`, `xdg-portal` on
Linux — no GTK tree)**. The native dialog is never the only path: the
field is directly settable, which is also what makes export drivable by
the bridge (the picker itself is OS-level and invisible to `tree`).
Flow: pre-flight computes the exact file list (relative paths) and
collisions → in-app confirm showing what would be overwritten (never
silent) → one background task renders + encodes + writes each component
**atomically** (`.tmp` + rename, the `save_json_atomic` precedent) →
summary toast (success: `N files → <dir>`; failure: per-file list, files
already written stay). The chosen dir persists in state. `rfd 0.17.2`
joins workspace deps with `default-features = false` +
`xdg-portal` on Linux.

### D8 — State persistence

`state/favicon.json` holding `DesignSpec` + platform selection + last
export dir, written through core's `DebouncedSave` (drafts pattern).
**Not** the app-DB rail — 0063 §9 ruled JSON stores stay put, and this
is user preference, not log data.

### D9 — Analytics: one record per export

`module "favicon"`, `op "export"`, `surface "page"`, `trigger "manual"`,
`status 200`, `cost None`, `ident` = platform set code (`"web+ios+pwa"`).
Riding the rail needs one additive change: **`Transport::Local`**
(`"local"`) — a new TEXT value in an existing column, no schema bump
(0057 D4 chain untouched); old rows and old readers are unaffected, the
0054 transport filter grows the entry. Amendment note rides the
implementation PR (§8).

### D10 — Bridge ids (0010 D10)

`favicon-page` (group), `favicon-source-tabs` + `favicon-source-{text,
emoji,image,svg}`, `favicon-text-input`, `favicon-font-family`,
`favicon-font-weight`, `favicon-emoji-grid`, `favicon-emoji-input`,
`favicon-image-upload`, `favicon-svg-input`, `favicon-fg-{light,dark}`,
`favicon-bg-{start,end}-{light,dark}`, `favicon-shape-radius`,
`favicon-shape-circle`, `favicon-preview-{16,32,96,180,512}`,
`favicon-theme-toggle`, `favicon-platform-{web,ios,pwa}`,
`favicon-ios-1024`, `favicon-app-name`, `favicon-export-dir`,
`favicon-export-browse`, `favicon-export`, `favicon-overwrite-confirm`,
`favicon-overwrite-cancel`. Roles: inputs/text fields, buttons,
checkboxes, images for preview frames.

### D11 — Fonts

System fonts only (#432 filed for bundling): a curated family list
(SF Pro/Helvetica/Inter-if-installed, Georgia, monos, …) plus a free-text
field — fontdb resolves any installed family at raster time. The
font-family string is embedded verbatim in the SVG.

### D12 — Emoji

A curated grid (~100 common emoji) + free-text input. The emitted SVG
carries the glyph as text. **S2 spike (implementation, before UI
polish):** verify resvg 0.46 rasterizes color emoji (CBDT/sbix/COLR) on
macOS + Linux; if not, the fallback is to rasterize the glyph once via
the platform text stack and embed it as a base64 `<image>` — same
emit-then-raster WYSIWYG, slightly larger SVG. The spike decides; the
design holds either way.

## 4. State & plumbing changes

```
crates/modules/favicon/          Cargo.toml (D1 deps)
  src/lib.rs                     module trait impl + mod list
  src/page.rs                    the generator page (tabs, pickers, matrix, export)
  src/spec.rs                    DesignSpec + serde (D3)
  src/render.rs                  SVG emit + resvg raster + ICO writer (D2/D4/D6)
  src/export.rs                  file plan, collisions, atomic writes, manifest/snippet (D5/D7)
  src/state.rs                   favicon.json store (D8)
crates/core/src/usage_records.rs Transport::Local (D9)
crates/core/src/usage_analytics.rs  transport filter entry (D9)
crates/app/src/main.rs           .with::<FaviconModule>(…) (D1)
Cargo.toml                       workspace: resvg =0.46, image =0.25 (png+jpeg), rfd =0.17
docs/                            README row; amendments §8
```

## 5. Testing

- **SVG emission goldens**: one fixture per source mode × light/dark ×
  shape — exact-string tests against `crates/modules/favicon/fixtures/`
  (they *are* the D4 contract).
- **Raster round-trip**: emitted SVG → resvg → `image::load_from_memory`
  decodes back with the exact target dimensions (every matrix size).
- **ICO bytes**: hand-built fixture (two frames) asserted byte-for-byte.
- **Manifest/snippet**: content follows the *selected* platforms only.
- **Export plan**: collision listing, atomic-write behavior on failure
  (tmp left behind, no partial file), dir remembered.
- **State round-trip**: spec → favicon.json → spec; older/partial files
  default in (D3 all-Option).
- **Transport**: `"local"` encodes/decodes; usage page filter test grows
  the entry.
- **Bridge receipt** (§6) committed under `crates/debug/verify/`.

## 6. Acceptance & verification

Bridge receipt script (Linux box, docs/0055 session): set source text →
set fonts/colors → toggle platforms → set export dir → export → assert
the summary toast and the file set on disk (`tree` + `logs` correlate).
macOS local: attach + logs smoke only (#401 gating).

Human checks (owner): preview matches exported files byte-identically
(D2 — open the SVG next to the 512 PNG); dark-mode swap renders in a
browser that follows the OS theme; emoji export correctness (S2 result);
rfd folder picker on both platforms; a v1 install with an old
favicon.json still opens.

## 7. Non-goals (each filed, nothing memorized)

- Icon library, Windows tiles, adaptive icons, ZIP, bundled fonts —
  **#428–#432**.
- macOS `.icns` app-icon sets — **#433** (filed with this design PR).
- SVG *editing* — uploads are normalized pass-through only.
- Batch/multi-icon projects, favicon site-testing/auditing, animated
  icons — out per #427.

## 8. Docs amendments (ride with the implementation PR)

- **0057** D-new: `Transport::Local` additive value (D9 here).
- **0054**: transport filter gains `local`; note that favicon rows are
  non-network records.
- **0003 §4.2**: the module registry's second consumer (a non-GitHub
  module id).
- README: 0064 row.

## 9. Review resolutions (owner)

_Filled during review of this PR._
