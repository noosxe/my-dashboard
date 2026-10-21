# 0079 — macOS app icon: the sidebar brand, extracted

*Tracked by: implementation PR (docs and code ship together).*

## Context

The dev `.app` bundle (docs/0060 D4, docs/0061) has never carried an icon:
Finder, the Dock, and Cmd-Tab show macOS's generic executable glyph. The
app *does* have a brand — the sidebar header mark
(`crates/core/src/shell.rs`, `SidebarHeader`): the lucide
`gallery-vertical-end` glyph (from the kit's vendored icon set) in the
theme's `primary`/`primary_foreground`. This doc extracts that mark into a
proper macOS app icon and wires it into the bundle.

## D1 — The brand, pinned

The icon source of truth is `crates/app/assets/macos/AppIcon.svg`. It
reproduces the sidebar mark exactly:

- **Glyph**: `gallery-vertical-end`, the same 24-unit paths the sidebar
  renders via `IconName::GalleryVerticalEnd` (kit assets, lucide upstream:
  two rules and a rounded card — a stack of panels, read as a dashboard).
- **Surface**: the factory primary — a vertical `#1E293B → #0F172A`
  gradient with the glyph in `#F8FAFC`. This is the *factory* Aurora
  primary (docs/0056), not a live theme token: an OS icon is fixed
  artwork, while the in-app brand follows the user's theme by design.
- **Proportion**: the sidebar pairs a 16px glyph with a 32px brand square
  (50%); the icon artwork keeps that spirit, filling ~53% of the body.

## D2 — Apple geometry, measured not remembered

Apple's HIG states the grid but ships the numbers only in templates, so
they were measured off Apple's own shipped icns (App Store, this machine):
artwork/canvas ≈ 0.81–0.83, margins ≈ 9–10% per side, corner radius ≈
23–25% of the body. The icon uses the canonical Big Sur-era spec those
measurements confirm: **1024×1024 canvas, 824×824 body centered (100px
margins), 185.4px corner radius**, plus a subtle baked drop shadow
(6px down, 10px blur, 30% black) — macOS renders the icns verbatim and
applies nothing itself.

## D3 — Pipeline: SVG in, .icns committed

`scripts/app-icon.sh` regenerates the icon: the SVG renders through
`resvg` (pure Rust, `cargo install resvg`) into the complete classic
10-entry iconset (16→1024 with all `@2x` steps — verified as the exact
entry set the system's own legacy icns carry), then `iconutil -c icns`
packs it. The **generated `AppIcon.icns` is committed** next to its SVG
so `scripts/macos-app.sh` never needs `resvg` at bundle time; the script
is run by hand, only when the brand changes.

`scripts/macos-app.sh` (both modes) copies the icns to
`Contents/Resources/AppIcon.icns` and sets
`CFBundleIconFile=AppIcon` — the classic flat-file reference; no asset
catalog, matching the bundle's keep-it-simple construction (docs/0061).

## D4 — Verification

- The rendered 1024px and Dock-size (64px) raster are eyeballed against
  the sidebar mark before landing (this PR's screenshots).
- After a bundle rebuild: `plutil -p Info.plist` shows `CFBundleIconFile`,
  `Contents/Resources/AppIcon.icns` exists, codesign still validates
  (`codesign -v`), and the Dock/Finder icon is human-checked (icon caches
  can lag; `killall Dock` / reopen clears it).

## Non-goals

- **No light/dark or per-size art variants**: one artwork; the glyph is
  legible from 16px up (checked) and macOS Docks don't switch app icons
  with system appearance.
- **No Icon Composer layered icon** (the 2025 liquid-glass pipeline):
  it requires Xcode-26 tooling and an asset-catalog build step; the flat
  icns is the classic, toolchain-free path. Filed as a follow-up issue
  (tagged `deferred`) if the layered treatment is ever wanted.
- **No theme-aware icon**: the OS icon is fixed artwork by design (D1).
