//! Table tests for `actions.rs`.

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
