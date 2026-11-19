//! docs/0084 D8, Q8: data that lands by itself never moves what the user
//! is reading. The timeline scrolls by a pixel offset from its first
//! child, so a landing that changes what sits above the reading position
//! (an older comment appears, a card above grows or goes) would slide the
//! content under the reader.
//!
//! The page remembers the first visible card by its key and how far
//! below the viewport's top it started ([`Anchor`]). After the new frame
//! has been laid out it moves the offset so that card is back where it
//! was ([`restored_offset`]). A landing that only appends below leaves
//! the offset alone, and a page scrolled to the top stays at the top.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Context, Pixels, ScrollHandle, px};

use crate::pr_page::PrDetailPage;

/// The keys of the timeline's children, in order, as the last frame
/// painted them. Shared with the next-frame callback.
pub(crate) type Keys = Rc<RefCell<Vec<String>>>;

/// A card the reader is looking at, and where on screen it was.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Anchor {
    pub(crate) key: String,
    /// The card's top relative to the viewport's top: negative when the
    /// card starts above the viewport.
    pub(crate) rel_top: f32,
}

/// The anchor for a viewport whose first visible child is `top_ix` at
/// `rel_top`. A timeline at its very top (`scrolled` false) has nothing
/// to keep.
pub(crate) fn capture(
    keys: &[String],
    top_ix: usize,
    rel_top: f32,
    scrolled: bool,
) -> Option<Anchor> {
    if !scrolled {
        return None;
    }
    keys.get(top_ix).map(|key| Anchor {
        key: key.clone(),
        rel_top,
    })
}

/// Where the anchored card is in the new key list.
pub(crate) fn find(keys: &[String], anchor: &Anchor) -> Option<usize> {
    keys.iter().position(|k| *k == anchor.key)
}

/// The scroll offset (negative when scrolled) that puts the card back at
/// its old place: `item_top` and `viewport_top` are layout coordinates.
pub(crate) fn restored_offset(anchor: &Anchor, item_top: f32, viewport_top: f32) -> f32 {
    (anchor.rel_top + viewport_top - item_top).min(0.0)
}

impl PrDetailPage {
    /// After building the timeline's children: record their keys, in
    /// the order they stand. The description and the cap row lead, one
    /// card per entry follows (an editor or a confirm takes its card's
    /// place), and the notices and the composer close the list.
    pub(in crate::pr_page) fn note_timeline_keys(&self, d: &crate::gql::PrDetail, children: usize) {
        let mut keys = vec!["~description".to_string()];
        if crate::pr_page::conversation_window::hidden_older(d) > 0 {
            keys.push("~older-cap".to_string());
        }
        for entry in crate::pr_page::timeline_events::entries(d) {
            use crate::pr_page::timeline_events::Entry;
            keys.push(match entry {
                Entry::Card(crate::pr_page::TimelineKind::Comment, ix) => {
                    d.comments.nodes[ix].id.clone()
                }
                Entry::Card(crate::pr_page::TimelineKind::Review, ix) => {
                    d.reviews.nodes[ix].id.clone()
                }
                Entry::Event(ix) => d.events.nodes[ix].id.clone(),
                Entry::Xref(ix) => d.references.nodes[ix].id.clone(),
            });
        }
        while keys.len() < children {
            keys.push(format!("~tail-{}", keys.len()));
        }
        *self.timeline_keys.borrow_mut() = keys;
    }

    /// Before a landing: remember what the reader is looking at.
    pub(in crate::pr_page) fn capture_timeline_anchor(&mut self) {
        if self.tab != crate::pr_page::PrTab::Conversation {
            return;
        }
        let handle = &self.timeline_scroll;
        let (top_ix, rel) = handle.logical_scroll_top();
        let scrolled = handle.offset().y < px(0.);
        self.timeline_anchor = capture(
            &self.timeline_keys.borrow(),
            top_ix,
            f32::from(rel),
            scrolled,
        )
        .map(|anchor| Pending {
            anchor,
            painted: false,
        });
    }

    /// At the start of a frame: after a landing the first frame paints the
    /// new cards, which gives them bounds; the next frame starts here by
    /// putting the remembered card back, before it lays anything out.
    ///
    /// The follow-up frame is asked for with `cx.notify()`, not
    /// `window.refresh()`: a retained-mode backend (gpui-fast) redraws a
    /// view only when something it tracks changed, so a bare refresh would
    /// never run this render again and the card would stay shifted.
    pub(in crate::pr_page) fn restore_timeline_anchor(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.timeline_anchor.as_mut() else {
            return;
        };
        if self.tab != crate::pr_page::PrTab::Conversation {
            self.timeline_anchor = None;
            return;
        }
        if !pending.painted {
            pending.painted = true;
            cx.notify();
            return;
        }
        let pending = self.timeline_anchor.take().expect("checked above");
        if apply(
            &self.timeline_scroll,
            &self.timeline_keys.borrow(),
            &pending.anchor,
        ) {
            cx.notify();
        }
    }
}

/// An anchor waiting for the frame that has laid the new cards out.
#[derive(Clone, Debug)]
pub(crate) struct Pending {
    anchor: Anchor,
    painted: bool,
}

/// Move `handle`'s offset; `true` when it moved.
fn apply(handle: &ScrollHandle, keys: &[String], anchor: &Anchor) -> bool {
    let Some(ix) = find(keys, anchor) else {
        return false;
    };
    let Some(item) = handle.bounds_for_item(ix) else {
        return false;
    };
    let want = restored_offset(
        anchor,
        f32::from(item.top()),
        f32::from(handle.bounds().top()),
    );
    let now = handle.offset();
    if (f32::from(now.y) - want).abs() < 0.5 {
        return false;
    }
    handle.set_offset(gpui::point(now.x, Pixels::from(want)));
    true
}

#[cfg(test)]
mod tests;
