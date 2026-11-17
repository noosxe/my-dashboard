//! The PR Commits tab's list (#793, #912): the repo Commits tab's day
//! headers and two-line row (docs/0081 D5), over the PR's newest-100
//! window, oldest first like GitHub's PR Commits tab. The rows sit in the
//! same virtual `List` as the repo tab's, so the keyboard is the same
//! (docs/0081 D6): one tab stop, ↑/↓ to select, Enter opens the commit,
//! ⌘/Ctrl+Enter copies its SHA.

use gpui::prelude::*;
use gpui::*;
use gpui_component::{
    ActiveTheme as _, IndexPath,
    list::{List, ListDelegate, ListItem, ListState},
    v_flex,
};

use crate::commit_row::{commit_row, day_header};
use crate::commits::{DaySection, day_sections};
use crate::gql::{self, CommitRow};

use super::commits_window;

/// The list's delegate: the PR's commits, sectioned by local day.
#[derive(Default)]
pub(crate) struct PrCommitsDelegate {
    rows: Vec<CommitRow>,
    sections: Vec<DaySection>,
    selected: Option<IndexPath>,
}

impl PrCommitsDelegate {
    /// Replace the rows. Compare-first: `false` when nothing the list
    /// renders changed, so the caller can skip the notify.
    pub(crate) fn set_rows(&mut self, rows: Vec<CommitRow>) -> bool {
        if rows == self.rows {
            return false;
        }
        self.sections = day_sections(&rows, &chrono::Local);
        self.rows = rows;
        true
    }

    fn row_at(&self, ix: IndexPath) -> Option<&CommitRow> {
        let section = self.sections.get(ix.section)?;
        (ix.row < section.len)
            .then(|| self.rows.get(section.start + ix.row))
            .flatten()
    }
}

impl ListDelegate for PrCommitsDelegate {
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
        // Commit order is kept and committer dates aren't monotonic, so
        // one day can head two sections: the first row index keeps ids
        // unique (a duplicate a11y id panics in dev builds).
        let at = self.sections.get(section);
        Some(day_header(at.and_then(|s| s.day), at.map(|s| s.start), cx))
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

    /// Click / Enter opens the commit on github.com; secondary-confirm
    /// (⌘/Ctrl+Enter or ⌘-click) copies the full SHA, the keyboard path
    /// to the clipboard (docs/0081 D6).
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
            log::debug!("pr commits: copied {}", row.oid);
            cx.write_to_clipboard(ClipboardItem::new_string(row.oid.clone()));
        } else if !row.url.is_empty() {
            log::debug!("pr commits: opened {} on GitHub", row.abbreviated_oid);
            cx.open_url(&row.url);
        }
    }
}

/// The new list entity, a tab stop (the pinned `ListState` creates its
/// handle without one; the flag lives on the shared handle state).
pub(super) fn new_list(window: &mut Window, cx: &mut App) -> Entity<ListState<PrCommitsDelegate>> {
    let list = cx.new(|cx| ListState::new(PrCommitsDelegate::default(), window, cx));
    let _ = list.read(cx).focus_handle(cx).tab_stop(true);
    list
}

/// The PR's commits as the repo row type, oldest first as connected.
pub(super) fn rows(d: &gql::PrDetail) -> Vec<CommitRow> {
    d.commits.nodes.iter().map(gql::PrCommit::to_row).collect()
}

/// The tab body: the cap caption (when the query cut older commits)
/// above the list, which scrolls itself.
pub(super) fn commits_list(
    list: &Entity<ListState<PrCommitsDelegate>>,
    d: &gql::PrDetail,
    cx: &App,
) -> AnyElement {
    v_flex()
        .id("pr-commits")
        .flex_1()
        .min_h_0()
        .px_1()
        // #876: the query reads the newest 100, so what it cut is older
        // than every row — the cap row leads the list.
        .when(commits_window::hidden_older(d) > 0, |col| {
            col.child(
                div()
                    .id("pr-commits-truncated")
                    .flex_none()
                    .px_2()
                    .pt_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(commits_window::cap_caption(
                        d.commits.nodes.len(),
                        d.commits.total_count,
                    )),
            )
        })
        .child(
            div()
                .id("pr-commits-list")
                .flex_1()
                .min_h_0()
                // The pinned `ListState` owns the `List` node and takes no
                // name: a named group carries it (as on the repo tab).
                .role(accesskit::Role::Group)
                .aria_label("Commits in this pull request")
                .child(List::new(list).size_full()),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use crate::gql::PrCommit;

    #[test]
    fn a_pr_commit_becomes_the_repo_row() {
        let c = PrCommit {
            oid: "abcdef1234567890".into(),
            headline: "Fix the parser".into(),
            committed_at: Some("2026-10-20T12:00:00Z".into()),
            author_name: Some("Ada".into()),
            author_login: None,
            author_avatar: None,
            url: "https://example.com/c/abcdef1".into(),
            signature: None,
            ci_state: Some("SUCCESS".into()),
        };
        let row = c.to_row();
        // The short SHA is seven characters; short oids don't panic.
        assert_eq!(row.abbreviated_oid, "abcdef1");
        assert_eq!(crate::commit_row::author_name(&row), "Ada");
        assert!(row.status_check_rollup.is_some());
        let short = PrCommit {
            oid: "abc".into(),
            ..c
        };
        assert_eq!(short.to_row().abbreviated_oid, "abc");
    }

    #[test]
    fn the_delegate_groups_by_day_and_skips_an_unchanged_window() {
        use super::PrCommitsDelegate;
        use gpui_component::IndexPath;
        let commit = |oid: &str, at: &str| PrCommit {
            oid: oid.into(),
            headline: "h".into(),
            committed_at: Some(at.into()),
            author_name: None,
            author_login: None,
            author_avatar: None,
            url: String::new(),
            signature: None,
            ci_state: None,
        };
        let rows: Vec<_> = [
            commit("aaaaaaaa", "2026-10-20T12:00:00Z"),
            commit("bbbbbbbb", "2026-10-20T13:00:00Z"),
            commit("cccccccc", "2026-10-22T12:00:00Z"),
        ]
        .iter()
        .map(PrCommit::to_row)
        .collect();
        let mut d = PrCommitsDelegate::default();
        assert!(d.set_rows(rows.clone()));
        assert!(!d.set_rows(rows), "the same window is a no-op");
        assert_eq!(d.sections.len(), 2);
        let last = d.row_at(IndexPath::new(0).section(1)).expect("third row");
        assert_eq!(last.oid, "cccccccc");
        assert!(d.row_at(IndexPath::new(5).section(1)).is_none());
    }

    #[test]
    fn a_day_that_heads_two_sections_has_distinct_start_indices() {
        use super::PrCommitsDelegate;
        let commit = |oid: &str, at: &str| PrCommit {
            oid: oid.into(),
            headline: "h".into(),
            committed_at: Some(at.into()),
            author_name: None,
            author_login: None,
            author_avatar: None,
            url: String::new(),
            signature: None,
            ci_state: None,
        };
        // Committer dates aren't monotonic: day A, day B, day A again.
        let rows: Vec<_> = [
            commit("aaaaaaaa", "2026-10-20T12:00:00Z"),
            commit("bbbbbbbb", "2026-10-22T12:00:00Z"),
            commit("cccccccc", "2026-10-20T12:30:00Z"),
        ]
        .iter()
        .map(PrCommit::to_row)
        .collect();
        let mut d = PrCommitsDelegate::default();
        d.set_rows(rows);
        assert_eq!(d.sections.len(), 3);
        assert_eq!(d.sections[0].day, d.sections[2].day);
        assert_ne!(d.sections[0].start, d.sections[2].start);
    }
}
