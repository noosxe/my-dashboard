//! Diff-row syntax highlighting (docs/0040) — syntect as a
//! *theme-independent tokenizer*.
//!
//! Three pure layers (0040 D8): [`resolve_syntax`] maps a file path to a
//! grammar (D4), [`tokenize_row`] runs one row through the parser and
//! classifies each span (D2), and [`token_style`] translates a token
//! kind into the theme's colors (D3 — the only color-aware layer).
//! Nothing here ever loads a syntect `.tmTheme`: colors must stay in
//! the house token discipline (0040 D2, clippy gate).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use gpui::{FontStyle, HighlightStyle, SharedString};
use gpui_component::ThemeColor;
use syntect::easy::ScopeRangeIterator;
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

use crate::service::{AnchoredLine, PatchLine};

/// The bundled Sublime syntax pack (~60 languages, 0040 D7). The ~100 ms
/// pack load must not run at app start — it amortizes to zero on first
/// highlighted diff instead.
pub(crate) fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Layer 1 (0040 D4): file path → grammar. Exact filename first
/// (`Makefile`, `Dockerfile`), then extension (`rs`, `ts`); no hit ⇒
/// `None` ⇒ the file renders exactly as it did before this module —
/// plain text, zero highlight spans.
pub(crate) fn resolve_syntax(path: &str) -> Option<&'static SyntaxReference> {
    let set = syntax_set();
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    if let Some(syntax) = set.find_syntax_by_token(name) {
        return Some(syntax);
    }
    // A dotless name (`Makefile`) must not be re-tried as an extension;
    // only split off a real extension when a dot exists.
    let (_, ext) = name.rsplit_once('.')?;
    set.find_syntax_by_extension(ext)
}

/// The color classes a diff span can carry (0040 D3's table, materialized).
/// Grammar scopes fold into one of these; anything unmapped renders in
/// the ambient foreground (no highlight entry at all).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TokenKind {
    Comment,
    StringLit,
    Number,
    Keyword,
    Function,
    Type,
    Invalid,
}

/// Layer 2 (0040 D2): tokenize one row. `state` must persist across the
/// rows of one file, in order (multi-line strings, heredocs); rows that
/// are not file text (hunk headers, `\ No newline` markers) are never
/// fed here — callers pass empty spans for them instead. The trailing
/// newline is appended for the parse only: the bundled pack is the
/// newlines variant (line-comment regexes end at `\n`, so without it a
/// comment would leak into every following row).
pub(crate) fn tokenize_row(line: &str, state: &mut ParseState) -> Vec<(Range<usize>, TokenKind)> {
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    let ops = match state.parse_line(&buf, syntax_set()) {
        Ok(ops) => ops,
        Err(_) => return Vec::new(), // tolerant: render the row plain
    };
    let mut out: Vec<(Range<usize>, TokenKind)> = Vec::new();
    let mut stack = ScopeStack::new();
    // ScopeRangeIterator yields (region, op-that-governs-it): fold the
    // op into the running stack BEFORE classifying the region. Regions
    // past the caller's text (the appended newline and empty op gaps)
    // are applied but not emitted.
    for (range, op) in ScopeRangeIterator::new(&ops, &buf) {
        let _ = stack.apply(op);
        if range.start >= line.len() || range.is_empty() {
            continue;
        }
        let end = range.end.min(line.len());
        if let Some(kind) = classify(&stack) {
            // Merge into the previous span when contiguous and same
            // kind — grammar boundaries (e.g. the string-opening quote's
            // punctuation.definition scope falling back to the string
            // scope) would otherwise emit runs that only cost paint.
            if let Some((last, last_kind)) = out.last_mut() {
                if last.end == range.start && *last_kind == kind {
                    last.end = end;
                    continue;
                }
            }
            out.push((range.start..end, kind));
        }
    }
    out
}

/// The 0040 D3 table: walk the span's scope stack innermost-first and
/// take the first prefix hit. Order matters — `constant.numeric` before
/// the broader families. Unmatched ⇒ `None` (ambient foreground).
fn classify(stack: &ScopeStack) -> Option<TokenKind> {
    use TokenKind::*;
    for scope in stack.as_slice().iter().rev() {
        let s = scope.build_string();
        if s.starts_with("comment") {
            return Some(Comment);
        }
        if s.starts_with("string") {
            return Some(StringLit);
        }
        if s.starts_with("constant.numeric") {
            return Some(Number);
        }
        if s.starts_with("constant.language") {
            return Some(Keyword);
        }
        if s.starts_with("keyword.operator") {
            // Operators stay ambient: coloring every `=`, `.`, `::` was
            // the single biggest decoration-run churn in real diffs
            // (~2x the runs — scroll cost) and reads louder than
            // github.com. See docs/0040 §4 D3.
            return None;
        }
        if s.starts_with("keyword") || s.starts_with("storage") {
            return Some(Keyword);
        }
        if s.starts_with("entity.name.function") || s.starts_with("support.function") {
            return Some(Function);
        }
        if s.starts_with("entity.name.tag") || s.starts_with("support.type") {
            return Some(Type);
        }
        if s.starts_with("variable.language") || s.starts_with("variable.other.constant") {
            return Some(Keyword);
        }
        // No `punctuation` arm: delimiters render ambient (as on
        // github.com); coloring them muted tripled the per-line
        // decoration runs for no readability gain (docs/0040 §4 D3).
        if s.starts_with("invalid") {
            return Some(Invalid);
        }
    }
    None
}

/// Layer 3 (0040 D3): token → `HighlightStyle`, derived from the active
/// theme's tokens only (no literal colors — the clippy gate). This is
/// the single place to re-tune the palette.
pub(crate) fn token_style(kind: TokenKind, theme: &ThemeColor) -> HighlightStyle {
    use TokenKind::*;
    let mut h = HighlightStyle::default();
    match kind {
        Comment => {
            h.color = Some(theme.muted_foreground);
            h.font_style = Some(FontStyle::Italic);
        }
        StringLit => h.color = Some(theme.warning),
        Number | Invalid => h.color = Some(theme.danger),
        Keyword => h.color = Some(theme.accent),
        Function => h.color = Some(theme.link),
        Type => h.color = Some(theme.success),
    }
    h
}

/// One file's per-row spans, aligned 1:1 with `anchor_patch` indices.
pub(crate) type RowSpans = Vec<(Range<usize>, TokenKind)>;

/// Per-file memo entry (0040 D5): `None` rows is a cached miss (no
/// grammar, or over the cap) — still a hit, so a miss costs once.
#[derive(Clone)]
pub(crate) struct HighlightEntry {
    patch_hash: u64,
    theme_key: SharedString,
    rows: Option<Arc<Vec<RowSpans>>>,
}

/// Where the D5 memo lives. A `RefCell` because it fills from inside
/// render (the diff body is drawn while the page entity is already
/// borrowed); it is a pure memo — never warrants `cx.notify()`.
#[derive(Default)]
pub(crate) struct HighlightCache(RefCell<BTreeMap<String, HighlightEntry>>);

impl HighlightCache {
    /// Span list for one file, computing on miss. Keyed by path;
    /// invalidated by patch text or palette change via the entry fields.
    pub(crate) fn spans_for(
        &self,
        path: &str,
        patch: &str,
        rows: &[AnchoredLine],
        theme_key: SharedString,
    ) -> Option<Arc<Vec<RowSpans>>> {
        let patch_hash = hash_patch(patch);
        let mut cache = self.0.borrow_mut();
        if let Some(entry) = cache.get(path) {
            if entry.patch_hash == patch_hash && entry.theme_key == theme_key {
                return entry.rows.clone();
            }
        }
        let computed = compute_rows(path, rows);
        cache.insert(
            path.to_string(),
            HighlightEntry {
                patch_hash,
                theme_key,
                rows: computed.clone(),
            },
        );
        computed
    }

    /// Drop every entry — the page's file list was replaced (0040 D5).
    pub(crate) fn clear(&self) {
        self.0.borrow_mut().clear();
    }
}

/// FNV-1a over the patch text — deterministic for the page's lifetime,
/// which is all the memo's invalidation needs (0040 D5).
fn hash_patch(patch: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in patch.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Fold a file's classified rows into per-row spans (0040 D5/D2): one
/// `ParseState` threads through the file-text rows in order; hunk
/// headers and `\ No newline` markers get empty spans and are never fed
/// to the parser. Over the cap, or no grammar for the path ⇒ `None`
/// (plain rendering, 0040 D5).
fn compute_rows(path: &str, rows: &[AnchoredLine]) -> Option<Arc<Vec<RowSpans>>> {
    if rows.len() > SYNTAX_HIGHLIGHT_MAX_LINES {
        return None;
    }
    let syntax = resolve_syntax(path)?;
    let mut state = ParseState::new(syntax);
    let spans: Vec<RowSpans> = rows
        .iter()
        .map(|anchored| match &anchored.line {
            PatchLine::Hunk(_) => Vec::new(),
            PatchLine::Context(text) => {
                // `\ No newline` markers arrive as context rows (the
                // anchor walk's convention) — not file text.
                if text.starts_with('\\') {
                    Vec::new()
                } else {
                    tokenize_row(text, &mut state)
                }
            }
            PatchLine::Add(text) => tokenize_row(text, &mut state),
            PatchLine::Del(text) => tokenize_row(text, &mut state),
        })
        .collect();
    Some(Arc::new(spans))
}

/// Files above this row count render plain (0040 D5) — the perf knob.
pub(crate) const SYNTAX_HIGHLIGHT_MAX_LINES: usize = 2_000;

/// The cache key for "which palette derived these colors": the resolved
/// values of exactly the tokens [`token_style`] reads. Any theme switch
/// that changes one of them changes the key, and every entry re-derives
/// on next render — mode/name plumbing unnecessary (0040 D5).
pub(crate) fn theme_key(theme: &ThemeColor) -> SharedString {
    format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
        theme.muted_foreground,
        theme.warning,
        theme.danger,
        theme.accent,
        theme.link,
        theme.success,
        theme.foreground,
        theme.background,
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::anchor_patch;

    fn spans_of<'a>(
        line: &'a str,
        spans: &[(Range<usize>, TokenKind)],
        kind: TokenKind,
    ) -> Vec<&'a str> {
        spans
            .iter()
            .filter(|(_, k)| *k == kind)
            .map(|(r, _)| &line[r.clone()])
            .collect()
    }

    fn tokenize(path: &str, line: &str) -> Vec<(Range<usize>, TokenKind)> {
        let syntax = resolve_syntax(path).expect("test path must resolve");
        tokenize_row(line, &mut ParseState::new(syntax))
    }

    #[test]
    fn rust_line_gets_string_comment_keyword_spans() {
        let line = r#"let s = "x"; // tail"#;
        let spans = tokenize("main.rs", line);
        // Operators stay ambient (D3 trim); the keyword itself colors.
        assert!(spans_of(line, &spans, TokenKind::Keyword).contains(&"let"));
        // Quote glyphs fall back to the string scope and merge with the
        // content; the `;` and `=` render ambient (no span).
        assert_eq!(spans_of(line, &spans, TokenKind::StringLit), vec!["\"x\""]);
        assert_eq!(spans_of(line, &spans, TokenKind::Comment), vec!["// tail"]);
        // The `=` and `;` are ambient (operators/delimiters render
        // uncolored after the D3 trim) — no span may cover them.
        for byte_idx in [6usize, 11] {
            assert!(
                spans.iter().all(|(r, _)| !r.contains(&byte_idx)),
                "byte {byte_idx} must be ambient",
            );
        }
    }

    #[test]
    fn unknown_extension_resolves_to_plain() {
        assert!(resolve_syntax("blob.definitelynotagrammar").is_none());
        // `Makefile` matches by extension in the default pack; there is
        // no Dockerfile grammar in it at all (verified 2026-09-17) —
        // such files render plain, which is the documented fallback.
        assert!(resolve_syntax("src/nested/Makefile").is_some());
        assert!(resolve_syntax("Dockerfile").is_none());
        // Dotless names must not be re-tried as extensions.
        assert!(resolve_syntax("README").is_none());
    }

    #[test]
    fn parser_state_persists_across_rows() {
        // A string literal opened on row 1 must still color row 2.
        let syntax = resolve_syntax("main.py").expect("py resolves");
        let mut state = ParseState::new(syntax);
        let l1 = r#"msg = ("hello"#;
        let l2 = r#"world")"#;
        let s1 = tokenize_row(l1, &mut state);
        let s2 = tokenize_row(l2, &mut state);
        assert!(!spans_of(l1, &s1, TokenKind::StringLit).is_empty());
        assert!(
            !spans_of(l2, &s2, TokenKind::StringLit).is_empty(),
            "continuation row must stay a string"
        );
    }

    #[test]
    fn compute_rows_aligns_with_anchor_patch() {
        let patch = "\
@@ -1,3 +1,3 @@\
\n fn main() {}\
\n-let old = 1;\
\n+let new = 2;\
\n \\ No newline at end of file";
        let rows = anchor_patch(patch);
        let spans = compute_rows("lib.rs", &rows).expect("rust file highlights");
        assert_eq!(spans.len(), rows.len());
        // Hunk header and the `\ No newline` marker are never fed to
        // the parser (docs/0040 §5.1) — their span lists stay empty.
        assert!(spans[0].is_empty(), "hunk header row");
        assert!(spans[rows.len() - 1].is_empty(), "marker row");
        // The file-text rows did get spans (keywords at minimum).
        assert!(
            rows.iter()
                .zip(spans.iter())
                .skip(1)
                .take(rows.len() - 2)
                .any(|(_, s)| !s.is_empty())
        );
    }

    #[test]
    fn over_cap_files_render_plain() {
        let patch: String = (0..SYNTAX_HIGHLIGHT_MAX_LINES + 1)
            .map(|_| "\n fn a() {}\n")
            .collect();
        let rows = anchor_patch(&patch);
        assert_eq!(compute_rows("lib.rs", &rows), None);
    }

    #[test]
    fn token_style_derives_from_theme_tokens() {
        let theme = ThemeColor::default();
        let table = [
            (TokenKind::Comment, theme.muted_foreground),
            (TokenKind::StringLit, theme.warning),
            (TokenKind::Number, theme.danger),
            (TokenKind::Invalid, theme.danger),
            (TokenKind::Keyword, theme.accent),
            (TokenKind::Function, theme.link),
            (TokenKind::Type, theme.success),
        ];
        for (kind, color) in table {
            assert_eq!(
                token_style(kind, &theme).color,
                Some(color),
                "{kind:?} must map to its documented token"
            );
        }
        // The italic column: comments only.
        assert!(token_style(TokenKind::Comment, &theme).font_style.is_some());
        assert!(token_style(TokenKind::Keyword, &theme).font_style.is_none());
    }

    #[test]
    fn cache_hits_and_invalidates() {
        let cache = HighlightCache::default();
        let rows = anchor_patch(" fn a() {}\n+let x = 1;");
        let key: gpui::SharedString = "Default Dark".into();
        let first = cache
            .spans_for("lib.rs", " fn a() {}\n+let x = 1;", &rows, key.clone())
            .expect("rust patch highlights");
        let hit = cache
            .spans_for("lib.rs", " fn a() {}\n+let x = 1;", &rows, key.clone())
            .expect("hit");
        assert!(Arc::ptr_eq(&first, &hit), "a memo hit must be the same Arc");
        // Patch text change invalidates (different hash → recompute).
        let other = cache
            .spans_for("lib.rs", " fn b() {}\n+let y = 2;", &rows, key.clone())
            .expect("recomputed");
        assert!(!Arc::ptr_eq(&first, &other));
        // clear() (file-list replacement) drops entries — next call
        // recomputes into a fresh Arc.
        cache.clear();
        let after_clear = cache
            .spans_for("lib.rs", " fn a() {}\n+let x = 1;", &rows, key.clone())
            .expect("recomputed after clear");
        assert!(!Arc::ptr_eq(&first, &after_clear));
        // Plain files memo as a `None` hit — cheap forever.
        assert!(
            cache
                .spans_for("blob.unknownext", "x", &rows, key.clone())
                .is_none()
        );
        assert!(
            cache
                .spans_for("blob.unknownext", "x", &rows, key.clone())
                .is_none()
        );
    }

    #[test]
    fn theme_key_tracks_the_tokens_the_palette_reads() {
        let a = theme_key(&ThemeColor::default());
        // A different accent must produce a different key.
        let changed = ThemeColor {
            accent: gpui::Hsla {
                h: 0.9,
                s: 0.1,
                l: 0.2,
                a: 1.0,
            },
            ..ThemeColor::default()
        };
        assert_ne!(a, theme_key(&changed));
        assert_eq!(a, theme_key(&ThemeColor::default()));
    }
}
