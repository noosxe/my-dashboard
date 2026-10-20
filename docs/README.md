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
| [0056](0056-factory-theme-aurora.md) | **Superseded by docs/0078.** Factory color theme: Aurora — the `Aurora Light`/`Aurora Dark` pair (multi-hue: slate neutrals, violet primary, 5-hue chart ramp), embedded at compile time and replayed on every registry rebuild; `Aurora Dark` is retired with the pair — recoverable from the doc's git history as a user theme file |
| [0057](0057-usage-records-sqlite-sink.md) | Usage records: the SQLite persistence sink — `SQLiteStore` (`UsageStore` impl behind docs/0053's `init_with` seam) becomes the default sink: `rusqlite` 0.40 `bundled` (hermetic amalgamation via Cargo.lock; WASIX default features opted out), DB at `AppPaths::data_dir()/usage.db` (docs/0004 §4.1), mutex buffer + one dedicated writer thread (1 s flush cadence, batch commit outside the lock, flush-on-quit beside docs/0045 §9), schema v1 + additive `user_version` migrations, WAL + one lazy reader connection, 90-day retention pruned on the writer thread only (never the startup path), direct indexed UI-thread reads (owner-accepted), degrade-to-memory on open/write failure (`is_degraded()` → future toasts #376), mutation token = commit counter; #333 |
| [0058](0058-toasts.md) | Toast notifications — shell alert surface built on upstream's `Root`-mounted `NotificationList` (no new widget): core `toasts::ToastHub` global (docs/0013 D2 InFlight pattern) with free-fn raise API + bounded spill channel for non-UI threads, severity levels (error sticky, rest 5 s autohide), dedupe via upstream's replace-on-id, theme TopRight placement with `max_items` 10→3, first consumer = usage-sink degrade (docs/0057 D6), log mirror + bridge-verifiable a11y (upstream role gap tracked); #376 |
| [0059](0059-notification-hub.md) | Notification hub — the durable side of toasts: every raise lands in a bounded unread log on the `ToastHub` entity, a rightmost status-bar bell carries an upstream `Badge` unread count (max 99+), and clicking opens a `Popover` panel of unread entries (per-row dismiss, clear-all, read-on-panel-open with an open-time snapshot view); only human actions mark read — toast ✕, opening the panel, row ✕, clear-all — timeouts and programmatic closes never do (documented fallback if upstream's `on_close` can't discriminate autohide from ✕), session-scoped, bridge-verified |
| [0060](0060-os-notifications-macos.md) | OS notification delivery on macOS — the #389 evaluation: hub raises also post to the macOS notification center via upstream `NotificationDelivery` when the window is unfocused (`Window::is_active` gate at the shell drain, all hub raises, keyed posts replace), banner click = activate + mark read (0059 D2), in-app ✕ retracts the banner; macOS-only via cfg gate, `scripts/macos-app.sh` dev bundling (ad-hoc signed, `~/Applications`), Windows/Linux stay in-app; #389 |
| [0061](0061-dev-signing-macos.md) | Development-profile signing for macOS dev builds — `scripts/macos-app.sh` signs with the owner's Apple Development identity by default (auto-discovery: `SIGN_IDENTITY` env → first `Apple Development` in the keychain → ad-hoc fallback), new `--dev` profile bundles the debug binary as `My Dashboard Dev.app` with its own bundle id so dev/release notification authorizations stay independent, certificate expiry printed per run; stability fix for the 0060 D4 ad-hoc re-prompt papercut; release signing deliberately parked |
| [0062](0062-notification-hub-settings.md) | Notification hub settings — the #388 round: core-owned Notifications subpage in Settings (badge style count/dot, per-level badge filtering — counts only, empty set = badge off; per-source mute, absolute at both hub ingest choke points, log-file mirror survives), `Toast.source` + source registry (app fallback, module labels), `AppSettings.notifications` persistence (all-Option serde, factory default = prior behavior, live apply); autohide tuning deferred (upstream-hardcoded 5 s, #417); #388 |
| [0063](0063-app-database-notification-log.md) | The app database — generalize docs/0057's `usage.db` into the single app DB (one file, central additive `user_version` chain, shared writer thread via a command buffer, failure isolation per concern) and land the notification log as its second table (#387): `notification_log` persists unread only (read = delete), boot restore before the window, engine-assigned rowid keys, no unread TTL + 500-row cap in the hourly prune, read-state survives restarts, no new toasts from the persistence layer; #387 |
| [0064](0064-favicon-generator.md) | Favicon generator module — the #427 round: new `dashboard-module-favicon` (text/emoji/image/SVG inputs, gradient + shape styling, light/dark palettes — one SVG, two looks via embedded `prefers-color-scheme`), single source of truth = emit-then-raster (every PNG/ICO frame is resvg rendering of the exact emitted SVG, preview included), per-platform output matrix (web/iOS/Android-PWA, manifest + snippet follow the selection), hand-rolled PNG-in-ICO writer, rfd folder picker alongside a settable path field (bridge-drivable), atomic exports with collision pre-flight, `state/favicon.json` persistence, one `Transport::Local` usage record per export; **implemented via #435 (this PR)** — S2 spike resolved: resvg 0.46 rasterizes color emoji natively (verified macOS; Linux via the receipt run); #427 |
| [0065](0065-ui-ux-quality-system.md) | UI/UX quality system for agent-authored surfaces — six complementary layers (tokens→objective checks→structural goldens→pixel goldens→vision critique→reference anchors): research-backed (Design2Code measured: vision judgment works, layout recall is weak — never pixels-only), a committed rubric the non-designer owner marks as checkboxes, a deterministic `audit` bridge verb over tree dumps, a contrast unit-test over aurora pairs, committed tree-snapshot goldens (cross-platform incl. macOS), Linux pixel goldens Playwright-style, a ≤3-iteration screenshot+tree critique ritual per UI PR, and owner-picked native-desktop reference anchors (taste by selection, not vocabulary); first backlog = the favicon redo (#437) + the usage/analytics review; Phases 1–3 implemented — #440 audit verb + contrast gate (2 real light-theme failures fixed) + structural goldens; #441 pixel goldens + paired `critique` captures; #442 owner-picked anchors (Linear, Things 3, provisional Zed) under `docs/references/`; remaining: #445 Linux-box run, then #437/#439 redos |
| [0066](0066-github-workflow-approval.md) | GitHub: manually-approved (gated) workflow runs — REST sweep surfaces `action_required` runs on the PR CI pane, new `CheckState::Waiting`, approve in place for both upstream gates (fork/external-contributor `POST …/runs/{id}/approve`; environment `POST …/runs/{id}/pending_deployments`), reject for the env gate only; live-verified against noosxe/worker-mcp#69 |
| [0067](0067-pr-update-branch-button.md) | PR page: the "Update branch" split button (merge-commit / rebase) — one new gate field (`viewerCanUpdateBranch`), GraphQL `updatePullRequestBranch` with an `expectedHeadOid` staleness guard, split button under the rail's out-of-date state line mirroring the merge button's pattern, rebase confirms (force-push warning), targeted refetch, no polling; schema-verified 2026-10-06; #455 |
| [0068](0068-agent-data-bridge.md) | Agent data bridge — AI agents operate the app from the data layer over MCP: product crate `crates/agent` with an in-app token-gated Unix-socket server (`agent.sock`, docs/0010 pattern) and a thin stdio MCP binary (`dashboard-agent-mcp`); module-registered agent-ops providers, phase 1 = zero-flight held-state reads (`app_status`, `list_repos`, `notifications_unread`, `repo_prs`/`repo_issues`, `api_usage` over `UsageSource`); every response carries mandatory freshness/truncation metadata; agent-originated dispatches attributed via new `Surface::Agent`/`Trigger::Agent` in the usage taxonomy; on-demand fetches deferred to a later phase; read-only by design — no mutations, no raw GraphQL passthrough; client wiring out of scope |
| [0073](0073-pr-single-store.md) | PR single store — one copy of every PR fact: `pulls` stays the only holder of PR documents; home/repo lanes become key indices + pagination books; views derive (filter/map) from the store at render time; `PrDetailPage.summary` snapshot and lane row copies deleted; confirmations write once by construction; journey-level integration test pins the merge→open-list seam; stages: lane index refactor, fact-copy audit, selective follow-ons |
| [0074](0074-built-in-theme-presets.md) | Built-in theme presets — the pinned gpui-component theme gallery vendored under `crates/core/assets/themes/presets/` and replayed into the registry after the user themes dir (user files shadow presets by name — Whiskers Catppuccin stays the override path); `is_default` stripped at load; parse-and-shape smoke test per preset; amends docs/0025 D3/D10. Amended by docs/0078: the gallery is verbatim (21 files / 36 themes, `aurora.json` included) and the shipped defaults are the kit's own pair — the app-owned factory pair and its AA gate are retired |
| [0075](0075-pr-comment-mention-completion.md) | @-mention completion in the PR comment composer — typing `@` in the main PR comment box opens a caret-anchored (composer-box fallback) popup of context people derived from the already-loaded PR document (review-requested → assignees → author → comment/review authors, deduped, top 10); keyboard-driven with focus kept in the textarea (↑/↓ wrap, Enter/Tab accept, Esc dismisses without bubbling), insert replaces the token with `@login ` in one undo step; built in-app on the public `InputBaseState` API because the pinned stack's completion machinery is `pub(crate)`/Editor-only, with an upstream textarea-completion API follow-up from the fork; scope v1 = main PR box only, other composers fast-follow |
| [0076](0076-live-check-logs.md) | Live check logs — progressive per-job availability for in-progress Actions runs, official-API-only (no streaming exists upstream: in-progress job logs 404, but logs materialize per job the moment that job completes, and run-jobs payloads embed live step status); expanding an in-progress check arms an on-demand watch (page-owned ~10 s poll, one GET/tick) rendering the matched job's step rows, and the pane lands into the existing docs/0039 tail-truncated log view the instant the job completes (404 during blob materialization retries, never errors); disarms on collapse/navigation/sign-out/landing/hard cap; logs stay page-local (docs/0039 D5), nothing enters the docs/0072 store |
| [0077](0077-repo-view-tabs.md) | Repo View Tabs — the open-repo right column becomes three tabs (Open PRs default / Open issues / Notifications), one fetch lane per tab: opening the repo fetches the active tab only, a switch fetches that lane (docs/0031 dedup + freshness floor absorb re-offers), the docs/0035 heartbeat refreshes only the visible lane; `Kind::RepoSnapshot` (one GraphQL query for PRs+issues) is retired for two single-collection queries (`RepoPulls`/`RepoIssues`) landing through the existing apply path — docs/0073 derivation, docs/0042 filters, left column, and header untouched |
| [0078](0078-bundled-themes-upstream-verbatim.md) | Bundled themes: upstream verbatim — the bundled set is exactly the pinned kit gallery (21 files / 36 themes, `aurora.json` included) and the shipped defaults are the kit's own `Default Light`/`Default Dark` (vendored verbatim, `is_default` kept, synced per bump); the app-owned Aurora pair is deleted (`Aurora Dark` retired — git history + user-file install is the recovery), the factory AA contrast gate retires with it (docs/0065 D4 scoped to app-owned content), #524 closes as moot-by-design; future themes ship as user-dir files only, never bundled; #524 |
| [0072](0072-github-data-store.md) | GitHub data store — one shared PR/issue document cache in `GithubState` keyed `(repo, kind, number)` with summary+detail freshness layers; stale-while-revalidate reads (warm PR opens paint instantly, revalidate in background, skeletons only when cold); optimistic store-scoped mutations (merge/close/update-branch flip the document to a pending state that every surface renders — home row pill + header "working…" — while the GraphQL call runs in the background, confirmed state lands via the store, success/failure toasts via the hub, inline errors unchanged); per-entry single-flight, token-guarded confirmations, eviction-pinned pending ops, 64-doc detail cap; supersedes docs/0016 D2's no-cache rule; persistence to `app_db` deferred; stages: store+SWR → optimistic mutations → remaining mutations → issues |
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
