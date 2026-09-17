# 0041 — GitHub Module: Virtualized Diff Scroll (Files tab)

Issue: #253. Related: #202/#252 (syntax-highlighted diffs, docs/0040), docs/0030
(review threads), docs/0018 (files tab layout).

## 1. Context & problem

Post-#252 human check: scrolling a large diff (460 rows in `syntax.rs`) is
"better, not ideal". The remaining cost is structural, not token-related:

1. **The diff pane is not virtualized.** `diff_body` (files.rs) materializes
   *every* row of the file — plus thread cards, the row composer, and queued
   drafts — as real elements on **every frame**. The outer container is a
   plain `overflow_y_scroll` div.
2. **`anchor_patch` parses the whole patch twice per render** (once in
   `thread_placement`, again in `diff_body`) — hundreds of `String` allocations
   per frame, O(file size) regardless of viewport.

**Measured** (headless probe, gpui-pre 0.3.3 test executor, release build,
900×600 viewport, 60 draws advancing ~40 px/frame, 2026-09-17; probe crate
`/tmp/bench_diff` — numbers removed with it, re-derivable):

| row builder | 460 rows | 2000 rows | rows built over 60 draws |
| --- | --- | --- | --- |
| div stack (status quo) | 218 ms | 1042 ms | 28,060 / 122,000 (460 / 2000 per frame) |
| `gpui::list` | 1.4 ms | 2.2 ms | 134 (~2.2/frame) |
| `gpui::uniform_list` | 1.1 ms | 1.9 ms | 122 (~2.0/frame) |

Frame cost today is O(file size): 17 ms/frame *in element construction and
layout alone* at 2,000 rows — below 60 fps before the GPU sees anything.
Virtualization makes it O(viewport) and file-size-independent (row counts for
460- and 2,000-row lists are identical).

## 2. Scope

- In: the diff body (rows + interleaved thread/composer/draft cards), the
  anchor cache, diff-pane scrollbars.
- Out: the file list/tree (already virtualized gpui-component `List`), the
  check-log pane (already scroll-handled, #249), conversation cards, the
  0040 highlight pipeline (unchanged interfaces), any change to row *content*
  or the 0030 card placement model.

## 3. Verified facts

All against the pinned `gpui-pre 0.3.3` and gpui-component `0c746df` sources,
2026-09-17:

1. `gpui::list(state: ListState, render_item)` — virtualized list for
   **differently sized** elements; item heights are measured, then cached in a
   `sum_tree` (O(log n) updates). State is intrusive on the owning view
   (elements/list.rs:24).
2. `ListState::new(count, ListAlignment::Top, overdraw)`, `reset(count)`,
   `reset_with_uniform_height(count, height)` (list.rs:314, 355, 372),
   `splice(old_range, count)` (list.rs:503),
   `scroll_to(ListOffset)` / `scroll_to_reveal_item(ix)` (list.rs:660, 677),
   `viewport_bounds()` (list.rs:799).
3. `ListHorizontalSizingBehavior::{FitList (default), Unconstrained}` —
   "items' width may go over the width of the list" (list.rs:218–224); the
   list tracks `max_item_width` during layout (list.rs:1040, 1119, 1487), so a
   horizontally-scrolling wrapper around the list is the supported shape for
   nowrap diff rows.
4. gpui-component `Scrollbar` implements `ScrollbarHandle for ListState`
   (scrollbar.rs:120) — the existing overlay-scrollbar pattern (check-log,
   #249) attaches to a `ListState` directly. The diff pane has **no scrollbar
   overlay today** (wheel only).
5. `gpui::uniform_list` requires uniform item height (uniform_list.rs:22) —
   unusable while 0030 interleaves variable-height cards between rows.
6. gpui-component `virtual_list` requires *declared* per-item sizes
   (virtual_list.rs header note) — wrong fit for cards whose height depends on
   collapsed/expanded state and text.
7. The file list already uses the same `ListState`-based machinery (files.rs
   imports `list::{List, ListDelegate}`) — precedent for behavior/feel.

## 4. Design decisions

### D1 — `gpui::list` (variable heights), not `uniform_list`

Diff rows are uniform-height, but 0030 D3 interleaves thread cards, the row
composer, and draft rows *between* diff rows, with content-dependent heights.
`uniform_list` can't represent that (verified fact 5); `gpui::list` measures
and caches per-item heights in a sum_tree and matches the card model
unchanged. No product behavior moves.

### D2 — Flattened item model: `Vec<DiffItem>`

`diff_body` stops building an element vec and builds a **descriptor vec**
instead, in the same order as today's `flat_map`:

```rust
enum DiffItem {
    Row(usize),                        // index into the file's AnchoredLine rows
    Thread(usize, DiffThread),         // thread card under its anchor row
    Composer(SharedString, DiffSide, u32),
    Draft(usize),                      // queued draft under its anchor row
}
```

`ListState::reset_with_uniform_height(row_count, ROW_H)` seeds row heights;
card items measure on first materialization (default `Visible` measuring) and
are cached until their count changes. The `render_item` closure maps
`DiffItem` → the *same* element constructors used today (`diff_row`,
`thread_card`, `line_composer_card`, `draft_rows`) — no row rendering logic
changes.

### D3 — Item-count changes splice, not reset

Expand/collapse a thread, add/delete a draft, or a refresh that changes the
flattened count calls `ListState::splice(old_range, count)` / `reset` for the
delta. The page already knows every transition (0030 state, 0031/0032
refresh). Scroll position survives splices above the viewport (sum_tree).
Compare-and-splice runs in the page's `&mut Context` paths (apply handlers),
not in render.

### D4 — One `anchor_patch` per file version (cached `Arc`)

Page-owned `AnchorCache`: `RefCell<BTreeMap<String /*path*/, (u64 /*FNV patch hash*/, Arc<Vec<AnchoredLine>>)>>`.
`thread_placement`, `diff_body`, and `pump_highlight` all read the same
`Arc`. Invalidation is per-path by patch hash, same pattern as the 0040
highlight cache (D5). The double parse disappears; re-parses happen only when
a file's patch actually changes.

### D5 — Horizontal scroll: wrapper + `Unconstrained`

Rows stay `whitespace_nowrap` (0018). The list element declares
`ListHorizontalSizingBehavior::Unconstrained` and is wrapped in an
`overflow_x_scroll` container (as today's body is); the list's own
`max_item_width` sizing makes the wrapper's horizontal scrollbar cover the
widest visible row. Vertical wheel lives in the list; horizontal wheel in the
wrapper — same axes as today, now with vertical virtualization intact.

### D5 (amended at impl, this PR) — horizontal width

`ListHorizontalSizingBehavior::Unconstrained` (§3 fact 3) has **no setter on
`gpui::list` in the pinned build** — the enum is dead code there. Worse,
`ListSizingBehavior::Infer` inside an x-scroll wrapper disables vertical
virtualization entirely (the wrapper measures height as MaxContent, so the
list lays out every row — measured: 807 rows/frame on a 460-row file, worse
than the div stack). What works, verified in the probe: keep the list
`Auto` and give it an explicit **content-derived width**
(`clamp(400, 120 + longest-row-chars × 8px, 16 384)` px — the row lengths
are already cached in the `AnchorCache`) inside the x-scroll wrapper, with
`.h_full()` pinning the viewport. Wheel axes unchanged; scrollbar travel is
honest up to the cap (rows longer than ~2 000 chars clip, like GitHub's own
wide-diff behavior).

### D6 — Scrollbars: `ScrollbarHandle for ListState`

Attach the existing gpui-component `Scrollbar` overlay to the diff list
(vertical, via `ListState`) and to the horizontal wrapper (via its
`ScrollHandle`). This is the same pattern as the check-log pane (#249) and
gives the diff pane its first visible scrollbars.

### D7 — Selection/scroll persistence across refresh

`ListState` keeps scroll offset and item heights intrusively on the page, so
the per-minute refresh (0032/0035) no longer reconstructs a scroll container.
Combined with 0040's selection preservation, the viewport stays where the
reviewer left it. `scroll_to_reveal_item` becomes available for follow-ups
(e.g. jump-to-comment, jump-to-hunk) — filed, not built here.

### D8 — The 0040 row cap stays; the *render* cap lifts

`SYNTAX_HIGHLIGHT_MAX_LINES` (tokenize cost, docs/0040 D5) is untouched.
The old implicit "the pane renders everything" cost stops bounding file
*rendering*; 2,000+ row files scroll at viewport cost. No new cap is added.

## 5. Alternatives considered

- **`uniform_list`** (fastest, 0.018 ms/frame): rejected — cannot host
  variable-height cards (D1). If cards ever move out of the row flow, this
  becomes the better primitive; noted for the future, not now.
- **gpui-component `virtual_list`**: rejected — requires declared sizes
  (verified fact 6); card heights aren't knowable without measuring.
- **Keep the div stack, cache row elements**: element trees are single-use in
  gpui (rebuilt per frame by design); caching buys nothing. The win is not
  building off-viewport rows at all.
- **Soft-wrap rows to kill horizontal scroll**: a product change (0018 D: no
  soft wrap), makes rows variable-height, and doesn't address O(file size)
  vertical cost by itself. Out of scope.

## 6. Testing

- Unit: `AnchorCache` hit/invalidate by patch hash; `DiffItem` flattening
  (rows, cards, composer, drafts in 0030 order; stray/file-level unchanged).
- Deterministic (`#[gpui::test]`): list-backed diff renders the same visible
  rows as the div stack at scroll-top and mid-scroll; thread expand/collapse
  splices (count changes, scroll survives); drafts land under their row;
  horizontal wrapper scrolls for a >viewport-width row; scrollbar handles
  attach (viewport_bounds non-degenerate).
- Perf probe (manual, recorded in the PR body): repeat the §1 bench against
  the real `diff_body` — expect rows-built-per-frame ≈ viewport rows.
- Gates: fmt, clippy (workspace, zero warnings), workspace tests.

## 7. Acceptance criteria

- [ ] Scrolling a 2,000-row diff is as smooth as a 40-row diff (frame cost
      independent of file size).
- [ ] Thread cards, composer, and drafts render exactly where 0030 places
      them; expand/collapse keeps scroll position.
- [ ] The diff pane has visible vertical + horizontal scrollbars.
- [ ] A per-minute refresh no longer resets or janks the diff viewport.
- [ ] `anchor_patch` runs once per file version, not twice per frame.

## 8. Human checks (owner)

- [ ] Scroll `syntax.rs` (460 rows) and a >2,000-row diff in the real app:
      buttery on wheel and scrollbar drag, no jank on refresh ticks.
- [ ] Open/collapse threads and type drafts mid-file: cards appear/disappear
      under the right rows without viewport jumps (besides intended growth).
- [ ] Horizontal scroll on a long-line file (e.g. a minified lockfile):
      gutter + content move together; horizontal scrollbar present.

## 9. Follow-ups (GitHub issues, not this PR)

- Jump-to-comment / jump-to-hunk via `ListState::scroll_to_reveal_item`.
- Revisit `uniform_list` if cards ever leave the row flow (D1 note).
- Apply the same virtualization to the check-log pane if its logs grow past
  viewport costs (currently fine at #249 sizes).

## 10. Shipped (2026-09-17, PR #257 — issue #256/#253)

Implemented as designed (D1–D8), with two recorded deviations:

- **D5 as amended above**: explicit content-derived list width instead of
  the dead `Unconstrained` enum; `Infer` rejected on measured evidence.
- **D2 superset**: the flattened item list also carries the file-level
  threads, the threads fetch row, and the stray bucket (`FileThread`,
  `FetchRow`, `StrayBucket` variants) — the list is now the pane's single
  vertical scroller, so nothing outside it can jump while rows virtualize.

Height-only changes (reply box, resolve picker, stray toggle, composer and
reply-editor typing) resync via precise `ListState::splice` calls from the
mutating event; composer/reply textareas are observed so per-keystroke
growth re-measures.

**Measured on the real page** (deterministic test
`virtualized_diff_builds_viewport_rows_per_frame`, 1 001-row diff, 10
mid-file scroll jumps): **838 rows built total ≈ 84/frame** — viewport +
overdraw, file-size independent. The div stack built all 1 001 rows every
frame; the pre-list headless bench measured 17 ms/frame at 2 000 rows.
