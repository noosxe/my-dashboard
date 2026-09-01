# 0009 — GitHub module: official `Select` + `List` components

Two hand-rolled UI pieces in the GitHub module page are replaced with
gpui-component primitives, both search-enabled (owner request):

| Piece | Today (docs/0005 §6.3) | Becomes |
|---|---|---|
| Org selector | inline popover: `render_org_dropdown` trigger + `render_org_overlay` (manual last-child overlay, `occlude()`, backdrop, hardcoded geometry) | `Select` + `SelectState<SearchableVec<SharedString>>`, `.searchable(true)` |
| Repo list | eager `div#repo-list` with `overflow_y_scroll` + manual row divs (`render_repo_row`) | `List` + `ListState<RepoListDelegate>`, `.searchable(true)` (virtualized) |

Owner decision: `Select`, **not** `Combobox` — Select is the
non-editable picker; Combobox adds editable-query semantics and extra
complexity we don't need.

## 1. Non-goals

- The right-column PR/notification rows (GroupBox contents, small fixed
  sets) stay as-is. Eager rendering is correct there.
- No "All orgs" sectioned repo list, no multi-select, no editable
  org filter — `SearchableVec` substring filtering is enough.
- No change to fetching, caching, or service behavior — this is a view
  swap over the same in-memory snapshot (0007 D6).

## 2. Verified upstream facts (pinned gpui-component rev `1081d2b`)

Everything below was read out of the checkout; nothing is remembered.

**Select** (`crates/ui/src/select.rs`):
- `SelectState::new(delegate, selected_index, window, cx)` — needs
  `&mut Window` + `Context<Self>`; returns a plain state struct stored in
  an `Entity<SelectState<D>>`. `.searchable(bool)` puts a search input at
  the top of the dropdown.
- `Select::new(&Entity<SelectState<D>>)` is the per-render builder:
  `.placeholder()`, `.search_placeholder()`, `.menu_width()`,
  `.menu_max_h()`, `.empty()`, `.appearance()`, `Sizable` (`.small()`).
- The open menu is drawn via `deferred(popover::dropdown_popup(...))`
  with `.occlude()`, `.with_priority(POPUP_PRIORITY)`, and
  `on_mouse_down_out` to close — **paint order, click-through, and
  outside-click are handled internally**. The app already renders
  gpui-component's `Root` (main.rs), which hosts the deferred layer.
- Selection commit: `EventEmitter<SelectEvent<D>>` with
  `SelectEvent::Confirm(Option<Value>)` (also `DismissEvent`).
- `set_items(&mut self, items: D, _: &mut Window, cx)` replaces the
  delegate wholesale. `set_selected_value(&v, window, cx)` clears any
  active query, looks up `position(v)`, and selects it (None if absent).
- Built-in `SearchableListItem` impl for `SharedString` (Value =
  SharedString) — org logins need zero custom item code.

**Searchable list machinery** (`crates/ui/src/searchable_list/vec.rs`):
- `SearchableVec<T>` = `items` + `matched_items`; `perform_search`
  rebuilds `matched_items` via `T::matches` (default: case-insensitive
  substring on `title()`). `From<Vec<T>>` exists. This is the org
  delegate, as-is.

**List** (`crates/ui/src/list/{list,delegate,cache,list_item}.rs`):
- `ListState::new(delegate, window, cx)` — an `Entity`, created once;
  `.searchable(bool)` renders a search input (search icon, cleanable)
  above the list. `List::new(&Entity)` is the per-render builder
  (`.search_placeholder()`, Styled, Sizable).
- Virtualization: `v_virtual_list` over a `RowsCache`. **Items must be
  uniform height** (trait doc). Our rows are one text line — satisfied.
- Cache invalidation: `RowsCache::prepare_if_needed` re-prepares when
  per-section item counts change (`new_sections != self.sections`), so
  filter-result and data-refresh count changes propagate automatically;
  visible-row *content* re-renders every frame regardless.
- Selection & input: click = select + confirm; keyboard up/down/enter/
  secondary-enter/escape are bound to the `"List"` key context by
  `list::init` (called from `gpui_component::init`, which main.rs
  already runs). Events: `ListEvent::{Select(IndexPath), Confirm(
  IndexPath), Cancel}`.
- `ListDelegate::render_item` returns `Self::Item: Selectable +
  IntoElement`; ListState wraps rows in `role=ListItem` divs with aria
  state and applies `.selected(bool)`. The stock `ListItem`
  (`list/list_item.rs:136` implements `Selectable`) provides the
  official row chrome (padding, hover, selected style) — so
  `Self::Item = ListItem` and we write no custom item type.
- `ListDelegate::render_empty` default is an Inbox icon; overridable.
- `ListState::set_selected_index` (public) does **not** scroll; the
  crate-internal `_set_selected_index` scrolls. Sync paths use the
  public one — background refreshes must not yank the viewport.

**Entity/context plumbing**:
- Both `new`s need `&mut Window` at construction time.
  `GithubModule::init` already receives `window: &mut Window`
  (lib.rs:46) — `GithubPage::new` gains a `window` parameter.
- Event handlers that need `&mut Window` use `cx.subscribe_in(entity,
  window, ...)` (the pattern gpui-component itself uses, e.g. list.rs
  `on_query_input_event`).
- The state observer (`cx.observe`) has no Window; updates there go
  through a stored `AnyWindowHandle` +
  `window.update(cx, |_, window, cx| ...)` — the same
  `let _ = window_handle.update(...)` pattern service.rs already uses.

## 3. Decisions

### D1 — Org selector: `SelectState<SearchableVec<SharedString>>`

- Delegate: `SearchableVec<SharedString>` (logins). Value = login.
- State: `.searchable(true)`; render via
  `Select::new(&state).placeholder("Organization…")
  .search_placeholder("Filter organizations…").small()` — `small()`
  matches the current trigger density; visual tweak at review.
- Before orgs load: placeholder shows; menu shows a muted
  `.empty()` → "No organizations yet".
- Q10 preserved: the personal account (first org) is the default — on
  first arrival `page.org` adopts logins[0] and the Select selects it.

### D2 — Repo list: `ListState<RepoListDelegate>`, `.searchable(true)`

`RepoListDelegate` (new, in `page.rs`):

```rust
struct RepoListDelegate {
    all: Vec<gql::Repo>,        // visible under current org, service order
    matched: Vec<gql::Repo>,    // all filtered by query
    query: String,              // last applied query (survives set_repos)
    selected: Option<IndexPath>, // stored for ListState
}
```

- `set_repos(repos)` replaces `all` and re-filters with `query`.
- `ListDelegate::items_count` → `matched.len()`; `render_item` →
  `ListItem::new(repo.key).child(name + lock glyph)` (uniform one-line
  rows; selection chrome applied by ListState via `Selectable`).
- `perform_search(query, ..)` → case-insensitive substring match on
  `name` **or** `description`; updates `query` + `matched`.
- `render_empty` → query non-empty: "No matches"; else
  "No repositories yet" (current copy).
- `set_selected_index` stores the index; `confirm`/`cancel` are no-ops —
  the delegate stays data-only; the page reacts to `ListEvent::Confirm`.
- `render_initial` unused (default None): the pre-search state is the
  normal list.

### D3 — Entity ownership & construction

`GithubPage` gains fields:

```rust
org_select: Entity<SelectState<SearchableVec<SharedString>>>,
repo_list:  Entity<ListState<RepoListDelegate>>,
window:     AnyWindowHandle,
```

`GithubPage::new(state, window, cx)` (called from `Module::init`, which
has the window) constructs both entities with empty delegates. The page
subscribes `subscribe_in` to both:
- `SelectEvent::Confirm(Some(login))` → org switch (D5)
- `ListEvent::Confirm(ix)` → `select_repo(repo_at(ix).full_name)`
  (existing method, unchanged fetch behavior)

### D4 — Selection model: `GithubState.selected` stays canonical

- Row click / Enter → `ListEvent::Confirm` → existing `select_repo`
  (sets `state.selected`, fetches repo-scoped specs). Arrow keys move
  the highlight (`ListEvent::Select`) **without** fetching — fetch on
  confirm only.
- Org switch → `state.selected = None`, list selection cleared, query
  cleared, delegate repopulated (D5).
- Background refreshes re-sync the list's selected index from
  `state.selected` (D6) — via the non-scrolling `set_selected_index`.

### D5 — Org switch flow

On `SelectEvent::Confirm(login)` (subscribe_in gives the window):

1. `page.org = Some(login)`
2. `state.update: s.selected = None` (selected repo belongs to the old
   org — current behavior)
3. `repo_list.update: delegate.set_repos(visible_repos)`,
   `set_selected_index(None)`, `set_query("")` (reset search with the
   scope change)
4. `cx.notify()`

### D6 — Background data sync (the async-arrival path)

The existing `cx.observe(&state, ...)` is extended — this is the path
that the old inline popover couldn't serve (`set_items` needs
`&mut Window`):

1. Compare `state.orgs` logins against the last-synced vec. On change:
   `window.update` → `org_select.set_items(SearchableVec::from(logins))`,
   then resolve selection via the pure helper `next_org_selection`
   (below) and `set_selected_value(&choice)`.
2. Compare visible repos (keys) against last-synced. On change:
   `repo_list.update` → `delegate.set_repos(...)`, then re-sync
   `set_selected_index(position of state.selected)` — non-scrolling,
   None if filtered out.
3. `cx.notify()`.

All `window.update` results are `let _ =`-discarded (window may be
closing). Sync is compare-first — untouched ticks cost two vec
comparisons, no entity writes.

```rust
// Pure, unit-tested:
fn next_org_selection(current: Option<&str>, logins: &[SharedString])
    -> Option<SharedString>
// None               + []            -> None            (nothing to pick)
// None               + [a, b]        -> Some(a)         (Q10 default)
// Some(x) + contains x            -> Some(x)          (keep)
// Some(x) + [a, b] without x      -> Some(a)          (vanished: reset)
```

### D7 — Left column layout

`v_flex().w(px(280.)).flex_none().h_full().border_r_1().p_2().gap_2()`
→ `.child(Select ...small().w_full())` → `.child(div().flex_1()
.min_h_0().child(List::new(&repo_list).search_placeholder(
"Filter repositories…")))`. `min_h_0` so the flex child can shrink; the
List owns scrolling and its vertical scrollbar
(`scrollbar_visible` defaults true). The manual `overflow_y_scroll`
div goes away.

### D8 — Deleted code

`render_org_dropdown`, `render_org_overlay`, `orgs_open`, the
`"repo-list"` scroll div, `render_repo_row`, and the page.rs module
header note about why Select wasn't used (the `&mut Window` constraint
is solved by D3/D6). `effective_org` stays as the pre-orgs fallback for
repo filtering (returns first org / all repos when no orgs exist).
`muted_cell` stays — right-column GroupBoxes still use it.

## 4. Testing

- `RepoListDelegate` unit tests (pure): filter on name /
  description / case; `set_repos` keeps the active query and re-filters;
  empty `matched` after filter; `repo_at` mapping.
- `next_org_selection` table tests (all four rows above).
- One `#[gpui::test]` (mirroring upstream select.rs tests):
  `cx.add_empty_window()` → build `SelectState` with logins + assert
  `selected_value`; build `ListState<RepoListDelegate>` + assert
  `set_selected_index` round-trip. Guards the entity wiring compiles
  and constructs with a real Window.
- Existing 66 tests untouched (no service changes).

## 5. Acceptance criteria

- No `render_org_*` / `render_repo_row` / `orgs_open` remain in
  page.rs; no manual overlay, backdrop, or `occlude()` in the module.
- Org menu: searchable, opens/closes on trigger and outside-click,
  Escape cancels (upstream behavior), picks update the repo scope.
- Repo list: search input filters rows live; arrow keys move the
  highlight; Enter/click select (same fetches as today); background
  tick refresh keeps rows and selection highlight correct without
  scrolling the viewport.
- Orgs arriving asynchronously (first snapshot) select the first org
  (Q10) without interaction.
- All orgs/repos edge states render: no orgs yet, no repos under org,
  no search matches.
- Gates: fmt, clippy 0 warnings, `cargo test --workspace`.

## 6. Human checks

- [ ] Org trigger opens a menu with a search field; typing filters
      orgs; picking one repopulates the repo list and clears the repo
      selection; Escape closes without changing the org.
- [ ] Repo search box filters by name and description (try
      capitalization); clearing the query restores the full list.
- [ ] Click a repo → right column loads (PRs/notifications fetch);
      ↑/↓ then Enter does the same; the selected row stays highlighted
      across background refreshes.
- [ ] Switch org while a repo search query is active → query resets
      and the new org's full repo list shows.
- [ ] With a large org (>100 repos), scrolling stays smooth
      (virtualization).
- [ ] Disconnect (sign out) → page shows the connect panel; reconnect
      → orgs repopulate and default to the personal account.

## 7. Follow-ups

- Right-column PR/notification lists could adopt `List`/`ListItem` for
  consistency — out of scope, to be filed only if wanted.
