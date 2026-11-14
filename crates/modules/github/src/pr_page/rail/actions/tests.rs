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
        f.queue_ready = true;
        f.mergeable = true;
    });
    assert_eq!(primaries(&all), [RemoveFromQueue]);
    let f = with(all, |f| f.can_dequeue = false);
    assert_eq!(primaries(&f), [DisableAutoMerge]);
    let f = with(f, |f| f.can_disable_auto = false);
    // Queued already, nothing to remove it with: the plain Merge is never
    // offered on a queued base, nor "Merge when ready" again.
    assert!(primaries(&f).is_empty());
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
fn wired_controls_show_and_unbuilt_ones_stay_off_the_screen() {
    // Convert to draft (#772) joins Close in the menu.
    assert_eq!(
        shown_menu(&open()),
        [
            MenuItem::ConvertToDraft,
            MenuItem::Separator,
            MenuItem::Close
        ]
    );
    assert_eq!(
        shown_menu(&with(open(), |f| f.can_close = false)),
        [MenuItem::ConvertToDraft]
    );
    assert_eq!(
        shown_menu(&with(open(), |f| f.can_update = false)),
        [MenuItem::Close]
    );
    // A draft's primary is Ready for review (#771), with Close in the menu.
    assert_eq!(
        shown_primaries(&with(open(), |f| f.draft = true)),
        [ReadyForReview]
    );
    assert_eq!(
        shown_menu(&with(open(), |f| f.draft = true)),
        [MenuItem::Close]
    );
    // A closed PR shows Reopen and Delete branch side by side (#770,
    // #773); a merged one Delete branch alone.
    assert_eq!(
        shown_primaries(&with(open(), |f| f.closed = true)),
        [Reopen, DeleteBranch]
    );
    assert_eq!(
        shown_primaries(&with(open(), |f| {
            f.closed = true;
            f.can_reopen = false
        })),
        [DeleteBranch]
    );
    assert_eq!(
        shown_primaries(&with(open(), |f| {
            f.closed = true;
            f.can_delete_branch = false
        })),
        [Reopen]
    );
    assert_eq!(
        shown_primaries(&with(open(), |f| f.merged = true)),
        [DeleteBranch]
    );
    assert!(
        shown_primaries(&with(open(), |f| {
            f.merged = true;
            f.can_delete_branch = false
        }))
        .is_empty()
    );
    assert_eq!(
        shown_primaries(&with(open(), |f| f.mergeable = true)),
        [Merge]
    );
    // Auto-merge (#774) and the queue (#775) are wired now.
    assert_eq!(shown_primaries(&open()), [EnableAutoMerge]);
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
    // Reopen follows `viewerCanReopen`; Ready for review follows
    // `viewerCanUpdate` on a draft.
    let d = detail(json!({"state": "CLOSED", "viewerCanReopen": true}));
    let f = Facts::of(&d, false);
    assert!(f.closed && f.can_reopen);
    assert_eq!(shown_primaries(&f), [Reopen]);
    let d = detail(json!({"state": "CLOSED", "viewerCanReopen": false}));
    assert!(shown_primaries(&Facts::of(&d, false)).is_empty());
    let d = detail(json!({"isDraft": true, "viewerCanUpdate": true}));
    assert_eq!(shown_primaries(&Facts::of(&d, false)), [ReadyForReview]);
    let d = detail(json!({"isDraft": true, "viewerCanUpdate": false}));
    assert!(shown_primaries(&Facts::of(&d, false)).is_empty());
}

/// The head branch as the query returns it, for `Facts::of`.
fn head(name: &str, perm: &str, default: &str) -> serde_json::Value {
    serde_json::json!({
        "headRef": {"id": "REF_1", "name": name},
        "headRepository": {"viewerPermission": perm, "defaultBranchRef": {"name": default}},
    })
}

#[test]
fn delete_branch_follows_the_head_repository_and_its_default_branch() {
    use crate::pr_page::rail::test_support::detail;
    use serde_json::json;
    let facts = |mut patch: serde_json::Value| {
        patch["state"] = json!("MERGED");
        Facts::of(&detail(patch), false)
    };
    // A writer on the head repository, a feature branch.
    assert!(facts(head("feat", "WRITE", "main")).can_delete_branch);
    // A fork: the base permission (ADMIN here) doesn't matter, the fork's
    // does.
    let mut fork = head("patch-1", "READ", "main");
    fork["isCrossRepository"] = json!(true);
    assert!(!facts(fork).can_delete_branch);
    let mut fork = head("patch-1", "WRITE", "main");
    fork["isCrossRepository"] = json!(true);
    assert!(facts(fork).can_delete_branch);
    // A deleted fork has no head repository.
    let mut gone_fork = head("patch-1", "WRITE", "main");
    gone_fork["headRepository"] = json!(null);
    assert!(!facts(gone_fork).can_delete_branch);
    // The default branch, and a head that is already gone.
    assert!(!facts(head("main", "ADMIN", "main")).can_delete_branch);
    assert!(!facts(json!({"headRef": null})).can_delete_branch);
    // `viewerCanDeleteHeadRef` is not the gate (docs/0083 §3.1).
    let mut flag_only = json!({"headRef": null});
    flag_only["viewerCanDeleteHeadRef"] = json!(true);
    assert!(!facts(flag_only).can_delete_branch);
    let mut flag_false = head("feat", "WRITE", "main");
    flag_false["viewerCanDeleteHeadRef"] = json!(false);
    assert!(facts(flag_false).can_delete_branch);
}

/// A writer on a base branch with a merge queue, PR not queued yet.
fn queue_base() -> Facts {
    with(open(), |f| f.queue_present = true)
}

#[test]
fn a_queued_base_never_offers_the_plain_merge() {
    // Settled 2026-11-14 (docs/0083 D4): the Merge gate passes ->
    // "Merge when ready"; blocked, behind or unknown -> Enable
    // auto-merge; nothing allowed -> a disabled "Merge when ready".
    let ready = with(queue_base(), |f| {
        f.queue_ready = true;
        f.mergeable = true
    });
    assert_eq!(primaries(&ready), [MergeWhenReady]);
    // Even when a method is missing (the queue sets its own method).
    assert_eq!(
        primaries(&with(ready, |f| f.mergeable = false)),
        [MergeWhenReady]
    );
    let blocked = queue_base();
    assert_eq!(primaries(&blocked), [EnableAutoMerge]);
    assert_eq!(
        primaries(&with(blocked, |f| f.can_enable_auto = false)),
        [MergeWhenReadyBlocked]
    );
    // A viewer who can't merge sees nothing to merge with; one who can only
    // enable auto-merge still gets that.
    assert!(
        primaries(&with(blocked, |f| {
            f.can_enable_auto = false;
            f.can_merge = false
        }))
        .is_empty()
    );
    // Clean but the viewer can't merge: nothing offers the plain Merge.
    assert!(
        !primaries(&with(ready, |f| {
            f.can_merge = false;
            f.can_enable_auto = false
        }))
        .iter()
        .any(|p| matches!(p, Merge | MergeBlocked))
    );
}

#[test]
fn queued_and_auto_merge_on_have_their_own_secondary() {
    let queued = with(queue_base(), |f| f.queued = true);
    assert_eq!(primaries(&queued), [RemoveFromQueue]);
    // No removal right and nothing else to offer on a queued PR.
    assert!(primaries(&with(queued, |f| f.can_dequeue = false)).is_empty());
    let on = with(open(), |f| f.auto_merge_on = true);
    assert_eq!(primaries(&on), [DisableAutoMerge]);
    // Without the disable right, no second Enable auto-merge shows.
    let f = with(on, |f| {
        f.can_disable_auto = false;
        f.can_enable_auto = false
    });
    assert_eq!(primaries(&f), [MergeBlocked]);
}

#[test]
fn the_new_controls_are_all_wired() {
    for p in [
        EnableAutoMerge,
        MergeWhenReady,
        MergeWhenReadyBlocked,
        RemoveFromQueue,
        DisableAutoMerge,
    ] {
        assert!(p.wired(), "{p:?}");
    }
}

#[test]
fn queue_and_auto_merge_facts_come_from_the_detail() {
    use crate::pr_page::rail::test_support::detail;
    use serde_json::json;
    let facts = |patch: serde_json::Value| Facts::of(&detail(patch), false);
    let base = json!({
        "mergeable": "MERGEABLE", "mergeStateStatus": "BLOCKED",
        "viewerCanEnableAutoMerge": true, "autoMergeAllowed": true,
    });
    let f = facts(base.clone());
    assert!(f.can_enable_auto && !f.can_disable_auto && !f.queue_present);
    assert_eq!(shown_primaries(&f), [EnableAutoMerge]);
    // Repository switch off: the control is hidden.
    let mut p = base.clone();
    p["autoMergeAllowed"] = json!(false);
    assert_eq!(shown_primaries(&facts(p)), [MergeBlocked]);
    // A queued base, clean PR.
    let mut p = base.clone();
    p["isMergeQueueEnabled"] = json!(true);
    p["mergeStateStatus"] = json!("CLEAN");
    let f = facts(p);
    assert!(f.queue_present && f.queue_ready);
    assert_eq!(shown_primaries(&f), [MergeWhenReady]);
    // Queued, with a viewer who can write.
    let f = facts(json!({"isMergeQueueEnabled": true, "isInMergeQueue": true}));
    assert!(f.queued && f.can_dequeue);
    assert_eq!(shown_primaries(&f), [RemoveFromQueue]);
    // Auto-merge on; the disable flag decides.
    let f = facts(json!({
        "autoMergeRequest": {"enabledBy": {"login": "dev"}, "mergeMethod": "SQUASH"},
        "viewerCanDisableAutoMerge": true,
    }));
    assert!(f.auto_merge_on);
    assert_eq!(shown_primaries(&f), [DisableAutoMerge]);
    // A reader gets none of it.
    let f = facts(
        json!({"viewerPermission": "READ", "isMergeQueueEnabled": true, "isInMergeQueue": true}),
    );
    assert!(shown_primaries(&f).is_empty());
}
