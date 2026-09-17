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

pub(crate) type SpanList = Option<Arc<Vec<RowSpans>>>;

/// Per-file memo entry (0040 D5, amended): tokenization runs on a
/// background task — a Rust file costs ~0.6 ms/line (fancy-regex;
/// measured 440 ms cold / 294 ms warm for 460 rows), far too slow to
/// do inside render — so an entry is `Tokenizing` (render plain) until
/// the task lands.
#[derive(Clone)]
pub(crate) enum HighlightFlight {
    Tokenizing,
    Done(SpanList),
}

#[derive(Clone)]
pub(crate) struct HighlightEntry {
    patch_hash: u64,
    theme_key: SharedString,
    pub(crate) flight: HighlightFlight,
}

/// Where the D5 memo lives. A `RefCell` because it fills from render
/// and from task completion (the page entity is already borrowed at
/// render time); it is a pure memo — never warrants `cx.notify()` on
/// its own.
#[derive(Default)]
pub(crate) struct HighlightCache(RefCell<BTreeMap<String, HighlightEntry>>);

/// The lookup outcome for one file on one frame.
pub(crate) enum Lookup {
    /// Spans ready (`None` = deliberately plain: no grammar or over cap).
    Done(SpanList),
    /// A background task is running — render plain, don't re-queue.
    Tokenizing,
    /// Nothing current — the caller queues a task.
    Miss,
}

impl HighlightCache {
    /// Read-only check for the current frame.
    pub(crate) fn lookup(&self, path: &str, patch_hash: u64, theme_key: &str) -> Lookup {
        let cache = self.0.borrow();
        match cache.get(path) {
            Some(entry) if entry.patch_hash == patch_hash && entry.theme_key == theme_key => {
                match &entry.flight {
                    HighlightFlight::Done(rows) => Lookup::Done(rows.clone()),
                    HighlightFlight::Tokenizing => Lookup::Tokenizing,
                }
            }
            // A finished entry under a stale key is a miss (the caller
            // re-queues; `finish` replaces the entry).
            _ => Lookup::Miss,
        }
    }

    /// Mark a task in flight; `false` if an equivalent one already is.
    pub(crate) fn begin(&self, path: &str, patch_hash: u64, theme_key: SharedString) -> bool {
        let mut cache = self.0.borrow_mut();
        if let Some(entry) = cache.get(path) {
            if entry.patch_hash == patch_hash
                && entry.theme_key == theme_key
                && matches!(entry.flight, HighlightFlight::Tokenizing)
            {
                return false;
            }
        }
        cache.insert(
            path.to_string(),
            HighlightEntry {
                patch_hash,
                theme_key,
                flight: HighlightFlight::Tokenizing,
            },
        );
        true
    }

    /// Store a finished result — but only if the entry still points at
    /// the same (hash, theme) the task was queued for; a refresh or
    /// theme switch in the meantime replaced it and queued its own task.
    pub(crate) fn finish(&self, path: &str, patch_hash: u64, theme_key: &str, rows: SpanList) {
        let mut cache = self.0.borrow_mut();
        if let Some(entry) = cache.get_mut(path) {
            if entry.patch_hash == patch_hash
                && entry.theme_key == theme_key
                && matches!(entry.flight, HighlightFlight::Tokenizing)
            {
                entry.flight = HighlightFlight::Done(rows);
            }
        }
    }

    /// Store a settled result synchronously (no-grammar / over-cap
    /// files: the plain look is the answer, no task needed).
    pub(crate) fn store_plain(&self, path: &str, patch_hash: u64, theme_key: SharedString) {
        self.0.borrow_mut().insert(
            path.to_string(),
            HighlightEntry {
                patch_hash,
                theme_key,
                flight: HighlightFlight::Done(None),
            },
        );
    }
}

/// FNV-1a over the patch text — deterministic for the page's lifetime,
/// which is all the memo's invalidation needs (0040 D5).
pub(crate) fn hash_patch(patch: &str) -> u64 {
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
/// to the parser. No grammar for the path ⇒ `None`. Runs on a
/// background task (0040 D5, amended) — never in render.
pub(crate) fn compute_rows(path: &str, rows: &[AnchoredLine]) -> SpanList {
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
    fn cache_flight_lifecycle() {
        use super::{HighlightCache, Lookup};
        let cache = HighlightCache::default();
        let key: gpui::SharedString = "Default Dark".into();

        // Miss → queue → in-flight → finish → hit (same Arc).
        let hash = hash_patch(" fn a() {}\n+let x = 1;");
        assert!(matches!(cache.lookup("lib.rs", hash, &key), Lookup::Miss));
        assert!(cache.begin("lib.rs", hash, key.clone()));
        assert!(
            matches!(cache.lookup("lib.rs", hash, &key), Lookup::Tokenizing),
            "in-flight renders plain and must not re-queue"
        );
        assert!(
            !cache.begin("lib.rs", hash, key.clone()),
            "a second begin for the same (path, hash, theme) is refused"
        );
        let spans: SpanList = Some(Arc::new(vec![vec![]]));
        cache.finish("lib.rs", hash, &key, spans.clone());
        let stored = spans.clone();
        match cache.lookup("lib.rs", hash, &key) {
            Lookup::Done(rows) => assert!(Arc::ptr_eq(&rows.unwrap(), &stored.unwrap())),
            _ => panic!("finished entry must be Done"),
        }

        // Theme change invalidates: miss again, and the stale task's
        // finish under the old key is ignored.
        let other_key: gpui::SharedString = "Default Light".into();
        assert!(matches!(
            cache.lookup("lib.rs", hash, &other_key),
            Lookup::Miss
        ));
        assert!(cache.begin("lib.rs", hash, other_key.clone()));
        cache.finish("lib.rs", hash, &key, spans.clone());
        assert!(
            matches!(cache.lookup("lib.rs", hash, &other_key), Lookup::Tokenizing),
            "a stale-theme finish must not satisfy the current queue"
        );

        // Patch change invalidates the same way.
        let other_hash = hash_patch(" fn b() {}");
        assert!(matches!(
            cache.lookup("lib.rs", other_hash, &key),
            Lookup::Miss
        ));

        // Plain files (no grammar / over cap) store a settled `None` —
        // a hit, cheap forever.
        cache.store_plain("blob.unknownext", hash_patch("x"), key.clone());
        assert!(matches!(
            cache.lookup("blob.unknownext", hash_patch("x"), &key),
            Lookup::Done(None)
        ));
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
