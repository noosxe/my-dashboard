//! The action row's rules (docs/0083 D4, D5): which primary controls a PR
//! state shows, in which precedence, and what the `⋯` menu holds. Pure;
//! `action_row.rs` draws the result.
//!
//! Every control of docs/0083 D4 and D5 ships: Merge, Close, Ready for
//! review, Convert to draft, Reopen, Delete branch, Enable and Disable
//! auto-merge, Merge when ready and Remove from queue. [`Primary::wired`]
//! and [`MenuItem::wired`] stay as the switch for a control a later
//! change lays out before it has a handler.
//!
//! On a base branch with a merge queue the plain Merge is never offered
//! (settled 2026-11-14): "Merge when ready" enqueues exactly when the Merge
//! gate passes, Enable auto-merge takes over when it doesn't, and a
//! disabled "Merge when ready" shows when the viewer can do neither.

use super::super::merge_gate::{close_visible, enable_auto_visible, merge_enabled, merge_visible};
use super::super::{PrStateTone, pr_state_tone};
use crate::gql::PrDetail;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Primary {
    /// The Merge split button, clickable.
    Merge,
    /// The Merge split button with its action half disabled (the caret
    /// stays usable); the tooltip is the verdict and the failing gates.
    MergeBlocked,
    EnableAutoMerge,
    /// "Merge when ready", clickable: enqueues after its confirm.
    MergeWhenReady,
    /// The same button, action half disabled (tooltip: the verdict).
    MergeWhenReadyBlocked,
    RemoveFromQueue,
    DisableAutoMerge,
    ReadyForReview,
    DeleteBranch,
    Reopen,
}

impl Primary {
    /// Whether the control has a handler yet.
    pub(crate) fn wired(self) -> bool {
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuItem {
    ConvertToDraft,
    Separator,
    Close,
}

impl MenuItem {
    pub(crate) fn wired(self) -> bool {
        true
    }
}

/// What the precedence rules read: the PR's state and what the viewer may
/// do. Fields for actions that haven't shipped stay `false` until their
/// issue queries the gate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Facts {
    pub merged: bool,
    pub closed: bool,
    pub draft: bool,
    pub queued: bool,
    pub auto_merge_on: bool,
    /// The base branch has a merge queue (#775).
    pub queue_present: bool,
    /// D2 rows 10–11 plus an allowed method: a normal merge would go.
    pub mergeable: bool,
    /// D2 rows 10–11 alone. The queue picks its own method, so a repository
    /// with no method of its own still enqueues.
    pub queue_ready: bool,
    pub can_merge: bool,
    pub can_enable_auto: bool,
    pub can_disable_auto: bool,
    pub can_dequeue: bool,
    pub can_ready: bool,
    pub can_delete_branch: bool,
    pub can_reopen: bool,
    pub can_update: bool,
    pub can_close: bool,
}

impl Facts {
    pub(crate) fn of(d: &PrDetail, no_methods: bool) -> Self {
        let tone = pr_state_tone(d.state.as_deref(), d.draft);
        Facts {
            merged: tone == PrStateTone::Merged,
            closed: tone == PrStateTone::Closed,
            draft: tone == PrStateTone::Draft,
            queued: d.queueing.in_merge_queue,
            auto_merge_on: d.queueing.auto_merge.is_some(),
            queue_present: d.queueing.queue_enabled,
            mergeable: merge_enabled(
                d.state.as_deref(),
                d.draft,
                d.mergeable.as_deref(),
                d.merge_state_status.as_deref(),
                false,
                no_methods,
            ),
            queue_ready: merge_enabled(
                d.state.as_deref(),
                d.draft,
                d.mergeable.as_deref(),
                d.merge_state_status.as_deref(),
                false,
                false,
            ),
            can_merge: merge_visible(d.viewer_permission.as_deref()),
            // #774: the repository allows auto-merge and the server says
            // the viewer may enable it; never while it is on or queued.
            can_enable_auto: enable_auto_visible(d),
            can_disable_auto: d.queueing.viewer_can_disable,
            // #775: GitHub has no dequeue flag; removing needs the same
            // write access as merging.
            can_dequeue: merge_visible(d.viewer_permission.as_deref()),
            // #771, #772: `viewerCanUpdate` is the gate, there is no flag of
            // its own.
            can_ready: d.viewer_can_update,
            can_update: d.viewer_can_update,
            can_close: close_visible(d.viewer_can_close),
            // #770: the server's own per-viewer answer.
            can_reopen: d.viewer_can_reopen,
            // #773: write access on the head repository, not its default
            // branch (docs/0083 §3.1).
            can_delete_branch: d.head_branch.as_ref().is_some_and(|h| h.deletable()),
        }
    }
}

/// The primary controls, first matching row of D4 only (a closed PR has
/// two). A row whose control the viewer can't use falls through to the
/// next; none left means no action row.
pub(crate) fn primaries(f: &Facts) -> Vec<Primary> {
    if f.merged {
        return f
            .can_delete_branch
            .then_some(Primary::DeleteBranch)
            .into_iter()
            .collect();
    }
    if f.closed {
        let mut out = Vec::new();
        if f.can_reopen {
            out.push(Primary::Reopen);
        }
        if f.can_delete_branch {
            out.push(Primary::DeleteBranch);
        }
        return out;
    }
    if f.draft {
        return f
            .can_ready
            .then_some(Primary::ReadyForReview)
            .into_iter()
            .collect();
    }
    let one = if f.queued && f.can_dequeue {
        Some(Primary::RemoveFromQueue)
    } else if f.auto_merge_on && f.can_disable_auto {
        Some(Primary::DisableAutoMerge)
    } else if f.queue_present {
        queue_primary(f)
    } else if f.mergeable && f.can_merge {
        Some(Primary::Merge)
    } else if !f.mergeable && f.can_enable_auto {
        Some(Primary::EnableAutoMerge)
    } else if f.can_merge {
        Some(Primary::MergeBlocked)
    } else {
        None
    };
    one.into_iter().collect()
}

/// The primary on a base branch with a merge queue: never the plain Merge.
/// A PR already queued has nothing but Remove from queue (handled by the
/// caller), so a viewer who can't remove it sees no primary.
fn queue_primary(f: &Facts) -> Option<Primary> {
    if f.queued {
        None
    } else if f.queue_ready && f.can_merge {
        Some(Primary::MergeWhenReady)
    } else if f.can_enable_auto {
        Some(Primary::EnableAutoMerge)
    } else if f.can_merge {
        Some(Primary::MergeWhenReadyBlocked)
    } else {
        None
    }
}

/// The `⋯` menu's items, top to bottom (D5): items the viewer can't use
/// are left out, and a menu with none hides the button.
pub(crate) fn menu(f: &Facts) -> Vec<MenuItem> {
    if f.merged || f.closed {
        return Vec::new();
    }
    let mut out = Vec::new();
    if !f.draft && f.can_update {
        out.push(MenuItem::ConvertToDraft);
    }
    if f.can_close {
        if !out.is_empty() {
            out.push(MenuItem::Separator);
        }
        out.push(MenuItem::Close);
    }
    out
}

/// What the row draws today: unbuilt items dropped, and a separator left
/// with nothing on one side dropped too.
pub(crate) fn shown_menu(f: &Facts) -> Vec<MenuItem> {
    let items: Vec<_> = menu(f)
        .into_iter()
        .filter(|i| *i == MenuItem::Separator || i.wired())
        .collect();
    let mut out: Vec<MenuItem> = Vec::new();
    for item in items {
        if item == MenuItem::Separator && out.last().is_none_or(|l| *l == MenuItem::Separator) {
            continue;
        }
        out.push(item);
    }
    while out.last() == Some(&MenuItem::Separator) {
        out.pop();
    }
    out
}

/// The primaries the row draws today.
pub(crate) fn shown_primaries(f: &Facts) -> Vec<Primary> {
    primaries(f).into_iter().filter(|p| p.wired()).collect()
}

#[cfg(test)]
mod tests;
