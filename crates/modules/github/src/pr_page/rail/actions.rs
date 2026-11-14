//! The action row's rules (docs/0083 D4, D5): which primary controls a PR
//! state shows, in which precedence, and what the `⋯` menu holds. Pure;
//! `action_row.rs` draws the result.
//!
//! Merge, Close, Ready for review, Convert to draft and Reopen ship so far.
//! The other controls (#773–#775) are laid out here by the same rules, so
//! each of those issues turns one flag on and adds its handler:
//! [`Primary::wired`] and [`MenuItem::wired`] keep an unbuilt control off
//! the screen until then.

use super::super::merge_gate::{close_visible, merge_enabled, merge_visible};
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
    MergeWhenReady,
    RemoveFromQueue,
    DisableAutoMerge,
    ReadyForReview,
    DeleteBranch,
    Reopen,
}

impl Primary {
    /// Whether the control has a handler yet (#770–#776 add the rest).
    pub(crate) fn wired(self) -> bool {
        matches!(
            self,
            Primary::Merge | Primary::MergeBlocked | Primary::ReadyForReview | Primary::Reopen
        )
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
            mergeable: merge_enabled(
                d.state.as_deref(),
                d.draft,
                d.mergeable.as_deref(),
                d.merge_state_status.as_deref(),
                false,
                no_methods,
            ),
            can_merge: merge_visible(d.viewer_permission.as_deref()),
            // #771, #772: `viewerCanUpdate` is the gate, there is no flag of
            // its own.
            can_ready: d.viewer_can_update,
            can_update: d.viewer_can_update,
            can_close: close_visible(d.viewer_can_close),
            // #770: the server's own per-viewer answer.
            can_reopen: d.viewer_can_reopen,
            // Wired by #773, #774, #775: their gates aren't queried yet.
            ..Facts::default()
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
    } else if f.queue_present && !f.queued && f.can_merge {
        Some(Primary::MergeWhenReady)
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
