# 0048 — Navigation history: back/forward over a page tree

- **Issue:** #303
- **Status:** design (docs-only PR)
- **Depends on:** 0003 (shell & Navigator), 0010 (debug bridge — new controls must be visible), 0014 §5 D7 (module-crumb re-entry), 0031 (fetch dedup — restores must not refetch-storm)

## 1. Summary

The app has no history: `Navigator` knows the current top-level page and
the module-reported breadcrumb chain, and that is all. Diving
`Home → GitHub → acme/one → PR #5` and then jumping to `Settings` leaves
no way back except re-clicking crumbs, and crumbs die on every `go()`.
This doc adds **history-based back/forward** (Alt+←/→, header buttons):
a single linear visit trail with a cursor, stored with the
`gpui_base::History<T>` primitive already shipped in our pinned
gpui-component tree, plus a minimal module contract so a restored
position re-activates the exact place ("go back to where I was").

The model, per the owner's clarification on #303:

- **The page tree is the IA** — modules → pages → sub-pages. It defines
  breadcrumbs, branches, and state ownership. It is *not* stored as a
  history structure.
- **Navigation history is linear** — one trail of visited locations,
  browser-style: back/forward move a cursor along visit order; a new
  visit while mid-trail truncates the forward tail.
- **The common-ancestor rule governs jumps, not the trail's shape:**
  crossing to a different branch collapses the abandoned branch's open
  path (its deep state tears down per the owning module's policy), then
  the destination is appended to the trail.

## 2. Current state and verified building blocks

**Today** (crates/core/src/nav.rs, 157 lines): `Navigator {
sidebar_collapsed, page: Page, crumbs: Vec<Crumb> }`; `go()` switches
`Page::Home | Module(_) | Settings` and clears crumbs; a `go` targeting
the current page emits `NavEvent::Reentered` (docs/0014 §5 D7, #100).
Modules own everything deeper: `ModuleEntry.main_page: AnyView` is a
**strong** handle — module state entities live for the whole session and
page switches only change what renders (shell.rs render match). The
GitHub page derives its crumbs from state each frame
(`GithubPage::sync_repo_crumb`, gated so it never writes while another
page owns the chrome).

**Building blocks verified at our pins:**

- `gpui_base::History<T>` (gpui-component `94a313a`,
  crates/base/src/history.rs; 283 lines, unit-tested): browser-style
  linear trail — `entries` + `forward_entries`, `push` truncates the
  forward branch, `back`/`forward` move one step (`T: Clone`),
  `replace_current`, `retain`, `can_back`/`can_forward`,
  `max_entries` (default 1,000). Generic over the entry type: it can
  hold route descriptors, not just views. This is the storage.
- `gpui_base::NavStack` (same rev, 833 lines, gpui::test-covered): a
  view stack (`AnyView`) with the same linear semantics plus animated
  transitions. **Not adopted**: it would force a pages-as-views
  refactor, and it holds views where we need routes. Noted as the
  pocket primitive if a module ever wants internal view-level
  sub-stacks.
- Zed's `pane::GoBack`/`GoForward` (crates/workspace/src/pane.rs, main
  as of 2026-09-20): per-pane `NavHistory` of `NavigationEntry { data,
  item: WeakItemHandle, is_preview, row }` — flat stacks with
  same-location dedup, a length cap, preview exclusion, a
  `deactivated()` push hook, and weak handles so closed items drop out.
  Lessons adopted here: dedup on a route key, an explicit cap, and
  recording at explicit navigation points rather than by state-diffing.
- crates.io GPUI routers (`gpui-router`, `gpui-nav`, `gpui-navigator`,
  `routers-rs`): all bind zed's crates.io `gpui ^0.2`; we are pinned to
  longbridge's `gpui-pre 0.3.3` — a second, type-incompatible gpui.
  Pattern reference only; not adoptable without forks (rejected).

## 3. Design

### D1 — One global trail, stored as routes, in `Navigator`

`Navigator` gains the trail:

```rust
pub struct Route {
    pub page: Page, // Home | Module(id) | Settings
    /// Owning module + its opaque position payload. `None` for
    /// shell-owned pages (Home, Settings).
    pub module: Option<(SharedString, Arc<dyn Any + Send + Sync>)>,
}
history: gpui_base::History<Route>, // max_entries(200)
```

`gpui-base` becomes a direct workspace dependency at the same pinned
rev (`94a313a72a2513aee2780240cd322d552b2395f0`) — one exact-pin line,
per the #126 policy. The payload is an `Arc<dyn Any>`: only the owning
module ever downcasts it (entries carry the module id), so core stays
module-agnostic and nothing needs serialization. The trail is
session-only; the initial state at launch is `[Home]` with no forward.

### D2 — The "tree" is the route shape, not a stored structure

Top-level branches are `Page`s; a module's sub-tree lives inside its
route payloads and is owned by the module. Consequently the LCA rule
splits naturally:

- **Cross-page jump** (GitHub → Settings, GitHub → Home): the shell
  knows the branches differ and instructs the abandoned module to
  collapse (D6).
- **Within-module jump** (repo A → repo B, PR #5 → issue #12): the
  module replaces its own position — our module pages are
  single-position state (a cursor, not a stack of mounted pages), so
  the "pop to common ancestor" costs nothing; the abandoned deep state
  evaporates by construction.

No tree data structure is maintained; breadcrumbs stay module-reported
exactly as today (`sync_repo_crumb` is untouched).

### D3 — Module contract: `ModuleNav`

```rust
pub trait ModuleNav: 'static {
    /// Stable identity of the current position ("repo:acme/one",
    /// "pr:acme/one#5"). Dedup key for the trail (D4).
    fn route_key(&self, cx: &App) -> SharedString;
    /// Snapshot the current position as a Route payload.
    fn snapshot(&self, cx: &App) -> Arc<dyn Any + Send + Sync>;
    /// Activate a payload previously returned by `snapshot`. Same code
    /// path as the user clicking: re-selects the repo, opens the PR,
    /// 0031 dedup + caches apply. Must never panic on a stale payload —
    /// fall back to the module root instead.
    fn restore(&self, route: &Arc<dyn Any + Send + Sync>, window: &mut Window, cx: &mut App);
    /// Tear down to the module root (the LCA collapse, D6).
    fn collapse(&self, window: &mut Window, cx: &mut App);
}
```

`ModuleEntry` gains `nav: Option<Arc<dyn ModuleNav>>` — `Option` keeps
the trait additive for modules that don't opt in yet. Modules *push
their own visits*: any user-driven move to a new place calls
`Navigator::visit(route, cx)` with the destination (shell pages call it
from the shell's own `go()` path). The Navigator records; nothing is
inferred by diffing state.

### D4 — What counts as a visit

Recorded (destination-oriented): selecting a repo, opening a PR, opening
an issue, opening the new-issue/new-PR form, opening a notification's
target, module root via sidebar/module entry, crumb clicks, Home,
Settings. **Not** recorded: filter typing, search, load-more paging,
in-page tabs (PR page Checks/Conversations), scrolling, pickers, drafts'
text. **Amended (2026-10-20, repo-tab trail):** the repo *view's* tabs
(docs/0077) are the one recorded in-page-tab kind — each tab switch is a
visit keyed `repo:<name>:<tab>`, because the tab selects the view's
primary content (the lane), not a sub-state of a deeper place. PR-page
tabs stay unrecorded. Dedup rule: if the new visit's `route_key` equals the trail
head's, the visit is ignored entirely — no push, and no spurious
forward-tail truncation. Any different-key visit pushes (truncating the
forward tail, the standard browser rule, which `History::push`
implements).

### D5 — Back/forward semantics

- `back()`/`forward()` move the cursor one entry; the shell then
  switches page (without recording) and, for module routes, calls the
  owning module's `restore`.
- **Deep-leaf restore**: Back across a branch jump lands on the exact
  abandoned place — `… → PR #5 → Settings → [Back]` re-activates
  `GitHub → acme/one → PR #5`, because `restore` re-activates the whole
  ancestor path from the route payload. Rationale: the owner's stated
  main need is "go back to where I was"; browsers do the same. (Decision
  to confirm in review — the alternative was landing on the module
  root.)
- Restores ride the existing open/select code paths, so 0031 inflight
  dedup, snapshots, and 0035 auto-refresh apply unchanged — going back
  is never a refetch storm.

### D6 — Collapse on cross-branch jump

When a visit's `Page` differs from the current page, the shell asks the
abandoned module to `collapse()` before appending the destination. This
is the owner's "pop the stack until the common ancestor" — expressed as
module state teardown, not trail surgery. Consequence, intended:
**sidebar away-and-back now lands on the module home** (the deep
position survives only through the trail — get it back with Back).
Today the module keeps its selection across away-and-back; that muscle
memory changes by design.

### D7 — Surface: buttons, keys, actions

Two icon buttons at the left edge of the breadcrumb bar (before the
Home crumb): `IconName::ArrowLeft` / `ArrowRight`, enabled iff
`History::can_back()` / `can_forward()`, ids `nav-back` / `nav-forward`
with the button role (docs/0010 D10 — invisible otherwise). Actions
`actions!(nav, [Back, Forward])`, bound globally in the shell's key
registrations: `KeyBinding::new("alt-left", Back, None)` and
`alt-right` (Linux/IDE-conventional; conflict-check against existing
bindings before merge — current bindings are `esc` in focus contexts
and module-local chords). `Reentered` (module crumb clicked
mid-module, docs/0014 §5 D7) becomes an ordinary visit to the module
root: record + collapse — one semantics for #100 and the trail.

### D8 — Bounds and robustness

`max_entries(200)` — generous for a session, trivial memory (routes are
small descriptors). No pruning hook in v1: a stale payload (repo gone
from the snapshot) makes `restore` fall back to the module root, which
is itself a place — the trail keeps working. No persistence across
restarts (follow-up, §9).

### D9 — Scope

v1 covers the GitHub module + Home + Settings as a top-level page.
Settings' *internal* subpages are not recorded (their selection lives
inside the settings component, per docs/0006; recording them wants the
same `ModuleNav` treatment — follow-up). Per-module trails (Zed's
per-pane split) are explicitly not built — one global trail.

## 4. Implementation plan (one PR, after this doc merges)

1. **Workspace**: add `gpui-base` git dep at the pinned rev; `core`
   depends on it.
2. **core/nav.rs**: `Route`, `History<Route>` integration, `visit`,
   `back`/`forward` (returning what to restore), `can_back`/`can_forward`;
   existing `Page`/crumb API unchanged; `Reentered` unchanged.
3. **core/module.rs**: `ModuleNav` trait; `ModuleEntry.nav: Option<…>`.
4. **core/shell.rs**: seed `[Home]` at startup; breadcrumb-bar buttons
   (+ ids, roles, disabled states); `Back`/`Forward` action handlers
   (cursor move + page switch + restore/collapse); actions + key
   registrations + conflict check.
5. **GitHub module**: `GithubRoute` payload enum (`Root`, `Repo`,
   `Pr`, `Issue`, `NewIssue`, `NewPr`); implement `ModuleNav`; call
   `visit` at each place-change (row clicks, crumb clicks, form opens,
   notification opens); `restore` maps to the existing select/open
   functions; `collapse` = the existing return-to-root path.
6. **Docs**: pointer from docs/0029 §3 (navigation rows) and an
   amendment note in docs/0003 §4.3 ("history added — see 0048").

## 5. Testing

- **nav unit tests**: push/dedup (same-key ignored, no truncation),
  back/forward cursor moves, push-after-back truncation, cap at 200,
  `[Home]` seed.
- **shell tests** (`gpui::test`): buttons render and enable/disable
  from the trail; Back/Forward click-through switches pages and calls
  the module's restore/collapse (fake module).
- **GitHub round-trips**: repo → PR → Settings → Back ×2 reopens the
  PR page with rows (bridge ids, no new requests beyond the deduped
  open); forward truncation when a fresh visit follows a back; stale
  payload restore falls back to module root without panicking.

## 6. Debug-bridge verification (docs/0010)

- `tree` shows `nav-back`/`nav-forward` with roles and disabled states.
- Drive a dive-and-jump by clicks, then `key alt-left` — assert the
  restored position via tree (deep leaf present), and `logs` shows no
  duplicate fetch storms on restore.

## 7. Acceptance criteria

- Every recorded navigation is reachable by Back; every Back is
  reachable by Forward until a new visit truncates.
- Back across a branch jump restores the deep leaf, not the module root.
- Cross-branch jumps collapse the abandoned module; sidebar away-and-back
  lands on the module home.
- Restores issue no redundant network work beyond a normal open.
- No `text_size`/`line_height`/literal-color lints; buttons are
  bridge-visible (docs/0010 D10).

## 8. Human checks (owner)

- [ ] Alt+←/→ feels right; back/forward across modules, deep dives,
      and into/out of Settings.
- [ ] Sidebar away-and-back lands on module home; Back returns to the
      exact PR/issue; Forward returns to where you jumped.
- [ ] Buttons enable/disable correctly at the trail's ends.
- [ ] No surprise refetches when walking back through PRs.

## 9. Follow-ups (GitHub issues, not this PR)

- Record Settings subpages (apply `ModuleNav` to the settings shell).
- Persist the last trail position across restarts (docs/0004 store).
- ~~Mouse back/forward buttons (X11 buttons 8/9) — needs a gpui
  capability check before designing.~~ **Shipped (#309) — see §10.**
- Retain/pruning hook if stale-payload fallback proves too coarse.

## 10. Shipped (implementation PR)

Implemented as designed (#303, implementation in #305), with these
final shapes now authoritative:

- **D1**: `Route { key, page, module: Option<(id, payload)> }` — the
  dedup key travels on the route (`Route::key()`), and `Route::payload()`
  gives restore callers read access. Constructors: `home()`,
  `settings()`, `module_root(id)`, `module_place(id, key, payload)`.
  `gpui-base` joined as a direct git dep at the family pin.
- **D3 (amended)**: `ModuleNav` has two methods — `restore(payload)` and
  `collapse()`. The design's `route_key`/`snapshot` folded into the
  route constructors: modules hand the key + payload at `visit` time,
  so a separate snapshot call was dead weight. `Navigator::visit` dedups
  by the head's key (same-key visits are ignored entirely — no push, no
  forward truncation). `Navigator::back`/`forward` return the route they
  land on and switch the page themselves; the shell then runs the
  module's restore/collapse (`DashboardShell::activate_route`).
- **Deep-leaf restore detail**: deep places imply the repo view beneath
  them — `GithubPage::restore_place` re-selects the repo silently when
  needed (`select_repo_inner`, no trail write: the ancestor activation
  is part of the deep restore, never its own visit). A foreign payload
  is ignored, never a panic.
- **Repo-place restore closes sub-pages**: Back to the repo place — a
  deep leaf's usual trail ancestor — lands on the repo view: the restore's
  repo arm closes the takeover slots (`pr`/`issue`/notification; creators
  hidden, drafts retained) exactly as a live click-out would. Without it
  the open issue/PR page stayed mounted and Back appeared dead.
- **D4 confirmed**: dismissal paths (Esc/`close_pr`/`close_issue`) and
  the notification page itself record nothing; the notification page's
  hand-off to an issue/PR records through the normal `open_*` path.
- **D7**: actions `nav::Back`/`nav::Forward`, bound to `alt-left` /
  `alt-right` (no conflicts), handled on the shell's focused root;
  `nav-back`/`nav-forward` ghost icon buttons sit left of the
  breadcrumb with `.debug_selector` for the bridge.
- **Keyboard focus after page switches** (post-merge fix, owner
  report): gpui dispatches key actions along the *focused node's*
  dispatch path — a module sub-page (PR page) steals focus on open,
  and Back unmounts it, leaving a dead focus whose dispatch fallback
  is the *window* root, which sits above the shell's `on_action`
  handlers — so the second Alt+← silently died (the GUI buttons
  bypass dispatch, which is why they kept working). Fix: the shell
  root takes focus after every trail-driven page switch — after
  `navigate`, and in Back/Forward *before* module re-activation so a
  deep restore (PR page) steals it back as a fresh click would.
  Regression-tested through the real keystroke pipeline
  (`simulate_keystrokes`) in `back_keystroke_survives_the_deep_view_unmounting`.
- **Tests**: +6 (4 navigator trail units; the shell contract —
  cross-branch navigate collapses, Back restores the payload — with a
  fake module; the GitHub restore/collapse/foreign-payload round trip).
  Workspace 483 green.

- **Mouse back/forward buttons** (#309, shipped): the capability check
  came back trivially positive — the Linux backend maps them natively
  (X11 buttons 8/9, Wayland `BTN_BACK`/`BTN_SIDE` and `BTN_FORWARD`/
  `BTN_EXTRA`) to `MouseButton::Navigate(Back|Forward)`. The shell root
  adds two `on_mouse_down` listeners that call the same `go_back`/
  `go_forward` the keyboard actions drive — global semantics, matching
  browsers (they work anywhere in the window, over pickers and inputs
  too). The §9 follow-up is closed by this.
