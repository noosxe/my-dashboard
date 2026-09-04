//! Files changed tab (docs/0018) — master–detail rework of the accordion:
//! a left sidebar (summary, view-mode `Select`, shared filter `Input`,
//! List or Tree of files) and a single-file diff pane to the right.
//!
//! Ownership: the page holds the sidebar entities (`files_view`,
//! `selected_file`, the four child entities) and the fetch payload; this
//! module owns the pure model (path filter, path forest), the List
//! delegate, and the render fns. Selection is path-keyed (0018 D7) —
//! GitHub paths are unique per PR diff and survive refetches; the old
//! accordion's index-keyed `collapsed_files` is gone with it.
//!
//! The List view's selection lives on the page (`selected_file`) with a
//! highlight mirror in the delegate; the Tree view's selection lives in
//! `TreeState` (upstream emits no selection event — clicks notify the
//! entity, the page observes and reads `selected_entry()`).

use gpui::prelude::FluentBuilder as _;
use gpui::*;

use gpui_component::{
    ActiveTheme, Icon, IconName, IndexPath, Sizable as _,
    badge::Badge,
    h_flex,
    input::Input,
    list::{List, ListDelegate, ListItem, ListState},
    select::Select,
    tree::{TreeEntry, TreeItem, tree},
    v_flex,
};

use crate::api::PrFile;
use crate::pr_page::PrDetailPage;
use crate::service::PatchLine;

/// Files sidebar view mode (0018 D3). `List` is the default — the flat
/// rendering is the closest to the accordion it replaces.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum FilesView {
    #[default]
    List,
    Tree,
}

/// The labels the view-mode `Select` shows (0018 D3); subscription
/// matching and construction must agree on these.
pub(crate) const VIEW_LIST_LABEL: &str = "List";
pub(crate) const VIEW_TREE_LABEL: &str = "Tree";

/// Sidebar row snapshot (0018 D4): what the List/Tree rows show, cloned
/// out of the fetch payload so the delegate owns its data (no patch
/// text — the diff pane reads the authoritative payload by path).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileEntry {
    pub(crate) path: SharedString,
    pub(crate) additions: i64,
    pub(crate) deletions: i64,
    pub(crate) status: SharedString,
}

impl FileEntry {
    pub(crate) fn from_api(file: &PrFile) -> Self {
        Self {
            path: file.filename.clone().into(),
            additions: file.additions,
            deletions: file.deletions,
            status: file.status.clone().into(),
        }
    }
}

/// The shared filter predicate (0018 D6): case-insensitive substring on
/// the full path; an empty (or whitespace-only) query matches all.
/// Pure — table-tested.
pub(crate) fn path_matches(path: &str, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    q.is_empty() || path.to_lowercase().contains(&q)
}

/// Path forest for the Tree view (0018 D5): folders carry the directory
/// path as id and the last segment as label; files carry the full path
/// as id and the basename as label; repeated dirs merge. Folders sort
/// before files (github.com tree parity), folders by segment, files in
/// payload order. All folders are built expanded — a filter rebuild
/// (D6) then never silently collapses anything. Pure — table-tested.
pub(crate) fn build_file_forest(entries: &[FileEntry]) -> Vec<TreeItem> {
    let refs: Vec<&FileEntry> = entries.iter().collect();
    build_level("", &refs)
}

/// One forest level: `dir` is the parent directory path ("" = root);
/// every entry's path starts with `dir`.
fn build_level(dir: &str, entries: &[&FileEntry]) -> Vec<TreeItem> {
    let mut folders: std::collections::BTreeMap<&str, Vec<&FileEntry>> = Default::default();
    let mut files: Vec<&FileEntry> = Vec::new();
    for e in entries {
        match rel_of(dir, e.path.as_ref()).split_once('/') {
            Some((seg, _)) => {
                folders.entry(seg).or_default().push(e);
            }
            None => files.push(e),
        }
    }
    let mut items = Vec::new();
    for (seg, group) in &folders {
        let child_dir = if dir.is_empty() {
            (*seg).to_string()
        } else {
            format!("{dir}/{seg}")
        };
        items.push(
            TreeItem::new(
                SharedString::from(child_dir.clone()),
                SharedString::from(*seg),
            )
            .children(build_level(&child_dir, group))
            .expanded(true),
        );
    }
    for f in files {
        items.push(TreeItem::new(
            f.path.clone(),
            SharedString::from(rel_of(dir, f.path.as_ref()).to_string()),
        ));
    }
    items
}

/// Path relative to `dir` (`dir` is "" at the root, else `a/b`); the
/// caller guarantees `path` starts with `dir`.
fn rel_of<'a>(dir: &str, path: &'a str) -> &'a str {
    if dir.is_empty() {
        path
    } else {
        &path[dir.len() + 1..]
    }
}

/// List-view delegate (0018 D4): the snapshot plus its filtered view.
/// Data-only — selection reacts flow through `ListEvent` on the page,
/// with `selected` as a highlight mirror the page keeps in sync (the
/// upstream `set_selected_index` path syncs it too, for keyboard
/// pre-selection).
#[derive(Default)]
pub(crate) struct FileListDelegate {
    all: Vec<FileEntry>,
    matched: Vec<FileEntry>,
    query: String,
    selected: Option<SharedString>,
}

impl FileListDelegate {
    /// Replace the data, re-applying the active query (a refresh must
    /// not clear the user's filter).
    pub(crate) fn set_files(&mut self, files: Vec<FileEntry>) {
        self.all = files;
        self.apply_query(&self.query.clone());
    }

    pub(crate) fn apply_query(&mut self, query: &str) {
        self.query = query.to_string();
        self.matched = self
            .all
            .iter()
            .filter(|f| path_matches(f.path.as_ref(), query))
            .cloned()
            .collect();
    }

    pub(crate) fn matched(&self) -> &[FileEntry] {
        &self.matched
    }

    pub(crate) fn path_at(&self, ix: IndexPath) -> Option<SharedString> {
        self.matched.get(ix.row).map(|f| f.path.clone())
    }

    pub(crate) fn set_selected(&mut self, path: Option<SharedString>) {
        self.selected = path;
    }
}

impl ListDelegate for FileListDelegate {
    type Item = ListItem;

    fn perform_search(
        &mut self,
        query: &str,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.apply_query(query);
        Task::ready(())
    }

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.matched.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let f = self.matched.get(ix.row)?;
        let selected = self.selected.as_ref() == Some(&f.path);
        Some(file_row(
            &f.path,
            f.additions,
            f.deletions,
            &f.status,
            selected,
            "pr-flist",
            cx,
        ))
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix.and_then(|ix| self.path_at(ix));
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        let text = if self.query.trim().is_empty() {
            "No files"
        } else {
            "No files match"
        };
        h_flex()
            .size_full()
            .justify_center()
            .pt_6()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(text)
            .into_any_element()
    }
}

/// The Files tab master–detail split (0018 D1/D2): fixed-width sidebar
/// (border right) and the selected file's diff pane. Called only with a
/// ready, non-empty payload.
pub(crate) fn render_split(
    page: &PrDetailPage,
    files: &[PrFile],
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    h_flex()
        .id("pr-files-split")
        .flex_1()
        .min_h_0()
        .w_full()
        .child(sidebar(page, files, cx))
        .child(diff_pane(page, files, cx))
        .into_any_element()
}

/// The sidebar (0018 D2): summary row, view-mode Select, shared filter
/// input, then the active view (the inactive one is not rendered).
fn sidebar(page: &PrDetailPage, files: &[PrFile], cx: &Context<PrDetailPage>) -> AnyElement {
    let (adds, dels): (i64, i64) = files
        .iter()
        .map(|f| (f.additions, f.deletions))
        .fold((0, 0), |(a, d), (fa, fd)| (a + fa, d + fd));
    let summary = files_summary(files.len(), adds, dels);
    v_flex()
        .id("pr-files-sidebar")
        .w(px(300.))
        .flex_none()
        // Structural height (0016 D5 lesson): stretch to the split's
        // definite cross size — never a percentage of it.
        .self_stretch()
        .min_h_0()
        .border_r_1()
        .border_color(cx.theme().border)
        .p_2()
        .gap_2()
        .child(
            h_flex().px_1().gap_2().text_sm().child(
                h_flex()
                    .gap_1()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} files changed", summary.files))
                    .child(format!("· +{}", summary.adds))
                    .child(format!("−{}", summary.dels)),
            ),
        )
        .child(
            Select::new(&page.view_select)
                .small()
                .w_full()
                .accessibility_label("Files view mode"),
        )
        .child(
            Input::new(&page.file_filter)
                .small()
                .w_full()
                .accessibility_id("pr-files-filter"),
        )
        .child(view_area(page, cx))
        .into_any_element()
}

/// The active view area (0018 D4/D5): the List or the Tree, filling the
/// sidebar's remaining height. Each manages its own scrolling.
fn view_area(page: &PrDetailPage, _cx: &Context<PrDetailPage>) -> AnyElement {
    // Explicit bounded wrapper (0016 D5 lesson): the view's height must
    // come from flex growth inside the sidebar's definite column — the
    // List/Tree fill it via their own `size_full`, no percentages ride
    // on the sidebar's `h_full` resolving.
    match page.files_view {
        FilesView::List => v_flex()
            .id("pr-files-view")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .child(List::new(&page.file_list))
            .into_any_element(),
        FilesView::Tree => v_flex()
            .id("pr-files-view")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .child(tree(&page.file_tree, |_, entry, selected, _window, cx| {
                tree_row(entry, selected, cx)
            }))
            .into_any_element(),
    }
}

/// One Tree row (0018 D5): depth indent, chevron for folders, mono
/// basename for files (the hierarchy carries the directories; the diff
/// pane header shows the full path). Rows are `ListItem`s — a11y
/// included upstream.
fn tree_row(entry: &TreeEntry, selected: bool, cx: &App) -> ListItem {
    let depth = entry.depth();
    let is_folder = entry.is_folder();
    let id = SharedString::from(format!("pr-ftree-{}", entry.item().id));
    ListItem::new(id).selected(selected).child(
        h_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .items_center()
            .when(depth > 0, |row| {
                row.child(div().flex_none().w(px(depth as f32 * 12.)))
            })
            .when(is_folder, |row| {
                let chevron = if entry.is_expanded() {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                };
                row.child(
                    Icon::new(chevron)
                        .xsmall()
                        .flex_none()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(div().truncate().text_sm().child(entry.item().label.clone()))
            })
            .when(!is_folder, |row| {
                // Spacer under the folder chevron so basenames align.
                row.child(div().flex_none().w(px(16.))).child(
                    div()
                        .truncate()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(entry.item().label.clone()),
                )
            }),
    )
}

/// The selected file's diff pane (0018 D8): header (path, badge,
/// `+adds/−dels`) over the vertical scroll area holding the existing
/// diff body. Empty state when the active view has no visible selection.
fn diff_pane(page: &PrDetailPage, files: &[PrFile], cx: &Context<PrDetailPage>) -> AnyElement {
    let selected = selected_file_path(page, cx);
    let file = selected
        .as_ref()
        .and_then(|path| files.iter().find(|f| f.filename.as_str() == path.as_ref()));
    let Some(file) = file else {
        return empty_pane(cx);
    };
    let (dir, name) = split_path(&file.filename);
    v_flex()
        .id("pr-file-diff")
        .flex_1()
        .min_w_0()
        // Structural height — same rationale as the sidebar.
        .self_stretch()
        .min_h_0()
        .child(
            h_flex()
                .px_3()
                .py_2()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    h_flex()
                        .min_w_0()
                        .flex_1()
                        .gap_1()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .when_some(dir, |row, dir| {
                            row.child(
                                div()
                                    .truncate()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{dir}/")),
                            )
                        })
                        .child(div().truncate().child(name.to_string())),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .gap_2()
                        .text_xs()
                        .child(
                            div()
                                .text_color(cx.theme().success)
                                .child(format!("+{}", file.additions)),
                        )
                        .child(
                            div()
                                .text_color(cx.theme().danger)
                                .child(format!("−{}", file.deletions)),
                        )
                        .child(file_status_badge(&file.status, cx)),
                ),
        )
        .child(
            v_flex()
                .id("pr-diff-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .py_2()
                .child(diff_body(file, cx)),
        )
        .into_any_element()
}

/// Centered muted state for an unselected pane (0018 D8).
fn empty_pane(cx: &Context<PrDetailPage>) -> AnyElement {
    div()
        .id("pr-diff-empty")
        .flex_1()
        .min_w_0()
        .min_h_0()
        .flex()
        .items_center()
        .justify_center()
        .text_color(cx.theme().muted_foreground)
        .text_sm()
        .child("Select a file to view its diff")
        .into_any_element()
}

/// The active view's selection, path-keyed (0018 D7): the List's
/// selection is the page's; the Tree's lives in `TreeState`.
pub(crate) fn selected_file_path(
    page: &PrDetailPage,
    cx: &Context<PrDetailPage>,
) -> Option<SharedString> {
    match page.files_view {
        FilesView::List => page.selected_file.clone(),
        FilesView::Tree => page
            .file_tree
            .read(cx)
            .selected_entry()
            .map(|e| e.item().id.clone()),
    }
}

/// One List row (0018 D4): mono path with muted dir prefix (truncating
/// from the front), `+adds/−dels`, change badge. Also the shape the
/// Tree's file rows echo, narrower.
fn file_row(
    path: &SharedString,
    adds: i64,
    dels: i64,
    status: &str,
    selected: bool,
    prefix: &str,
    cx: &App,
) -> ListItem {
    let (dir, name) = split_path(path);
    ListItem::new(SharedString::from(format!("{prefix}-{path}")))
        .selected(selected)
        .text_xs()
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .items_center()
                .child(
                    h_flex()
                        .min_w_0()
                        .flex_1()
                        .gap_1()
                        .font_family(cx.theme().mono_font_family.clone())
                        .when_some(dir, |row, dir| {
                            row.child(
                                div()
                                    .truncate()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{dir}/")),
                            )
                        })
                        .child(div().truncate().child(name.to_string())),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .gap_1p5()
                        .text_xs()
                        .child(
                            div()
                                .text_color(cx.theme().success)
                                .child(format!("+{adds}")),
                        )
                        .child(
                            div()
                                .text_color(cx.theme().danger)
                                .child(format!("−{dels}")),
                        )
                        .child(file_status_badge(status, cx)),
                ),
        )
}

/// Summary-line text pieces (moved from pr_page, 0018): the pure fn
/// stays table-testable.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FilesSummary {
    pub(crate) files: usize,
    pub(crate) adds: i64,
    pub(crate) dels: i64,
}

pub(crate) fn files_summary(files: usize, adds: i64, dels: i64) -> FilesSummary {
    FilesSummary { files, adds, dels }
}

/// Change-type badge (moved from pr_page, 0018): the four GitHub kinds
/// map to tones; anything else (future API kinds, 0007 D3) renders as
/// the raw string, muted.
pub(crate) fn file_status_badge(status: &str, cx: &App) -> AnyElement {
    let (label, tone) = match status {
        "added" => (status, cx.theme().success),
        "removed" => (status, cx.theme().danger),
        "renamed" => (status, cx.theme().muted_foreground),
        _ => (status, cx.theme().muted_foreground),
    };
    Badge::new()
        .xsmall()
        .color(tone)
        .child(label.to_string())
        .into_any_element()
}

/// Split a repo path for the row header (moved from pr_page, 0018): dir
/// segments render muted ahead of the basename, github.com-style. Pure
/// for the table tests.
pub(crate) fn split_path(path: &str) -> (Option<&str>, &str) {
    match path.rsplit_once('/') {
        Some((dir, name)) => (Some(dir), name),
        None => (None, path),
    }
}

/// The diff body (moved from pr_page, 0018): horizontal scroll, mono
/// `text_xs`, `flex_none` rows — no soft wrap. Tints come from the
/// semantic tokens at low opacity (0016 D10); the sign glyph carries
/// the saturated color.
fn diff_body(file: &PrFile, cx: &Context<PrDetailPage>) -> AnyElement {
    let Some(patch) = file.patch.as_deref() else {
        return div()
            .px_3()
            .py_2()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child("Binary file or diff not shown")
            .into_any_element();
    };
    let rows: Vec<AnyElement> = crate::service::parse_patch(patch)
        .into_iter()
        .map(|line| diff_row(line, cx))
        .collect();
    v_flex()
        .id(SharedString::from(format!("pr-diff-{}", file.filename)))
        .min_w_0()
        .overflow_x_scroll()
        .children(rows)
        .into_any_element()
}

fn diff_row(line: PatchLine, cx: &Context<PrDetailPage>) -> AnyElement {
    let mono = cx.theme().mono_font_family.clone();
    match line {
        PatchLine::Hunk(text) => div()
            .flex_none()
            .whitespace_nowrap()
            .px_2()
            .py_0p5()
            .text_xs()
            .font_family(mono)
            .text_color(cx.theme().muted_foreground)
            .bg(cx.theme().accent.opacity(0.15))
            .child(text),
        PatchLine::Context(text) => div()
            .flex_none()
            .whitespace_nowrap()
            .px_2()
            .text_xs()
            .font_family(mono)
            .child(format!(" {text}")),
        PatchLine::Add(text) => div()
            .flex_none()
            .whitespace_nowrap()
            .px_2()
            .text_xs()
            .font_family(mono)
            .bg(cx.theme().success.opacity(0.12))
            .text_color(cx.theme().success)
            .child(format!("+{text}")),
        PatchLine::Del(text) => div()
            .flex_none()
            .whitespace_nowrap()
            .px_2()
            .text_xs()
            .font_family(mono)
            .bg(cx.theme().danger.opacity(0.12))
            .text_color(cx.theme().danger)
            .child(format!("−{text}")),
    }
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{
        FileEntry, FileListDelegate, FilesSummary, build_file_forest, files_summary, path_matches,
        split_path,
    };
    fn entry(path: &str) -> FileEntry {
        FileEntry {
            path: path.into(),
            additions: 1,
            deletions: 2,
            status: "modified".into(),
        }
    }

    fn paths(items: &[gpui_component::tree::TreeItem]) -> Vec<String> {
        items.iter().map(|i| i.id.to_string()).collect()
    }

    #[test]
    fn path_matches_is_case_insensitive_substring() {
        assert!(path_matches("crates/core/src/lib.rs", ""));
        assert!(path_matches("crates/core/src/lib.rs", "   "));
        assert!(path_matches("crates/core/src/lib.rs", "CORE"));
        assert!(path_matches("crates/core/src/lib.rs", "src/lib"));
        assert!(!path_matches("crates/core/src/lib.rs", "card"));
        assert!(!path_matches("crates/core/src/lib.rs", "lib.rsx"));
    }

    #[test]
    fn forest_keeps_root_files_at_the_top_level() {
        let entries = [entry("README.md"), entry("src/lib.rs")];
        let forest = build_file_forest(&entries);
        assert_eq!(paths(&forest), vec!["src", "README.md"]);
        assert!(forest[1].children.is_empty());
        assert_eq!(forest[1].label, "README.md");
    }

    #[test]
    fn forest_merges_repeated_dirs_and_names_them() {
        let entries = [
            entry("a/b/one.rs"),
            entry("a/b/two.rs"),
            entry("a/c.rs"),
            entry("top.txt"),
        ];
        let forest = build_file_forest(&entries);
        // Folders first (sorted), then payload-order root files.
        assert_eq!(paths(&forest), vec!["a", "top.txt"]);
        let a = &forest[0];
        assert_eq!(a.label, "a");
        assert_eq!(paths(&a.children), vec!["a/b", "a/c.rs"]);
        let b = &a.children[0];
        assert_eq!(b.label, "b");
        assert_eq!(paths(&b.children), vec!["a/b/one.rs", "a/b/two.rs"]);
    }

    #[test]
    fn forest_builds_every_folder_expanded() {
        let entries = [entry("a/b/c.rs")];
        let forest = build_file_forest(&entries);
        let a = &forest[0];
        assert!(a.is_expanded());
        let b = &a.children[0];
        assert!(b.is_expanded());
        assert_eq!(b.children[0].id, "a/b/c.rs");
    }

    #[test]
    fn forest_is_empty_for_no_entries() {
        assert!(build_file_forest(&[]).is_empty());
    }

    #[test]
    fn delegate_filters_and_maps_indexes() {
        let mut d = FileListDelegate::default();
        d.set_files(vec![entry("a.rs"), entry("dir/b.rs"), entry("dir/c.txt")]);
        assert_eq!(d.matched().len(), 3);
        assert_eq!(
            d.path_at(gpui_component::IndexPath::new(1))
                .map(|p| p.to_string()),
            Some("dir/b.rs".into())
        );
        d.apply_query("B");
        assert_eq!(d.matched().len(), 1);
        assert_eq!(
            d.path_at(gpui_component::IndexPath::new(0))
                .map(|p| p.to_string()),
            Some("dir/b.rs".into())
        );
        d.apply_query("zzz");
        assert!(d.matched().is_empty());
        assert_eq!(d.path_at(gpui_component::IndexPath::new(0)), None);
    }

    #[test]
    fn delegate_refetch_keeps_the_query() {
        let mut d = FileListDelegate::default();
        d.apply_query("dir");
        d.set_files(vec![entry("a.rs"), entry("dir/b.rs")]);
        assert_eq!(d.matched().len(), 1);
    }

    #[test]
    fn split_path_dir_and_basename() {
        assert_eq!(split_path("a/b/c.rs"), (Some("a/b"), "c.rs"));
        assert_eq!(split_path("c.rs"), (None, "c.rs"));
    }

    #[test]
    fn files_summary_carries_the_line_numbers() {
        assert_eq!(
            files_summary(2, 10, 3),
            FilesSummary {
                files: 2,
                adds: 10,
                dels: 3,
            }
        );
    }
}
