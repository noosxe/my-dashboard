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
| [0027](0027-ci-workflows.md) | CI — GitHub Actions Gates on GitHub-Hosted Runners (fmt/clippy/test, no Nix) |
| [0028](0028-github-401-refresh-retry.md) | GitHub Module: Immediate 401 Refresh with Retry Queue (single-flight gate, FIFO retries; amends 0011 §4.3) |
| [0029](0029-github-module-architecture.md) | GitHub Module: Architectural Overview (what works, gaps, improvement roadmap) |
| [0030](0030-pr-review-threads.md) | GitHub Module: PR Review Threads (inline code review on the Files tab — read + write design) |
| [0031](0031-fetch-record-and-inflight-dedup.md) | GitHub Module: Fetch Record + Per-Key In-Flight Dedup (shared freshness substrate for #188/#192/#193/#194) |
| [0032](0032-auto-refresh-detail-pages.md) | GitHub Module: Auto-Refresh Detail Pages (heartbeat + focus triggers on the 0031 records; #188, folds #194) |
| [0033](0033-last-synced-indicator.md) | GitHub Module: Last-Synced Indicator (per-record `fetched_at` labels on the detail pages; #192) |
| [0034](0034-cache-ttl-and-staleness-marking.md) | GitHub Module: Cache TTL & Staleness Marking (cold-start cache ages + stale chips; mark-not-evict, #193) |
| [0035](0035-list-surface-auto-refresh.md) | GitHub Module: List-Surface Auto-Refresh (heartbeat for the open repo page's rows; #224) |
| [0036](0036-refresh-backoff.md) | GitHub Module: Refresh Backoff (per-lane exponential backoff + rate-limit gate; minimize-pause rejected, #225) |
| [0037](0037-freshness-system.md) | GitHub Module: Freshness System (one clock trait + header component; detail-page / list-surface patterns; repo-page synced label as pilot) |
| [0038](0038-realtime-detail-pages.md) | GitHub Module: Realtime Detail Pages (engaged conditional polling; push rejected on evidence; 304 = unchanged landing; #226) |
| [0039](0039-check-run-logs.md) | GitHub Module: Check-run Log Viewing (documented 4-call resolution chain; page-owned one-shot kind; inline accordion pane, tail-truncated; #201) |
| [0040](0040-syntax-highlighted-diffs.md) | GitHub Module: Syntax-Highlighted Diffs (syntect tokenizer, theme-token color mapping; #202) |
| [0041](0041-virtualized-diff-scroll.md) | GitHub Module: Virtualized Diff Scroll (gpui `list` over a flattened `DiffItem` stream; page-owned anchor cache; perf contract test; #253) |
| [0042](0042-repo-view-list-filters.md) | GitHub Module: Repo-View List Filters & Sort (client-side filter/sort engine over snapshot nodes; per-box toolbars + shared quick filter; labels & review-requests wire additions; #190) |
| [0043](0043-pr-metadata-editing.md) | GitHub Module: PR Page Metadata Editing (reviewers via `requestReviews`; labels/assignees/milestone via `updatePullRequest`; 0022 picker machinery to `pickers.rs`; #189) |
| [0044](0044-pr-creation.md) | GitHub Module: PR Creation Flow (repo-header entry; branch selects + 0024 form chrome; `createPullRequest` minimal input; same-repo v1; #191) |
| [0045](0045-draft-persistence.md) | GitHub Module: Draft Persistence (comment boxes + issue/PR creators to `<data>/modules/github/drafts/`; 500 ms debounced auto-save, restore, clear-on-submit; amends 0024 D7; #199) |
| [0046](0046-issue-templates.md) | GitHub Module: Issue Templates in the Creation Flow (GraphQL `issueTemplates` markdown templates + `blank_issues_enabled` via Contents API; pill picker on the issue creator; composes with 0045 drafts; #198) |
| [0047](0047-github-home-open-prs.md) | GitHub Module: Home View — Account-wide Open PRs (GraphQL search lane `org:`/`user:` scoped, 1 pt/request flat; reuses PR rows + 0042 filters; 1,000-result search cap with honest hint; snapshot-union degraded mode; #295) |
| [0048](0048-navigation-history.md) | Shell: Navigation History — Back/Forward over the Page Tree (linear visit trail on `gpui_base::History` routes; `ModuleNav` snapshot/restore/collapse contract; deep-leaf restore; LCA collapse on cross-branch jumps; Alt+←/→ + breadcrumb-bar buttons; #303) |
| [0049](0049-content-a11y-retrofit.md) | Content A11y Retrofit — ids, roles and labels for every interactive surface (conventions + 125-site inventory; upstream patches for SidebarMenuItem/ListItem labels #57/#95; bridge-verification protocol; #311) |
| [0050](0050-github-usage-calculator.md) | GitHub API Usage Calculator — server-reported budgets as the ledger, per bucket, shown verbatim (`CountingHttp` decorator on the single `dyn Http` seam; pure `UsageBook` with per-response reconciliation; free `/rate_limit` probe folding `core` only — `resources.graphql` is a phantom; remaining-only both-buckets footer chip (`GQL ?` until first GraphQL traffic) with a per-bucket hover tooltip via `ModuleEntry.status_item`; #294) |
| [0051](0051-github-view-driven-fetches.md) | GitHub View-Driven Minimal Fetches — kill `GLOBAL_SNAPSHOT_QUERY` (~302 pts/page by GitHub's connection-cap cost formula; ~604 per bootstrap, self rate-limiting in ~40 min of open time) in favor of three cheap lanes: `CardTotals` (totals-only, home-shown), `SessionInventory` (login/orgs/inventory, once per launch), existing `ACCOUNT_PULL_REQUESTS` search on module-home shown; D6 union fallback deleted; skeletons + in-memory retention; no-autorefresh beyond the snapshot's own cycle; #325 |
| [0052](0052-github-no-autorefresh.md) | GitHub No-Autorefresh — nothing fetches on a timer: the page heartbeat, engaged-tier cadences (0038), and the notifications tick arm are deleted; refresh happens on view-shown, app refocus (15 s floor, every visible surface incl. the home card), or manual; the service tick keeps only non-content duties (token arming, pause clearing, free usage probe, bootstrap); age labels advance via a render-only wake; #326 |
| [0053](0053-github-usage-records.md) | Usage records — a module-global per-HTTP-flight cost log (records, not counts) living in core as a `gpui::Global` (the status-tracker pattern; `module` field on every record, Forgejo/Gitea-ready), origin attribution via surface × trigger stamped from each module's closed enums (required field — compiler-enforced), an injectable `UsageSink` storage trait (v1 = capped in-memory FIFO; SQLite later is a swapped impl), and a sqlite-ready flat row as the future analytics/persistence contract; the analytics UI, persistence, and visit analytics stay deferred; #324 |
| [0055](0055-live-debug-sessions.md) | Live Debug Sessions — the scripted, isolated verification environment over the 0010 bridge: managed headless-sway app instances (`session start/status/stop`), `--session` discovery, frame-aware waiting (`wait-frame`/`--wait-frame`), named tree snapshots + structural diff, tab-stop maps, coordinate-click fallback, grim screenshots, replayable receipt scripts, and a `doctor`; #349 |
| [0056](0056-factory-theme-aurora.md) | Factory color theme: Aurora — the shipped `Aurora Light`/`Aurora Dark` pair (multi-hue: slate neutrals, violet primary, 5-hue chart ramp) embedded at compile time and replayed on every registry rebuild, replacing the upstream shadcn-neutral defaults (docs/0025 D3/D5/D9 amended); boot-time slot install falls back to the embedded pair so factory and stale-name boots land on Aurora; amends-doc, one PR with the asset |
| [0057](0057-usage-records-sqlite-sink.md) | Usage records: the SQLite persistence sink — `SQLiteStore` (`UsageStore` impl behind docs/0053's `init_with` seam) becomes the default sink: `rusqlite` 0.40 `bundled` (hermetic amalgamation via Cargo.lock; WASIX default features opted out), DB at `AppPaths::data_dir()/usage.db` (docs/0004 §4.1), mutex buffer + one dedicated writer thread (1 s flush cadence, batch commit outside the lock, flush-on-quit beside docs/0045 §9), schema v1 + additive `user_version` migrations, WAL + one lazy reader connection, 90-day retention pruned on the writer thread only (never the startup path), direct indexed UI-thread reads (owner-accepted), degrade-to-memory on open/write failure (`is_degraded()` → future toasts #376), mutation token = commit counter; #333 |
| [0058](0058-toasts.md) | Toast notifications — shell alert surface built on upstream's `Root`-mounted `NotificationList` (no new widget): core `toasts::ToastHub` global (docs/0013 D2 InFlight pattern) with free-fn raise API + bounded spill channel for non-UI threads, severity levels (error sticky, rest 5 s autohide), dedupe via upstream's replace-on-id, theme TopRight placement with `max_items` 10→3, first consumer = usage-sink degrade (docs/0057 D6), log mirror + bridge-verifiable a11y (upstream role gap tracked); #376 |
| [0059](0059-notification-hub.md) | Notification hub — the durable side of toasts: every raise lands in a bounded unread log on the `ToastHub` entity, a rightmost status-bar bell carries an upstream `Badge` unread count (max 99+), and clicking opens a `Popover` panel of unread entries (per-row dismiss, clear-all, read-on-panel-open with an open-time snapshot view); only human actions mark read — toast ✕, opening the panel, row ✕, clear-all — timeouts and programmatic closes never do (documented fallback if upstream's `on_close` can't discriminate autohide from ✕), session-scoped, bridge-verified |
| [0060](0060-os-notifications-macos.md) | OS notification delivery on macOS — the #389 evaluation: hub raises also post to the macOS notification center via upstream `NotificationDelivery` when the window is unfocused (`Window::is_active` gate at the shell drain, all hub raises, keyed posts replace), banner click = activate + mark read (0059 D2), in-app ✕ retracts the banner; macOS-only via cfg gate, `scripts/macos-app.sh` dev bundling (ad-hoc signed, `~/Applications`), Windows/Linux stay in-app; #389 |
| [0054](0054-usage-analytics-page.md) | Usage analytics page — the shell-level read side of the records: global range picker (1h/1d/1w/1m) + filter facets above a tab strip (one tab per use case), everything frozen at a memoized `as_of` with an explicit refresh button (nothing live); a pure analytics calculator module (per-tab procedures, serde-serializable outputs, thin renderers — the future CLI/MCP seam), a writer/reader split of the storage contract (`UsageSource`/`UsageStore`), and three additive record fields (`limit_after`, `queue_ms`, `resp_bytes`); quota tab = dimension ranking + charts, latency tab = metric ranking + trend + slowest flights, overview = at-a-glance cards + burn-down/spend/p90 charts; #324 |

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
