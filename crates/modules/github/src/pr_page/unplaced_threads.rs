//! Review threads the Files tab can't hang on a diff row (#760, docs/0030
//! D3): threads on paths that are no longer in the file list (a force-push
//! or rename moved them) and threads on the selected file when that file
//! shows a notice instead of a diff (`diff_notice.rs`).
//!
//! They collect in one collapsed pane-level bucket under the diff, next
//! to the per-file "Outdated conversations" bucket and styled like it, so
//! no thread is ever dropped. The open state rides `outdated_open` under
//! [`BUCKET_KEY`], which no file path can equal.

use gpui::{
    AnyElement, Entity, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, px,
};
use gpui_component::{
    IconName, Sizable as _,
    button::{Button, ButtonVariants},
    v_flex,
};

use super::PrDetailPage;
use crate::api::PrFile;
use crate::files::{Placed, thread_card};
use crate::gql::PrThread;

/// The `outdated_open` key of the pane-level bucket. A NUL can't appear
/// in a git path.
pub(crate) const BUCKET_KEY: &str = "\u{0}unplaced";

/// Threads that no diff row of the current list can host. Pure.
///
/// `noticed` is the selected file's path when its diff isn't drawn.
pub(crate) fn unplaced<'a>(
    threads: &'a [PrThread],
    files: &[PrFile],
    noticed: Option<&str>,
) -> Vec<Placed<'a>> {
    threads
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            Some(t.path.as_str()) == noticed || !files.iter().any(|f| f.filename == t.path)
        })
        .collect()
}

/// The bucket row, or `None` when there is nothing to show.
pub(crate) fn render(
    page: &PrDetailPage,
    page_h: &Entity<PrDetailPage>,
    window: &mut Window,
    files: &[PrFile],
    noticed: Option<&str>,
    cx: &gpui::App,
) -> Option<AnyElement> {
    let placed = unplaced(page.threads_nodes(), files, noticed);
    if placed.is_empty() {
        return None;
    }
    let open = page.outdated_open.get(BUCKET_KEY).copied().unwrap_or(false);
    let n = placed.len();
    let mut out = vec![
        Button::new("pr-unplaced-toggle")
            .ghost()
            .small()
            .label(if open {
                format!("Hide conversations not shown in the diff ({n})")
            } else {
                format!("Conversations not shown in the diff ({n})")
            })
            .icon(if open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .on_click(window.listener_for(page_h, move |this, _, _, cx| {
                let entry = this.outdated_open.entry(BUCKET_KEY.into()).or_insert(false);
                *entry = !*entry;
                cx.notify();
            }))
            .into_any_element(),
    ];
    if open {
        out.extend(
            placed
                .iter()
                .map(|&(ix, t)| thread_card(page, page_h, window, ix, t, cx)),
        );
    }
    Some(
        v_flex()
            .id("pr-unplaced-bucket")
            .items_start()
            .px_3()
            .py_1()
            .gap_1()
            .max_h(px(280.))
            .overflow_y_scroll()
            .children(out)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests;
