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
| [0010](0010-agent-debug-bridge.md) | Agent Debug Bridge (attach over socket, component tree dump, non-disruptive input) |
| [0011](0011-github-token-refresh.md) | GitHub Module: Token Refresh (expiring tokens, rotation-safe keychain credential) |
| [0012](0012-github-open-issues.md) | GitHub Module: Open Issues (home-card count + per-repo section) |
| [0013](0013-global-status-bar.md) | Global Status Bar (in-flight request indicators) |
| [0014](0014-github-selected-repo-breadcrumb.md) | GitHub Module: Selected-repo Breadcrumb |
| [0015](0015-notification-row-click.md) | GitHub Module: Notification Row Click (open repo notifications page) |
| [0016](0016-github-pr-detail-page.md) | GitHub Module: PR Detail Page (review, comment, merge, close) |
| [0017](0017-markdown-bodies.md) | GitHub Module: Markdown Rendering for PR Bodies |
| [0018](0018-files-sidebar.md) | GitHub Module: Files Tab Sidebar (list/tree views, master–detail diff) |
| [0019](0019-pr-comment-edit-delete.md) | GitHub Module: PR Page Comment Edit/Delete (timeline rows, schema-gated) |
| [0020](0020-github-issue-page.md) | GitHub Module: Issue Detail Page (comment, close, reopen; PR-page reuse) |
| [0021](0021-github-issue-timeline-events.md) | GitHub Module: Issue Page Timeline Event Rows (itemTypes-filtered, merged conversation) |
| [0022](0022-github-issue-metadata.md) | GitHub Module: Issue Page Metadata Editing (title, labels, assignees, milestone — one `updateIssue`) |
| [0023](0023-github-issue-reactions-locking.md) | GitHub Module: Issue Page Reactions + Conversation Locking (toggle chips, static picker, permission-derived lock gate) |
| [0024](0024-github-issue-creation.md) | GitHub Module: Issue Creation Flow (repo-header entry, creator sub-page, `createIssue`, draft retention) |
| [0025](0025-file-based-color-themes.md) | File-based Color Themes (user themes dir, hot reload, defaults-only bundle) |
| [0026](0026-github-notification-detail-page.md) | GitHub Module: Notification Detail Page (in-app thread view, auto-seen, read state) |
| [0027](0027-ci-workflows.md) | CI — GitHub Actions Gates on Self-Hosted Runners (fmt/clippy/test, no Nix) |
| [0028](0028-github-401-refresh-retry.md) | GitHub Module: Immediate 401 Refresh with Retry Queue (single-flight gate, FIFO retries; amends 0011 §4.3) |
| [0029](0029-github-module-architecture.md) | GitHub Module: Architectural Overview (what works, gaps, improvement roadmap) |
| [0030](0030-pr-review-threads.md) | GitHub Module: PR Review Threads (inline code review on the Files tab — read + write design) |
| [0031](0031-fetch-record-and-inflight-dedup.md) | GitHub Module: Fetch Record + Per-Key In-Flight Dedup (shared freshness substrate for #188/#192/#193/#194) |
| [0032](0032-auto-refresh-detail-pages.md) | GitHub Module: Auto-Refresh Detail Pages (heartbeat + focus triggers on the 0031 records; #188, folds #194) |

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
- Build, packaging & distribution per OS — the **NixOS/home-manager path
  ships without a design doc** (flake package + overlay + NixOS/HM modules
  + root README install guide, #147); full cross-OS packaging (macOS/
  Windows, binary caches, release CI) still takes the next free number
  when written (0006 was reused by the settings doc)
- ~~File-based color themes~~ → design drafted, see doc 0025 above (tracked
  in #23)
- ~~Notification detail page~~ → implemented (docs/0026, #151);
  ~~notification mark-as-done~~ → implemented (docs/0026 D8, #153);
  ~~jump to latest comment~~ → implemented (docs/0026 D9, #154)
