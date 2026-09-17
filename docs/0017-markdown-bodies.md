# 0017 — GitHub module: markdown rendering for PR bodies

## 1. Context & problem

The PR detail page (0016) renders the description, comment, and review bodies
as raw markdown text. Real PR bodies are markdown: Dependabot bodies carry
lists and links, CI bots post tables, humans use everything else. The owner
asks for rendered markdown; edit mode keeps plain-text editing (that part is
expected — it is GitHub's own model: rendered view, raw source editor).

## 2. Scope

**In:**

- Rendered markdown for the timeline bodies — description, comments, and
  reviews alike. They share one seam (`body_card`), and GitHub renders
  markdown in all three; rendering only the description would make comments
  look broken next to it.
- GFM subset: headings, emphasis/strong/strikethrough, inline code, fenced
  code blocks, ordered/unordered/task lists, blockquotes, tables, thematic
  breaks, links (clickable, external browser), autolinks.
- Hard breaks and soft breaks (soft break → space, hard break → newline).

**Out (file later if wanted):**

- Image fetching — `![](url)` renders as a link labeled with its alt text.
  Network image loading + caching is its own feature.
- Syntax highlighting inside code blocks.
- A preview toggle in the description editor (edit stays the raw textarea).
- Markdown in composer inputs or repo-view/notification one-liners (single
  line, plain text stays).

## 3. Verified facts

**Upstream (pinned checkouts, verified 2026-09-04):**

- gpui-component @ `0c746df` ships **no** markdown/HTML element (`crates/ui/
  src` listing checked — nothing matching markdown|html|rich).
- zed's own `markdown` crate (same git source as gpui) depends on zed
  workspace internals (`language`, `theme`, `settings`, `sum_tree`,
  `mermaid_render`, …) — not extractable into our tree (its `Cargo.toml`
  at the pinned rev).
- `pulldown-cmark` **0.13** (zed pins `0.13.0`, `default-features = false`)
  is the parser: CommonMark plus opt-in GFM via
  `Options::ENABLE_TABLES | ENABLE_STRIKETHROUGH | ENABLE_TASKLISTS`;
  event-stream model (`Start/End/Text/Code/Html/InlineHtml/SoftBreak/
  HardBreak/Rule/TaskListMarker`).
- gpui @ pinned rev has **`InteractiveText`** (`crates/gpui/src/elements/
  text.rs:981`): `InteractiveText::new(id, StyledText)
  .on_click(Vec<Range<usize>>, Fn(usize, &mut Window, &mut App))` plus
  `on_hover`/`tooltip`. It maps mouse positions to character indices, so
  clickable link runs inside a wrapping paragraph need no per-run elements.
  Styled runs come from `StyledText` + `HighlightStyle` (color, underline,
  font weight/family).
- `cx.open_url(&url)` is the established external-link path (used by
  `pr-open-external`, pr_page.rs:723).

**Our tree (`main` @ `ae006e1`):**

- `body_card` (pr_page.rs) is the single render seam for all three body
  kinds; bodies arrive as raw `&str` on `PrDetail` (0016 D2) — no schema or
  service changes needed.
- Theme has the mono token and semantic colors (0016 D10); clippy.toml
  bans literal colors and ad-hoc text sizes/line heights.
- The bridge dumps interactive nodes only (0010): markdown text is invisible
  to `tree`; verification relies on geometry, logs, and the owner's eyes.

## 4. Owner decisions (requested 2026-09-04)

- Render markdown on the PR description (this doc's reason to exist).
- Edit mode stays plain-text markdown editing (confirmed in the request).

Everything else in §5 is proposal — settled by the review of this doc PR.

## 5. Design

### D1 — Parser: pulldown-cmark 0.13, GFM flags, no HTML

`markdown_blocks(body: &str) -> Vec<Block>` — pure fn over
`Parser::new_ext(body, Options::ENABLE_TABLES | ENABLE_STRIKETHROUGH |
ENABLE_TASKLISTS | ENABLE_GFM)` (options verified on docs.rs 0.13.4;
`ENABLE_GFM` carries the GitHub blockquote alerts — `[!NOTE]`,
`[!TIP]`, `[!IMPORTANT]`, `[!WARNING]`, `[!CAUTION]`). `Html`/
`InlineHtml` events are **dropped** (no raw-HTML rendering surface, no
sanitizer to maintain). Footnotes stay disabled for now (rare in PRs;
the flag exists if wanted later).

**GFM autolink literals** (bare `https://…` / `www.` URLs) are the one
GFM-spec extension pulldown does **not** parse — GitHub links them
automatically and bot comments are full of them. Post-process plain-text
runs with the `linkify` crate (the same pairing zed's markdown crate
uses) and emit link runs; visible text stays as written. The pass emits
exactly one run per matched span and always advances its cursor past
it — `LinkKind` is `#[non_exhaustive]` upstream (0.11: `Url`/`Email`
only), so unknown future kinds render verbatim as plain text, neither
duplicated nor dropped (#169); the kind→URL mapping lives in a free
fn (`autolink_target`) unit-tested without `LinkFinder`.

**GitHub platform expansions are out of scope** (server-side
post-processing, not markdown — no parser provides them; they render as
plain text): `@mentions`, `#123` / `org/repo#123` issue references, SHA
links, `:emoji:` shortcodes, `$$math$$`, mermaid diagrams,
`<details>` collapsibles (dropped with all raw HTML).

### D2 — Owned intermediate model

Events borrow the input and arrive flat; the pure fn folds them into an
owned tree the renderer can hold across frames:

```rust
enum Block {
    Paragraph(Vec<Run>),
    Heading { level: u8, runs: Vec<Run> },
    Code { info: Option<String>, text: String },
    Quote(Vec<Block>),
    List { ordered: bool, start: u64, items: Vec<ListItem> },
    Table { align: Vec<Align>, head: Vec<Vec<Run>>, rows: Vec<Vec<Vec<Run>>> },
    Rule,
}
struct Run { text: String, style: RunStyle }   // strong/em/strike/code/link(url)
```

Pure and table-testable without a window (doc/0016 stage pattern).

### D3 — Inline runs: one `StyledText` per paragraph, links via `InteractiveText`

Each paragraph/heading/table cell becomes a single text node: flat string +
`HighlightStyle` runs (strong → bold weight, em → italic, strike →
strikethrough, code → mono token). When the block carries links, wrap in
`InteractiveText` with one range per link; the click listener looks up the
URL by range index and calls `cx.open_url` (D5). Link styling: accent color
+ underline via `HighlightStyle`. **Shipped correction (2026-09-04, dark-theme
link fix):** "accent" was the wrong token — `accent` is an accent-*background*
token (dark: neutral-800), which reads as invisible against the card surface.
Links now use the dedicated `link` token (dark: #fafafa, light: near-black,
both underlined) per `ThemeColor::link`/`link_hover`; hover restyle would
need mouse tracking on `InteractiveText` — not wired v1.
links — plain `StyledText` (cheaper, no state).

### D4 — Block layout (theme tokens only)

- Stack: `v_flex().gap_2()` of blocks.
- Headings: `text_lg`/`text_base`/`text_sm`/`text_xs` by level with
  semibold weight, first block tighter spacing.
- Code block: one multiline mono `text_xs` node on a muted surface
  (theme surface token + border), soft-wrapped (v1; per-line + horizontal
  scroll is polish, revisit if real bodies demand it).
- Blockquote: left border (theme border) + muted foreground, nested blocks.
- Lists: per-item flex row — marker column (`•`/`1.`/task `☑`/`☐` glyphs in
  theme foreground) + nested blocks; task markers read-only.
- Tables: header row semibold on muted surface, body rows beneath, single
  `border` color between rows; wide tables wrap cells (no horizontal scroll
  v1 — CI tables are usually 3–5 columns).
- `Rule`: the existing divider affordance.

- Alerts (`ENABLE_GFM` blockquote tags): rendered as a quote variant —
  recognized tag label as a bold first line (`Note`, `Warning`, …), left
  border tinted by the matching semantic token where one exists
  (WARNING → warning, CAUTION → danger, others → accent); unknown tags
  fall back to a plain quote.
### D5 — Links

Click → `cx.open_url(url)` — identical to `pr-open-external`; no tooltip,
no hover preview v1. Autolinks (`<https://…>`, bare URLs marked by
pulldown) take the same path.

### D6 — Parse once per body version

Bodies are immutable per fetch; parsing on every frame (gpui re-renders)
would waste the most expensive step. Memoize at the card level with gpui
element state (`with_element_state` keyed by the card id): store
`(body_hash: u64, Vec<Block>)`, re-parse only when the hash differs.
Refetch naturally invalidates (new body → new hash). Service stays pure —
no state plumbing through `GithubState`.

### D7 — Compliance (0016 D10 rules carry over)

Semantic theme tokens only; mono token for code; the clippy token-discipline
lints (no literal colors, no ad-hoc `text_size`/`line_height`). Interactive
elements get ids: each `InteractiveText` is `{card-id}-p-{n}`; links are
ranges inside it, not separate nodes — the bridge will not show them
(recorded limitation, D-verification below).

### D8 — Wiring & files

- New `crates/modules/github/src/markdown.rs`: model + parser + renderer +
  unit tests (pure, no window).
- `body_card` swaps its raw-text child for `render_markdown(...)`;
  everything else in pr_page untouched.
- Workspace `Cargo.toml`: `pulldown-cmark = "0.13"` (`default-features =
  false`) as a workspace dependency; github module picks it up.
- No core, service, schema, or doc-number changes. Docs amendments on
  landing: 0016 D4 (bodies render markdown) + README index row (written
  with this doc).

### D9 — Testing

- `markdown.rs` unit tests (pure): heading levels, nested lists, task
  markers, ordered start index, table shape + alignment capture, link URL
  extraction, autolink, soft vs hard break, HTML dropped, code fence info
  string; alert tags (recognized, unknown); bare-URL runs through the
  linkify pass; autolink round-trip (every input byte survives exactly
  once, mixed links/email/plain) and the kind→URL mapping (#169).
- Run-style mapping tests (strong/em/strike/code/link → `RunStyle`).
- Renderer smoke via the existing `#[gpui::test]` pattern where cheap;
  geometry/interaction is bridge + owner territory (below).

### D10 — Bridge verification & owner checks

- Agent (bridge): page renders a rich body (#55 Dependabot: lists + links)
  with sane geometry, no panic, description card taller than raw text but
  inside the scroll region (0016 layout fix holds); tab round-trips fine.
- Owner (human, PR checklist): links open the browser; tables/code/quotes/
  task lists look right on a body of their choosing; edit → save round-trip
  re-renders; a comment they post renders.

## 6. Implementation plan

Single stage, one issue + one PR (bounded: one new module file, one seam,
no schema/service work):

1. `markdown.rs` (model, parser, renderer, unit tests).
2. `body_card` wiring + element-state memo.
3. Gates + bridge verification; docs amendments (0016 D4 note) ride along.

**Shipped (2026-09-04, issue #104).** Deviations from the sketch above,
all recorded here so the doc stays the source of truth:

- **D6 mechanism:** the memo is a page-level `BTreeMap<card-id,
  Arc<[Block]>>` rebuilt in `build_md_cache` every time a detail payload
  lands — not gpui element state (that API is paint-phase-only, unusable
  from `render`). The invariant holds: bodies parse once per detail
  payload, never per frame; the service stays pure.
- **D2:** the table alignment vector is not carried (cells render
  left-aligned v1) and the code-fence info string is not captured (no
  highlighting to feed it to).
- **D3:** `HighlightStyle` has no `font_family` in the pinned gpui rev —
  inline code renders with a muted background tint only; code *blocks*
  stay mono via the container.
- **D1:** linkify 0.10 does not match scheme-less `www.` hosts (GitHub
  does) — http(s):// URLs and emails autolink, bare `www.` stays plain
  text until a dedicated scanner is justified.
- Verified live (worker-mcp#55): markdown paragraphs appear as
  `InteractiveText` nodes in the bridge dump, rail pinned (0016 layout
  fix holds), Files↔Conversation round-trip stable, drift 0, no panics.

## 7. Testing

Gates as always (`fmt`, clippy zero warnings — token lints included —
`cargo test --workspace`). New unit tests per D9. No live mutation needed:
bodies are read-only data already fetched.

## 8. Amendment (2026-09-17, #195): GitHub cross-reference autolinks

The D1 autolink pass gains a second detector: GitHub cross-references
in plain text — `owner/repo#123`, bare `#123`, `@user` — become link
runs through the *same* machinery as D1 (`RunStyle::link` →
`InteractiveText` → `open_url`); render-side is untouched.

- **Scanner, not regex**: a hand-rolled `xref_spans` walk (no new
  dependency). Boundary rules keep it deterministic and out of other
  syntax: a match must start on a non-word boundary (`#` may not follow
  a word char or `&` — decoded HTML entities; `@` may not follow a word
  char — emails; an owner run may not follow a word char or `/` —
  path tails like `a/b/c#1` stay plain); the digits/name run must end on
  a boundary (`#123abc` stays plain); `@`-names are greedy over
  `[A-Za-z0-9-]` with trailing hyphens trimmed (usernames can't end in
  one).
- **Linkify wins overlaps**: both span sources merge into one ascending
  list; any cross-ref starting inside a linkify span is dropped —
  ref-text inside a URL or email belongs to that link.
- **Ambient repo**: bare `#123` is ambiguous without a repo. New
  `parse_markdown_in_repo(body, repo_key)` (`parse_markdown` delegates
  with `None`) — every detail page passes its `owner/repo` (the pages'
  `repo` field; the new-issue preview its target repo). Without it, bare
  refs stay plain and the self-scoped forms still link.
- **Targets**: issues/PRs open `github.com/{owner}/{repo}/issues/{n}`
  (GitHub redirects across the issue/PR split); mentions open
  `github.com/{user}`.
- **Not in-app navigation**: links open the browser like every other
  markdown link v1 — routing refs into in-app pages needs a callback
  plumbed through every markdown consumer; filed as a follow-up.
- **Code and existing links excluded**: code spans/blocks never reach
  `push_text`; runs inside markdown links short-circuit as before.
