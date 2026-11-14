//! The action row's rules (docs/0083 D4, D5): which primary controls a PR
//! state shows, in which precedence, and what the `⋯` menu holds. Pure;
//! `action_row.rs` draws the result.
//!
//! Only Merge and Close ship with the status block. The other controls
//! (#770–#776) are laid out here by the same rules, so each of those
//! issues turns one flag on and adds its handler: [`Primary::wired`] and
//! [`MenuItem::wired`] keep an unbuilt control off the screen until then.

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
        matches!(self, Primary::Merge | Primary::MergeBlocked)
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
        !matches!(self, MenuItem::ConvertToDraft)
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
            // #771: `viewerCanUpdate` is the gate, there is no flag of its own.
            can_ready: d.viewer_can_update,
            can_update: d.viewer_can_update,
            can_close: close_visible(d.viewer_can_close),
            // Wired by #770, #773, #774, #775: their gates aren't queried yet.
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
mod tests {
    use super::*;
    use Primary::*;

    /// A writer who can do everything, on an open PR that can't merge yet.
    fn open() -> Facts {
        Facts {
            can_merge: true,
            can_enable_auto: true,
            can_disable_auto: true,
            can_dequeue: true,
            can_ready: true,
            can_delete_branch: true,
            can_reopen: true,
            can_update: true,
            can_close: true,
            ..Facts::default()
        }
    }

    fn with(f: Facts, change: impl FnOnce(&mut Facts)) -> Facts {
        let mut f = f;
        change(&mut f);
        f
    }

    #[test]
    fn merged_closed_and_draft_states() {
        assert_eq!(
            primaries(&with(open(), |f| f.merged = true)),
            [DeleteBranch]
        );
        assert_eq!(
            primaries(&with(open(), |f| f.closed = true)),
            [Reopen, DeleteBranch]
        );
        assert_eq!(
            primaries(&with(open(), |f| f.draft = true)),
            [ReadyForReview]
        );
        // The viewer can't use it: nothing falls through for a terminal or
        // draft PR, so there is no action row.
        assert!(
            primaries(&with(open(), |f| {
                f.merged = true;
                f.can_delete_branch = false
            }))
            .is_empty()
        );
        assert_eq!(
            primaries(&with(open(), |f| {
                f.closed = true;
                f.can_reopen = false
            })),
            [DeleteBranch]
        );
        assert!(
            primaries(&with(open(), |f| {
                f.draft = true;
                f.can_ready = false
            }))
            .is_empty()
        );
    }

    #[test]
    fn open_precedence_follows_the_doc() {
        // queued > auto-merge > queue present > mergeable > auto-merge
        // possible > blocked.
        let all = with(open(), |f| {
            f.queued = true;
            f.auto_merge_on = true;
            f.queue_present = true;
            f.mergeable = true;
        });
        assert_eq!(primaries(&all), [RemoveFromQueue]);
        let f = with(all, |f| f.can_dequeue = false);
        assert_eq!(primaries(&f), [DisableAutoMerge]);
        let f = with(f, |f| f.can_disable_auto = false);
        // Queued already: "Merge when ready" is not offered again.
        assert_eq!(primaries(&f), [Merge]);
        let f = with(f, |f| f.queued = false);
        assert_eq!(primaries(&f), [MergeWhenReady]);
        let f = with(f, |f| f.queue_present = false);
        assert_eq!(primaries(&f), [Merge]);
        let f = with(f, |f| f.mergeable = false);
        assert_eq!(primaries(&f), [EnableAutoMerge]);
        let f = with(f, |f| f.can_enable_auto = false);
        assert_eq!(primaries(&f), [MergeBlocked]);
        let f = with(f, |f| f.can_merge = false);
        assert!(primaries(&f).is_empty(), "a reader gets no action row");
    }

    #[test]
    fn a_mergeable_pr_never_offers_auto_merge() {
        let f = with(open(), |f| f.mergeable = true);
        assert_eq!(primaries(&f), [Merge]);
        // Viewer can't merge but could enable auto-merge on a clean PR:
        // GitHub refuses it, so nothing shows.
        assert!(primaries(&with(f, |f| f.can_merge = false)).is_empty());
    }

    #[test]
    fn the_menu_holds_what_is_not_the_primary() {
        assert_eq!(
            menu(&open()),
            [
                MenuItem::ConvertToDraft,
                MenuItem::Separator,
                MenuItem::Close
            ]
        );
        assert_eq!(menu(&with(open(), |f| f.draft = true)), [MenuItem::Close]);
        assert!(menu(&with(open(), |f| f.merged = true)).is_empty());
        assert!(menu(&with(open(), |f| f.closed = true)).is_empty());
        // Items the viewer can't use are left out, separator included.
        assert_eq!(
            menu(&with(open(), |f| f.can_update = false)),
            [MenuItem::Close]
        );
        assert_eq!(
            menu(&with(open(), |f| f.can_close = false)),
            [MenuItem::ConvertToDraft]
        );
        assert!(
            menu(&with(open(), |f| {
                f.can_update = false;
                f.can_close = false
            }))
            .is_empty()
        );
    }

    #[test]
    fn unbuilt_controls_stay_off_the_screen() {
        // Convert to draft has its slot but no handler yet (#772).
        assert_eq!(shown_menu(&open()), [MenuItem::Close]);
        assert!(shown_menu(&with(open(), |f| f.can_close = false)).is_empty());
        assert_eq!(shown_primaries(&with(open(), |f| f.draft = true)), []);
        assert_eq!(
            shown_primaries(&with(open(), |f| f.mergeable = true)),
            [Merge]
        );
        assert_eq!(
            shown_primaries(&open()),
            [EnableAutoMerge]
                .iter()
                .copied()
                .filter(|p| p.wired())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            shown_primaries(&with(open(), |f| f.can_enable_auto = false)),
            [MergeBlocked]
        );
    }

    #[test]
    fn facts_come_from_the_detail() {
        use crate::pr_page::rail::test_support::detail;
        use serde_json::json;
        let d = detail(
            json!({"mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN", "viewerCanClose": true, "viewerCanUpdate": true}),
        );
        let f = Facts::of(&d, false);
        assert!(f.mergeable && f.can_merge && f.can_close && f.can_update);
        assert_eq!(shown_primaries(&f), [Merge]);
        // No allowed method: the merge can't go, so it is blocked.
        let f = Facts::of(&d, true);
        assert_eq!(shown_primaries(&f), [MergeBlocked]);
        let d = detail(
            json!({"viewerPermission": "READ", "viewerCanClose": false, "viewerCanUpdate": false}),
        );
        let f = Facts::of(&d, false);
        assert!(shown_primaries(&f).is_empty() && shown_menu(&f).is_empty());
        let d = detail(json!({"state": "MERGED", "merged": true}));
        assert!(Facts::of(&d, false).merged);
    }
}
