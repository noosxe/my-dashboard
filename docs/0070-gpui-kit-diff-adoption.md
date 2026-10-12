# 0070 — GitHub Module: Adopt the gpui-kit Diff Viewer

- **References:** #513 (gpui-kit 0.8 excluded breaking set — related bump
  tracking), #203 (split diff view), docs/0016 D8 (unified diff renderer),
  docs/0018 (files sidebar/diff body), docs/0029 §3/§7 (roadmap rows:
  split diff = Medium), docs/0040 (syntect syntax highlighting),
  docs/0041 (virtualized diff scroll), docs/0069 (resizable pane seam),
  docs/0025 (file-based color themes), docs/0002 D5/D9/D10 + `clippy.toml`
  (token discipline), docs/0007/0008 (verify against pinned upstream).
- **Status:** design — implementation tracked in the issue(s) filed from
  this doc.
- **Date:** 2026-10-12. Upstream facts verified against the pinned
  gpui-component `67265a3` (v0.7.3) checkout and the gpui-pre 0.3.9 stack
  on this date. Line numbers below refer to that checkout.

## 1. Context & problem

gpui-kit v0.7.3 shipped a readonly diff component
(`gpui_kit::component::diff`, ~6.5k impl lines + ~1.6k test lines in
`crates/component/src/diff/`). It overlaps almost exactly with the diff
renderer we hand-built across docs/0016 D8, docs/0040 and docs/0041 —
and it includes, working today, every feature we explicitly cut from
those arcs (split view, intra-line highlighting, expandable context,
selection/copy) plus a review-annotation API shaped like our review
threads.

Maintaining two renderers for the same job is the worst of both worlds.
This doc decides the migration: adopt the upstream Diff for the diff
body, retire our 0040/0041 machinery, and — per the product decision
taken with this doc — enable upstream's tree-sitter stack fully,
retiring our syntect tokenizer.

## 2. Scope

- **In:** the Files-tab diff body (`files.rs` — the `diff_pane` /
  virtualized list of docs/0041) moves to `Diff` + `Entity<DiffState>`.
- **In:** review-thread anchoring remaps onto `DiffAnnotation`
  (`DiffLinePosition` = path + side + source line).
- **In:** dependency change: `gpui-component` gains
  `features = ["tree-sitter", "tree-sitter-languages"]`; the `syntect`
  workspace dep and `crates/modules/github/src/syntax.rs` are deleted.
- **In:** refresh contract of docs/0041 is preserved (see D4 — this is
  the one upstream behavior that needs design, not just wiring).
- **Out:** split view as a shipped mode — the enum comes free
  (`DiffMode::Split`) but the initial migration ships Unified only to
  keep the diff to renderer-swap; the toggle is follow-up #203 work.
- **Out:** conflict-resolution UI (`resolve_conflict` /
  `resolved_text`) — read-only viewer; we never edit files locally.
- **Out:** upstream Markdown/CodeEditor components (their TextView is a
  separate question from docs/0017; our pulldown-cmark renderer stands).
- **Out:** `gpui-fast` backend and the `component-shell` media registry —
  unchanged stance from the workspace comment block.

## 3. Verified facts (at `67265a3`)

1. **Virtualized rendering.** mod.rs:1 — "Readonly, virtualized patch
   display"; renders through gpui `list` (mod.rs:558) with vertical +
   horizontal `Scrollbar` and arrow/page key bindings. Variable row
   heights are inherent to `list` (annotation rows expand under code
   rows). This is the same architecture docs/0041 D1/D2 chose.
2. **Patch-parsing input.** `DiffFile::parse(&str)` (parser.rs, 610
   lines + tests) consumes unified and Git patch text: renames
   (`original_path`), added/deleted files, mode changes, missing final
   newline, unicode paths, merge conflicts (conflict.rs). Same input
   model as our REST-patch pipeline — no recomputation, no
   old/new-text requirement. `similar 2.7 unicode` is used only for
   the intra-line word/character pass (`DiffInlineUnit`).
3. **Review annotations.** `DiffAnnotation::line(id, DiffLinePosition)`
   / `::file(id, path)` (mod.rs:146/:154); the element takes
   `.annotations(...)` + `.render_annotation(...)` /
   `.annotation_content(...)` and `.on_add_annotation(...)` (mod.rs:296
   –:410). `review_tests.rs` (372 lines) covers the flow.
4. **Selection & navigation.** Pointer/gutter/keyboard selection with
   `DiffEvent::SelectionStarted/Changed/Ended` (state.rs:79–:87),
   `selected_text()`, `set_selected_lines()`; `scroll_to_line`
   reveals collapsed context first (state.rs:765–:775);
   `scroll_to_file`, `next_change`, `previous_change`.
5. **Syntax highlighting is tree-sitter, feature-gated.**
   `Diff::syntax_highlight(bool)` (default true, "once the state has
   prepared it" — state.rs:1342 prepares files one at a time). The
   engine is `component/src/highlighter/` (highlighter.rs, languages.rs,
   registry.rs), shared with Markdown and CodeEditor. Features
   (component Cargo.toml:22, :38–:76): `tree-sitter = ["dep:tree-sitter",
   "dep:tree-sitter-json"]` and `tree-sitter-languages` = 35 grammars
   (astro … zig, including `tree-sitter-diff`). Individual grammars can
   be enabled one by one (`tree-sitter-rust = ["tree-sitter",
   "dep:tree-sitter-rust"]`, …). We enable none of them today.
6. **Colors flow through the component Theme.** The theme adapter
   derives all rich-text styling from `crate::Theme` fields
   (`text/mod.rs:35–:64`: foreground, muted, accent, selection,
   `is_dark()`); syntax styles merge with selection with documented
   precedence — syntax color survives under selection background
   (selection_tests.rs:38). No upstream diff-path color is hardcoded
   outside the theme adapter.
7. **`set_files` resets everything** — the one landmine.
   state.rs:556–:590: clears selection, **clears annotations**,
   `expanded` is rebuilt empty, `layout = None`, geometry cleared,
   `rebuild(true)`, horizontal scroll zeroed. Only `set_mode`
   (`rebuild_at_anchor`, "preserving … a nearby source row",
   state.rs:592+) and the expansion setters (state.rs:426, "preserving
   selection and scrolling") are anchor-preserving. A naive
   refresh→`set_files` call violates the docs/0041 contract ("a
   per-minute refresh no longer resets or janks the diff viewport").
8. **Ours today.** Patch model + anchor walk in `service.rs`
   (`PatchLine`/`AnchoredLine`); rendering in `files.rs`
   (`diff_pane`:485, `DiffItem` list :778, anchor cache :804,
   `flatten_diff_items` :833, diff body list :909–:935, `diff_row`
   :1085); syntax in `syntax.rs` (533 lines, syntect 5 as a
   color-blind tokenizer); `syntect` declared at workspace Cargo.toml:85,
   consumed only by the github module. `thread_placement` (:889) and
   the conversation read path anchor review threads by
   (path, side, source line) — the same triple `DiffLinePosition`
   carries.

## 4. Design decisions

### D1 — Adopt the upstream Diff wholesale for the diff body

`Diff` + `Entity<DiffState>` replaces the docs/0041 list machinery:
`DiffItem`, the page-owned anchor cache, `flatten_diff_items`'s render
half, the diff body `gpui::list`, and `diff_row` are deleted; docs/0041
gets a dated superseded-by amendment (its D1/D2 architecture is
preserved upstream; its cached-heights ownership moves into the
component). docs/0016 D8's renderer facts get the usual #125-style dated
amendment. The patch model in `service.rs` **stays**: review threads
arrive via GQL and the anchor walk feeds the conversation path; it
shrinks to a mapping role (D5), not a rendering role.

### D2 — Tree-sitter, fully (product decision)

Enable both features on the git dep:

```toml
gpui-component = { git = "https://github.com/longbridge/gpui-component",
                   rev = "67265a3…",  # v0.7.3 — keep in sync with the family
                   features = ["tree-sitter", "tree-sitter-languages"] }
```

This pulls the `tree-sitter` runtime + 35 grammar crates (C sources,
`cc`-built) and retires `syntect` (workspace line + module dep +
`syntax.rs`). docs/0040 D1 ("syntect, not tree-sitter") is consciously
reversed: the deciding factor then was house control over the
scope→color mapping; the deciding factor now is that the Diff component
renders its own rows — an engine we can't inject into. Full-pack enable
over per-grammar cherry-picking: the pack is upstream's tested
configuration, `tree-sitter-diff` highlights patch headers themselves,
and cherry-picking 35 feature names is upkeep we don't want. Build-time
cost (35 `cc` crates, one-time per toolchain) is accepted; runtime
behavior is upstream's chunked per-file preparation (state.rs:1342),
which is exactly the shape docs/0040 §5 prescribed to avoid the
livelock class.

### D3 — Colors: theme-driven, upstream-mapped

docs/0040 D2/D3 (tokenize-only, house scope→token mapping) are retired
with the syntect engine. Colors remain theme-derived end to end: our
0025 themes feed the component `Theme` (fact 6), the highlighter maps
tree-sitter captures to `HighlightStyle` from that theme, and no house
per-scope mapping survives. The 0002 clippy token lints never applied
to dependency code and still don't. Light/dark parity is a §7
acceptance item, not a new mapping table to maintain.

### D4 — Refresh contract: gate, then restore (never naive `set_files`)

Fact 7 means the refresh path is: (a) skip `set_files` entirely when
the fetched patch set is unchanged (compare the patch-text hash the
page already holds — most refreshes are no-ops, consistent with
docs/0052's no-autorefresh stance); (b) when it *did* change, capture
the list offset, call `set_files`, restore the offset, and re-apply
file-collapse state (`set_file_collapsed` — `collapsed` survives
`set_files` only for still-present paths, fact 7). A probe test
against the pinned checkout asserts: same-patch refresh is a no-op and
changed-patch refresh keeps the viewport within one viewport-height of
its pre-refresh anchor — the docs/0041 §10 bar. If upstream later ships
an anchor-preserving `set_files` (the `set_mode` pattern), we take it
and delete the shim; that's a follow-up issue, not this PR's scope.

### D5 — Review threads become `DiffAnnotation`s

`thread_placement`'s (path, side, source line) triple maps directly to
`DiffLinePosition`; threads render through `render_annotation` with the
existing conversation card body, and `on_add_annotation` +
`set_selected_lines` give the future "comment on lines" flow its hook
for free. `flatten_diff_items`' remaining callers shrink accordingly.
The pr_page conversation read path keeps its GQL data — only the
placement-into-rows logic changes.

### D6 — Presentation: defaults first, Unified at ship

Unified mode (upstream default), line numbers on, soft wrap off
(docs/0016's horizontal band stands), upstream default separator /
indicator / hover styling — we adopt the component's look instead of
re-decorating it (docs/references restraint: compose with the anchor,
don't re-theme it). Hunk separators and indicators have knobs if the
owner wants a different flavor after human review. Split view stays a
follow-up (#203) — free capability, deliberately unshipped here to keep
the migration PR reviewable.

### D7 — Dependency & docs shape

Lockfile delta: `+ tree-sitter`, `+ 35 grammar crates` (+ their transitive
C deps); `− syntect` (and, if nothing else holds it, the fancy-regex
path with it — verify at impl). Docs: this doc; dated amendments to
docs/0016 (D8 renderer facts), docs/0018 (diff-body section), and
superseded-by notes on docs/0040 + docs/0041 — all in the same PR as
the implementation (docs/0000 lifecycle rule). docs/0029 roadmap rows
for split diff stay open (#203).

## 5. Design

### 5.1 Target shape

- `files.rs`: `diff_pane` becomes a thin construction — build
  `Vec<DiffFile>` via `DiffFile::parse` per file (or feed the raw patch
  set), hold one `Entity<DiffState>` on the page, render
  `Diff::new(&state)` inside the existing `resizable_panel()` seam
  (docs/0069), with `.annotations(thread_annotations(...))` and
  `.render_annotation(...)` rendering the conversation card bodies.
  File-card headers move into `.render_header*` hooks (checks,
  +/- badges, view-file button) — same information, upstream chrome.
- `service.rs`: patch model + anchor walk unchanged; gains a
  patch→`DiffFile::parse` passthrough (no behavior).
- Deleted: `syntax.rs`, the 0041 list/flatten/row render block,
  the syntect workspace + module deps.
- A `#[gpui::test]` port of
  `diff_rows_keep_numbers_on_their_code_band` (pr_page.rs:7238) asserts
  the equivalent invariant through the new surface.

### 5.2 Interaction with existing features

- **0069 resizable seam**: unchanged — `Diff` is the pane's child.
- **0052 no-autorefresh**: unchanged; D4's gate adds a second no-op
  guard under it.
- **0017 markdown**: untouched (Out of scope).
- **0066 workflow approval / comment flows**: `on_add_annotation` is
  reserved as the hook; no comment-UI ships in this arc.

## 6. Testing

- Port the row-invariant probe (5.1) against `DiffState`.
- D4 probe: no-op refresh; changed-patch refresh keeps viewport (§7).
- Annotation mapping: threads at first/last hunk line, both sides,
  renamed files (`original_path`), collapsed files (reveal on
  `scroll_to_line` — fact 4).
- Syntax spot-checks: rust/ts/python rows get spans; unknown extensions
  render plain (upstream fallback); > `syntax_max_line_length` rows
  degrade gracefully (default 1,000 bytes, state.rs:419).
- Full gates + `#125`-style re-verification of every doc fact quoted
  above against the pinned checkout at implementation time.

## 7. Acceptance criteria

- Files-tab diff body renders through `Diff`; `syntax.rs` and the 0041
  render machinery are gone; `cargo tree` shows `tree-sitter` in,
  `syntect` out.
- docs/0041 §7/§10 viewport contract still passes (D4 probe).
- Light and dark 0025 themes both read correctly in the diff body
  (syntax + selection + diff tints) — human-checked.
- Big-PR scroll stays smooth: no frame > the docs/0041 §10 table's
  budget on the reference PR.
- All gates green; 0065 `audit` + `tree --tab-stops` receipts on the
  Linux box; before/after `critique` on the Files pane (D7 cap: 3
  iterations, findings listed, never silent).

## 8. Human checks (owner)

- [ ] Diff body looks right in both themes; inline word-diff off by
      default reads cleanly (we ship `inline_unit` off at first — same
      restraint as D6 — flip-on is a settings follow-up if wanted).
- [ ] Review threads land on the correct lines; adding a comment flow
      (existing) still anchors correctly after refresh.
- [ ] Scroll position survives a manual refresh (D4's visible promise).
- [ ] Long lines still scroll horizontally; no wrap regressions.
- [ ] Renamed/binary/mode-change files render sensibly.

## 9. Follow-ups (GitHub issues, not this PR)

- Split-view toggle (#203, `deferred`) — capability ships free with D6.
- Upstream issue/patch: anchor-preserving `set_files` (the `set_mode`
  pattern) — once upstream ships it, delete the D4 shim and take it.
- Inline word-diff opt-in setting (`DiffInlineUnit`), if wanted.
- `on_add_annotation` → line-comment composer (0066 adjacency).

## 10. Shipped

Implemented in two PRs: part 1 (#520) enabled the `tree-sitter` /
`tree-sitter-languages` features (build-only); part 2 (#516) swapped the
renderer. Deviations from this doc, recorded at implementation:

- **D6 correction:** upstream's default `inline_unit` is `Some(Word)`,
  not off — the state pins it off explicitly at construction
  (`with_inline_unit(None)`).
- **Composer placement:** the row composer is pane-level (under the
  Diff), not an inline annotation — its textarea grows while typing, and
  upstream has no public re-measure for a growing annotation. Thread
  card height changes the annotation *set* can't see (reply boxes,
  resolve picker, stray toggle) go through `bump_diff_annotations`,
  which re-suffixes the annotation ids so the set differs and upstream
  re-measures; focus stays on the editor entity.
- **Probe swap:** three of our probes retired with the DOM they measured —
  the `#413` row-band probe, the 0041 §6 viewport-perf probe (the Diff is
  upstream's virtualized `list`; its kit suite carries the perf burden),
  and the `#413` pinned-gutter x-scroll probe (upstream owns horizontal
  scroll). Replaced by `parse_pr_file_maps_the_rest_patch` (mapping) and
  `sync_diff_noops_on_unchanged_patch_and_swaps_on_change` (the D4
  no-op/swap contract, via a test-only swap counter).
- **Renames:** our `PrFile` never deserialized `previous_filename`, so a
  renamed file parses with the new path on both sides (display path
  right; old-side numbers are the new file's) — follow-up issue filed.
- `DiffState::set_files` retains collapse state for surviving paths on
  its own, so D4's restore is the scroll-back only.

Probe results and receipts: see the part-2 PR description.
