# 0018 — Files changed tab: sidebar with list/tree views and master–detail diff

Date: 2026-09-04
Status: Shipped (2026-09-04, see §8)

## 1. Context

The Files changed tab (0016 D8) renders each file as an accordion card:
a `Collapsible` header per file, diff body inline underneath. With more
than a handful of files this becomes a wall of diffs — there is no
overview, no navigation, and opening a file pushes all others down.

The product owner wants GitHub-style master–detail instead:

- a **left sidebar** inside the Files tab content area;
- at its top a **Select** choosing between **List view** and **Tree view**;
- a **filter/search input** above the file views (applies to both);
- the files themselves as a flat **List** or a path **Tree**;
- selecting a file shows **its diff to the right** of the sidebar.

## 2. Goals and non-goals

**Goals**

- Sidebar (select + filter + file views) and a single-file diff pane.
- List and Tree are two renderings of the same file set; both select a
  file; the diff pane is shared.
- Keep the diff rendering itself (0016 D8: `parse_patch`, `diff_row`,
  semantic tints) unchanged.
- Keyboard/pointer parity with the repo list patterns already shipped;
  a11y per docs/0010 (`.id()` + role on every interactive element).

**Non-goals (v1)**

- "Viewed" checkboxes, expand-context hunks, per-file commenting.
- Image/binary diffs beyond today's placeholder.
- Sidebar drag-resize; persistence of view mode across page opens.
- Virtualized tree (upstream `Tree` is a uniform list internally —
  fine for PR-scale file counts).

## 3. Upstream facts (verified against pinned gpui-component `0c746df`)

> **Re-verified at the v0.6.0 pin `94a313a` (2026-09-09, #125):** the
> GPUI Kit rebrand moved the crate tree (`crates/ui/src` →
> `crates/component/src`; crate name and public paths unchanged —
> invisible to this repo). Per-file result: `select.rs`, the textarea
> module and the whole `list/` directory are **byte-identical** (line
> cites below hold exactly: `delegate()`/`delegate_mut()` at
> list.rs:147/151, `set_query` at 215); `base/src/tree.rs` is
> **byte-identical** (`on_entry_click` still at 411–415, sets
> `selected_ix` + `cx.notify()`, emits no event); `theme_color.rs`
> identical (all semantic tokens survive). The Select `size_full` trap
> below **survives** — `size_full` still at select.rs:465/782 — so the
> wrapper-div workaround (§8) stays.

All of this ships upstream today; no new dependencies:

- **Select** — `SelectState::new(SearchableVec::from(items), None,
  window, cx)` + `set_selected_value`; already used for the rail's
  method select (pr_page.rs:162).
- **List** — `ListState::new(delegate, window, cx)`; `List::new(&state)`
  is blessed as a flex child with `.flex_1().min_h_0()` (page.rs:860,
  list_story.rs). Selection arrives as `ListEvent::Confirm(IndexPath)`
  via `cx.subscribe_in` (page.rs:109 precedent). The delegate is
  reachable (`delegate()`/`delegate_mut()`, list.rs:147/151), and
  `ListState::set_query(query, window, cx)` drives the delegate's
  `perform_search` (list.rs:215).
- **Tree** — `ui::tree::{tree, TreeState, TreeItem, TreeEntry,
  TreeEvent}` (re-exported from gpui-base). `TreeItem::new(id,
  label).children(..).expanded(bool)`; `TreeState::new(cx)`,
  `set_items`, `selected_entry()`, `set_selected_index`, `index_of`.
  The render callback is `Fn(usize, &TreeEntry, selected, &mut Window,
  &mut App) -> ListItem` — rows are `ListItem`s (a11y role included).
  **Selection emits no event**: `TreeState::on_entry_click` sets
  `selected_ix`, toggles expand, and calls `cx.notify()` (base/tree.rs
  411–415) — so the page **observes** the `TreeState` entity and reads
  `selected_entry()` on re-render.
- **Input** — single-line `InputState` (`InputBaseState<InputMode>`) +
  `Input::new(&state)`; text changes arrive as `InputEvent::Change`.

## 4. Design decisions

### D1 — Master–detail split

`render_files` (fetch-ready branch) returns an `h_flex()` filling the
tab area: **sidebar** (fixed `w_75` ≈ 300px, `border_r` on
`cx.theme().border`, `h_full`, own internal scroll regions) +
**diff pane** (`flex_1().min_w_0()`). Both children are
`v_flex().min_h_0()` so internal scrolls never grow the tab (0016 D5
lesson: size containers explicitly).

### D2 — Sidebar composition

Top-to-bottom, all inside one `v_flex().gap_2().p_2()`:

1. **Summary row** (moved from the old header): `N files · +a −d`,
   muted, `text_xs` — unchanged data, `files_summary` stays.
2. **View-mode Select** (D3).
3. **Filter input** (D6) — one shared `Input`, placeholder
   `Filter files…`.
4. **View area** (`flex_1().min_h_0()`): the List (D4) or the Tree
   (D5), depending on the view mode. The inactive view is not rendered.

### D3 — View-mode Select

`FilesView` enum `{ List, Tree }` with `Default = List`, stored as a
page field. The `SelectState` entity is built lazily on the first
Files render with a detail payload (same pattern as `method_select`,
pr_page.rs:118–120 — `SelectState` needs a window). Values: `List`,
`Tree`. Switching modes updates `files_view` and resets the selection
(D7).

### D4 — List view

A `ListState<FileListDelegate>` entity built lazily alongside the
others; rendered as `List::new(&state).flex_1().min_h_0().w_full()`
with `.searchable(false)` — the shared input (D6) is the only search
box, so the List never renders its own.

- The delegate holds a lightweight snapshot of the fetch payload:
  `FileEntry { path, additions, deletions, status }` (no patch text —
  the diff pane reads the authoritative payload, D8) plus the active
  `query: String`.
- Rows render like the old card headers, minus the chevron: mono
  `text_xs` path (muted dir prefix via `split_path`), `+adds/−dels`
  in success/danger, change badge (`file_status_badge` stays). The
  selected row uses the `ListItem` selected styling.
- `ListEvent::Confirm(IndexPath)` → `selected_file = Some(path)`; the
  delegate owns the ix→path mapping (table-tested).
- Rebuild on fetch-ready (`set_items`-equivalent: delegate swap +
  notify); refetch-safe because selection is path-keyed.

### D5 — Tree view

- Pure fn `build_file_forest(entries: &[FileEntry]) -> Vec<TreeItem>`:
  folder nodes carry the directory path as id and the last segment as
  label; file nodes carry the full path as id and the basename as
  label; duplicate dirs merge. Table-tested.
- A `TreeState` entity built lazily; items set on fetch-ready and on
  filter changes (D6). Rendered via `tree(&state, render_item)` —
  file rows reuse the list-row look (no adds/dels in tree rows to
  keep them narrow; the diff pane header shows them), folder rows get
  the chevron from `TreeEntry::is_expanded()`.
- Folders start expanded (`expanded(true)`); while a filter is active
  every rebuilt folder is expanded (filtering collapses nothing
  silently).
- Selection: the page `cx.observe_in(&file_tree, …)` → `cx.notify()`;
  the diff pane reads `file_tree.read(cx).selected_entry()` (id =
  path). `set_selected_index`/`index_of` are used for programmatic
  selection (auto-select, filter survival).

### D6 — Shared filter input

One standalone single-line `Input` (entity `file_filter`, built lazily
with the others). `InputEvent::Change` → `query = trimmed text`:

- **List**: `file_list.update(…, st.set_query(q, …))` — the delegate's
  `perform_search` filters `FileEntry`s by case-insensitive substring
  on the full path.
- **Tree**: rebuild the forest from matching entries only (a folder
  survives iff it has a matching descendant; ancestors of matches are
  kept) and `set_items` on the `TreeState`.
- Selection survives a filter change iff the selected path is still in
  the filtered set; otherwise the diff pane falls back to the empty
  state (D8). Switching views keeps the query text (same input).

### D7 — Selection model

- **Path-keyed** (`SharedString` full path) everywhere — GitHub paths
  are unique per PR diff, and paths survive refetches that re-order
  nothing but invalidate indexes (the old `collapsed_files` set had
  the index caveat).
- **Auto-select** the first file when a fetch lands (fetch-ready
  apply step) so the pane is never mysteriously empty; clearing the
  selection is not exposed v1.
- List and Tree keep their own selection state (page field vs
  `TreeState`); switching view mode shows the new view's selection
  (initially none → empty state). Recorded as a deliberate v1
  simplification.

### D8 — Diff pane

Header row: `split_path` dir (muted) + basename (mono), change badge,
`+adds/−dels` — then the existing `diff_body` (horizontal scroll, mono
`text_xs` rows) inside a `flex_1().min_h_0().overflow_y_scroll()`
container (`pr-diff-scroll`). No `Collapsible`; no per-pane reload —
`diff_body` reads the `PrFile` looked up by path from `files_fetch`.

- **Empty state** (no selection or selection filtered out): centered
  muted text "Select a file to view its diff".
- The old `file_card`/`collapsed_files` accordion path is **deleted**
  (0016 D8 amended with a pointer here). `parse_patch`, `diff_row`,
  `diff_body`, `file_status_badge`, `split_path`, `files_summary` all
  survive unchanged.

### D9 — Accessibility (docs/0010)

Every interactive element gets `.id()` + role, verifiable in the
bridge dump: `pr-files-view` (Select → ComboBox), `pr-files-filter`
(Input → TextInput), `pr-flist-{path}` rows (ListItem + selected
state), `pr-ftree-{id}` rows (ListItem), `pr-file-diff` container.
New interactive elements without ids are bugs, per 0010 D10.

### D10 — Tokens ([DG])

Semantic colors only (border, muted_foreground, success, danger,
accent via components); mono token (`cx.theme().mono_font_family`)
for paths and diff rows; no ad-hoc `text_size`/`line_height`/literal
colors — clippy.toml lints enforce.

## 5. Testing

- Table tests (pure fns): `build_file_forest` (nesting, root-level
  files, repeated dirs), path filter (case-insensitive, substring,
  no match), `FileListDelegate` ix→path mapping, `FilesView` default.
- Gates as always: fmt, clippy zero warnings (token lints included),
  `cargo test --workspace`.
- Bridge verification (0010): dump shows the sidebar select + filter +
  rows and the diff pane; clicking a list row mounts its diff; the
  Tree view renders folders; filtering narrows rows; Tab round-trip
  keeps geometry; drift 0; no panics. Color/visual nuances stay with
  the owner's human check.

## 6. Implementation plan

Single stage, one PR:

1. `files.rs` (new module in the github crate): `FilesView`,
   `FileEntry`, `build_file_forest`, path filter, `FileListDelegate`,
   sidebar/diff-pane render fns, tests — keeps pr_page.rs from
   growing further.
2. pr_page.rs wiring: page fields (D3/D6/D7), lazy entity build,
   fetch-ready apply, `render_files` swap, accordion deletion (D8).
3. Docs: 0016 D8 amendment (superseded → 0018) and 0018 shipped note
   ride along; README index row for 0018.

## 7. Open questions (owner)

1. Tree folders expanded by default — proposed yes (small PRs read
   flat); happy to start collapsed top-level instead.
2. Auto-select first file on fetch — proposed yes (GitHub-like);
   alternative is a always-empty pane until click.
3. Sidebar fixed at ~300px, no resize — confirm acceptable for v1.

## 8. Shipped (2026-09-04, issue #109)

Implemented as designed, with three deviations from the written design:

1. **Sidebar entities are built eagerly in `new`, not lazily on the
   first Files render** (D3/D6 wording). `new` already has the window
   that `SelectState`/`InputState` need, so up-front construction with
   the subscriptions attached once is simpler than a render-time
   branch; the payload is still applied fetch-ready (D6/D7 unchanged).
2. **List auto-selection skips the upstream selection mechanism**
   (D7). `ListState::set_selected_index` needs a window, which the
   fetch-ready apply (an async context) doesn't have — the delegate's
   `selected` mirror drives the row highlight instead, and the trait's
   `set_selected_index` keeps it in sync for keyboard pre-selection.
3. **Tree auto-selection intentionally does not mirror the List's**
   (D7 simplification, made concrete): entry 0 of a forest is a folder,
   so blindly selecting it would pick a non-file. Each view starts with
   its own (empty) selection; the diff pane shows the empty state.

Also moved into the new `files.rs` module (§6 plan said sidebar/diff
render fns; the pure helpers `split_path`/`files_summary`/
`file_status_badge` and the `diff_body`/`diff_row` pair moved too, so
pr_page.rs shrank instead of growing). Their table tests moved with
them and the forest/filter/delegate tests were added alongside.

A follow-up on the first human check found the List/Tree view area
collapsed to zero height on the live build: upstream `Select` renders
with a `size_full` root, so as a *direct* flex child of the sidebar
column it resolved `height: 100%` against the sidebar and claimed the
whole main axis, leaving `flex_1` nothing to grow into (the filter
input then sat visually at the bottom of an all-controls sidebar).
The sidebar's fixed rows are now each wrapped in a plain content-sized
`div()` — percentage-height controls must never be direct children of
a flex column that owns a `flex_1` view — and the sidebar/diff pane
heights ride on `self_stretch` (the split's definite cross size), not
on `h_full` percentages. The regression guard is a layout probe test
(`files_split_has_real_layout`): it renders the real page in a test
window and asserts measured bounds via gpui `debug_bounds`, with
test-only `debug_selector` anchors in `files.rs` (noop in release).
Executable half re-run on the v0.6.0 pin (2026-09-09, #125): the probe
passes on `94a313a` — the workaround remains necessary and proven.
