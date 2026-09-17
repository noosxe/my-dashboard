//! Markdown bodies (docs/0017): pulldown-cmark → owned `Block`/`Run` tree →
//! gpui elements. The parse side is pure and table-tested; the render side
//! builds `StyledText` runs (highlights refine the ambient text style, so
//! containers set size/weight) and `InteractiveText` for link ranges.

use std::{ops::Range, sync::Arc};

use gpui::{
    AnyElement, App, Div, FontStyle, FontWeight, HighlightStyle, InteractiveText, IntoElement,
    ParentElement, SharedString, StrikethroughStyle, Styled, StyledText, UnderlineStyle, div, px,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use linkify::{LinkFinder, LinkKind};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// One markdown document, folded from pulldown events into owned blocks
/// (D2). Held across frames via the page-level cache — never re-parsed per
/// render (D6).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Block {
    Paragraph(Vec<Run>),
    Heading {
        level: u8,
        runs: Vec<Run>,
    },
    /// Fenced or indented code; one string, trailing newline trimmed.
    /// `lang` is the fence info's first token — 0030 D6 needs it to
    /// recognize ```suggestion blocks; indented fences carry None.
    Code {
        lang: Option<String>,
        code: String,
    },
    /// Blockquote; `alert` is the GFM `[!NOTE]`-style tag (D1, `ENABLE_GFM`).
    Quote {
        alert: Option<Alert>,
        blocks: Vec<Block>,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<ListItem>,
    },
    /// GFM table (D1). Alignment is not carried — cells render
    /// left-aligned v1 (recorded deviation from the D2 sketch).
    Table {
        head: Vec<Vec<Run>>,
        rows: Vec<Vec<Vec<Run>>>,
    },
    Rule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Alert {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

/// A run of inline text. Flags compose (bold link, emphasized code, …);
/// the fence info string is not captured v1 (no highlighting — D2 note).
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Run {
    pub text: String,
    pub style: RunStyle,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct RunStyle {
    pub strong: bool,
    pub emphasis: bool,
    pub strike: bool,
    pub code: bool,
    /// Bare-URL runs (linkify pass, D1) and markdown links/images share
    /// this — images render as their alt text linking the asset (D2 scope).
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ListItem {
    /// GFM task list: `Some(checked)`; `None` for a normal item.
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

/// Parse a markdown body (D1): CommonMark plus the GFM extensions
/// (tables, strikethrough, tasklists, blockquote alerts). Raw HTML events
/// are dropped; footnotes stay disabled. Bare URLs/emails in plain text
/// become link runs (linkify); GitHub cross-references autolink too
/// (`owner/repo#123`, `@user` always, bare `#123` when `repo_key` names
/// the ambient repo — an `owner/repo` string or an `owner/repo#N` page
/// key; anything else degrades to no ambient repo and bare refs stay
/// plain text).
pub(crate) fn parse_markdown_in_repo(body: &str, repo_key: Option<&str>) -> Arc<[Block]> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_GFM);
    let mut p = ParseState {
        repo: repo_key.and_then(ambient_repo).map(str::to_string),
        ..ParseState::default()
    };
    for ev in Parser::new_ext(body, opts) {
        p.event(ev);
    }
    p.flush_inline();
    p.root.into()
}

#[derive(Default)]
struct ParseState {
    /// Ambient `owner/repo` for bare `#123` cross-references (docs/0017
    /// D1 extension); `None` leaves them plain text.
    repo: Option<String>,
    root: Vec<Block>,
    stack: Vec<Frame>,
    runs: Vec<Run>,
    /// What the inline buffer flushes as. Tight lists emit item text with
    /// no `Paragraph` tag, so `None` + non-empty runs flushes as a
    /// paragraph.
    mode: Option<InlineMode>,
    style: RunStyle,
    saved: Vec<RunStyle>,
    code: Option<String>,
    /// The active fenced block's info string (0030 D6); indented → None.
    code_lang: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum InlineMode {
    Paragraph,
    Heading(u8),
    Cell,
}

enum Frame {
    Quote {
        alert: Option<Alert>,
        blocks: Vec<Block>,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<ListItem>,
    },
    Item {
        task: Option<bool>,
        blocks: Vec<Block>,
    },
    Table {
        head: Option<Vec<Vec<Run>>>,
        rows: Vec<Vec<Vec<Run>>>,
        row: Vec<Vec<Run>>,
    },
}

impl ParseState {
    fn event(&mut self, ev: Event<'_>) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                if let Some(code) = &mut self.code {
                    code.push_str(&t);
                } else {
                    self.push_text(&t);
                }
            }
            Event::Code(c) => {
                let mut style = self.style.clone();
                style.code = true;
                self.runs.push(Run {
                    text: c.to_string(),
                    style,
                });
            }
            Event::SoftBreak => self.break_run(' '),
            Event::HardBreak => self.break_run('\n'),
            Event::TaskListMarker(checked) => {
                if let Some(Frame::Item { task, .. }) = self.stack.last_mut() {
                    *task = Some(checked);
                }
            }
            Event::Rule => {
                self.flush_inline();
                self.flush_block(Block::Rule);
            }
            // D1: raw HTML is dropped; math/footnotes/definition lists are
            // not enabled and never arrive, but stay covered defensively.
            Event::Html(_) | Event::InlineHtml(_) => {}
            Event::InlineMath(t) | Event::DisplayMath(t) | Event::FootnoteReference(t) => {
                let _ = t;
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                self.runs.clear();
                self.mode = Some(InlineMode::Paragraph);
            }
            Tag::Heading { level, .. } => {
                self.runs.clear();
                self.mode = Some(InlineMode::Heading(level as u8));
            }
            Tag::TableCell => {
                self.runs.clear();
                self.mode = Some(InlineMode::Cell);
            }
            Tag::BlockQuote(kind) => {
                self.flush_inline();
                self.stack.push(Frame::Quote {
                    alert: kind.map(|k| match k {
                        pulldown_cmark::BlockQuoteKind::Note => Alert::Note,
                        pulldown_cmark::BlockQuoteKind::Tip => Alert::Tip,
                        pulldown_cmark::BlockQuoteKind::Important => Alert::Important,
                        pulldown_cmark::BlockQuoteKind::Warning => Alert::Warning,
                        pulldown_cmark::BlockQuoteKind::Caution => Alert::Caution,
                    }),
                    blocks: Vec::new(),
                });
            }
            Tag::List(start) => {
                self.flush_inline();
                self.stack.push(Frame::List {
                    ordered: start.is_some(),
                    start: start.unwrap_or(1),
                    items: Vec::new(),
                });
            }
            Tag::Item => {
                self.flush_inline();
                self.stack.push(Frame::Item {
                    task: None,
                    blocks: Vec::new(),
                });
            }
            Tag::Table(_) => {
                self.flush_inline();
                self.stack.push(Frame::Table {
                    head: None,
                    rows: Vec::new(),
                    row: Vec::new(),
                });
            }
            Tag::CodeBlock(kind) => {
                self.flush_inline();
                self.code = Some(String::new());
                // 0030 D6: the fence info's first token ("suggestion",
                // "rust", …). Indented blocks carry no info string.
                self.code_lang = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split(|c: char| c.is_whitespace())
                        .next()
                        .filter(|t| !t.is_empty())
                        .map(str::to_string),
                    CodeBlockKind::Indented => None,
                };
            }
            Tag::Strong => {
                self.saved.push(self.style.clone());
                self.style.strong = true;
            }
            Tag::Emphasis => {
                self.saved.push(self.style.clone());
                self.style.emphasis = true;
            }
            Tag::Strikethrough => {
                self.saved.push(self.style.clone());
                self.style.strike = true;
            }
            // Images render as their alt text linking the asset (D2 scope).
            Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                self.saved.push(self.style.clone());
                self.style.link = Some(dest_url.to_string());
            }
            // TableHead/TableRow carry no inline state; cells drive rows.
            Tag::TableHead | Tag::TableRow => {}
            // Not enabled (D1) or metadata blocks; defensive no-ops.
            Tag::HtmlBlock
            | Tag::MetadataBlock(_)
            | Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::TableCell => self.flush_inline(),
            TagEnd::BlockQuote(_) => {
                self.flush_inline();
                if let Some(Frame::Quote { alert, blocks }) = self.stack.pop() {
                    self.flush_block(Block::Quote { alert, blocks });
                }
            }
            TagEnd::Item => {
                self.flush_inline();
                let item = match self.stack.last_mut() {
                    Some(Frame::Item { task, blocks }) => ListItem {
                        task: *task,
                        blocks: std::mem::take(blocks),
                    },
                    _ => return,
                };
                self.stack.pop();
                if let Some(Frame::List { items, .. }) = self.stack.last_mut() {
                    items.push(item);
                }
            }
            TagEnd::List(_) => {
                if let Some(Frame::List {
                    ordered,
                    start,
                    items,
                }) = self.stack.pop()
                {
                    self.flush_block(Block::List {
                        ordered,
                        start,
                        items,
                    });
                }
            }
            TagEnd::Table => {
                if let Some(Frame::Table { head, rows, .. }) = self.stack.pop() {
                    self.flush_block(Block::Table {
                        head: head.unwrap_or_default(),
                        rows,
                    });
                }
            }
            TagEnd::TableHead => {
                if let Some(Frame::Table { head, row, .. }) = self.stack.last_mut() {
                    *head = Some(std::mem::take(row));
                }
            }
            TagEnd::CodeBlock => {
                if let Some(code) = self.code.take() {
                    let text = code.trim_end_matches('\n').to_string();
                    let lang = self.code_lang.take();
                    self.flush_block(Block::Code { lang, code: text });
                }
            }
            TagEnd::TableRow => {
                if let Some(Frame::Table { rows, row, .. }) = self.stack.last_mut() {
                    rows.push(std::mem::take(row));
                }
            }
            TagEnd::Strong
            | TagEnd::Emphasis
            | TagEnd::Strikethrough
            | TagEnd::Link
            | TagEnd::Image => {
                if let Some(style) = self.saved.pop() {
                    self.style = style;
                }
            }
            _ => {}
        }
    }

    fn break_run(&mut self, sep: char) {
        if let Some(code) = &mut self.code {
            code.push(sep);
        } else {
            self.runs.push(Run {
                text: sep.to_string(),
                style: self.style.clone(),
            });
        }
    }

    fn push_text(&mut self, text: &str) {
        // D1: bare URLs/emails autolink (linkify), except inside existing
        // links — code spans/blocks never autolink (they don't come here).
        // The D1 extension adds GitHub cross-references (`#123`,
        // `owner/repo#123`, `@user`); linkify wins overlaps — a ref-text
        // inside a URL or email belongs to that link.
        if self.style.link.is_some() {
            self.push_run_plain(text);
            return;
        }
        let mut finder = LinkFinder::new();
        finder.kinds(&[LinkKind::Url, LinkKind::Email]);
        let mut merged: Vec<(usize, usize, Option<String>)> = Vec::new();
        let mut covered: Vec<(usize, usize)> = Vec::new();
        for span in finder.spans(text) {
            let Some(kind) = span.kind() else {
                continue;
            };
            let (s, e) = (span.start(), span.end());
            merged.push((s, e, autolink_target(kind, &text[s..e])));
            covered.push((s, e));
        }
        merged.extend(
            xref_spans(text, self.repo.as_deref(), &covered)
                .into_iter()
                .map(|x| (x.start, x.end, Some(x.url))),
        );
        merged.sort_by_key(|&(s, _, _)| s);
        // Both sources are individually disjoint and ascending; the
        // sort is stable so linkify spans win same-start ties, and any
        // xref overlapping a kept span is dropped — the stronger parse
        // (a real URL/email) claims the bytes.
        let mut last_end = 0usize;
        merged.retain(|&(s, e, _)| {
            let keep = s >= last_end;
            if keep {
                last_end = e;
            }
            keep
        });
        let mut base = 0usize;
        for (s, e, url) in merged {
            // Exactly one run per matched span, and `base` always
            // advances past it (#169): a `None` target stays plain
            // text — neither duplicated (base stuck behind the
            // already-pushed prefix, re-pushed by the flush) nor
            // dropped.
            if s > base {
                self.push_run_plain(&text[base..s]);
            }
            match url {
                Some(url) => {
                    let mut style = self.style.clone();
                    style.link = Some(url);
                    self.runs.push(Run {
                        text: text[s..e].to_string(),
                        style,
                    });
                }
                None => self.push_run_plain(&text[s..e]),
            }
            base = e;
        }
        if base < text.len() {
            self.push_run_plain(&text[base..]);
        }
    }

    fn push_run_plain(&mut self, text: &str) {
        self.runs.push(Run {
            text: text.to_string(),
            style: self.style.clone(),
        });
    }

    /// Flush the inline buffer as the block its mode names (or an implicit
    /// paragraph for tight-list text).
    fn flush_inline(&mut self) {
        let mode = self.mode.take().unwrap_or(InlineMode::Paragraph);
        let runs = std::mem::take(&mut self.runs);
        if runs.is_empty() {
            return;
        }
        match mode {
            InlineMode::Cell => {
                if let Some(Frame::Table { row, .. }) = self.stack.last_mut() {
                    row.push(runs);
                }
            }
            InlineMode::Heading(level) => {
                self.flush_block(Block::Heading { level, runs });
            }
            InlineMode::Paragraph => self.flush_block(Block::Paragraph(runs)),
        }
    }

    fn flush_block(&mut self, block: Block) {
        match self.stack.last_mut() {
            Some(Frame::Quote { blocks, .. } | Frame::Item { blocks, .. }) => blocks.push(block),
            _ => self.root.push(block),
        }
    }
}

/// Render blocks into a vertical stack (D4). `id_prefix` names interactive
/// children (`{prefix}-p-{n}`) — links are ranges inside those, invisible
/// to the bridge's node dump (D7).
pub(crate) fn render_blocks(blocks: &[Block], id_prefix: &str, cx: &App) -> AnyElement {
    v_flex()
        .w_full()
        .gap_2()
        .text_sm()
        .children(
            blocks
                .iter()
                .enumerate()
                .map(|(ix, block)| render_block(block, &format!("{id_prefix}-{ix}"), cx)),
        )
        .into_any_element()
}

/// The `owner/repo` prefix of an `owner/repo` string or `owner/repo#N`
/// page key; `None` without a `/`-bearing prefix (degrades to no ambient
/// repo — bare `#123` stays plain text).
fn ambient_repo(repo_key: &str) -> Option<&str> {
    let prefix = repo_key.split('#').next().unwrap_or(repo_key);
    prefix.contains('/').then_some(prefix)
}

/// One GitHub cross-reference match (docs/0017 D1 extension): the byte
/// range `[start, end)` plus the github.com target it opens.
struct XrefSpan {
    start: usize,
    end: usize,
    url: String,
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Scan plain text for GitHub cross-references — `owner/repo#123`,
/// bare `#123` (needs the ambient repo), `@user` — as disjoint
/// ascending spans. Boundary rules keep them deterministic and out of
/// other syntax: a match must start on a non-word boundary (`#` may not
/// follow a word char or `&` — HTML entities; `@` may not follow a word
/// char — emails; an owner may not follow a word char or `/` — paths),
/// the digits/name run must end on a boundary (`#123abc` stays plain),
/// and any match starting inside a linkify span is skipped (`covered`;
/// linkify wins — ref-text inside a URL or email belongs to that link).
fn xref_spans(text: &str, repo: Option<&str>, covered: &[(usize, usize)]) -> Vec<XrefSpan> {
    fn is_covered(covered: &[(usize, usize)], at: usize) -> bool {
        covered.iter().any(|&(s, e)| at >= s && at < e)
    }
    let b = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        // Matches start at ASCII bytes, which are always char
        // boundaries; non-ASCII bytes are stepped over one at a time.
        let start = i;
        let span = match b[i] {
            b'@' if !is_covered(covered, i) && (i == 0 || !is_word(b[i - 1])) => mention_span(b, i)
                .map(|(end, name)| XrefSpan {
                    start,
                    end,
                    url: format!("https://github.com/{name}"),
                }),
            b'#' if repo.is_some()
                && !is_covered(covered, i)
                && (i == 0 || (!is_word(b[i - 1]) && b[i - 1] != b'&')) =>
            {
                number_span(b, i, i + 1).map(|end| XrefSpan {
                    start,
                    end,
                    url: format!(
                        "https://github.com/{}/issues/{}",
                        repo.expect("guarded above"),
                        &text[i + 1..end]
                    ),
                })
            }
            _ if b[i].is_ascii_alphanumeric()
                && !is_covered(covered, i)
                && (i == 0 || (!is_word(b[i - 1]) && b[i - 1] != b'/')) =>
            {
                // `owner/repo#N` — the owner run starts here.
                repo_number_span(b, i)
                    .filter(|end| b[*end - 1].is_ascii_digit())
                    .map(|end| {
                        // Split `owner/repo` off the front of the
                        // matched run.
                        let hash = text[start..end].rfind('#').expect("digits after #");
                        XrefSpan {
                            start,
                            end,
                            url: format!(
                                "https://github.com/{}/issues/{}",
                                &text[start..start + hash],
                                &text[start + hash + 1..end]
                            ),
                        }
                    })
            }
            _ => None,
        };
        match span {
            Some(sp) => {
                i = sp.end;
                spans.push(sp);
            }
            None => i += 1,
        }
    }
    spans
}

/// `#N` starting at the `#` (`at`): the digit run, ending on a boundary
/// — the next byte is not alphanumeric or `_`.
fn number_span(b: &[u8], hash: usize, digits: usize) -> Option<usize> {
    debug_assert_eq!(b[hash], b'#');
    // `digits` is the first candidate digit byte — right after the `#`
    // for bare refs, after `owner/repo` for repo refs.
    if !b.get(digits).is_some_and(|d| d.is_ascii_digit()) {
        return None;
    }
    let mut end = digits + 1;
    while end < b.len() && b[end].is_ascii_digit() {
        end += 1;
    }
    (end >= b.len() || !is_word(b[end])).then_some(end)
}

/// `owner/repo#N` starting at the owner run (`at`): two `.`/`-`/word
/// runs around a `/`, then [`number_span`]. The caller checks the last
/// matched byte is a digit (no digits → not a ref).
fn repo_number_span(b: &[u8], at: usize) -> Option<usize> {
    fn name_run(b: &[u8], mut i: usize) -> usize {
        while i < b.len() && (is_word(b[i]) || b[i] == b'.' || b[i] == b'-') {
            i += 1;
        }
        i
    }
    let slash = name_run(b, at);
    if slash >= b.len() || b[slash] != b'/' {
        return None;
    }
    let repo_end = name_run(b, slash + 1);
    if repo_end >= b.len() || b[repo_end] != b'#' {
        return None;
    }
    number_span(b, repo_end, repo_end + 1)
}

/// `@name` starting at the `@` (`at`): an alphanumeric start, then word
/// chars and hyphens, trailing hyphens trimmed off the linked name; the
/// byte after the run may not be a word char or hyphen.
fn mention_span(b: &[u8], at: usize) -> Option<(usize, &str)> {
    let mut end = at + 1;
    if end >= b.len() || !b[end].is_ascii_alphanumeric() {
        return None;
    }
    while end < b.len() && (is_word(b[end]) || b[end] == b'-') {
        end += 1;
    }
    // Trim trailing hyphens off the linked name (GitHub usernames may
    // not end in one); the run was consumed greedily, so what follows
    // it is already a boundary.
    let mut name_end = end;
    while name_end > at + 1 && b[name_end - 1] == b'-' {
        name_end -= 1;
    }
    Some((name_end, std::str::from_utf8(&b[at + 1..name_end]).ok()?))
}

/// Target URL a matched linkify span should open, or `None` when the
/// kind must stay plain text — `LinkKind` is `#[non_exhaustive]`
/// upstream, so unknown future kinds render verbatim (#169). Kept a
/// free fn so the mapping (incl. the scheme-less fallback) is unit-
/// testable without going through `LinkFinder`.
fn autolink_target(kind: &LinkKind, raw: &str) -> Option<String> {
    match *kind {
        LinkKind::Email => Some(format!("mailto:{raw}")),
        LinkKind::Url => Some(
            if raw.starts_with("http://") || raw.starts_with("https://") {
                raw.to_string()
            } else {
                format!("http://{raw}")
            },
        ),
        _ => None,
    }
}

pub(crate) fn render_block(block: &Block, id: &str, cx: &App) -> AnyElement {
    match block {
        Block::Paragraph(runs) => render_runs(runs, id, cx).into_any_element(),
        Block::Heading { level, runs } => {
            let text = render_runs(runs, id, cx)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(match level {
                    1..=3 => cx.theme().foreground,
                    _ => cx.theme().muted_foreground,
                });
            // H1 > H2 > H3..6 mirror the text scale; body stays text_sm.
            let sized = match level {
                1 => text.text_lg(),
                2 => text.text_base(),
                _ => text,
            };
            text_wrap(sized).into_any_element()
        }
        Block::Code { code, .. } => text_wrap(
            div()
                .font_family(cx.theme().mono_font_family.clone())
                .text_xs()
                .child(code.clone()),
        )
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().muted.opacity(0.4))
        .p_2()
        .into_any_element(),
        Block::Quote { alert, blocks } => {
            let (tint, label) = match alert {
                Some(Alert::Warning) => (cx.theme().warning, "Warning"),
                Some(Alert::Caution) => (cx.theme().danger, "Caution"),
                Some(_) => (cx.theme().accent, "Note"),
                None => (cx.theme().border, ""),
            };
            v_flex()
                .gap_2()
                .border_l_2()
                .border_color(tint)
                .pl_3()
                .children((!label.is_empty()).then(|| {
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(tint)
                        .child(label)
                }))
                .children(
                    blocks
                        .iter()
                        .enumerate()
                        .map(|(ix, b)| render_block(b, &format!("{id}-{ix}"), cx)),
                )
                .into_any_element()
        }
        Block::List {
            ordered,
            start,
            items,
        } => v_flex()
            .gap_1()
            .children(items.iter().enumerate().map(|(ix, item)| {
                let marker = match (ordered, item.task) {
                    (_, Some(true)) => "✓".to_string(),
                    (_, Some(false)) => " ".to_string(),
                    (true, None) => format!("{}.", start + ix as u64),
                    (false, None) => "•".to_string(),
                };
                let marker_div = div()
                    .flex_none()
                    .w_4()
                    .text_color(cx.theme().muted_foreground)
                    .child(marker);
                h_flex()
                    .items_start()
                    .gap_2()
                    .child(marker_div)
                    .child(
                        v_flex().flex_1().min_w_0().gap_2().children(
                            item.blocks
                                .iter()
                                .enumerate()
                                .map(|(bix, b)| render_block(b, &format!("{id}-{ix}-{bix}"), cx)),
                        ),
                    )
                    .into_any_element()
            }))
            .into_any_element(),
        Block::Table { head, rows } => {
            let head_cells = head.clone();
            v_flex()
                .gap_0()
                .child(
                    h_flex()
                        .gap_0()
                        .bg(cx.theme().secondary.opacity(0.4))
                        .children(head_cells.iter().map(|cell| {
                            render_runs(cell, &format!("{id}-h"), cx)
                                .flex_1()
                                .min_w_0()
                                .px_2()
                                .py_1()
                                .font_weight(FontWeight::SEMIBOLD)
                                .into_any_element()
                        })),
                )
                .children(rows.iter().enumerate().map(|(rix, row)| {
                    h_flex()
                        .gap_0()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .children(row.iter().enumerate().map(|(cix, cell)| {
                            render_runs(cell, &format!("{id}-{rix}-{cix}"), cx)
                                .flex_1()
                                .min_w_0()
                                .px_2()
                                .py_1()
                                .into_any_element()
                        }))
                        .into_any_element()
                }))
                .into_any_element()
        }
        Block::Rule => div()
            .border_t_1()
            .border_color(cx.theme().border)
            .into_any_element(),
    }
}

/// Runs → one wrapping text node; links (if any) wrap it in
/// `InteractiveText` and open externally on click (D3/D5).
fn render_runs(runs: &[Run], id: &str, cx: &App) -> Div {
    let mut text = String::new();
    let mut highlights: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    let mut links: Vec<(Range<usize>, String)> = Vec::new();
    for run in runs {
        let start = text.len();
        text.push_str(&run.text);
        let range = start..text.len();
        if run.style == (RunStyle::default()) {
            continue;
        }
        let mut h = HighlightStyle::default();
        let st = &run.style;
        if st.code {
            // No `font_family` on HighlightStyle in this gpui rev — inline
            // code carries a muted tint only; blocks are mono (D3 note).
            h.background_color = Some(cx.theme().muted.opacity(0.6));
        }
        if st.strong {
            h.font_weight = Some(FontWeight::BOLD);
        }
        if st.emphasis {
            h.font_style = Some(FontStyle::Italic);
        }
        if st.strike {
            h.strikethrough = Some(StrikethroughStyle {
                thickness: px(1.),
                color: None,
            });
        }
        if let Some(url) = &st.link {
            h.color = Some(cx.theme().link);
            h.underline = Some(UnderlineStyle {
                thickness: px(1.),
                color: None,
                wavy: false,
            });
            links.push((range.clone(), url.clone()));
        }
        highlights.push((range, h));
    }
    let styled = StyledText::new(text).with_highlights(highlights);
    if links.is_empty() {
        return div().child(styled);
    }
    let urls: Arc<[String]> = links.iter().map(|(_, u)| u.clone()).collect();
    let ranges: Vec<Range<usize>> = links.into_iter().map(|(r, _)| r).collect();
    div().child(
        InteractiveText::new(SharedString::from(id.to_string()), styled).on_click(
            ranges,
            move |ix, _window, cx| {
                if let Some(url) = urls.get(ix) {
                    cx.open_url(url.as_str());
                }
            },
        ),
    )
}

/// Paragraph text must be allowed to wrap (`StyledText` inside a plain
/// `div` inherits the default nowrap of flex children in some nesting
/// contexts, so pin normal wrapping like body_card does).
fn text_wrap(d: Div) -> Div {
    d.whitespace_normal()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: &str) -> Vec<Block> {
        parse_markdown_in_repo(body, None).to_vec()
    }

    /// Parse with the ambient repo (bare `#123` support).
    fn parse_in(body: &str, repo: &str) -> Vec<Block> {
        parse_markdown_in_repo(body, Some(repo)).to_vec()
    }

    /// All link targets of a body's first paragraph, in run order.
    fn links(blocks: &[Block]) -> Vec<String> {
        let Block::Paragraph(runs) = &blocks[0] else {
            panic!("paragraph expected");
        };
        runs.iter().filter_map(|r| r.style.link.clone()).collect()
    }

    fn plain(text: &str) -> Run {
        Run {
            text: text.to_string(),
            style: RunStyle::default(),
        }
    }

    #[test]
    fn headings_carry_levels() {
        let blocks = parse("# one\n\n## two\n\n### three");
        assert_eq!(
            blocks,
            vec![
                Block::Heading {
                    level: 1,
                    runs: vec![plain("one")]
                },
                Block::Heading {
                    level: 2,
                    runs: vec![plain("two")]
                },
                Block::Heading {
                    level: 3,
                    runs: vec![plain("three")]
                },
            ]
        );
    }

    #[test]
    fn emphasis_strong_strike_code_compose() {
        let blocks = parse("**bold** and *em* and ~~gone~~ and `code`");
        let Block::Paragraph(runs) = &blocks[0] else {
            panic!("paragraph expected");
        };
        let styled: Vec<(bool, bool, bool, bool)> = runs
            .iter()
            .filter(|r| !r.text.starts_with(" and "))
            .map(|r| {
                (
                    r.style.strong,
                    r.style.emphasis,
                    r.style.strike,
                    r.style.code,
                )
            })
            .collect();
        assert_eq!(
            styled,
            vec![
                (true, false, false, false),
                (false, true, false, false),
                (false, false, true, false),
                (false, false, false, true)
            ]
        );
    }

    #[test]
    fn nested_emphasis_composes_bold_italic() {
        let blocks = parse("***both***");
        let Block::Paragraph(runs) = &blocks[0] else {
            panic!("paragraph expected");
        };
        assert!(runs.iter().any(|r| r.style.strong && r.style.emphasis));
    }

    #[test]
    fn links_extract_text_and_url() {
        let blocks = parse("see [the docs](https://docs.rs/x) now");
        let Block::Paragraph(runs) = &blocks[0] else {
            panic!("paragraph expected");
        };
        let link = runs
            .iter()
            .find(|r| r.style.link.is_some())
            .expect("link run");
        assert_eq!(link.text, "the docs");
        assert_eq!(link.style.link.as_deref(), Some("https://docs.rs/x"));
    }

    #[test]
    fn autolinks_become_links() {
        let blocks = parse("see <https://example.com/a>");
        let Block::Paragraph(runs) = &blocks[0] else {
            panic!("paragraph expected");
        };
        let link = runs.iter().find(|r| r.style.link.is_some()).unwrap();
        assert_eq!(link.style.link.as_deref(), Some("https://example.com/a"));
    }

    #[test]
    fn bare_urls_autolink_with_scheme_and_email_gets_mailto() {
        let blocks = parse("go https://example.com/p ok a@b.io end");
        let Block::Paragraph(runs) = &blocks[0] else {
            panic!("paragraph expected");
        };
        let urls: Vec<&str> = runs
            .iter()
            .filter_map(|r| r.style.link.as_deref())
            .collect();
        assert_eq!(urls, vec!["https://example.com/p", "mailto:a@b.io"]);
    }

    #[test]
    fn cross_refs_autolink_with_the_ambient_repo() {
        // docs/0017 D1 extension: `#123`, `owner/repo#456` and `@user`
        // become github.com links riding the existing run machinery.
        let blocks = parse_in("fix #123, see acme/other#456 by @octocat", "acme/one");
        assert_eq!(
            links(&blocks),
            vec![
                "https://github.com/acme/one/issues/123",
                "https://github.com/acme/other/issues/456",
                "https://github.com/octocat",
            ]
        );
        let Block::Paragraph(runs) = &blocks[0] else {
            panic!("paragraph expected");
        };
        let joined: String = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(joined, "fix #123, see acme/other#456 by @octocat");
    }

    #[test]
    fn bare_refs_stay_plain_without_the_ambient_repo() {
        // No repo context → `#123` is ambiguous — plain text; the
        // self-scoped forms still link.
        let blocks = parse("fix #123, see acme/other#456 by @octocat");
        assert_eq!(
            links(&blocks),
            vec![
                "https://github.com/acme/other/issues/456",
                "https://github.com/octocat",
            ]
        );
    }

    #[test]
    fn cross_refs_respect_boundaries() {
        // Word char before `#` (x#1), entity escape (`&#35;` decodes to
        // a bare `#`), trailing word chars (`#123abc`), email mentions
        // and `a/b/c#1` path tails all stay plain; URLs keep winning
        // their inner ref-text.
        let blocks = parse_in(
            "x#1 &#35; #123abc a@b.io acme/sub/dir#9 https://x.dev/#7",
            "acme/one",
        );
        assert_eq!(links(&blocks), vec!["mailto:a@b.io", "https://x.dev/#7"]);
    }

    #[test]
    fn refs_inside_links_and_code_stay_plain() {
        let blocks = parse_in(
            "see [docs](https://e.dev) and `#123` and [x](https://y.dev#9)",
            "acme/one",
        );
        assert_eq!(links(&blocks), vec!["https://e.dev", "https://y.dev#9"]);
    }

    #[test]
    fn mention_trims_trailing_hyphens_but_never_mid_word() {
        let blocks = parse_in("hey @octo-cat-, not @-x", "acme/one");
        assert_eq!(links(&blocks), vec!["https://github.com/octo-cat"]);
    }

    #[test]
    fn autolink_target_maps_kinds() {
        // Email → mailto, http(s) passthrough, scheme-less fallback
        // (defensive: linkify only matches schemed URLs with default
        // settings, #169).
        assert_eq!(
            autolink_target(&LinkKind::Email, "a@b.io").as_deref(),
            Some("mailto:a@b.io")
        );
        assert_eq!(
            autolink_target(&LinkKind::Url, "https://x.y/z").as_deref(),
            Some("https://x.y/z")
        );
        assert_eq!(
            autolink_target(&LinkKind::Url, "http://x.y/z").as_deref(),
            Some("http://x.y/z")
        );
        assert_eq!(
            autolink_target(&LinkKind::Url, "example.com/z").as_deref(),
            Some("http://example.com/z")
        );
    }

    #[test]
    fn autolink_pass_never_duplicates_or_drops_text() {
        // The #169 invariant: every character of the input survives the
        // autolink pass exactly once — mixed links, email, plain text
        // and a bare www. (matched as no link at all today).
        let input = "go https://example.com/p mail a@b.io www.example.com/x end";
        let blocks = parse(input);
        let Block::Paragraph(runs) = &blocks[0] else {
            panic!("paragraph expected");
        };
        let joined: String = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(joined, input);
        // And the two linkable spans are the only links.
        let urls: Vec<&str> = runs
            .iter()
            .filter_map(|r| r.style.link.as_deref())
            .collect();
        assert_eq!(urls, vec!["https://example.com/p", "mailto:a@b.io"]);
    }

    #[test]
    fn bare_www_domains_stay_plain_v1() {
        // linkify 0.10 does not match scheme-less www. hosts (GitHub does).
        // Recorded limitation: http(s):// and emails autolink, www. stays
        // plain text until we add a dedicated scanner.
        let blocks = parse("go www.example.com/x now");
        let Block::Paragraph(runs) = &blocks[0] else {
            panic!("paragraph expected");
        };
        assert!(runs.iter().all(|r| r.style.link.is_none()));
        let joined: String = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(joined, "go www.example.com/x now");
    }

    #[test]
    fn images_render_as_alt_text_links() {
        let blocks = parse("![logo](https://x/img.png)");
        let Block::Paragraph(runs) = &blocks[0] else {
            panic!("paragraph expected");
        };
        let link = runs.iter().find(|r| r.style.link.is_some()).unwrap();
        assert_eq!(link.text, "logo");
        assert_eq!(link.style.link.as_deref(), Some("https://x/img.png"));
    }

    #[test]
    fn soft_break_is_space_hard_break_is_newline() {
        let soft = parse("a\nb");
        let Block::Paragraph(runs) = &soft[0] else {
            panic!("paragraph expected");
        };
        let joined: String = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(joined, "a b");

        let hard = parse("a  \nb");
        let Block::Paragraph(runs) = &hard[0] else {
            panic!("paragraph expected");
        };
        let joined: String = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(joined, "a\nb");
    }

    #[test]
    fn raw_html_is_dropped_but_inner_text_survives() {
        let blocks = parse("hi <b>bold text</b> bye");
        let Block::Paragraph(runs) = &blocks[0] else {
            panic!("paragraph expected");
        };
        let joined: String = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(joined, "hi bold text bye");
        assert!(runs.iter().all(|r| r.style == RunStyle::default()));
    }

    #[test]
    fn fenced_code_extracts_text_without_trailing_newline() {
        let blocks = parse("```\nlet x = 1;\nlet y = 2;\n```");
        assert_eq!(
            blocks,
            vec![Block::Code {
                lang: None,
                code: "let x = 1;\nlet y = 2;".into(),
            }]
        );
    }

    #[test]
    fn fenced_code_carries_the_info_token() {
        // 0030 D6: the fence info's first token survives the parse —
        // that's how ```suggestion blocks are recognized downstream.
        let blocks = parse("```suggestion\nfn fixed() {}\n```");
        assert_eq!(
            blocks,
            vec![Block::Code {
                lang: Some("suggestion".into()),
                code: "fn fixed() {}".into(),
            }]
        );
        // Extra info words ("rust ignore") keep only the first token;
        // indented code blocks carry None.
        let blocks = parse("```rust ignore\nx\n```");
        assert_eq!(
            blocks,
            vec![Block::Code {
                lang: Some("rust".into()),
                code: "x".into(),
            }]
        );
        let blocks = parse("    indented");
        assert_eq!(
            blocks,
            vec![Block::Code {
                lang: None,
                code: "indented".into(),
            }]
        );
    }

    #[test]
    fn ordered_list_carries_start_index() {
        let blocks = parse("3. c\n4. d");
        let Block::List {
            ordered,
            start,
            items,
        } = &blocks[0]
        else {
            panic!("list expected");
        };
        assert!(ordered);
        assert_eq!(*start, 3);
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.task.is_none()));
    }

    #[test]
    fn task_list_markers_are_captured() {
        let blocks = parse("- [x] done\n- [ ] todo\n- plain");
        let Block::List { items, .. } = &blocks[0] else {
            panic!("list expected");
        };
        assert_eq!(
            items.iter().map(|i| i.task).collect::<Vec<_>>(),
            vec![Some(true), Some(false), None]
        );
    }

    #[test]
    fn nested_list_lives_inside_item() {
        let blocks = parse("- top\n  - inner");
        let Block::List { items, .. } = &blocks[0] else {
            panic!("list expected");
        };
        assert_eq!(items.len(), 1);
        let nested = items[0]
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::List { items, .. } => Some(items.len()),
                _ => None,
            })
            .expect("nested list");
        assert_eq!(nested, 1);
    }

    #[test]
    fn blockquotes_nest_and_carry_text() {
        let blocks = parse("> quoted line");
        let Block::Quote { alert, blocks } = &blocks[0] else {
            panic!("quote expected");
        };
        assert!(alert.is_none());
        assert!(matches!(&blocks[0], Block::Paragraph(r) if r[0].text == "quoted line"));
    }

    #[test]
    fn github_alerts_are_recognized() {
        for (src, alert) in [
            ("> [!NOTE]\n> note", Alert::Note),
            ("> [!TIP]\n> tip", Alert::Tip),
            ("> [!IMPORTANT]\n> important", Alert::Important),
            ("> [!WARNING]\n> warning", Alert::Warning),
            ("> [!CAUTION]\n> caution", Alert::Caution),
        ] {
            let blocks = parse(src);
            let Block::Quote { alert: got, .. } = &blocks[0] else {
                panic!("quote expected for {src}");
            };
            assert_eq!(*got, Some(alert));
        }
    }

    #[test]
    fn unknown_alert_tag_stays_plain_quote() {
        let blocks = parse("> [!SOMETHING]\n> body");
        let Block::Quote { alert, blocks } = &blocks[0] else {
            panic!("quote expected");
        };
        assert!(alert.is_none());
        let joined: String = blocks
            .iter()
            .map(|b| match b {
                Block::Paragraph(runs) => runs.iter().map(|r| r.text.as_str()).collect::<String>(),
                _ => String::new(),
            })
            .collect();
        assert!(joined.contains("[!SOMETHING]"));
        assert!(joined.contains("body"));
    }

    #[test]
    fn tables_split_head_and_rows() {
        let blocks = parse("| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |");
        let Block::Table { head, rows } = &blocks[0] else {
            panic!("table expected");
        };
        assert_eq!(head.len(), 2);
        assert_eq!(head[0][0].text, "a");
        assert_eq!(head[1][0].text, "b");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1][0][0].text, "3");
    }

    #[test]
    fn rule_is_a_block() {
        let blocks = parse("above\n\n---\n\nbelow");
        assert_eq!(blocks[1], Block::Rule);
    }
}
