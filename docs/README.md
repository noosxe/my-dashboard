# Design Docs — `my-dashboard`

This directory contains the design documentation for **my-dashboard**, a native,
cross-platform, multi-modular universal personal dashboard built with Rust and
[gpui-component](https://github.com/longbridge/gpui-component) on top of [GPUI](https://github.com/zed-industries/zed).

## Conventions

- **Naming:** `NNNN-kebab-case-title.md`, zero-padded, monotonically increasing
  (e.g. `0001-bootstrap-hello-world.md`).
- **Immutability:** a merged doc is historical record; substantive changes go
  into a new doc that supersedes it, so history is preserved.
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

| Doc | Title |
| --- | --- |
| [0001](0001-bootstrap-hello-world.md) | Phase 0 — Bootstrap: Hello World gpui-component Application (PR #1) |
| [0002](0002-app-shell-window-chrome.md) | Phase 1 — Application Shell & Window Chrome (PR #4) |
| [0003](0003-multi-module-architecture.md) | Phase 2 — Multi-module Architecture (Module Trait, Registry, Navigation) (PR #9) |
| [0004](0004-state-persistence.md) | Phase 3 — State Management & Persistence (XDG / native dirs) (PR #13) |
| [0005](0005-github-module.md) | Phase 4 — GitHub Module (device-flow auth, keychain, notifications + PRs) (PR #17) |
| [0006](0006-settings-page.md) | Settings (app settings shell, module-provided subpages) (PR #22) |
| [0007](0007-github-graphql-global-snapshot.md) | GitHub Module: GraphQL Global Snapshot (repos, PRs, issues) (PR #32) |
| [0008](0008-logging-system.md) | Logging System (log facade, flexi_logger, file + stderr, levels) |
| [0009](0009-github-select-and-list.md) | GitHub Module: Official `Select` + `List` Components (searchable org picker, virtualized repo list) |

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

- ~~0004 — State management & persistence~~ → implemented (PR #13)
- 0005 — First real module: GitHub → design drafted, see doc 0005 above
- ~~0006~~ — Settings page (number reused; see doc 0006 above). Build,
  packaging & distribution per OS moves to a new number when written
- File-based color themes (user themes dir, hot reload via
  `ThemeRegistry::watch_dir`) — proposed, tracked in #23; takes the next
  free number when written (upstream basis verified in PR #22)
