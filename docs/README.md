# Design Docs — `my-dashboard`

This directory contains the design documentation for **my-dashboard**, a native,
cross-platform, multi-modular universal personal dashboard built with Rust and
[gpui-component](https://github.com/longbridge/gpui-component) on top of [GPUI](https://github.com/zed-industries/zed).

## Conventions

- **Naming:** `NNNN-kebab-case-title.md`, zero-padded, monotonically increasing
  (e.g. `0001-bootstrap-hello-world.md`).
- **Status lifecycle:** each doc carries a `Status` header that moves through:
  `Draft` → `Accepted` → `Implemented` → (`Superseded` if replaced by a later doc).
- **Immutability:** once a doc reaches `Accepted`, changes go into a new doc that
  supersedes it, so history is preserved. `Implemented` docs describe what shipped.
- **Content:** design docs should be concrete enough to implement from: decisions,
  rationale, file layouts, code listings, acceptance criteria. Small decisions may be
  recorded inline; sweeping cross-cutting decisions may later be split into an `adr/`
  sub-directory if the volume justifies it.
- **Language:** "MUST / SHOULD / MAY" per RFC 2119.
- **UI design authority:** all UI work follows the gpui-component [Design
  Guides](https://longbridge.github.io/gpui-component/docs/design-guides)
  (normative: **must** = correctness constraint, **should** = default needing a
  concrete reason to override, **may** = optional). Key obligations for this
  project: semantic theme colors from `cx.theme()` (no raw hex/rgb at call
  sites), the shared spacing scale (~2/4/8/12/16/24/32 px) and typography scale
  via tokens, component `Size`/density tiers instead of one-off heights, one
  icon family with tooltips on icon-only buttons, distinct interaction states
  (hover/focus/selected/disabled/loading/error), desktop command surfaces
  (menus/actions/keybindings) over hover-only toolbars, and designed
  empty/loading/error states for every module surface. The guide ships in our
  pinned gpui-component rev (`website/docs/design-guides.md` @ `1081d2b`);
  the live page is canonical. Design docs cite it as **[DG]**.

## Index

| Doc | Title | Status |
| --- | --- | --- |
| [0001](0001-bootstrap-hello-world.md) | Phase 0 — Bootstrap: Hello World gpui-component Application | Implemented (PR #1) |
| [0002](0002-app-shell-window-chrome.md) | Phase 1 — Application Shell & Window Chrome | Implemented (PR #4) |

## Planned docs (roadmap, not yet written)

### 0003 — Multi-module architecture (module trait, registry, navigation)

Core requirements (owner, 2026-08-28 — drive the design, not decisions):

- **Module = domain unit**: a module owns its service layer (e.g. GitHub API
  client, polling, cache), its state (one gpui entity), and two presentation
  faces: an **overview section** on the home page and a **main page**.
- **Pages**: a module may provide multiple pages. Its **main page** is the nav
  entry point; other pages are reachable from it if needed (module-internal
  navigation, surfaced in the breadcrumb).
- **Home page**: fixed layout (not user-reorderable in v1) but **responsive**
  across window sizes — must degrade gracefully from full-screen to narrow
  tiling-WM widths.
- **App navigation**: gpui-component `Sidebar` shell, exactly the layout of
  `crates/story/src/stories/sidebar_story.rs` (verified at rev 1081d2b):
  `Sidebar` (Header / Groups / Menu+sub-items / Footer) + content column
  whose top row is `SidebarToggleButton` **left of a `Breadcrumb`**; menu
  clicks update the active page (the story's `last_active_item` pattern).
  Default `SidebarCollapsible::Icon` (collapse-to-rail for tiling WMs).
- Registry stays compile-time (static registration in `main.rs`).

### 0004 — State management & persistence (settings, layout serialization)
- 0005 — Build, packaging & distribution per OS
