//! @-mention completion for the GitHub composers (docs/0075, #578).
//!
//! The pinned stack has no public completion path for plain textareas
//! (docs/0075 Background): the kit's completion machinery is `pub(crate)`
//! and EditorState-only. This module rebuilds the interaction in-app on the
//! public `InputBaseState` API — D1 in-app popup, D2 caret-anchored with a
//! viewport-flip, D3 trigger/token rules, D6 candidate ranking. The popup
//! element mirrors the kit's caret-anchored pattern: a zero-size element
//! inside `deferred()` whose prepaint resolves the anchor from the input's
//! fresh `PrepaintCaretGeometry` and paints the row list at window coords.
//!
//! The pure half (`scan_token`, `ranked_candidates`, `filter_candidates`)
//! carries the D3/D6 contracts and is unit-tested without gpui; the element
//! is exercised by the composer's integration tests (pr_page).

use std::ops::Range;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Sizable as _, ThemeStyled as _,
    avatar::Avatar,
    h_flex,
    input::{PrepaintCaretGeometry, TextareaState},
    v_flex,
};

use std::rc::Rc;

use crate::gql::{IssueDetail, PrDetail};
use crate::issue_page::IssueDetailPage;
use crate::pr_page::PrDetailPage;

/// D6: filtered results cap — 10 rows fits the ~240px popup budget.
pub(crate) const MAX_ROWS: usize = 10;

/// Row height used for the popup's flip math (avatar + one text line).
const ROW_H: Pixels = px(26.0);
/// Gap between the caret line and the popup edge (kit's completion gap).
const GAP: Pixels = px(4.0);
/// Popup width — GitHub's mention list is a narrow column, not a sheet.
const POPUP_W: Pixels = px(300.0);

/// One mentionable person (D6). `name` stays empty until the loaded
/// document carries display names — filtering and rendering degrade to
/// login-only without a schema change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MentionCandidate {
    pub login: String,
    pub name: String,
    pub avatar_url: String,
}

/// Which composer an open popup belongs to (#578). One page-level state
/// slot serves every composer: the tag routes sync/accept to the right
/// textarea handle, and each render site gates on its own tag — so at
/// most one popup exists per page, and typing in another composer
/// replaces (or closes) the old one, matching blur-dismiss semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MentionComposer {
    /// PR detail — the main comment box (docs/0075 D8).
    Comment,
    /// Files tab — the single-line review comment composer (0030 D4).
    LineComposer,
    /// Review thread — the reply composer (0030).
    Reply,
    /// Review thread — the prefilled comment edit (0030).
    ThreadEdit,
    /// Issue detail — the comment box (0020 D4).
    IssueComment,
}

/// Open-popup state, held by the page (D3 lifecycle). Offsets are char
/// offsets — the units `TextareaState::cursor()` and
/// `set_selected_range()` speak (ropey slice semantics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MentionState {
    /// Char offset of the `@` that opened the popup.
    pub token_start: usize,
    /// Which composer owns this popup.
    pub composer: MentionComposer,
    /// Query text between `@` and the caret (no `@`).
    pub query: String,
    /// Filtered + ranked rows (D6), ≤ [`MAX_ROWS`].
    pub candidates: Vec<MentionCandidate>,
    pub selected: usize,
}

/// D3 trigger scan: the whitespace-delimited run ending at `cursor` must
/// start with `@`, and that `@` must sit at line start or right after
/// whitespace (`user@example.com` never triggers). The query is the text
/// between the `@` and the caret — empty at a bare `@`, which opens the
/// unfiltered list, matching GitHub. Returns the `@`'s char offset.
pub(crate) fn scan_token(chars: &[char], cursor: usize) -> Option<(usize, String)> {
    if cursor == 0 || cursor > chars.len() {
        return None;
    }
    let mut start = cursor;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    if chars.get(start) != Some(&'@') {
        return None;
    }
    // An `@` at or after the caret has not been typed up to the caret yet
    // (cursor on the whitespace before it) — no token, no popup.
    if cursor <= start {
        return None;
    }
    Some((start, chars[start + 1..cursor].iter().collect::<String>()))
}

/// D6 ranking from the already-loaded document — review-requested
/// reviewers, then assignees, then the PR author, then comment and review
/// authors by recency (document order is oldest→newest, so walk in
/// reverse). Dedup by login, first occurrence wins.
pub(crate) fn ranked_candidates(d: &PrDetail) -> Vec<MentionCandidate> {
    let mut out: Vec<MentionCandidate> = Vec::new();
    let push = |out: &mut Vec<MentionCandidate>, login: &str, avatar: &str| {
        if login.is_empty() || out.iter().any(|c| c.login == login) {
            return;
        }
        out.push(MentionCandidate {
            login: login.to_string(),
            name: String::new(),
            avatar_url: avatar.to_string(),
        });
    };
    for r in d.review_requests.iter().filter(|r| r.is_user()) {
        push(&mut out, &r.login, "");
    }
    for a in &d.assignees {
        push(&mut out, &a.login, &a.avatar_url);
    }
    if let Some(a) = &d.author {
        push(&mut out, &a.login, &a.avatar_url);
    }
    for c in d.comments.nodes.iter().rev() {
        if let Some(a) = &c.author {
            push(&mut out, &a.login, &a.avatar_url);
        }
    }
    for r in d.reviews.nodes.iter().rev() {
        if let Some(a) = &r.author {
            push(&mut out, &a.login, &a.avatar_url);
        }
    }
    out
}

/// D6 for issues (#578): no review requests or reviews exist, so the
/// ranking degrades to assignees → issue author → comment authors by
/// recency — same dedup, same cap, zero new GraphQL.
pub(crate) fn ranked_issue_candidates(d: &IssueDetail) -> Vec<MentionCandidate> {
    let mut out: Vec<MentionCandidate> = Vec::new();
    let push = |out: &mut Vec<MentionCandidate>, login: &str, avatar: &str| {
        if login.is_empty() || out.iter().any(|c| c.login == login) {
            return;
        }
        out.push(MentionCandidate {
            login: login.to_string(),
            name: String::new(),
            avatar_url: avatar.to_string(),
        });
    };
    for a in &d.assignees {
        push(&mut out, &a.login, &a.avatar_url);
    }
    if let Some(a) = &d.author {
        push(&mut out, &a.login, &a.avatar_url);
    }
    for c in d.comments.nodes.iter().rev() {
        if let Some(a) = &c.author {
            push(&mut out, &a.login, &a.avatar_url);
        }
    }
    out
}

/// One D3 rescan for any composer (#578): scan the textarea under the
/// caret, filter the ranked pool, and build the next state — `None`
/// means closed. Same token+query on the same composer keeps the state
/// untouched (stable selection); a tag change always rebuilds, so
/// typing into another composer replaces the popup.
pub(crate) fn rescan(
    current: &Option<MentionState>,
    textarea: &Entity<TextareaState>,
    composer: MentionComposer,
    pool: &[MentionCandidate],
    cx: &App,
) -> Option<MentionState> {
    let value = textarea.read(cx).value().to_string();
    let chars: Vec<char> = value.chars().collect();
    let cursor = textarea.read(cx).cursor().min(chars.len());
    let (token_start, query) = scan_token(&chars, cursor)?;
    let same = current.as_ref().is_some_and(|m| {
        m.composer == composer && m.token_start == token_start && m.query == query
    });
    if same {
        return current.clone();
    }
    let candidates = filter_candidates(pool, &query);
    (!candidates.is_empty()).then_some(MentionState {
        token_start,
        composer,
        query,
        candidates,
        selected: 0,
    })
}

/// D6 filtering: case-insensitive substring against login, then name;
/// ranked order preserved, capped at [`MAX_ROWS`].
pub(crate) fn filter_candidates(pool: &[MentionCandidate], query: &str) -> Vec<MentionCandidate> {
    let q = query.to_lowercase();
    pool.iter()
        .filter(|c| {
            c.login.to_lowercase().contains(&q)
                || (!c.name.is_empty() && c.name.to_lowercase().contains(&q))
        })
        .take(MAX_ROWS)
        .cloned()
        .collect()
}

/// Wrap-around selection move (D4).
pub(crate) fn step_selection(selected: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (selected as isize + delta).rem_euclid(len as isize) as usize
}

/// D2 anchor: window-coords origin for the popup, mirroring the kit's
/// caret-anchor math (`cursor_bounds` + line height + gap) with a flip
/// above the caret when the below-caret placement would overflow the
/// viewport.
fn popup_anchor(geo: &PrepaintCaretGeometry, rows: usize, viewport: Size<Pixels>) -> Point<Pixels> {
    let height = rows as f32 * ROW_H + px(10.0);
    let caret = geo.cursor_bounds().origin + gpui::point(px(0.0), geo.scroll_offset().y);
    let below = caret.y + geo.line_height() + GAP;
    let y = if below + height + px(8.0) <= viewport.height {
        below
    } else {
        (caret.y - GAP - height).max(px(0.0))
    };
    Point::new(caret.x - px(4.0), y)
}

/// The deferred, zero-size popup element (D1/D2): prepaint reads the
/// input's fresh caret geometry, builds the row list from the page's
/// mention state, and paints it at the anchor — outside every scroll
/// container's clip.
/// Fresh popup state each prepaint — pages hold the one mention slot,
/// the closure just reads it through whatever page owns it.
pub(crate) type MentionSnapshot = Rc<dyn Fn(&App) -> Option<MentionState>>;
/// Row-click accept, routed by the owning page to the composer the open
/// state is tagged with (#578).
pub(crate) type MentionPick = Rc<dyn Fn(usize, String, &mut Window, &mut App)>;

pub(crate) struct MentionPopup {
    comment: WeakEntity<TextareaState>,
    mention: MentionSnapshot,
    pick: MentionPick,
    content: Option<AnyElement>,
}

impl MentionPopup {
    pub(crate) fn new(
        comment: WeakEntity<TextareaState>,
        mention: MentionSnapshot,
        pick: MentionPick,
    ) -> Self {
        Self {
            comment,
            mention,
            pick,
            content: None,
        }
    }
}

/// The PR detail page's popup wiring: state read + accept routed
/// through [`PrDetailPage`], anchored at `textarea`.
pub(crate) fn pr_popup(
    page: WeakEntity<PrDetailPage>,
    textarea: WeakEntity<TextareaState>,
) -> MentionPopup {
    let pick_page = page.clone();
    MentionPopup::new(
        textarea,
        Rc::new(move |cx| page.upgrade().and_then(|p| p.read(cx).mention.clone())),
        Rc::new(move |token_start, login, window, cx| {
            let Some(page) = pick_page.upgrade() else {
                return;
            };
            page.update(cx, |page, cx| {
                page.accept_mention(token_start, login, window, cx)
            });
        }),
    )
}

/// The issue detail page's popup wiring (#578) — same shape, issue page.
pub(crate) fn issue_popup(
    page: WeakEntity<IssueDetailPage>,
    textarea: WeakEntity<TextareaState>,
) -> MentionPopup {
    let pick_page = page.clone();
    MentionPopup::new(
        textarea,
        Rc::new(move |cx| page.upgrade().and_then(|p| p.read(cx).mention.clone())),
        Rc::new(move |token_start, login, window, cx| {
            let Some(page) = pick_page.upgrade() else {
                return;
            };
            page.update(cx, |page, cx| {
                page.accept_mention(token_start, login, window, cx)
            });
        }),
    )
}

impl Element for MentionPopup {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (window.request_layout(Style::default(), [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: gpui::Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(comment) = self.comment.upgrade() else {
            return;
        };
        let Some(geo) = comment.read(cx).prepaint_caret_geometry() else {
            return;
        };
        let Some(mention) = (self.mention)(cx) else {
            return;
        };
        let rows = mention.candidates.len().min(MAX_ROWS);
        if rows == 0 {
            return;
        }
        let anchor = popup_anchor(&geo, rows, window.viewport_size());
        let mut content = render_rows(&self.pick, &mention, window, cx);
        let viewport = window.viewport_size();
        content.layout_as_root(
            Size {
                width: AvailableSpace::Definite(viewport.width),
                height: AvailableSpace::Definite(viewport.height),
            },
            window,
            cx,
        );
        content.prepaint_at(anchor, window, cx);
        self.content = Some(content);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: gpui::Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(content) = self.content.as_mut() {
            content.paint(window, cx);
        }
    }
}

impl IntoElement for MentionPopup {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// D1/D4: the kit's themed popover surface, listbox roles, per-row ids
/// (docs/0010 D10) — the bridge sees "Mention suggestions" and its rows.
/// Clicking a row accepts it: the closure captures the token start and
/// login at build time, so it stays correct even if a blur cleared the
/// page's mention state mid-dispatch.
fn render_rows(
    pick: &MentionPick,
    mention: &MentionState,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let pick = pick.clone();
    let token_start = mention.token_start;
    let selected = mention.selected;
    v_flex()
        .id("mention-popup")
        .occlude()
        .w(POPUP_W)
        .p_1()
        .popover_style(cx)
        .role(accesskit::Role::Group)
        .aria_label("Mention suggestions")
        .children(mention.candidates.iter().enumerate().map(|(ix, cand)| {
            let row_pick = pick.clone();
            let login = cand.login.clone();
            h_flex()
                .id(ix)
                .h(ROW_H)
                .w_full()
                .items_center()
                .gap_2()
                .px_2()
                .rounded(cx.theme().radius.half())
                .role(accesskit::Role::ListItem)
                .aria_label(format!("@{}", cand.login))
                .when(ix == selected, |row| {
                    row.bg(cx.theme().accent)
                        .text_color(cx.theme().accent_foreground)
                        .aria_selected(true)
                })
                .hover(|row| row.bg(cx.theme().accent.opacity(0.5)))
                .on_mouse_down(MouseButton::Left, move |_, window, cx: &mut App| {
                    row_pick(token_start, login.clone(), window, cx);
                    cx.stop_propagation();
                })
                .child(Avatar::new().src(cand.avatar_url.clone()).xsmall())
                .child(
                    div()
                        .text_sm()
                        .child(gpui::SharedString::from(format!("@{}", cand.login))),
                )
        }))
        .into_any_element()
}

/// D5: replace the `@token` ending at `end` with `@login ` in one atomic
/// edit. Char offsets throughout — the state's native units.
pub(crate) fn replace_token_range(
    state: &mut TextareaState,
    token_start: usize,
    end: usize,
    login: &str,
    window: &mut Window,
    cx: &mut Context<TextareaState>,
) {
    let end = end.max(token_start);
    state.set_selected_range(
        Range {
            start: token_start,
            end,
        },
        cx,
    );
    state.replace(format!("@{login} "), window, cx);
}

#[cfg(test)]
mod tests {
    use crate::gql::{Author, IssueAssignee, PrComment, PrDetail, PrReview, PrReviewRequest};
    use crate::mentions::{
        MentionCandidate, filter_candidates, ranked_candidates, scan_token, step_selection,
    };
    // NOTE: this module's tests must stay free of `serde_json::json!` /
    // parser fixtures — a fixture here SIGBUSes rustc deterministically
    // (same expansion hazard page.rs documents for `use super::*`). The
    // `ranked_issue_candidates` fixture lives in issue_page.rs tests.

    fn cand(login: &str) -> MentionCandidate {
        MentionCandidate {
            login: login.into(),
            name: String::new(),
            avatar_url: String::new(),
        }
    }

    fn scan(s: &str, cursor: usize) -> Option<(usize, String)> {
        let chars: Vec<char> = s.chars().collect();
        scan_token(&chars, cursor)
    }

    // D3 — trigger rules.

    #[test]
    fn bare_at_opens_with_empty_query() {
        assert_eq!(scan("@", 1), Some((0, String::new())));
    }

    #[test]
    fn at_after_space_triggers_with_query() {
        assert_eq!(scan("hi @ali", 7), Some((3, "ali".to_string())));
        assert_eq!(scan("x\n@", 3), Some((2, String::new())));
        assert_eq!(scan("\t@ab", 4), Some((1, "ab".to_string())));
    }

    #[test]
    fn mid_word_at_never_triggers() {
        assert_eq!(scan("a@b", 3), None);
        assert_eq!(scan("user@example.com", 16), None);
        assert_eq!(scan("email me user@example.com now", 21), None);
    }

    #[test]
    fn cursor_at_or_before_at_is_noop() {
        assert_eq!(scan("@abc", 0), None);
        assert_eq!(scan("hi @", 3), None); // cursor on the space
    }

    #[test]
    fn cursor_out_of_range_is_noop() {
        assert_eq!(scan("@", 9), None);
    }

    #[test]
    fn cursor_mid_token_reads_query_prefix() {
        assert_eq!(scan("@alice bob", 4), Some((0, "ali".to_string())));
    }

    // D6 — ranking and filtering.

    fn detail_fixture() -> PrDetail {
        let author = |login: &str| {
            Some(Author {
                login: login.into(),
                avatar_url: format!("https://a/{login}.png"),
            })
        };
        let comment = |login: &str| PrComment {
            author: author(login),
            body: String::new(),
            created_at: None,
            id: String::new(),
            database_id: None,
            viewer_can_update: false,
            viewer_can_delete: false,
            updated_at: None,
            viewer_can_react: false,
            reaction_groups: Vec::new(),
        };
        let review = |login: &str| PrReview {
            author: author(login),
            state: None,
            body: None,
            submitted_at: None,
            id: String::new(),
            viewer_can_update: false,
            viewer_can_delete: false,
            updated_at: None,
            viewer_can_react: false,
            reaction_groups: Vec::new(),
        };
        PrDetail {
            id: String::new(),
            number: 7,
            title: String::new(),
            body: None,
            state: None,
            draft: false,
            viewer_can_update: false,
            viewer_can_update_branch: false,
            viewer_can_close: false,
            viewer_can_reopen: false,
            viewer_default_merge_method: None,
            viewer_can_react: false,
            reaction_groups: Vec::new(),
            labels: Vec::new(),
            labels_total: 0,
            assignees_total: 1,
            assignees: vec![IssueAssignee {
                id: None,
                login: "assignee".into(),
                avatar_url: "https://a/assignee.png".into(),
            }],
            milestone: None,
            review_requests: vec![PrReviewRequest {
                id: None,
                login: "reviewer".into(),
                ..Default::default()
            }],
            review_requests_total: 1,
            ending: Default::default(),
            queueing: Default::default(),
            head_branch: None,
            review_decision: None,
            latest_opinionated_reviews: Vec::new(),
            latest_reviews: Vec::new(),
            viewer_can_label: false,
            viewer_can_assign: false,
            viewer_permission: None,
            mergeable: None,
            merge_state_status: None,
            rollup_state: None,
            merge_methods: Default::default(),
            changed_files: None,
            base_ref: None,
            head_ref: None,
            head_oid: None,
            url: String::new(),
            created_at: None,
            updated_at: None,
            author: author("prauthor"),
            commits: Default::default(),
            checks: Default::default(),
            reviews: crate::gql::PrReviews {
                total_count: 1,
                nodes: vec![review("reviewauthor")],
            },
            comments: crate::gql::PrComments {
                total_count: 2,
                nodes: vec![comment("oldcommenter"), comment("newcommenter")],
            },
        }
    }

    #[test]
    fn ranking_follows_d6_order_with_dedup() {
        let mut d = detail_fixture();
        // A login seen earlier must not be re-added from a later pool.
        d.reviews.nodes.push(PrReview {
            author: Some(Author {
                login: "reviewer".into(),
                avatar_url: String::new(),
            }),
            state: None,
            body: None,
            submitted_at: None,
            id: String::new(),
            viewer_can_update: false,
            viewer_can_delete: false,
            updated_at: None,
            viewer_can_react: false,
            reaction_groups: Vec::new(),
        });
        let ranked = ranked_candidates(&d);
        let logins: Vec<&str> = ranked.iter().map(|c| c.login.as_str()).collect();
        assert_eq!(
            logins,
            vec![
                "reviewer",     // review requests first
                "assignee",     // then assignees
                "prauthor",     // then the PR author
                "newcommenter", // comment authors, most recent first
                "oldcommenter",
                "reviewauthor", // review authors, most recent first
            ]
        );
        // Dedup: the review-pool "reviewer" did not duplicate the
        // review-request entry.
        assert_eq!(logins.iter().filter(|l| **l == "reviewer").count(), 1);
    }

    #[test]
    fn filter_is_case_insensitive_substring_capped() {
        let pool: Vec<MentionCandidate> = (0..15)
            .map(|ix| cand(&format!("User{ix:02}")))
            .chain([cand("nomatch-xyz")])
            .collect();
        let hits = filter_candidates(&pool, "user0");
        assert_eq!(hits.len(), 10); // User00..User09 — cap, ranked order
        assert_eq!(hits[0].login, "User00");
        assert!(filter_candidates(&pool, "nomatch-xyz").len() == 1);
        assert!(filter_candidates(&pool, "zzz").is_empty());
    }

    #[test]
    fn selection_wraps_both_ways() {
        assert_eq!(step_selection(0, 3, -1), 2);
        assert_eq!(step_selection(2, 3, 1), 0);
        assert_eq!(step_selection(1, 3, 1), 2);
        assert_eq!(step_selection(0, 0, 1), 0);
    }
}
