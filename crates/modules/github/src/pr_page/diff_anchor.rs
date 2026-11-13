//! Keeps the diff scroll position across a refresh that changes the
//! patch (#762, docs/0070 D4).
//!
//! `DiffState::set_files` resets the list to the top, and upstream
//! offers no read of the viewport's top row, so the page can't capture
//! the scroll offset. It tracks the nearest public signal instead: the
//! last source line the pointer was over (hover or click). After a
//! swap of the *same* file's patch the line is scrolled back into view
//! when it still exists; otherwise the pane stays at the file header,
//! which is where `set_files` leaves it anyway. A different file, or a
//! refresh with no change, never touches the position (the unchanged
//! case is the hash gate in `sync_diff`).
//!
//! The head-change refetch (`head_change.rs`) lands through the same
//! `sync_diff`, so it keeps scroll by the same path.

use std::{cell::RefCell, rc::Rc};

use gpui::{Context, SharedString};
use gpui_component::diff::{DiffLinePosition, DiffState};

/// The last line the pointer was over, shared with the Diff's hover and
/// click handlers (which can't borrow the page without re-rendering it).
#[derive(Clone, Default)]
pub(crate) struct DiffAnchor {
    line: Rc<RefCell<Option<DiffLinePosition>>>,
    /// Test-only: every position handed to `scroll_to_line`.
    #[cfg(test)]
    pub(crate) restored: Rc<RefCell<Vec<DiffLinePosition>>>,
}

impl DiffAnchor {
    /// A hover or click landed on `position`. Leaving a line (`None`)
    /// keeps the last one: the reader's place is where they last were.
    pub(crate) fn note(&self, position: Option<&DiffLinePosition>) {
        if let Some(position) = position {
            *self.line.borrow_mut() = Some(position.clone());
        }
    }

    /// The remembered line, if it belongs to `path`.
    pub(crate) fn for_path(&self, path: &str) -> Option<DiffLinePosition> {
        self.line
            .borrow()
            .as_ref()
            .filter(|p| p.path().as_str() == path)
            .cloned()
    }

    pub(crate) fn clear(&self) {
        *self.line.borrow_mut() = None;
    }

    /// After `set_files`: bring the remembered line back when this is
    /// the same file whose patch changed, else show the file header.
    pub(crate) fn restore(
        &self,
        state: &mut DiffState,
        path: &SharedString,
        same_file: bool,
        cx: &mut Context<DiffState>,
    ) {
        match same_file.then(|| self.for_path(path)).flatten() {
            Some(position) => {
                #[cfg(test)]
                self.restored.borrow_mut().push(position.clone());
                state.scroll_to_line(position, cx);
            }
            None => {
                self.clear();
                state.scroll_to_file(path, cx);
            }
        }
    }
}

#[cfg(test)]
mod tests;
