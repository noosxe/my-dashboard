# 0040 — GitHub Module: Syntax-Highlighted Diffs (Files tab)

- **References:** #202 ("Syntax-highlighted diffs in the Files tab"),
  docs/0029 §3/§7 (roadmap rows), docs/0016 D10 (diff tints), docs/0018
  (Files sidebar/diff body move), docs/0017 D2 (fence info not captured —
  markdown highlighting is *out* of scope here), docs/0025 (file-based
  color themes), docs/0002 D5/D9/D10 + `clippy.toml` (token discipline).
- **Status:** design — implementation tracked in the issue(s) filed from
  this doc.
- **Date:** 2026-09-17. Upstream facts verified against the pinned
  gpui-pre 0.3.3 source and syntect 5.3.0 (crates.io) on this date.

## 1. Context & problem

The Files tab renders unified diffs as plain monospace text: every
Context/Add/Del row is one `div` child string, colored per-row by the
0016 D10 scheme (row background tint at low opacity, saturated sign
glyph). There is no per-token colorization, which makes reading code
changes markedly harder than on github.com and is the last big gap
before the app can carry a non-trivial review on its own (docs/0029:
"Cannot serve as a code-review tool" — this row is Tier 3, Medium).

The rendering machinery for spans already exists and is proven in this
codebase: markdown bodies build `StyledText::with_highlights(Vec<(Range,
HighlightStyle)>)` (docs/0017 D3). What is missing is a *tokenizer* that
maps diff line text to spans, and a *color policy* that stays inside the
house token discipline.

## 2. Scope

- **In:** the PR Files tab diff body (`files.rs::diff_body` — the
  `anchor_patch` rows): Context, Add and Del line *content* gets
  syntax-highlighted spans; Hunk headers and gutter cells stay as they
  are.
- **In:** language detection from the file's name (extension + special
  filenames), per file.
- **Out:** markdown code-fence highlighting (docs/0017 D2 note stands;
  it would reuse this module later but is a separate arc).
- **Out:** check-run log pane highlighting (docs/0039 D4 text styling
  stays; same future-reuse note).
- **Out:** split diff view (#203), editor-grade features (folding,
  diagnostics, incremental re-lexing of edits) — read-only coloring
  only.
- **Out:** user-configurable syntax palettes; colors derive from the
  active theme's tokens (D3).

## 3. Verified facts

All checked against the pinned `gpui-pre 0.3.3` sources and the
syntect 5.3.0 crate source on 2026-09-17.

1. **gpui `HighlightStyle` (pinned rev)** carries: `color: Option<Hsla>`,
   `font_weight`, `font_style`, `background_color`, `underline`,
   `strikethrough`, `fade_out`. There is **no `font_family`** field —
   the mono font must come from the row container (markdown.rs D3 note
   records the same constraint).
2. **gpui-component `ThemeColor`** (the `cx.theme()` token set,
   shadcn-style) has **no syntax tokens** (no keyword/string/comment
   entries — verified against `theme/theme_color.rs` in the pinned rev).
   Any highlighting palette MUST therefore be derived from existing
   tokens (D3) — the alternative (extending the schema with syntax
   fields) would fork us from upstream's theme format and break
   docs/0025 file compatibility for zero gain.
3. **syntect 5.3.0** (current crates.io release):
   - Default feature `default-onig` pulls the C Oniguruma bindings;
     the **`default-fancy`** feature set is the pure-Rust equivalent
     (`regex-fancy` via fancy-regex) — no C toolchain dependency,
     same syntax/theme packs.
   - `SyntaxSet::load_defaults_newlines()` loads the ~60 bundled
     Sublime syntaxes from an embedded binary dump (feature
     `dump-load`; the newlines pack is the recommended variant when
     lines carry their terminator).
   - `find_syntax_by_extension("rs")` / `find_syntax_by_token("Makefile")`
     / `find_syntax_plain_text()` do the language lookup.
   - **Theme-independent tokenization exists:** `ParseState::new(syntax)`
     → `parse_line(line, &syntax_set)` → `Vec<(usize, ScopeStackOp)>`,
     and `syntect::easy::ScopeRangeIterator::new(&ops, line)` walks the
     ops into `(Range<usize>, &ScopeStackOp)` regions (the consumer
     folds the ops into a running `ScopeStack` to know the scope at
     each region — this is what `HighlightIterator` does internally
     before consulting a Sublime theme). Nothing in this path requires
     a syntect `Theme`, so we never adopt `.tmTheme` palettes (D2).
   - Statefulness: `ParseState` must persist across the lines of one
     file (heredocs, multi-line strings) and be fed lines *in order*;
     `parse_line` is O(line) with an internal per-call search cache.
4. **Diff rows are already classified text** (docs/0030 D2):
   `anchor_patch(patch) -> Vec<AnchoredLine>` yields
   `PatchLine::{Hunk, Context, Add, Del}(String)` — the highlighter's
   input is the row's `String`, no re-parsing of the unified diff
   needed.
5. **Clippy gate** (`clippy.toml`): `gpui::rgb/rgba/hsla` are
   disallowed app-wide — highlight colors must be produced from
   `cx.theme()` tokens (`Hsla` values), never constructed from
   literals (D3 again; this is enforced, not aspirational).

## 4. Design decisions

### D1 — Engine: syntect, not tree-sitter

tree-sitter (the Zed ecosystem choice) is the wrong weight class for a
read-only diff viewer: it needs one C grammar crate + one highlight
*query set* per language we support, its incremental/partial parsing
superpower is worthless on immutable diff lines, and its accuracy edge
matters for editing, not for coloring a review. syntect gives ~60
languages from one dependency with a pure-Rust regex backend, no
runtime assets to manage, and a theme-independent parse API (fact 3).
The issue itself names syntect as the lightweight candidate. Rejected:
hand-rolled regex tokenizer (accuracy/maintenance for no payoff).

### D2 — No syntect themes; tokenize only

We never load or adopt a Sublime `.tmTheme` palette: colors would then
bypass the docs/0025 theme system and the token-discipline gate
(fact 5). We use syntect purely as a tokenizer (`parse_line` +
`ScopeRangeIterator`, fact 3) and map scopes to **our** tokens (D3).

### D3 — Scope→theme-token mapping, derived colors only

A small, documented table maps syntect scope prefixes onto existing
`ThemeColor` tokens; unmapped scopes render in the ambient foreground
(no highlight entry). The initial mapping (tuned against the bundled
Default Dark/Light palettes, adjusted freely at impl against real
diffs):

| Scope (prefix match, innermost first) | Theme token | Extra style |
| --- | --- | --- |
| `comment.` | `muted_foreground` | italic |
| `string.` | `warning` | — |
| `constant.numeric.` | `danger` | — |
| `constant.language.` | `accent` | — |
| `keyword.`, `storage.` | `accent` | — |
| `entity.name.function.`, `support.function.` | `link` | — |
| `entity.name.tag.`, `support.type.` | `success` | — |
| `variable.language.`, `variable.other.constant.` | `accent` | — |
| `punctuation.` | `muted_foreground` | — |
| `invalid.` | `danger` | — |

Matching walks the region's `ScopeStack` from the innermost (last)
scope outward and takes the first table hit; the row's `HighlightStyle`
gets `color` (and the italic/weight column where shown). Everything
else — identifiers, whitespace, plaintext files — stays untouched.
The table is the single place to tune; rationale for each row lives
next to it in code. `font_family` is deliberately absent (fact 1);
weight is limited to `FontWeight::BOLD` where a token needs emphasis.

### D4 — Language detection: name-based, plain-text fallback

Per file, resolve the syntax once: exact filename first
(`find_syntax_by_token`: `Makefile`, `Dockerfile`, `CMakeLists.txt`),
then extension (`find_syntax_by_extension`: `rs`, `ts`, `py`, …).
No hit → `find_syntax_plain_text()` → the whole file renders exactly
as today (zero highlight entries). We do NOT sniff content (shebangs,
embargo lists) v1; wrong-extension diff blobs (`.patch`, `.lock` files)
render plain — acceptable for a review surface, noted in follow-ups.

### D5 — Caching: page-owned, keyed by (path, patch, theme); line cap

Highlight output is cached on `PrDetailPage` per file path, entry =
`(patch hash, theme name, Arc<Vec<RowSpans>>)`:

- **Patch hash** invalidates when the detail refetch brings new patch
  text (docs/0032 auto-refresh) — a cheap `DefaultHasher` over the
  patch string.
- **Theme name** invalidates on theme switch (colors are derived at
  build time from `cx.theme()`, so a switch must re-derive).
- The map is cleared when the page's file list is replaced.
- **Perf guard:** files with more than `SYNTAX_HIGHLIGHT_MAX_LINES =
  2_000` diff rows render un-highlighted (plain), with the cap
  recorded in code and here. Typical review files are ≤ a few hundred
  rows; syntect lexes thousands of short lines in single-digit
  milliseconds (fact 3), so highlighting runs **synchronously at
  first render** of a file's body — no background job lane, no
  loading state. If a real-world file ever makes that laggy, the cap
  is the knob, not new machinery.

`RowSpans` per row is `Vec<(Range<usize>, TokenKind)>` with `TokenKind`
an 8-variant enum mirroring D3's rows — spans are computed once, the
token→`HighlightStyle` translation (with `cx.theme()`) happens at every
render, keeping theme switches cheap and cache entries theme-blind
except for the name key.

### D6 — Rendering: `StyledText` runs inside the existing rows

`diff_row` keeps its exact layout (gutter cells, sign glyph, row tint,
hover comment button — docs/0016 D10, docs/0030 D2/D4). Only the
content `String` changes: Context/Add/Del rows build
`StyledText::new(text).with_highlights(highlights)` — the markdown.rs
D3 mechanism — with ranges offset by the sign glyph's one character
for Add/Del rows (`+text`/`−text` stay verbatim). Hunk-header rows and
the "Binary file or diff not shown" placeholder are untouched. Rows
with an empty span list render the plain string as today, byte-for-byte.

### D7 — Dependency shape: syntect 5, `default-fancy` feature set

```toml
syntect = { version = "5", default-features = false, features = [
    "parsing", "default-syntaxes", "dump-load", "regex-fancy",
] }
```

Pure-Rust regex backend (no C/Oniguruma toolchain — keeps vendor builds
and Nix closures clean), embedded default syntax pack (~60 languages),
no theme/plist/yaml loaders we wouldn't use. The exact feature set was
re-verified at impl (`cargo tree -i onig` matches nothing — fancy-regex
only). `SyntaxSet` loads once into a `OnceLock<SyntaxSet>` lazily at
first highlighted diff (the ~100 ms pack load must not run at app
start; it amortizes to zero afterwards).

**Impl measurement (2026-09-17):** release-binary delta **+846 KB**
(72,191,248 → 73,037,440 bytes) — under the ~3 MB watch threshold; no
further action. Pack-content note: the bundled pack has **no Dockerfile
grammar** (verified 2026-09-17); Dockerfile diffs render plain under
the D4 fallback until a grammar is added upstream or via a custom pack
(follow-up).

### D8 — Module layout

New `crates/modules/github/src/syntax.rs`, three pure layers so each is
unit-testable without gpui:

1. `fn resolve_syntax(path: &str) -> Option<SyntaxId>` — D4 lookup
   (None ⇒ plain).
2. `fn tokenize_row(line: &str, state: &mut ParseState, set: &SyntaxSet)
   -> Vec<(Range<usize>, TokenKind)>` — D2's parse walk + D3's scope
   matching, no colors, no gpui types.
3. `fn token_style(kind: TokenKind, theme: &ThemeColor) ->
   Option<HighlightStyle>` — D3's table, the only color-aware layer.

`files.rs` consumes 2+3 and owns the D5 cache; `service.rs` and the
fetch pipeline are untouched (highlighting is presentation, never a
fetch kind).

## 5. Design

### 5.1 Pipeline

```
detail fetch (unchanged)
  └─ files: Vec<PrFile> { filename, patch }
       └─ diff_body(page, file)
            ├─ anchor_patch(patch) → Vec<AnchoredLine>      (unchanged)
            ├─ page.highlight_cache.entry(filename)          (D5)
            │    hit (patch+theme)  → Arc<Vec<RowSpans>>
            │    miss → if rows > cap { skip } else
            │            resolve_syntax(filename)             (D4)
            │              └─ None   → plain (cache the miss)
            │            fold rows through tokenize_row       (D2/D3)
            └─ diff_row: StyledText spans + existing tint/glyph (D6)
```

`ParseState` continuity: rows arrive in file order, so one `ParseState`
threads through the fold; `\\ No newline` marker rows and hunk headers
feed the tokenizer a dummy context line (or skip feeding — either is
correct for a Sublime grammar since markers are not file text; impl
picks one and tests it).

### 5.2 Interaction with existing features

- **Auto-refresh / realtime (docs/0032, 0038):** a refetch that lands
  new patch text changes the hash → re-highlight on next render; same
  patches are free.
- **Review threads (docs/0030):** thread cards, composers and drafts
  render under rows unchanged; span ranges never affect row identity
  (`AnchoredLine` untouched).
- **Theme files (docs/0025):** nothing to add to theme JSON; both
  bundled palettes drive D3 immediately, as do user files.

## 6. Testing

- `syntax.rs` unit tests (no gpui): a Rust line (`let s = "x"; // c`)
  yields exactly the string/comment/keyword ranges; an unknown
  extension yields no syntax; the D3 table maps every `TokenKind` to a
  distinct token path; `ParseState` continuity across a two-line
  string literal produces one string span per line; the `\\ No
  newline` marker does not disturb line numbering or state.
- `files.rs` tests: cache miss→hit on second render, invalidation on
  patch change and on theme name change; rows > cap render plain
  (spans empty) but rows ≤ cap in the same file stay highlighted;
  Add-row span ranges are offset by the sign glyph (the `+` is never
  colored as code).
- Workspace gates stay green; new dependency compiles warning-free
  under the pinned toolchain (clippy runs over the workspace).

## 7. Acceptance criteria

1. A PR diff for `.rs`/`.ts`/`.py`/`.go`/`.c`/`.md` files shows
   per-token colors consistent with the active theme; added/removed
   rows keep their tint and sign glyph; context rows highlight too.
2. Unknown-extension and plaintext files render exactly as today.
3. Switching themes (docs/0025) recolors highlighted diffs on the next
   frame with no stale colors.
4. A >2,000-row diff file renders un-highlighted and does not stall
   the UI when scrolled into view.
5. No literal colors anywhere new (clippy gate); fonts come from the
   row container only.
6. Binary size delta from syntect recorded in the impl PR (§7 D7).

## 8. Human checks (owner)

- Eyeball a familiar PR: colors read like an editor, not like a
  Christmas tree; diff semantics (add/remove/hunk) still unmistakable.
- Flip light/dark theme mid-session; highlighted diffs follow.
- Scroll a large diff; no perceptible hitch on first expand.
- A plaintext-heavy PR (lockfiles, YAML with odd extensions) looks
  unchanged.

## 9. Follow-ups (GitHub issues, not this PR)

- Content sniffing for extension-less/odd blobs (shebang → language).
- Markdown fence highlighting reusing `syntax.rs` (lifts docs/0017 D2).
- Check-run log pane highlighting (docs/0039 D4 reuse) — likely with a
  dedicated low-chroma mapping since logs are not source code.
- Per-language syntax pack trimming (custom `dump-create` pack) if the
  binary delta ever matters.

## 10. Shipped (2026-09-17, PR #252 — issue #202/#251)

Implemented in PR #252 as designed, with two recorded facts (both in
§4): release-binary delta **+846 KB** (under the D7 watch threshold)
and the default pack's **missing Dockerfile grammar** (D4 fallback
renders such diffs plain). One deviation-in-the-letter, none in
spirit: the D5 theme invalidation key is the Debug rendering of
exactly the tokens the D3 palette reads (robust for any mode switch,
including System) rather than the theme's mode/name pair.
