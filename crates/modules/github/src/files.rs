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

use std::collections::BTreeMap;
use std::ops::Range;

use gpui::prelude::FluentBuilder as _;
use gpui::*;

use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, IndexPath, Sizable as _,
    badge::Badge,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, Textarea, TextareaState},
    list::{List, ListDelegate, ListItem, ListState},
    scroll::ScrollableElement as _,
    select::Select,
    tree::{TreeEntry, TreeItem, tree},
    v_flex,
};

use crate::api::PrFile;
use crate::gql::{DiffSide, PrThread, ThreadSubject};
use crate::markdown;
use crate::pr_page::{
    ErrorScope, FinishEvent, PrDetailPage, ResolutionReason, delete_confirm_card, inline_edit_card,
    review_batch_allowed, row_is_edited,
};
use crate::service::{AnchoredLine, PatchLine, anchor_patch};
use crate::syntax::{self, TokenKind};
use dashboard_core::freshness::relative_age;
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
    page_h: &Entity<PrDetailPage>,
    page: &PrDetailPage,
    files: &[PrFile],
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    h_flex()
        .id("pr-files-split")
        // Test-only layout anchor (noop outside `cargo test`): the
        // probe test reads it via `debug_bounds`.
        .debug_selector(|| "files-split".into())
        .flex_1()
        .min_h_0()
        .w_full()
        .child(sidebar(page, files, cx))
        .child(diff_pane(page_h, page, files, cx))
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
        .debug_selector(|| "files-sidebar".into())
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
            div().debug_selector(|| "files-summary-wrap".into()).child(
                h_flex().px_1().gap_2().text_sm().child(
                    h_flex()
                        .gap_1()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{} files changed", summary.files))
                        .child(format!("· +{}", summary.adds))
                        .child(format!("−{}", summary.dels)),
                ),
            ),
        )
        .child(
            div().debug_selector(|| "files-select-wrap".into()).child(
                Select::new(&page.view_select)
                    .small()
                    .w_full()
                    .accessibility_label("Files view mode"),
            ),
        )
        .child(
            div().debug_selector(|| "files-filter-wrap".into()).child(
                Input::new(&page.file_filter)
                    .small()
                    .w_full()
                    .accessibility_id("pr-files-filter"),
            ),
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
            .debug_selector(|| "files-view".into())
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .child(List::new(&page.file_list))
            .into_any_element(),
        FilesView::Tree => v_flex()
            .id("pr-files-view")
            .debug_selector(|| "files-view-tree".into())
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
fn diff_pane(
    page_h: &Entity<PrDetailPage>,
    page: &PrDetailPage,
    files: &[PrFile],
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    let selected = selected_file_path(page, cx);
    let file = selected
        .as_ref()
        .and_then(|path| files.iter().find(|f| f.filename.as_str() == path.as_ref()));
    let Some(file) = file else {
        return empty_pane(cx);
    };
    let (dir, name) = split_path(&file.filename);
    let rows = page
        .anchor_cache
        .rows_for(&file.filename, file.patch.as_deref().unwrap_or_default());
    v_flex()
        .id("pr-file-diff")
        .flex_1()
        .min_w_0()
        // Structural height — same rationale as the sidebar.
        .self_stretch()
        .min_h_0()
        .child(diff_header(file, dir, name, cx))
        .children(files_error_row(page, cx))
        .child(diff_body(page_h, page, file, &rows, cx))
        // 0030 D4: the Finish-review bar — sticky under the diff while
        // drafts exist (outside the scroll).
        .children(finish_review_bar(page, cx))
        .into_any_element()
}

/// The selected file's header row: mono path with muted dir prefix
/// (truncating from the front), +adds/−dels, change badge.
fn diff_header(file: &PrFile, dir: Option<&str>, name: &str, cx: &Context<PrDetailPage>) -> Div {
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
                        .child(format!("\u{2212}{}", file.deletions)),
                )
                .child(file_status_badge(&file.status, cx)),
        )
}

/// Centered muted state for an unselected pane (0018 D8).
fn empty_pane(cx: &Context<PrDetailPage>) -> AnyElement {
    div()
        .id("pr-diff-empty")
        .debug_selector(|| "pr-diff-empty-key".into())
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

// ---------------------------------------------------------------------------
// Review threads on the diff (docs/0030 D2/D3/D6) — stage 1 read side
// ---------------------------------------------------------------------------

/// A thread positioned against one file's diff, tagged with its index in
/// the page's thread list (`pr-thread-{ix}` ids and md-cache keys ride it).
pub(crate) type Placed<'a> = (usize, &'a PrThread);

/// Threads placed against one file's diff (0030 D3). Pure — table-tested.
pub(crate) struct ThreadPlacement<'a> {
    /// LINE threads anchored under a diff row, keyed by that row's index
    /// in the `anchor_patch` output. A row hosts at most one side's line
    /// number, so the key is unique per thread anchor.
    pub(crate) anchored: BTreeMap<usize, Vec<Placed<'a>>>,
    /// FILE-subject threads — rendered under the file header.
    pub(crate) file_level: Vec<Placed<'a>>,
    /// Outdated threads, plus LINE threads whose line is not in the
    /// current diff (patch truncated, force-push mid-session) — the
    /// end-of-file bucket. Never dropped.
    pub(crate) stray: Vec<Placed<'a>>,
}

/// Place the page's threads against one file's diff rows (0030 D3):
/// the pure core table tests hit directly.
pub(crate) fn place_threads<'a>(
    threads: &'a [PrThread],
    path: &str,
    rows: &[AnchoredLine],
) -> ThreadPlacement<'a> {
    let mut placed = ThreadPlacement {
        anchored: BTreeMap::new(),
        file_level: Vec::new(),
        stray: Vec::new(),
    };
    // Index the diff rows by their line numbers per side.
    let mut anchor_at: std::collections::HashMap<(DiffSide, u32), usize> = Default::default();
    for (ix, row) in rows.iter().enumerate() {
        if let Some(new) = row.new {
            anchor_at.insert((DiffSide::Right, new), ix);
        }
        if let Some(old) = row.old {
            anchor_at.insert((DiffSide::Left, old), ix);
        }
    }
    for (ix, t) in threads.iter().enumerate() {
        if t.path != path {
            continue;
        }
        match t.subject {
            ThreadSubject::File => placed.file_level.push((ix, t)),
            ThreadSubject::Line => {
                let line = t
                    .line
                    .and_then(|l| u32::try_from(l).ok())
                    .filter(|l| *l != 0);
                let start_line = t
                    .start_line
                    .and_then(|l| u32::try_from(l).ok())
                    .filter(|l| *l != 0);
                let slot = match (t.is_outdated, line) {
                    (true, _) | (_, None) => None,
                    (false, Some(end)) => match start_line {
                        // #211: ranges anchor under the LAST row their
                        // span maps to (GitHub renders the card at the
                        // range end); a span that only partly overlaps
                        // the patch still anchors. Absurd spans fall
                        // back to the end row alone.
                        Some(s) if s < end && end - s <= 10_000 => (s..=end)
                            .rev()
                            .find_map(|l| anchor_at.get(&(t.side, l)).copied()),
                        _ => anchor_at.get(&(t.side, end)).copied(),
                    },
                };
                match slot {
                    Some(row_ix) => placed.anchored.entry(row_ix).or_default().push((ix, t)),
                    None => placed.stray.push((ix, t)),
                }
            }
        }
    }
    placed
}

/// One entry of the virtualized diff body (docs/0041 D2): a structural
/// descriptor. Page state that changes a card's *height* is deliberately
/// NOT part of the descriptor — height-only changes resync via
/// `PrDetailPage::splice_diff_from` from the mutating event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DiffItem {
    /// A file-level review thread (0030 D3), above the diff rows.
    FileThread(usize),
    /// A diff row, index into the file's anchored rows.
    Row(usize),
    /// A review thread card anchored under its row (`thread_ix` indexes
    /// the page's thread list).
    ThreadCard(usize),
    /// The row composer (0030 D4), under the row it arms.
    Composer(usize),
    /// A queued draft chip, under its row.
    Draft(usize),
    /// Threads load hint / retry row (0030 D1).
    FetchRow,
    /// The end-of-file outdated bucket (0030 D3) — toggle + cards.
    StrayBucket,
}

/// Cached rows for one file version: patch hash plus the parsed lines.
type AnchorEntry = (u64, Arc<Vec<crate::service::AnchoredLine>>);

/// Page-owned anchor cache (docs/0041 D4): one `anchor_patch` parse per
/// file version, shared by thread placement, the diff body, and the
/// highlight pump. Entries self-invalidate per path by patch hash.
#[derive(Default)]
pub(crate) struct AnchorCache(RefCell<BTreeMap<String, AnchorEntry>>);

impl AnchorCache {
    pub(crate) fn rows_for(
        &self,
        path: &str,
        patch: &str,
    ) -> Arc<Vec<crate::service::AnchoredLine>> {
        let hash = crate::syntax::hash_patch(patch);
        if let Some((h, rows)) = self.0.borrow().get(path) {
            if *h == hash {
                return rows.clone();
            }
        }
        let rows = Arc::new(anchor_patch(patch));
        self.0
            .borrow_mut()
            .insert(path.to_string(), (hash, rows.clone()));
        rows
    }
}

use std::cell::RefCell;
use std::sync::Arc;

/// Flatten one file's diff into the virtualized item list (docs/0041 D2),
/// in the exact order the pre-list pane rendered: file-level threads,
/// then per row [row, threads, composer?, drafts*], then the fetch row
/// and the stray bucket.
pub(crate) fn flatten_diff_items(
    page: &PrDetailPage,
    rows: &[crate::service::AnchoredLine],
    placed: &ThreadPlacement<'_>,
) -> Vec<DiffItem> {
    let mut items = Vec::new();
    for &(ix, _) in placed.file_level.iter() {
        items.push(DiffItem::FileThread(ix));
    }
    for row_ix in 0..rows.len() {
        items.push(DiffItem::Row(row_ix));
        if let Some(threads) = placed.anchored.get(&row_ix) {
            for &(ix, _) in threads.iter() {
                items.push(DiffItem::ThreadCard(ix));
            }
        }
        if let Some(anchored) = rows.get(row_ix) {
            let armed = row_anchor(anchored);
            let composer_here =
                page.line_composer
                    .as_ref()
                    .zip(armed)
                    .is_some_and(|(c, (side, line))| {
                        c.path == page.selected_file.as_deref().unwrap_or_default()
                            && c.side == side
                            && c.line == line
                    });
            if composer_here {
                items.push(DiffItem::Composer(row_ix));
            }
            if let Some((side, line)) = armed {
                if page.drafts.iter().any(|d| {
                    d.path == page.selected_file.as_deref().unwrap_or_default()
                        && d.side == side
                        && d.line == line
                }) {
                    items.push(DiffItem::Draft(row_ix));
                }
            }
        }
    }
    if threads_fetch_row_present(page) {
        items.push(DiffItem::FetchRow);
    }
    if !placed.stray.is_empty() {
        items.push(DiffItem::StrayBucket);
    }
    items
}

/// Place the page's threads against one file's diff (0030 D3): the
/// render-side wrapper — rows come from the file's own patch.
pub(crate) fn thread_placement<'a>(
    page: &'a PrDetailPage,
    file: &PrFile,
    rows: &'a [crate::service::AnchoredLine],
) -> ThreadPlacement<'a> {
    let threads = page.threads_nodes();
    if threads.is_empty() {
        return ThreadPlacement {
            anchored: BTreeMap::new(),
            file_level: Vec::new(),
            stray: Vec::new(),
        };
    }
    place_threads(threads, &file.filename, rows)
}

/// The diff body (moved from pr_page, 0018): horizontal scroll, mono
/// `text_xs`, `flex_none` rows — no soft wrap. Tints come from the
/// semantic tokens at low opacity (0016 D10); the sign glyph carries
/// the saturated color. 0030 D2: rows gain a two-column line-number
/// The diff body (docs/0041 D1/D2): a virtualized `gpui::list` over the
/// flattened `DiffItem` descriptors. Only viewport rows materialize per
/// frame; the page's `diff_list` owns scroll position and cached heights.
/// Horizontal scroll (rows stay `whitespace_nowrap`, 0018): the list is
/// sized to a content-derived width (capped) inside an x-scroll wrapper —
/// vertical wheel lives in the list, horizontal in the wrapper. Scrollbars:
/// vertical tracks the `ListState`, horizontal the wrapper handle (D6).
/// Tints come from the semantic tokens at low opacity (0016 D10); the sign
/// glyph carries the saturated color (0040 §4 D6 amendment).
fn diff_body(
    page_h: &Entity<PrDetailPage>,
    page: &PrDetailPage,
    file: &PrFile,
    rows: &[crate::service::AnchoredLine],
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    let path = file.filename.clone();
    let spans = match page.highlight_cache.lookup(
        &path,
        syntax::hash_patch(file.patch.as_deref().unwrap_or_default()),
        &syntax::theme_key(cx.theme()),
    ) {
        syntax::Lookup::Done(spans) => spans,
        syntax::Lookup::Tokenizing | syntax::Lookup::Miss => None,
    };
    let items = page.diff_items.clone();
    let state = page.diff_list.clone();
    let x_handle = page.diff_x_scroll.clone();
    let page_h = page_h.clone();
    let rows: std::sync::Arc<[crate::service::AnchoredLine]> = rows.iter().cloned().collect();
    let rows_for_width = rows.clone();
    let list = list(
        state.clone(),
        move |ix: usize, window: &mut Window, cx: &mut App| {
            let page = page_h.read(cx);
            let empty = || div().into_any_element();
            match items.get(ix) {
                Some(DiffItem::FileThread(tix)) => match page.threads_nodes().get(*tix) {
                    Some(t) => thread_card(page, &page_h, window, *tix, t, cx),
                    None => empty(),
                },
                Some(DiffItem::Row(rix)) => {
                    let Some(anchored) = rows.get(*rix) else {
                        return empty();
                    };
                    let row_spans = spans.as_ref().and_then(|v| v.get(*rix)).map(Vec::as_slice);
                    diff_row(page, &page_h, window, &path, anchored, row_spans, cx)
                }
                Some(DiffItem::ThreadCard(tix)) => match page.threads_nodes().get(*tix) {
                    Some(t) => thread_card(page, &page_h, window, *tix, t, cx),
                    None => empty(),
                },
                Some(DiffItem::Composer(rix)) => match rows.get(*rix) {
                    Some(anchored) => {
                        line_composer_card(page, &page_h, window, &path, anchored, cx)
                            .unwrap_or_else(empty)
                    }
                    None => empty(),
                },
                Some(DiffItem::Draft(rix)) => {
                    let mut cards = v_flex().w_full().min_w_0();
                    if let Some(anchored) = rows.get(*rix) {
                        for card in draft_rows(page, &page_h, window, &path, anchored, cx) {
                            cards = cards.child(card);
                        }
                    }
                    cards.into_any_element()
                }
                Some(DiffItem::FetchRow) => {
                    threads_fetch_row(page, &page_h, window, cx).unwrap_or_else(empty)
                }
                Some(DiffItem::StrayBucket) => {
                    let threads = page.threads_nodes();
                    let placed = place_threads(threads, &path, &rows);
                    stray_bucket(page, &page_h, window, &placed, cx).unwrap_or_else(empty)
                }
                None => empty(),
            }
        },
    );
    div()
        .id("pr-diff-scroll")
        .debug_selector(|| "pr-diff-scroll-key".into())
        .flex_1()
        .min_h_0()
        .min_w_0()
        .overflow_x_scroll()
        .track_scroll(&x_handle)
        .horizontal_scrollbar(&x_handle)
        .vertical_scrollbar(&state)
        // Padding the old scroll container had (py_2) lives on the items
        // now; the list sizes itself, so keep the wrapper clean.
        .child(list.w(px(content_width(&rows_for_width))).h_full())
        .into_any_element()
}

/// The list's content width (docs/0041 D5): the longest row's text at a
/// generous mono advance, plus the gutter and padding, capped — honest
/// scrollbar travel without unbounded width.
fn content_width(rows: &[crate::service::AnchoredLine]) -> f32 {
    const CHAR_W: f32 = 8.0;
    const CHROME: f32 = 120.0;
    const CAP: f32 = 16_384.0;
    let max_len = rows.iter().map(row_text_len).max().unwrap_or(0) as f32;
    (CHROME + max_len * CHAR_W).clamp(400.0, CAP)
}

fn row_text_len(anchored: &crate::service::AnchoredLine) -> usize {
    match &anchored.line {
        crate::service::PatchLine::Hunk(t)
        | crate::service::PatchLine::Context(t)
        | crate::service::PatchLine::Add(t)
        | crate::service::PatchLine::Del(t) => t.chars().count(),
    }
}

/// The gutter cells for one row (0030 D2): old-side number, new-side
/// number — muted, fixed width, blank on hunk rows and unanchored rows.
fn gutter_cell(n: Option<u32>, cx: &App) -> Div {
    div()
        .flex_none()
        .w(px(44.))
        .text_right()
        .pr_2()
        .text_xs()
        .font_family(cx.theme().mono_font_family.clone())
        .text_color(cx.theme().muted_foreground)
        .child(n.map(|v| v.to_string()).unwrap_or_default())
}

/// The row's comment anchor (docs/0030 D2/D4): Add/Context rows arm
/// RIGHT + new-side line; Del rows arm LEFT + old-side line; hunk and
/// unanchored rows arm nothing.
fn row_anchor(anchored: &AnchoredLine) -> Option<(DiffSide, u32)> {
    match &anchored.line {
        PatchLine::Hunk(_) => None,
        PatchLine::Context(_) | PatchLine::Add(_) => anchored.new.map(|n| (DiffSide::Right, n)),
        PatchLine::Del(_) => anchored.old.map(|o| (DiffSide::Left, o)),
    }
}

#[cfg(test)]
pub(crate) static DIFF_ROWS_BUILT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn diff_row(
    page: &PrDetailPage,
    page_h: &Entity<PrDetailPage>,
    window: &mut Window,
    path: &str,
    anchored: &AnchoredLine,
    spans: Option<&[(Range<usize>, TokenKind)]>,
    cx: &App,
) -> AnyElement {
    #[cfg(test)]
    DIFF_ROWS_BUILT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mono = cx.theme().mono_font_family.clone();
    // docs/0040 §4 D6 amendment: the row's comment button renders only
    // while this row is hovered — 460 always-on Buttons+tooltips were
    // the diff-pane scroll jank.
    let row_id = row_comment_id(path, anchored);
    match &anchored.line {
        PatchLine::Hunk(text) => div()
            .flex_none()
            .whitespace_nowrap()
            .px_2()
            .py_0p5()
            .text_xs()
            .font_family(mono)
            .text_color(cx.theme().muted_foreground)
            .bg(cx.theme().accent.opacity(0.15))
            .child(text.clone())
            .into_any_element(),
        PatchLine::Context(text) => h_flex()
            .flex_none()
            .whitespace_nowrap()
            .child(gutter_cell(anchored.old, cx))
            .child(gutter_cell(anchored.new, cx))
            .px_2()
            .text_xs()
            .font_family(mono)
            .child(glyph_cell("", None))
            .child(diff_content(text, spans, cx))
            .children(row_comment_button(
                page, page_h, window, &row_id, path, anchored,
            ))
            .id(row_id.clone())
            .on_hover(
                window.listener_for(page_h, move |this, hovered: &bool, _, cx| {
                    let next = hovered.then(|| row_id.clone());
                    if this.hovered_row != next {
                        this.hovered_row = next;
                        cx.notify();
                    }
                }),
            )
            .into_any_element(),
        // docs/0040 §4 D6 (amended): the tint stays in the background,
        // the sign glyph alone carries the saturated color (0016 D10's
        // letter), and the content renders in ambient foreground with
        // token spans — the old whole-row saturated `text_color` fought
        // the token palette and made added-line keywords unreadable.
        PatchLine::Add(text) => h_flex()
            .flex_none()
            .whitespace_nowrap()
            .child(gutter_cell(anchored.old, cx))
            .child(gutter_cell(anchored.new, cx))
            .px_2()
            .text_xs()
            .font_family(mono)
            .bg(cx.theme().success.opacity(0.12))
            .child(glyph_cell("+", Some(cx.theme().success)))
            .child(diff_content(text, spans, cx))
            .children(row_comment_button(
                page, page_h, window, &row_id, path, anchored,
            ))
            .id(row_id.clone())
            .on_hover(
                window.listener_for(page_h, move |this, hovered: &bool, _, cx| {
                    let next = hovered.then(|| row_id.clone());
                    if this.hovered_row != next {
                        this.hovered_row = next;
                        cx.notify();
                    }
                }),
            )
            .into_any_element(),
        PatchLine::Del(text) => h_flex()
            .flex_none()
            .whitespace_nowrap()
            .child(gutter_cell(anchored.old, cx))
            .child(gutter_cell(anchored.new, cx))
            .px_2()
            .text_xs()
            .font_family(mono)
            .bg(cx.theme().danger.opacity(0.12))
            .child(glyph_cell("\u{2212}", Some(cx.theme().danger)))
            .child(diff_content(text, spans, cx))
            .children(row_comment_button(
                page, page_h, window, &row_id, path, anchored,
            ))
            .id(row_id.clone())
            .on_hover(
                window.listener_for(page_h, move |this, hovered: &bool, _, cx| {
                    let next = hovered.then(|| row_id.clone());
                    if this.hovered_row != next {
                        this.hovered_row = next;
                        cx.notify();
                    }
                }),
            )
            .into_any_element(),
    }
    .into_any_element()
}

/// The fixed-width sign cell (one of `" "`, `"+"`, `"\u{2212}"`) —
/// colored separately from the content so token colors never compete
/// with the row tint (docs/0040 §4 D6, amended).
fn glyph_cell(glyph: &'static str, color: Option<Hsla>) -> Div {
    let mut cell = div()
        .flex_none()
        .w(px(10.))
        .text_center()
        .text_xs()
        .child(glyph);
    if let Some(color) = color {
        cell = cell.text_color(color);
    }
    cell
}

/// One row's content: the bare text, or the same text as a
/// `StyledText` carrying the token spans (docs/0040 D6). Without spans
/// — plain file, over the cap, or nothing to color — this is the plain
/// string exactly as before docs/0040; the sign glyph is a separate
/// cell, so span ranges need no re-basing.
fn diff_content(text: &str, spans: Option<&[(Range<usize>, TokenKind)]>, cx: &App) -> AnyElement {
    let Some(spans) = spans else {
        return text.to_string().into_any_element();
    };
    if spans.is_empty() {
        return text.to_string().into_any_element();
    }
    let highlights: Vec<(Range<usize>, gpui::HighlightStyle)> = spans
        .iter()
        .map(|(range, kind)| (range.clone(), syntax::token_style(*kind, cx.theme())))
        .collect();
    StyledText::new(text.to_string())
        .with_highlights(highlights)
        .into_any_element()
}

/// The hover comment affordance for one armable row (docs/0030 D4/D8):
/// hidden until its row is hovered; opens the row composer.
/// The stable identity of one armable diff row — the comment button's
/// element id and the page's hovered-row key.
fn row_comment_id(path: &str, anchored: &AnchoredLine) -> SharedString {
    match row_anchor(anchored) {
        Some((side, line)) => {
            SharedString::from(format!("pr-line-{}-{line}-{path}-comment", side.id_token()))
        }
        None => SharedString::default(),
    }
}

fn row_comment_button(
    page: &PrDetailPage,
    page_h: &Entity<PrDetailPage>,
    window: &mut Window,
    row_id: &SharedString,
    path: &str,
    anchored: &AnchoredLine,
) -> Option<AnyElement> {
    let (side, line) = row_anchor(anchored)?;
    // docs/0040 §4 D6 amendment: one button under the pointer, not 460.
    if page.hovered_row.as_ref() != Some(row_id) {
        return None;
    }
    let path = path.to_string();
    let id = SharedString::from(format!("pr-line-{}-{line}-comment", side.id_token()));
    Some(
        div()
            .child(
                Button::new(id)
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip("Comment on this line")
                    .accessibility_label("Comment on this line")
                    .on_click(window.listener_for(page_h, move |this, _, window, cx| {
                        this.open_line_composer(path.clone(), side, line, window, cx)
                    })),
            )
            .into_any_element(),
    )
}

/// The row composer (docs/0030 D4), rendered under its armed row while
/// open: **Add single comment** publishes an immediate thread,
/// **Start a review** queues a local draft for the Finish-review bar.
fn line_composer_card(
    page: &PrDetailPage,
    page_h: &Entity<PrDetailPage>,
    window: &mut Window,
    path: &str,
    anchored: &AnchoredLine,
    cx: &App,
) -> Option<AnyElement> {
    let composer = page.line_composer.as_ref()?;
    let (side, line) = row_anchor(anchored)?;
    if composer.path != path || composer.side != side || composer.line != line {
        return None;
    }
    let busy = page.busy.is_some();
    Some(
        v_flex()
            .debug_selector(|| "pr-line-composer-key".into())
            .id("pr-line-composer")
            .w_full()
            .min_w_0()
            .mt_1()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .p_2()
            .gap_1()
            .when_some(composer.start, |card, (_, s)| {
                card.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "Commenting on lines {}\u{2013}{} — pick another row to extend",
                            s, composer.line
                        )),
                )
            })
            .child(Textarea::new(&composer.editor))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("pr-line-comment-send")
                            .primary()
                            .xsmall()
                            .label("Add single comment")
                            .disabled(busy)
                            .on_click(window.listener_for(page_h, |this, _, window, cx| {
                                this.add_single_comment(window, cx)
                            })),
                    )
                    .child(
                        Button::new("pr-line-review-start")
                            .ghost()
                            .xsmall()
                            .label("Start a review")
                            .disabled(busy)
                            .on_click(window.listener_for(page_h, |this, _, window, cx| {
                                this.start_review_draft(window, cx)
                            })),
                    )
                    .child(
                        Button::new("pr-line-comment-cancel")
                            .ghost()
                            .xsmall()
                            .label("Cancel")
                            .disabled(busy)
                            .on_click(window.listener_for(page_h, |this, _, window, cx| {
                                this.cancel_line_composer(window, cx)
                            })),
                    ),
            )
            .into_any_element(),
    )
}

/// The queued drafts anchored to this row (docs/0030 D4) — *Pending*
/// chips like GitHub's; each discards individually, the Finish-review
/// bar publishes the whole batch.
fn draft_rows(
    page: &PrDetailPage,
    page_h: &Entity<PrDetailPage>,
    window: &mut Window,
    path: &str,
    anchored: &AnchoredLine,
    cx: &App,
) -> Vec<AnyElement> {
    let Some((side, line)) = row_anchor(anchored) else {
        return Vec::new();
    };
    page.drafts
        .iter()
        .enumerate()
        .filter(|(_, d)| d.path == path && d.side == side && d.line == line)
        .map(|(ix, d)| {
            v_flex()
                .debug_selector(move || format!("pr-draft-{ix}-key"))
                .id(SharedString::from(format!("pr-draft-{ix}")))
                .w_full()
                .min_w_0()
                .mt_1()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .p_2()
                .gap_1()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Badge::new()
                                .xsmall()
                                .color(cx.theme().warning)
                                .child("Pending"),
                        )
                        .when_some(d.start, |chip, (_, s)| {
                            chip.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("lines {}\u{2013}{}", s, d.line)),
                            )
                        })
                        .child(div().flex_1())
                        .child(
                            Button::new(SharedString::from(format!("pr-draft-{ix}-discard")))
                                .ghost()
                                .xsmall()
                                .icon(IconName::CircleX)
                                .tooltip("Discard draft")
                                .accessibility_label("Discard draft comment")
                                .on_click(window.listener_for(page_h, move |this, _, _, cx| {
                                    this.drafts.remove(ix);
                                    this.splice_diff_drafts();
                                    cx.notify();
                                })),
                        ),
                )
                .child(div().w_full().min_w_0().text_sm().child(d.body.clone()))
                .into_any_element()
        })
        .collect()
}

/// The fetch-row visibility predicate (0030 D1) — shared by
/// `threads_fetch_row` and the flatten pass so the virtualized item list
/// and the rendered element can never disagree.
fn threads_fetch_row_present(page: &PrDetailPage) -> bool {
    if page.threads.is_idle() && !page.threads.in_flight {
        return false;
    }
    !(page.threads.data().is_some() && page.threads.load_error().is_none())
}

/// While threads load with nothing landed: a muted hint under the diff;
/// on failure (nothing landed) a retry row — the diff itself must never
/// break (0030 D1, docs/0031 D3).
fn threads_fetch_row(
    page: &PrDetailPage,
    page_h: &Entity<PrDetailPage>,
    window: &mut Window,
    cx: &App,
) -> Option<AnyElement> {
    // Idle-with-data renders no row (the threads are up); a first-load
    // fetch shows the hint; a failure — with nothing landed — the retry
    // row (0030 D1). A failed refresh of landed data also shows the retry
    // row: `load_error` covers `Failed` and the D3 `last_error` slot.
    if !threads_fetch_row_present(page) {
        return None;
    }
    if let Some(msg) = page.threads.load_error() {
        return Some(
            h_flex()
                .id("pr-threads-error")
                .px_3()
                .py_2()
                .gap_2()
                .text_xs()
                .text_color(cx.theme().danger)
                .child(msg.to_string())
                .child(
                    Button::new("pr-threads-retry")
                        .ghost()
                        .xsmall()
                        .icon(IconName::RotateCw)
                        .tooltip("Retry")
                        .accessibility_label("Retry loading review threads")
                        .on_click(window.listener_for(page_h, |this, _, _, cx| {
                            this.fetch_threads(
                                crate::usage::Origin::new(
                                    crate::usage::Surface::PrPage,
                                    crate::usage::Trigger::Manual,
                                ),
                                cx,
                            )
                        })),
                )
                .into_any_element(),
        );
    }
    if page.threads.is_loading() {
        return Some(
            div()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Loading review threads…")
                .into_any_element(),
        );
    }
    None
}

/// The end-of-file bucket for outdated / unanchorable threads (0030 D3):
/// collapsed by default, expandable per file; expand state is page state
/// keyed by path so refetches don't collapse it behind the user's back.
fn stray_bucket(
    page: &PrDetailPage,
    page_h: &Entity<PrDetailPage>,
    window: &mut Window,
    placed: &ThreadPlacement<'_>,
    cx: &App,
) -> Option<AnyElement> {
    if placed.stray.is_empty() {
        return None;
    }
    let path = placed
        .stray
        .first()
        .map(|(_, t)| t.path.clone())
        .unwrap_or_default();
    let open = page
        .outdated_open
        .get(path.as_str())
        .copied()
        .unwrap_or(false);
    let n = placed.stray.len();
    let mut out = vec![
        div()
            .debug_selector(|| "pr-outdated-toggle-key".into())
            .child(
                Button::new("pr-outdated-toggle")
                    .ghost()
                    .small()
                    .label(if open {
                        format!("Hide outdated conversations ({n})")
                    } else {
                        format!("Outdated conversations ({n})")
                    })
                    .icon(if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .on_click(window.listener_for(page_h, move |this, _, _, cx| {
                        let entry = this.outdated_open.entry(path.clone()).or_insert(false);
                        *entry = !*entry;
                        this.splice_diff_stray();
                        cx.notify();
                    })),
            )
            .into_any_element(),
    ];
    if open {
        out.extend(
            placed
                .stray
                .iter()
                .map(|&(ix, t)| thread_card(page, page_h, window, ix, t, cx)),
        );
    }
    Some(
        v_flex()
            .id("pr-outdated-bucket")
            .px_3()
            .py_1()
            .gap_1()
            .children(out)
            .into_any_element(),
    )
}

/// One review thread card (0030 D3/D6): header (author, comment count,
/// resolved/outdated chips, Open on GitHub) over the comment bodies —
/// markdown with suggestion blocks rendered as cards (D6). Read-only in
/// stage 1: reply/resolve affordances land with stage 2 (#208).
fn thread_card(
    page: &PrDetailPage,
    page_h: &Entity<PrDetailPage>,
    window: &mut Window,
    ix: usize,
    t: &PrThread,
    cx: &App,
) -> AnyElement {
    let author = t
        .comments
        .first()
        .and_then(|c| c.author.as_ref())
        .map(|a| a.login.clone())
        .unwrap_or_else(|| "ghost".into());
    let replies = t.comments.len().saturating_sub(1);
    let permalink = t.comments.first().and_then(|c| c.url.clone());
    // #211: multi-line threads say which lines they span.
    let end = t
        .line
        .and_then(|l| u32::try_from(l).ok())
        .filter(|l| *l != 0);
    let start = t
        .start_line
        .and_then(|l| u32::try_from(l).ok())
        .filter(|l| *l != 0);
    let span = match (start, end) {
        (Some(s), Some(e)) if s < e => format!("lines {s}\u{2013}{e}"),
        (_, Some(e)) => format!("line {e}"),
        _ => String::new(),
    };
    let header = if replies > 0 && !span.is_empty() {
        format!("{author} · {span} · {} replies", replies + 1)
    } else if replies > 0 {
        format!("{author} · {} replies", replies + 1)
    } else if !span.is_empty() {
        format!("{author} · {span}")
    } else {
        author.clone()
    };
    // docs/0049 D1: the card is a labeled region — author/span header
    // plus resolved state.
    let thread_label = format!(
        "Review thread — {header}{}",
        if t.is_resolved { " · resolved" } else { "" }
    );
    div()
        .debug_selector(move || format!("pr-thread-{ix}-card"))
        .id(SharedString::from(format!("pr-thread-{ix}")))
        .role(accesskit::Role::ListItem)
        .aria_label(thread_label)
        .w_full()
        .min_w_0()
        .mt_1()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .p_2()
        .gap_1()
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(header),
                )
                .child(thread_chip(t, cx))
                .child(div().flex_1())
                // 0030 D4/D5: reply and resolve/unresolve — schema-gated
                // (viewerCanReply / viewerCanResolve / viewerCanUnresolve).
                .when(t.viewer_can_reply, |row| {
                    row.child(
                        Button::new(SharedString::from(format!("pr-thread-{ix}-reply")))
                            .ghost()
                            .xsmall()
                            .label("Reply")
                            .disabled(page.busy.is_some())
                            .on_click({
                                let thread_id = t.id.clone();
                                window.listener_for(page_h, move |this, _, window, cx| {
                                    this.open_reply(thread_id.clone(), window, cx)
                                })
                            }),
                    )
                })
                .when(t.viewer_can_resolve && !t.is_resolved, |row| {
                    row.child(
                        Button::new(SharedString::from(format!("pr-thread-{ix}-resolve")))
                            .ghost()
                            .xsmall()
                            .label("Resolve")
                            .disabled(page.busy.is_some())
                            .on_click({
                                let thread_id = t.id.clone();
                                window.listener_for(page_h, move |this, _, window, cx| {
                                    this.open_resolve_picker(thread_id.clone(), window, cx)
                                })
                            }),
                    )
                })
                .when(t.viewer_can_unresolve && t.is_resolved, |row| {
                    row.child(
                        Button::new(SharedString::from(format!("pr-thread-{ix}-unresolve")))
                            .ghost()
                            .xsmall()
                            .label("Unresolve")
                            .disabled(page.busy.is_some())
                            .on_click({
                                let thread_id = t.id.clone();
                                window.listener_for(page_h, move |this, _, window, cx| {
                                    this.set_thread_resolved(
                                        thread_id.clone(),
                                        false,
                                        None,
                                        window,
                                        cx,
                                    )
                                })
                            }),
                    )
                }),
        )
        .children(
            t.comments
                .iter()
                .enumerate()
                .map(|(m, c)| thread_comment(page, page_h, window, ix, m, c, cx)),
        )
        .when_some(permalink, |row, url| {
            row.child(
                Button::new(SharedString::from(format!("pr-thread-{ix}-open")))
                    .ghost()
                    .xsmall()
                    .icon(IconName::ExternalLink)
                    .tooltip("Open on GitHub")
                    .accessibility_label("Open thread on GitHub")
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            )
        })
        .when(
            page.resolve_picker.as_deref() == Some(t.id.as_str())
                && t.viewer_can_resolve
                && !t.is_resolved,
            |card| {
                card.child(resolve_picker_row(
                    page_h,
                    window,
                    page.busy.is_some(),
                    ix,
                    t.id.clone(),
                ))
            },
        )
        .when_some(
            page.reply_composer
                .as_ref()
                .filter(|(tid, _)| tid == &t.id)
                .map(|(_, editor)| editor.clone()),
            |card, editor| {
                card.child(reply_box(
                    page_h,
                    window,
                    page.busy.is_some(),
                    ix,
                    editor,
                    cx,
                ))
            },
        )
        .into_any_element()
}

/// The resolve-reason picker (#213) — the Resolve button opens this
/// row; every choice fires the resolve immediately (the enum's
/// `ADDRESSED | WONT_FIX | INVALID`, or the plain resolve with the
/// field omitted). Always visible once open (0019 house style — no
/// hover-only discovery).
fn resolve_picker_row(
    page_h: &Entity<PrDetailPage>,
    window: &mut Window,
    busy: bool,
    ix: usize,
    thread_id: String,
) -> AnyElement {
    let mut row = h_flex()
        .debug_selector(|| "pr-resolve-picker-key".into())
        .id(SharedString::from(format!("pr-thread-{ix}-resolve-picker")))
        .w_full()
        .min_w_0()
        .mt_1()
        .flex_wrap()
        .gap_2()
        .child(
            Button::new(SharedString::from(format!("pr-thread-{ix}-resolve-plain")))
                .ghost()
                .xsmall()
                .label("Resolve (no reason)")
                .disabled(busy)
                .on_click({
                    let thread_id = thread_id.clone();
                    window.listener_for(page_h, move |this, _, window, cx| {
                        this.set_thread_resolved(thread_id.clone(), true, None, window, cx)
                    })
                }),
        );
    for (label, reason) in ResolutionReason::CHOICES {
        row = row.child(
            Button::new(SharedString::from(format!(
                "pr-thread-{ix}-resolve-{}",
                reason.id_token()
            )))
            .ghost()
            .xsmall()
            .label(label)
            .disabled(busy)
            .on_click({
                let thread_id = thread_id.clone();
                window.listener_for(page_h, move |this, _, window, cx| {
                    this.set_thread_resolved(thread_id.clone(), true, Some(reason), window, cx)
                })
            }),
        );
    }
    row.child(
        Button::new(SharedString::from(format!("pr-thread-{ix}-resolve-cancel")))
            .ghost()
            .xsmall()
            .label("Cancel")
            .disabled(busy)
            .on_click(window.listener_for(page_h, |this, _, _, cx| this.cancel_resolve_picker(cx))),
    )
    .into_any_element()
}

/// The thread's open reply box (docs/0030 D4) — Send posts immediately
/// via `addPullRequestReviewComment { body, inReplyTo }` (replies
/// cannot ride a draft batch).
fn reply_box(
    page_h: &Entity<PrDetailPage>,
    window: &mut Window,
    busy: bool,
    ix: usize,
    editor: Entity<TextareaState>,
    cx: &App,
) -> AnyElement {
    v_flex()
        .id(SharedString::from(format!("pr-thread-{ix}-reply-box")))
        .w_full()
        .min_w_0()
        .mt_1()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .p_2()
        .gap_1()
        .child(Textarea::new(&editor))
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new(SharedString::from(format!("pr-thread-{ix}-reply-send")))
                        .primary()
                        .xsmall()
                        .label("Send")
                        .disabled(busy)
                        .on_click(window.listener_for(page_h, |this, _, window, cx| {
                            this.send_reply(window, cx)
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("pr-thread-{ix}-reply-cancel")))
                        .ghost()
                        .xsmall()
                        .label("Cancel")
                        .disabled(busy)
                        .on_click(window.listener_for(page_h, |this, _, window, cx| {
                            this.cancel_reply(window, cx)
                        })),
                ),
        )
        .into_any_element()
}

/// The sticky Finish-review bar (docs/0030 D4) — present while drafts
/// exist: optional summary, the event choice (Comment default),
/// **Submit** running ONE batched `addPullRequestReview`, and Discard.
fn finish_review_bar(page: &PrDetailPage, cx: &Context<PrDetailPage>) -> Option<AnyElement> {
    let n = page.drafts.len();
    if n == 0 {
        return None;
    }
    let busy = page.busy.is_some();
    let mut bar = v_flex()
        .debug_selector(|| "pr-finish-review-key".into())
        .id("pr-finish-review")
        .w_full()
        .px_3()
        .py_2()
        .gap_2()
        .border_t_1()
        .border_color(cx.theme().border);
    if let Some(summary) = page.finish_summary.as_ref() {
        bar = bar.child(
            div()
                .id("pr-finish-review-summary")
                .w_full()
                .min_w_0()
                .child(Textarea::new(summary)),
        );
    }
    let mut actions = h_flex().w_full().gap_2().items_center().child(
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(if n == 1 {
                "1 pending comment — finish your review".to_string()
            } else {
                format!("{n} pending comments — finish your review")
            }),
    );
    for (label, event) in FinishEvent::CHOICES {
        let selected = page.finish_event == event;
        let id = SharedString::from(format!("pr-finish-review-event-{}", event.id_token()));
        let button = if selected {
            Button::new(id).primary().xsmall().label(label)
        } else {
            Button::new(id).ghost().xsmall().label(label)
        };
        actions = actions.child(
            button
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| this.set_finish_event(event, cx))),
        );
    }
    actions = actions
        .child(div().flex_1())
        .child(
            Button::new("pr-finish-review-submit")
                .primary()
                .xsmall()
                .label(if busy {
                    "Submitting…"
                } else {
                    "Submit review"
                })
                .disabled(!review_batch_allowed(n, busy))
                .on_click(cx.listener(|this, _, window, cx| this.submit_review(window, cx))),
        )
        .child(
            Button::new("pr-finish-review-discard")
                .ghost()
                .xsmall()
                .label("Discard")
                .disabled(busy)
                .on_click(cx.listener(|this, _, window, cx| this.discard_review(window, cx))),
        );
    Some(bar.child(actions).into_any_element())
}

/// The Files-scope mutation error (docs/0030 D7) — an inline danger row
/// above the diff; never modal, never breaks the pane.
fn files_error_row(page: &PrDetailPage, cx: &Context<PrDetailPage>) -> Option<AnyElement> {
    let error = page
        .mutation_error
        .as_ref()
        .filter(|e| e.scope == ErrorScope::Files)?;
    Some(
        div()
            .id("pr-files-error")
            .debug_selector(|| "pr-files-error-key".into())
            .w_full()
            .px_3()
            .py_2()
            .text_xs()
            .text_color(cx.theme().danger)
            .child(format!("Review comment failed: {}", error.message))
            .into_any_element(),
    )
}

/// The resolved / outdated chip on a thread header (0030 D3).
fn thread_chip(t: &PrThread, cx: &App) -> AnyElement {
    if t.is_resolved {
        let by = t
            .resolved_by
            .as_deref()
            .map(|l| format!("Resolved by {l}"))
            .unwrap_or_else(|| "Resolved".into());
        Badge::new()
            .xsmall()
            .color(cx.theme().success)
            .child(by)
            .into_any_element()
    } else if t.is_outdated {
        Badge::new()
            .xsmall()
            .color(cx.theme().muted_foreground)
            .child("Outdated")
            .into_any_element()
    } else {
        div().into_any_element()
    }
}

/// One comment inside a thread card (0030 D6): author + age over the
/// markdown body; ```suggestion blocks render as suggestion cards.
fn thread_comment(
    page: &PrDetailPage,
    page_h: &Entity<PrDetailPage>,
    window: &mut Window,
    ix: usize,
    m: usize,
    c: &crate::gql::PrThreadComment,
    cx: &App,
) -> AnyElement {
    let author = c
        .author
        .as_ref()
        .map(|a| a.login.clone())
        .unwrap_or_else(|| "ghost".into());
    let key = format!("pr-thread-{ix}-comment-{m}");
    let mut age = relative_age(c.created_at.as_deref());
    if row_is_edited(c.created_at.as_deref(), c.updated_at.as_deref()) {
        age.push_str(" (edited)");
    }
    let blocks = page.md.get(&format!("thread-{ix}-comment-{m}")).cloned();
    div()
        .id(SharedString::from(format!("pr-thread-{ix}-comment-{m}")))
        .w_full()
        .min_w_0()
        .border_t_1()
        .border_color(cx.theme().border)
        .pt_1()
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{author} · {age}")),
                )
                // #212: edit/delete on the viewer's own comments — the
                // 0019 D4 affordances, schema-gated (viewerCanUpdate /
                // viewerCanDelete).
                .when(c.viewer_can_update, |row| {
                    row.child(
                        Button::new(SharedString::from(format!("{key}-edit")))
                            .ghost()
                            .xsmall()
                            .label("Edit")
                            .disabled(page.busy.is_some())
                            .on_click({
                                let node_id = c.id.clone();
                                let body = c.body.clone();
                                window.listener_for(page_h, move |this, _, window, cx| {
                                    this.start_thread_edit(
                                        node_id.clone(),
                                        body.clone(),
                                        window,
                                        cx,
                                    )
                                })
                            }),
                    )
                })
                .when(c.viewer_can_delete, |row| {
                    row.child(
                        Button::new(SharedString::from(format!("{key}-delete")))
                            .ghost()
                            .xsmall()
                            .label("Delete")
                            .disabled(page.busy.is_some())
                            .on_click({
                                let node_id = c.id.clone();
                                window.listener_for(page_h, move |this, _, _, cx| {
                                    this.start_thread_delete_confirm(node_id.clone(), cx)
                                })
                            }),
                    )
                }),
        )
        // #212: the comment swaps to its editor or delete confirm while
        // one is active for this node (0019 D4).
        .when_some(
            page.thread_edit
                .as_ref()
                .filter(|(nid, _)| nid == &c.id)
                .map(|(_, e)| e.clone()),
            |row, editor| {
                row.child(inline_edit_card(
                    &format!("{key}-edit"),
                    "Editing comment",
                    &editor,
                    page.busy.is_some(),
                    window.listener_for(page_h, |this, _, window, cx| {
                        this.save_thread_edit(window, cx)
                    }),
                    window.listener_for(page_h, |this, _, window, cx| {
                        this.cancel_thread_edit(window, cx)
                    }),
                    cx,
                ))
            },
        )
        .when(
            page.thread_delete_confirm.as_deref() == Some(c.id.as_str()),
            |row| {
                row.child(delete_confirm_card(
                    &format!("{key}-delete"),
                    "Delete this comment?",
                    page.busy.is_some(),
                    window.listener_for(page_h, |this, _, window, cx| {
                        this.confirm_thread_delete(window, cx)
                    }),
                    window.listener_for(page_h, |this, _, _, cx| {
                        this.cancel_thread_delete_confirm(cx)
                    }),
                    cx,
                ))
            },
        )
        .children(blocks.map(|blocks| {
            v_flex()
                .w_full()
                .min_w_0()
                .text_sm()
                .gap_1()
                .children(blocks.iter().enumerate().map(|(b, block)| match block {
                    markdown::Block::Code {
                        lang: Some(lang),
                        code,
                    } if lang == "suggestion" => suggestion_card(ix, m, b, code, cx),
                    _ => markdown::render_block(
                        block,
                        &format!("pr-thread-{ix}-comment-{m}-{b}"),
                        cx,
                    ),
                }))
                .into_any_element()
        }))
        .into_any_element()
}

/// The suggestion card (0030 D6): label + the replacement lines + Copy —
/// applying has no public API, so the flow finishes on GitHub (the card
/// above the thread links out).
fn suggestion_card(ix: usize, m: usize, b: usize, code: &str, cx: &App) -> AnyElement {
    let id = SharedString::from(format!("pr-suggestion-{ix}-{m}-{b}-copy"));
    let payload = code.to_string();
    v_flex()
        .id(SharedString::from(format!("pr-suggestion-{ix}-{m}-{b}")))
        .role(accesskit::Role::ListItem)
        .aria_label("Suggested change")
        .debug_selector(move || format!("pr-suggestion-{ix}-{m}-{b}-card"))
        .w_full()
        .min_w_0()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .p_2()
        .gap_1()
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Suggested change"),
                )
                .child(
                    Button::new(id)
                        .ghost()
                        .xsmall()
                        .label("Copy")
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(payload.clone()));
                        }),
                ),
        )
        .child(
            // Suggestion code renders one non-wrapping row per line — the
            // diff-pane convention (0018 D8); gpui has no pre-wrap mode.
            v_flex()
                .min_w_0()
                .font_family(cx.theme().mono_font_family.clone())
                .text_xs()
                .children(
                    code.lines()
                        .map(|l| div().flex_none().whitespace_nowrap().child(l.to_string())),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{
        FileEntry, FileListDelegate, FilesSummary, build_file_forest, files_summary, path_matches,
        row_anchor, split_path,
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

    // --- thread placement (docs/0030 D3/§5) ------------------------------

    use crate::files::place_threads;
    use crate::gql::{DiffSide, PrThread, PrThreadComment, ThreadSubject};
    use crate::service::{AnchoredLine, PatchLine, anchor_patch};

    fn fixture_thread(
        ix: usize,
        path: &str,
        line: Option<i64>,
        side: DiffSide,
        subject: ThreadSubject,
        outdated: bool,
    ) -> PrThread {
        fixture_thread_range(ix, path, line, None, side, subject, outdated)
    }

    fn fixture_thread_range(
        ix: usize,
        path: &str,
        line: Option<i64>,
        start_line: Option<i64>,
        side: DiffSide,
        subject: ThreadSubject,
        outdated: bool,
    ) -> PrThread {
        PrThread {
            id: format!("T_{ix}"),
            path: path.into(),
            line,
            start_line,
            side,
            subject,
            is_outdated: outdated,
            is_resolved: false,
            resolved_by: None,
            viewer_can_reply: false,
            viewer_can_resolve: false,
            viewer_can_unresolve: false,
            comments: vec![PrThreadComment {
                id: format!("T_{ix}_C_0"),
                author: None,
                body: "body".into(),
                created_at: None,
                updated_at: None,
                last_edited_at: None,
                url: None,
                viewer_can_update: false,
                viewer_can_delete: false,
                viewer_did_author: false,
            }],
        }
    }

    fn rows_for(patch: &str) -> Vec<AnchoredLine> {
        anchor_patch(patch)
    }

    #[test]
    fn placement_anchors_ranges_at_their_last_mapped_row() {
        // #211 (D3): a thread spanning lines anchors when ANY line of
        // its range maps to a row — under the LAST such row, GitHub's
        // range-end convention. Rows: 0 hunk, 1 ctx(new 11), 2 del(old
        // 11), 3 add(new 12).
        let rows = rows_for("@@ -10,2 +11,2 @@\n ctx\n-del\n+add");
        let threads = [
            // 2..=12 on RIGHT: only 11 and 12 map; anchors under row 3.
            fixture_thread_range(
                0,
                "f.rs",
                Some(12),
                Some(2),
                DiffSide::Right,
                ThreadSubject::Line,
                false,
            ),
            // 1..=11 on RIGHT: only 11 maps (range start outside the
            // patch) — still anchored, under row 1.
            fixture_thread_range(
                1,
                "f.rs",
                Some(11),
                Some(1),
                DiffSide::Right,
                ThreadSubject::Line,
                false,
            ),
            // 12..=40 on RIGHT: inverted span falls back to the end row.
            fixture_thread_range(
                2,
                "f.rs",
                Some(12),
                Some(40),
                DiffSide::Right,
                ThreadSubject::Line,
                false,
            ),
            // 1..=5 on RIGHT: nothing in range maps — stray.
            fixture_thread_range(
                3,
                "f.rs",
                Some(5),
                Some(1),
                DiffSide::Right,
                ThreadSubject::Line,
                false,
            ),
        ];
        let placed = place_threads(&threads, "f.rs", &rows);
        assert_eq!(placed.anchored.get(&3).unwrap()[0].0, 0);
        assert_eq!(placed.anchored.get(&1).unwrap()[0].0, 1);
        assert_eq!(
            placed.stray.iter().map(|&(ix, _)| ix).collect::<Vec<_>>(),
            vec![3]
        );
        assert!(placed.file_level.is_empty());
    }

    #[test]
    fn placement_anchors_threads_on_both_sides() {
        // Rows: 0 hunk, 1 ctx(new 11), 2 del(old 11), 3 add(new 12).
        let rows = rows_for("@@ -10,2 +11,2 @@\n ctx\n-del\n+add");
        let threads = [
            fixture_thread(
                0,
                "f.rs",
                Some(11),
                DiffSide::Right,
                ThreadSubject::Line,
                false,
            ),
            fixture_thread(
                1,
                "f.rs",
                Some(11),
                DiffSide::Left,
                ThreadSubject::Line,
                false,
            ),
        ];
        let placed = place_threads(&threads, "f.rs", &rows);
        // RIGHT@11 is the context row (1); LEFT@11 is the deleted row (2).
        assert_eq!(placed.anchored.get(&1).unwrap().len(), 1);
        assert_eq!(placed.anchored.get(&2).unwrap()[0].0, 1);
        assert!(placed.stray.is_empty() && placed.file_level.is_empty());
    }

    #[test]
    fn placement_buckets_outdated_unanchorable_and_file_threads() {
        let rows = rows_for("@@ -10,2 +11,2 @@\n ctx\n-del");
        let threads = [
            // Outdated but line present → stray (the bucket, not the row).
            fixture_thread(
                0,
                "f.rs",
                Some(11),
                DiffSide::Right,
                ThreadSubject::Line,
                true,
            ),
            // Line outside the diff (force-push mid-session) → stray.
            fixture_thread(
                1,
                "f.rs",
                Some(999),
                DiffSide::Right,
                ThreadSubject::Line,
                false,
            ),
            // FILE-subject → the header group.
            fixture_thread(2, "f.rs", None, DiffSide::Right, ThreadSubject::File, false),
            // Another path → ignored entirely.
            fixture_thread(
                3,
                "other.rs",
                Some(11),
                DiffSide::Right,
                ThreadSubject::Line,
                false,
            ),
            // LINE thread with no line at all → stray.
            fixture_thread(4, "f.rs", None, DiffSide::Right, ThreadSubject::Line, false),
        ];
        let placed = place_threads(&threads, "f.rs", &rows);
        assert!(placed.anchored.is_empty());
        assert_eq!(
            placed.stray.iter().map(|&(ix, _)| ix).collect::<Vec<_>>(),
            vec![0, 1, 4]
        );
        assert_eq!(placed.file_level.len(), 1);
        assert_eq!(placed.file_level[0].0, 2);
    }
    #[test]
    fn row_anchor_arms_the_right_side_and_line() {
        // docs/0030 D2/D4: Add/Context rows arm RIGHT + new line; Del rows
        // arm LEFT + old line; hunk rows and unanchored rows arm nothing.
        let anchored =
            |line: PatchLine, old: Option<u32>, new: Option<u32>| AnchoredLine { line, old, new };
        type Arm = (PatchLine, Option<u32>, Option<u32>, Option<(DiffSide, u32)>);
        let cases: [Arm; 5] = [
            (
                PatchLine::Hunk("@@ -1,2 +1,2 @@".into()),
                Some(1),
                Some(1),
                None,
            ),
            (
                PatchLine::Context("ok".into()),
                Some(7),
                Some(7),
                Some((DiffSide::Right, 7)),
            ),
            (
                PatchLine::Add("new".into()),
                None,
                Some(9),
                Some((DiffSide::Right, 9)),
            ),
            (
                PatchLine::Del("old".into()),
                Some(3),
                None,
                Some((DiffSide::Left, 3)),
            ),
            (PatchLine::Add("no line".into()), None, None, None),
        ];
        for (line, old, new, want) in cases {
            assert_eq!(row_anchor(&anchored(line, old, new)), want);
        }
    }
}
