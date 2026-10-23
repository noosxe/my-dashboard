//! Commits tab (docs/0081 stages 2–3): the day-grouped virtual list over
//! a commits book (D5/D6) and its pure model — local-day sections, the
//! loaded-rows quick filter (F2) and the row's accessible name — plus
//! the D7 branch selector's items, search delegate and branch source.
//!
//! Ownership: the book lives on `GithubState::commit_lanes` (D3); the
//! page copies the active book's rows into [`CommitsDelegate`] through
//! [`CommitsDelegate::set_book`] whenever the state or the shared
//! quick filter changes. The delegate never dispatches a fetch itself —
//! `load_more` hops to the page, which owns the spec runner.

use chrono::{DateTime, NaiveDate, TimeZone};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Icon, IconName, IndexPath, Sizable as _,
    avatar::Avatar,
    button::{Button, ButtonVariants},
    h_flex,
    list::{ListDelegate, ListItem, ListState},
    searchable_list::{SearchableListDelegate, SearchableListItem, SearchableVec},
    spinner::Spinner,
    tag::Tag,
    tooltip::Tooltip,
    v_flex,
};

use dashboard_core::freshness::relative_age;

use crate::card::GIT_BRANCH;
use crate::gql::{self, CommitRow};
use crate::page::{GithubPage, ci_row_glyph};
use crate::service::{GithubState, RefKey, SpecResult};
use crate::usage::{Origin, Surface, Trigger};

/// One local calendar day of commits (D5): `start..start + len` into
/// the filtered rows. `day` is `None` for rows whose `committedDate`
/// doesn't parse — they keep their place under an undated header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DaySection {
    pub day: Option<NaiveDate>,
    pub start: usize,
    pub len: usize,
}

/// The local calendar day a commit landed on (D5 groups by the reader's
/// day, not UTC's).
fn commit_day<Tz: TimeZone>(row: &CommitRow, tz: &Tz) -> Option<NaiveDate> {
    DateTime::parse_from_rfc3339(&row.committed_date)
        .ok()
        .map(|t| t.with_timezone(tz).date_naive())
}

/// Group newest-first rows into consecutive day sections (D5). Derived
/// from the held rows each sync, so a day split across a page boundary
/// merges as soon as the next page lands.
pub(crate) fn day_sections<Tz: TimeZone>(rows: &[CommitRow], tz: &Tz) -> Vec<DaySection> {
    let mut sections: Vec<DaySection> = Vec::new();
    for (ix, row) in rows.iter().enumerate() {
        let day = commit_day(row, tz);
        match sections.last_mut() {
            Some(last) if last.day == day => last.len += 1,
            _ => sections.push(DaySection {
                day,
                start: ix,
                len: 1,
            }),
        }
    }
    sections
}

/// The day header's text (D5): "Commits on Oct 21, 2026".
pub(crate) fn day_label(day: Option<NaiveDate>) -> String {
    match day {
        Some(d) => format!("Commits on {}", d.format("%b %-d, %Y")),
        None => "Commits on an unknown date".to_string(),
    }
}

/// The header's debug id day (D6): `commits-day-<yyyy-mm-dd>`.
fn day_key(day: Option<NaiveDate>) -> String {
    match day {
        Some(d) => d.format("%Y-%m-%d").to_string(),
        None => "unknown".to_string(),
    }
}

/// The name a row shows for its author (D5): the linked login, else the
/// git author name for unlinked emails.
pub(crate) fn author_name(row: &CommitRow) -> String {
    row.author
        .as_ref()
        .and_then(|a| {
            a.user
                .as_ref()
                .map(|u| u.login.clone())
                .or_else(|| a.name.clone())
        })
        .unwrap_or_default()
}

/// The verification tag (D5): `Some(true)` = Verified, `Some(false)` =
/// Unverified (a signature exists but is invalid), `None` = unsigned —
/// GitHub's default without vigilant mode.
pub(crate) fn verification(row: &CommitRow) -> Option<bool> {
    row.signature.as_ref().map(|s| s.is_valid)
}

/// The quick filter over the **loaded** commits (D6/F2): a
/// case-insensitive substring over the headline, author and short SHA,
/// or a prefix of the full SHA. Empty keeps every row.
pub(crate) fn filter_commits(rows: &[CommitRow], query: &str) -> Vec<CommitRow> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return rows.to_vec();
    }
    rows.iter()
        .filter(|r| {
            r.message_headline.to_lowercase().contains(&q)
                || author_name(r).to_lowercase().contains(&q)
                || r.abbreviated_oid.to_lowercase().contains(&q)
                || r.oid.to_lowercase().starts_with(&q)
        })
        .cloned()
        .collect()
}

/// The row's accessible name (D6, docs/0049 D1), in reading order:
/// `"<headline> — <login>, committed <relative>, <Verified|Unverified>,
/// <CI text>, <short sha>"`. The unsigned and no-CI parts are omitted,
/// never left as empty commas.
pub(crate) fn commit_label(row: &CommitRow, relative: &str, ci_text: Option<&str>) -> String {
    let mut parts = Vec::new();
    let author = author_name(row);
    if !author.is_empty() {
        parts.push(author);
    }
    if !relative.is_empty() {
        parts.push(format!("committed {relative}"));
    }
    match verification(row) {
        Some(true) => parts.push("Verified".to_string()),
        Some(false) => parts.push("Unverified".to_string()),
        None => {}
    }
    if let Some(ci) = ci_text {
        parts.push(ci.to_string());
    }
    parts.push(row.abbreviated_oid.clone());
    format!("{} — {}", row.message_headline, parts.join(", "))
}

/// The first 12 hex digits of an oid — the row's debug-id stem (D6).
pub(crate) fn oid12(oid: &str) -> &str {
    &oid[..oid.len().min(12)]
}

/// The Commits list's delegate (D5/D6): the active book's rows after
/// the quick filter, sectioned by local day.
pub(crate) struct CommitsDelegate {
    page: WeakEntity<GithubPage>,
    /// The filtered rows, newest first.
    rows: Vec<CommitRow>,
    sections: Vec<DaySection>,
    /// The quick filter (D6): non-empty turns paging off.
    query: String,
    /// The book may page (`CommitLane::can_load_more`, synced).
    can_more: bool,
    selected: Option<IndexPath>,
}

impl CommitsDelegate {
    pub(crate) fn new(page: WeakEntity<GithubPage>) -> Self {
        Self {
            page,
            rows: Vec::new(),
            sections: Vec::new(),
            query: String::new(),
            can_more: false,
            selected: None,
        }
    }

    /// Replace the rows from the book, re-applying the quick filter and
    /// re-deriving the day sections. Compare-first: returns `false`
    /// when nothing the list renders changed, so the caller can skip
    /// the notify.
    pub(crate) fn set_book(&mut self, rows: &[CommitRow], query: &str, can_more: bool) -> bool {
        let filtered = filter_commits(rows, query);
        if filtered == self.rows && query == self.query && can_more == self.can_more {
            return false;
        }
        self.sections = day_sections(&filtered, &chrono::Local);
        self.rows = filtered;
        self.query = query.to_string();
        self.can_more = can_more;
        true
    }

    /// The quick filter the rows were last derived with.
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// The filtered rows (the tail strip counts them).
    pub(crate) fn rows(&self) -> &[CommitRow] {
        &self.rows
    }

    #[cfg(test)]
    pub(crate) fn sections(&self) -> &[DaySection] {
        &self.sections
    }

    /// Paging is off while the quick filter has text (D6/F2): typing
    /// never chain-fetches history.
    fn may_page(&self) -> bool {
        self.can_more && self.query.trim().is_empty()
    }

    /// The flat row index of a list position.
    fn flat_index(&self, ix: IndexPath) -> Option<usize> {
        let section = self.sections.get(ix.section)?;
        (ix.row < section.len).then_some(section.start + ix.row)
    }

    fn row_at(&self, ix: IndexPath) -> Option<&CommitRow> {
        self.rows.get(self.flat_index(ix)?)
    }
}

impl ListDelegate for CommitsDelegate {
    type Item = ListItem;

    fn sections_count(&self, _cx: &App) -> usize {
        self.sections.len()
    }

    fn items_count(&self, section: usize, _cx: &App) -> usize {
        self.sections.get(section).map(|s| s.len).unwrap_or(0)
    }

    fn render_section_header(
        &mut self,
        section: usize,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<impl IntoElement> {
        // The measure pass asks for section 0 even before any rows exist
        // (list.rs prepare_items_if_needed) — a sized placeholder keeps
        // the measured header height honest.
        let day = self.sections.get(section).and_then(|s| s.day);
        let text = day_label(day);
        let id = format!("commits-day-{}", day_key(day));
        Some(
            div()
                .id(SharedString::from(id.clone()))
                .debug_selector(move || id.clone())
                .role(accesskit::Role::Heading)
                .aria_level(3)
                .aria_label(text.clone())
                .px_2()
                .pt_3()
                .pb_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(text),
        )
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let row = self.row_at(ix)?.clone();
        Some(commit_row(&row, cx))
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix;
    }

    /// Click / Enter opens the commit on github.com (D6, Q1);
    /// secondary-confirm (⌘/Ctrl+Enter or ⌘-click) copies the full SHA —
    /// the keyboard path to the clipboard.
    fn confirm(
        &mut self,
        secondary: bool,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        let Some(row) = self.selected.and_then(|ix| self.row_at(ix)) else {
            return;
        };
        if secondary {
            log::debug!("commits: copied {}", row.oid);
            cx.write_to_clipboard(ClipboardItem::new_string(row.oid.clone()));
        } else {
            log::debug!("commits: opened {} on GitHub", row.abbreviated_oid);
            cx.open_url(&row.url);
        }
    }

    fn has_more(&self, _cx: &App) -> bool {
        self.may_page()
    }

    fn load_more(&mut self, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        let Some(page) = self.page.upgrade() else {
            return;
        };
        page.update(cx, |p, cx| {
            p.load_more_commits(
                Origin::new(Surface::RepoView, Trigger::Paginate),
                window,
                cx,
            )
        });
    }
}

/// One fixed two-line commit row (D5).
fn commit_row(row: &CommitRow, cx: &App) -> ListItem {
    let key = oid12(&row.oid).to_string();
    let author = author_name(row);
    let relative = relative_age(Some(&row.committed_date));
    let rollup = gql::rollup_state(
        row.status_check_rollup
            .as_ref()
            .and_then(|r| r.state.as_deref()),
    );
    let ci_text = rollup.map(|s| ci_row_glyph(s, cx).2);
    let label = commit_label(row, &relative, ci_text);
    let absolute = DateTime::parse_from_rfc3339(&row.committed_date)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%b %-d, %Y, %H:%M")
                .to_string()
        })
        .unwrap_or_default();
    let muted = cx.theme().muted_foreground;

    ListItem::new(SharedString::from(format!("commit-{key}")))
        .debug_selector({
            let probe = format!("commit-{key}");
            move || probe.clone()
        })
        // The `ListState` wraps each row in its own unnamed `ListItem`
        // node (position, size, selected) and takes no label; a label on
        // a role-less element makes no node at all. A `Group` role gives
        // the D6 name a node of its own inside that item.
        .role(accesskit::Role::Group)
        .accessibility_label(label)
        .px_2()
        .py_1p5()
        .rounded_sm()
        .child(
            v_flex()
                .w_full()
                .min_w_0()
                .gap_0p5()
                // Line 1: headline, then the verification tag and CI glyph.
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .child(row.message_headline.clone()),
                        )
                        .when_some(verification(row), |line, valid| {
                            line.child(if valid {
                                Tag::success().outline().small().child("Verified")
                            } else {
                                Tag::secondary().outline().small().child("Unverified")
                            })
                        })
                        .when_some(rollup, |line, state| {
                            let (icon, color, tip) = ci_row_glyph(state, cx);
                            let id: SharedString = format!("commit-ci-{key}").into();
                            line.child(
                                div()
                                    .id(id.clone())
                                    .debug_selector(move || id.to_string())
                                    .flex_none()
                                    .child(if state == gql::CheckState::Pending {
                                        Spinner::new().icon(icon).color(color).into_any_element()
                                    } else {
                                        Icon::new(icon)
                                            .size_4()
                                            .text_color(color)
                                            .into_any_element()
                                    })
                                    .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx)),
                            )
                        }),
                )
                // Line 2: avatar, author, "committed 3h"; the short SHA
                // and its copy button right.
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .text_xs()
                        .text_color(muted)
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .items_center()
                                .child({
                                    let avatar = row.author.as_ref();
                                    Avatar::new()
                                        .when_some(
                                            avatar.and_then(|a| a.avatar_url.clone()),
                                            |a, url| a.src(url),
                                        )
                                        .name(author.clone())
                                        .xsmall()
                                })
                                .child(div().truncate().child(author))
                                .child(
                                    div()
                                        .id(SharedString::from(format!("commit-age-{key}")))
                                        .flex_none()
                                        .child(format!("committed {relative}"))
                                        .when(!absolute.is_empty(), |el| {
                                            el.tooltip(move |window, cx| {
                                                Tooltip::new(absolute.clone()).build(window, cx)
                                            })
                                        }),
                                ),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .gap_1()
                                .items_center()
                                .child(
                                    div()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .child(row.abbreviated_oid.clone()),
                                )
                                .child(CopyShaButton {
                                    id: format!("commit-copy-{key}").into(),
                                    oid: row.oid.clone(),
                                    short: row.abbreviated_oid.clone(),
                                }),
                        ),
                ),
        )
}

/// The row's copy-SHA button (D5). The pinned `Clipboard` renders a
/// tab-stop `Button` with no way to opt out, which would put one tab
/// stop per visible row inside the list — D6 makes the list a single
/// stop with secondary-confirm as the keyboard copy. This is the same
/// button with `tab_stop(false)` and the same two-second check mark.
#[derive(IntoElement)]
struct CopyShaButton {
    id: SharedString,
    oid: String,
    short: String,
}

#[derive(Default)]
struct CopiedState {
    copied: bool,
}

impl RenderOnce for CopyShaButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| CopiedState::default());
        let copied = state.read(cx).copied;
        let probe = self.id.to_string();
        Button::new(self.id.clone())
            .debug_selector(move || probe.clone())
            .icon(if copied {
                IconName::Check
            } else {
                IconName::Copy
            })
            .ghost()
            .xsmall()
            .tab_stop(false)
            .tooltip("Copy full SHA")
            .accessibility_label(format!("Copy SHA {}", self.short))
            .on_click(move |_, _, cx| {
                // The row's own click opens the commit — the copy must
                // not bubble into it.
                cx.stop_propagation();
                cx.write_to_clipboard(ClipboardItem::new_string(self.oid.clone()));
                state.update(cx, |s, cx| {
                    s.copied = true;
                    cx.notify();
                });
                let state = state.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(2))
                        .await;
                    state.update(cx, |s, cx| {
                        s.copied = false;
                        cx.notify();
                    });
                })
                .detach();
            })
    }
}

// --- D7: the branch selector ---------------------------------------------

/// docs/0081 D7 — one branch in the ref selector. The value is the
/// short branch name; the default branch carries a "default" tag in the
/// menu.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RefItem {
    pub name: SharedString,
    pub default: bool,
}

impl SearchableListItem for RefItem {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.name.clone()
    }

    /// The trigger reads as GitHub's branch button: glyph + name.
    fn display_title(&self) -> Option<AnyElement> {
        Some(
            h_flex()
                .gap_1p5()
                .items_center()
                .min_w_0()
                .child(Icon::empty().path(GIT_BRANCH).size_3p5())
                .child(div().truncate().child(self.name.clone()))
                .into_any_element(),
        )
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .justify_between()
            .child(div().truncate().child(self.name.clone()))
            .when(self.default, |row| {
                row.child(Tag::secondary().outline().small().child("default"))
            })
    }

    fn value(&self) -> &SharedString {
        &self.name
    }
}

/// docs/0081 D7 — the selector's items: the default branch first (as
/// GitHub lists it), then the page's branches in its alphabetical
/// order, plus the current ref when the page doesn't hold it (a branch
/// picked from a server search). Deduplicated.
pub(crate) fn ref_items(
    names: &[String],
    default_name: Option<&str>,
    current: Option<&str>,
) -> Vec<RefItem> {
    let mut items: Vec<RefItem> = Vec::with_capacity(names.len() + 2);
    let mut push = |name: &str| {
        if !items.iter().any(|i| i.name.as_ref() == name) {
            items.push(RefItem {
                name: name.to_string().into(),
                default: Some(name) == default_name,
            });
        }
    };
    if let Some(d) = default_name {
        push(d);
    }
    for n in names {
        push(n);
    }
    if let Some(c) = current {
        push(c);
    }
    items
}

/// docs/0081 D7 — the book a picked branch name selects: the default
/// branch shares the `RefKey::Default` book (one book, one clock),
/// anything else is a branch book.
pub(crate) fn ref_key_for(name: &str, default_name: Option<&str>) -> RefKey {
    if Some(name) == default_name {
        RefKey::Default
    } else {
        RefKey::Branch(name.to_string())
    }
}

/// docs/0081 D7 — the branch list behind the selector, page-owned and
/// fetched lazily on the selector's first open per repo selection (the
/// docs/0044 picker-source pattern). Not retained across repos.
#[derive(Default)]
pub(crate) struct RefSource {
    /// The repo this source was fetched for.
    pub repo: Option<String>,
    /// The alphabetical default page: `None` = not fetched yet.
    pub page: Option<Result<gql::RepoBranches, String>>,
    pub loading: bool,
    /// docs/0044 §9 — the server-filtered page for the live query, only
    /// fetched when the default page is truncated (more than 100
    /// branches).
    pub search: Option<gql::RepoBranches>,
    /// Debounce generation for `search`.
    pub search_gen: u64,
}

impl RefSource {
    /// Should opening the selector fetch? A first open for this repo,
    /// or a reopen after a failure (retry on reopen).
    pub(crate) fn needs_fetch(&self, repo: &str) -> bool {
        if self.repo.as_deref() != Some(repo) {
            return true;
        }
        !self.loading && !matches!(self.page, Some(Ok(_)))
    }

    /// The landed default page for `repo`, if any.
    pub(crate) fn page_for(&self, repo: &str) -> Option<&gql::RepoBranches> {
        match (&self.page, self.repo.as_deref() == Some(repo)) {
            (Some(Ok(page)), true) => Some(page),
            _ => None,
        }
    }

    /// The names the selector lists for `repo`: the live search page
    /// while one is held, else the default page.
    pub(crate) fn names_for(&self, repo: &str) -> &[String] {
        if self.repo.as_deref() != Some(repo) {
            return &[];
        }
        self.search
            .as_ref()
            .or(self.page_for(repo))
            .map(|p| p.names.as_slice())
            .unwrap_or(&[])
    }
}

/// docs/0044 D4 — parse a `RepoBranches` result the 0007 raw-envelope
/// way. A non-200 is forwarded to the shared handling first (auth loss,
/// rate-limit pause), then reported as the failure.
pub(crate) fn branches_from_result(
    result: SpecResult,
    state: &Entity<GithubState>,
    cx: &mut App,
) -> Result<gql::RepoBranches, String> {
    if let Some(m) = &result.network {
        return Err(format!("Network error: {m}"));
    }
    if result.status != 200 {
        let status = result.status;
        state.update(cx, |s, cx| {
            s.apply(result);
            cx.notify();
        });
        return Err(format!("GitHub answered HTTP {status}"));
    }
    let body = result.body.ok_or_else(|| "Empty response".to_string())?;
    let env = serde_json::from_str::<gql::Envelope<serde_json::Value>>(&body)
        .map_err(|e| format!("Malformed payload ({e})"))?;
    let empty = serde_json::Value::Null;
    let repo = env
        .data
        .as_ref()
        .and_then(|d| d.get("repository"))
        .unwrap_or(&empty);
    Ok(gql::branches_from_value(repo))
}

/// docs/0044 §9 / docs/0081 D7 — the selector's delegate:
/// [`SearchableVec`]'s instant local filter, plus a ping that lets the
/// page swap in a server-filtered page when the repo has more branches
/// than the loaded page holds.
pub(crate) struct RefSearchDelegate {
    inner: SearchableVec<RefItem>,
    page: WeakEntity<GithubPage>,
}

impl RefSearchDelegate {
    pub(crate) fn new(items: Vec<RefItem>, page: WeakEntity<GithubPage>) -> Self {
        Self {
            inner: SearchableVec::new(items),
            page,
        }
    }
}

impl SearchableListDelegate for RefSearchDelegate {
    type Item = RefItem;

    fn items_count(&self, _section: usize) -> usize {
        self.inner.items_count(0)
    }

    fn item(&self, ix: IndexPath) -> Option<&Self::Item> {
        self.inner.item(ix)
    }

    fn position<V>(&self, value: &V) -> Option<IndexPath>
    where
        Self::Item: SearchableListItem<Value = V>,
        V: PartialEq,
    {
        self.inner.position(value)
    }

    fn perform_search(&mut self, query: &str, window: &mut Window, cx: &mut App) -> Task<()> {
        let local = self.inner.perform_search(query, window, cx);
        let query = query.trim().to_string();
        let _ = self.page.update_in(cx, |page, window, cx| {
            page.queue_ref_search(query, window, cx);
        });
        local
    }
}

#[cfg(test)]
mod tests {
    // Named imports: the parent's `gpui::*` glob carries gpui's own
    // `test` attribute, which would shadow the built-in `#[test]`.
    use super::{CommitsDelegate, commit_label, day_key, day_label, day_sections, filter_commits};
    use crate::gql::{self, CommitRow};
    use chrono::{FixedOffset, NaiveDate};
    use gpui::WeakEntity;

    fn row(oid: &str, headline: &str, date: &str) -> CommitRow {
        CommitRow {
            oid: oid.to_string(),
            abbreviated_oid: oid[..7].to_string(),
            message_headline: headline.to_string(),
            committed_date: date.to_string(),
            authored_date: date.to_string(),
            url: format!("https://github.com/o/r/commit/{oid}"),
            author: Some(gql::CommitActor {
                name: Some("Ada Lovelace".into()),
                email: Some("ada@example.com".into()),
                avatar_url: None,
                user: Some(gql::CommitUser {
                    login: "ada".into(),
                }),
            }),
            signature: None,
            status_check_rollup: None,
        }
    }

    fn oid(n: u8) -> String {
        format!("{:0<40}", format!("{n:x}abc"))
    }

    #[test]
    fn sections_group_by_local_day_across_midnight() {
        // UTC+2: 23:30Z on the 20th is 01:30 on the 21st locally.
        let tz = FixedOffset::east_opt(2 * 3600).unwrap();
        let rows = vec![
            row(&oid(1), "a", "2026-10-21T10:00:00Z"),
            row(&oid(2), "b", "2026-10-20T23:30:00Z"),
            row(&oid(3), "c", "2026-10-20T21:59:00Z"),
            row(&oid(4), "d", "2026-10-19T08:00:00Z"),
        ];
        let s = day_sections(&rows, &tz);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].day, NaiveDate::from_ymd_opt(2026, 10, 21));
        assert_eq!(
            (s[0].start, s[0].len),
            (0, 2),
            "the 23:30Z commit is local Oct 21"
        );
        assert_eq!(s[1].day, NaiveDate::from_ymd_opt(2026, 10, 20));
        assert_eq!((s[1].start, s[1].len), (2, 1));
        assert_eq!((s[2].start, s[2].len), (3, 1));
        // The same rows in UTC split differently — the grouping is local.
        let utc = FixedOffset::east_opt(0).unwrap();
        assert_eq!(day_sections(&rows, &utc).len(), 3);
        assert_eq!(day_sections(&rows, &utc)[0].len, 1);
    }

    #[test]
    fn sections_merge_a_day_split_across_a_page_boundary() {
        let tz = FixedOffset::east_opt(0).unwrap();
        let mut rows = vec![
            row(&oid(1), "a", "2026-10-21T10:00:00Z"),
            row(&oid(2), "b", "2026-10-20T12:00:00Z"),
        ];
        let before = day_sections(&rows, &tz);
        assert_eq!(before.len(), 2);
        // Page 2 lands more commits from Oct 20, then Oct 19.
        rows.push(row(&oid(3), "c", "2026-10-20T08:00:00Z"));
        rows.push(row(&oid(4), "d", "2026-10-19T08:00:00Z"));
        let after = day_sections(&rows, &tz);
        assert_eq!(after.len(), 3, "Oct 20 stays one section");
        // Section indices before the append are stable.
        assert_eq!(after[0], before[0]);
        assert_eq!(after[1].day, before[1].day);
        assert_eq!(after[1].start, before[1].start);
        assert_eq!(after[1].len, 2);
    }

    #[test]
    fn unparseable_dates_keep_their_place() {
        let tz = FixedOffset::east_opt(0).unwrap();
        let rows = vec![
            row(&oid(1), "a", "2026-10-21T10:00:00Z"),
            row(&oid(2), "b", "garbage"),
        ];
        let s = day_sections(&rows, &tz);
        assert_eq!(s.len(), 2);
        assert_eq!(s[1].day, None);
        assert_eq!(day_label(None), "Commits on an unknown date");
    }

    #[test]
    fn day_labels_read_like_github() {
        assert_eq!(
            day_label(NaiveDate::from_ymd_opt(2026, 10, 21)),
            "Commits on Oct 21, 2026"
        );
        assert_eq!(
            day_label(NaiveDate::from_ymd_opt(2026, 1, 3)),
            "Commits on Jan 3, 2026"
        );
        assert_eq!(day_key(NaiveDate::from_ymd_opt(2026, 1, 3)), "2026-01-03");
    }

    #[test]
    fn label_reads_in_order_and_omits_missing_parts() {
        let mut r = row(&oid(1), "Fix the parser", "2026-10-21T10:00:00Z");
        assert_eq!(
            commit_label(&r, "3h", None),
            format!("Fix the parser — ada, committed 3h, {}", r.abbreviated_oid),
            "unsigned and no CI: both omitted"
        );
        r.signature = Some(gql::CommitSignature {
            is_valid: true,
            state: Some("VALID".into()),
            was_signed_by_github: false,
        });
        assert_eq!(
            commit_label(&r, "3h", Some("CI: passing")),
            format!(
                "Fix the parser — ada, committed 3h, Verified, CI: passing, {}",
                r.abbreviated_oid
            )
        );
        r.signature.as_mut().unwrap().is_valid = false;
        assert!(commit_label(&r, "3h", None).contains(", Unverified, "));
        // Unlinked email: the git author name stands in for the login.
        r.author.as_mut().unwrap().user = None;
        assert!(commit_label(&r, "3h", None).starts_with("Fix the parser — Ada Lovelace,"));
    }

    #[test]
    fn quick_filter_matches_loaded_rows_only() {
        let rows = vec![
            row(&oid(1), "Fix the Parser", "2026-10-21T10:00:00Z"),
            row(&oid(2), "Add docs", "2026-10-21T09:00:00Z"),
        ];
        assert_eq!(filter_commits(&rows, "").len(), 2);
        assert_eq!(
            filter_commits(&rows, "  parser ").len(),
            1,
            "case-insensitive"
        );
        assert_eq!(filter_commits(&rows, "ADA").len(), 2, "author login");
        // A full-SHA prefix longer than the short SHA still matches.
        let long = &rows[1].oid[..10];
        let hit = filter_commits(&rows, long);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].oid, rows[1].oid);
        assert!(filter_commits(&rows, "nothing like it").is_empty());
    }

    #[test]
    fn set_book_is_compare_first_and_the_filter_stops_paging() {
        let rows = vec![row(&oid(1), "a", "2026-10-21T10:00:00Z")];
        let mut d = CommitsDelegate::new(WeakEntity::new_invalid());
        assert!(d.set_book(&rows, "", true));
        assert!(!d.set_book(&rows, "", true), "unchanged → no notify");
        assert!(d.may_page());
        assert!(d.set_book(&rows, "zzz", true));
        assert!(d.rows().is_empty());
        assert_eq!(d.sections().len(), 0);
        assert!(!d.may_page(), "a non-empty filter stops paging");
        assert!(d.set_book(&rows, "", false));
        assert!(!d.may_page(), "the book's own gate");
    }

    #[test]
    fn ref_items_put_the_default_first_and_keep_the_pick() {
        use super::ref_items;
        let names = ["feat".to_string(), "main".to_string(), "zeta".to_string()];
        let items = ref_items(&names, Some("main"), Some("main"));
        let got: Vec<(&str, bool)> = items.iter().map(|i| (i.name.as_ref(), i.default)).collect();
        assert_eq!(got, [("main", true), ("feat", false), ("zeta", false)]);
        // A branch picked from a server search stays listed.
        let items = ref_items(&names, Some("main"), Some("far/away"));
        assert_eq!(items.last().unwrap().name.as_ref(), "far/away");
        // Before any fetch: just the current ref.
        let items = ref_items(&[], Some("main"), Some("main"));
        assert_eq!(items.len(), 1);
        assert!(ref_items(&[], None, None).is_empty());
    }

    #[test]
    fn ref_key_maps_the_default_branch_to_the_default_book() {
        use super::ref_key_for;
        use crate::service::RefKey;
        assert_eq!(ref_key_for("main", Some("main")), RefKey::Default);
        assert_eq!(
            ref_key_for("feat", Some("main")),
            RefKey::Branch("feat".into())
        );
        assert_eq!(ref_key_for("main", None), RefKey::Branch("main".into()));
    }

    #[test]
    fn ref_source_fetches_once_per_repo_and_retries_failures() {
        use super::RefSource;
        let mut src = RefSource::default();
        assert!(src.needs_fetch("o/r"));
        src.repo = Some("o/r".into());
        src.loading = true;
        assert!(!src.needs_fetch("o/r"), "in flight");
        src.loading = false;
        src.page = Some(Err("boom".into()));
        assert!(src.needs_fetch("o/r"), "a failure retries on reopen");
        assert!(src.names_for("o/r").is_empty());
        src.page = Some(Ok(gql::RepoBranches {
            default_branch: Some("main".into()),
            total_count: 1,
            names: vec!["main".into()],
        }));
        assert!(!src.needs_fetch("o/r"));
        assert!(src.needs_fetch("o/other"), "a new repo fetches its own");
        assert_eq!(src.names_for("o/r"), ["main".to_string()]);
        assert!(src.names_for("o/other").is_empty());
        src.search = Some(gql::RepoBranches {
            default_branch: None,
            total_count: 1,
            names: vec!["feat-x".into()],
        });
        assert_eq!(src.names_for("o/r"), ["feat-x".to_string()]);
    }
}
