//! Who sees the rail's Merge and Close buttons, and when Merge is
//! clickable (docs/0016 D5, #708, #737).
//!
//! Two different questions, answered differently on purpose:
//!
//! * **Visibility** is about the viewer. A viewer the server would refuse
//!   gets no button at all (hidden, not disabled), matching the sidebar's
//!   rule for edit controls.
//! * **Enablement** is about the PR's state. A button the viewer may use
//!   but not right now stays visible and disabled, with the state line as
//!   its tooltip.
//!
//! Schema facts (live GraphQL introspection, 2026-11-06): there is no
//! `PullRequest.viewerCanMerge`; `viewerCanClose` exists; `viewerPermission`
//! is `Repository.viewerPermission` (`ADMIN`, `MAINTAIN`, `WRITE`,
//! `MODERATE`, `TRIAGE`, `READ`); `MergeStateStatus` is `DIRTY`, `UNKNOWN`,
//! `BLOCKED`, `BEHIND`, `UNSTABLE`, `HAS_HOOKS`, `CLEAN`.

/// Merge visibility: the viewer must have WRITE or higher on the
/// repository. `MODERATE` and `TRIAGE` cannot merge; an absent value
/// (degraded payload) hides the button, never guesses.
pub(crate) fn merge_visible(viewer_permission: Option<&str>) -> bool {
    matches!(viewer_permission, Some("WRITE" | "MAINTAIN" | "ADMIN"))
}

/// Close visibility: the server's own per-viewer answer.
pub(crate) fn close_visible(viewer_can_close: bool) -> bool {
    viewer_can_close
}

/// Close-button gating: the PR is open, idle and addressable. Shared by
/// the click path and the confirm so both ask the same question.
pub(crate) fn close_enabled(state: Option<&str>, id_empty: bool, busy: bool) -> bool {
    state == Some("OPEN") && !id_empty && !busy
}

/// Ready for review (#771): a draft the viewer may update, idle and
/// addressable. `viewerCanUpdate` is the gate; there is no flag of its own.
pub(crate) fn ready_enabled(
    state: Option<&str>,
    draft: bool,
    can_update: bool,
    id_empty: bool,
    busy: bool,
) -> bool {
    state == Some("OPEN") && draft && can_update && !id_empty && !busy
}

/// Convert to draft (#772): the mirror of [`ready_enabled`] on an open
/// PR that isn't a draft yet. Shared by the menu item and its confirm.
pub(crate) fn convert_enabled(
    state: Option<&str>,
    draft: bool,
    can_update: bool,
    id_empty: bool,
    busy: bool,
) -> bool {
    state == Some("OPEN") && !draft && can_update && !id_empty && !busy
}

/// Reopen (#770): a closed, unmerged PR (`MERGED` is its own state) the
/// server says the viewer can reopen, idle and addressable.
pub(crate) fn reopen_enabled(
    state: Option<&str>,
    can_reopen: bool,
    id_empty: bool,
    busy: bool,
) -> bool {
    state == Some("CLOSED") && can_reopen && !id_empty && !busy
}

/// Delete branch (#773): a merged or closed PR whose head branch still
/// exists and the viewer may delete (`HeadBranch::deletable`: write access
/// on the head repository, not its default branch), idle.
pub(crate) fn delete_branch_enabled(
    state: Option<&str>,
    head: Option<&crate::gql::HeadBranch>,
    busy: bool,
) -> bool {
    matches!(state, Some("MERGED" | "CLOSED")) && head.is_some_and(|h| h.deletable()) && !busy
}

/// Whether `mergeStateStatus` lets a normal merge through. Allowlist:
/// `CLEAN`, `HAS_HOOKS` and `UNSTABLE` are documented as mergeable;
/// `BLOCKED` (required reviews or checks), `BEHIND` (strict up-to-date
/// rule), `DIRTY` (conflicts), `UNKNOWN` (not yet computed) and anything
/// absent or unrecognised stay disabled. The viewer-can-bypass case
/// (`viewerCanMergeAsAdmin`) is deliberately not handled here.
pub(crate) fn merge_state_allows(status: Option<&str>) -> bool {
    matches!(status, Some("CLEAN" | "HAS_HOOKS" | "UNSTABLE"))
}

/// Merge-button gating (D5): open, not draft, no conflicts
/// (`mergeable == MERGEABLE`), merge state allows it, idle, and at least
/// one allowed method.
pub(crate) fn merge_enabled(
    state: Option<&str>,
    draft: bool,
    mergeable: Option<&str>,
    merge_state_status: Option<&str>,
    busy: bool,
    no_methods: bool,
) -> bool {
    state == Some("OPEN")
        && !draft
        && mergeable == Some("MERGEABLE")
        && merge_state_allows(merge_state_status)
        && !busy
        && !no_methods
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_visible_table() {
        for (perm, want) in [
            (Some("ADMIN"), true),
            (Some("MAINTAIN"), true),
            (Some("WRITE"), true),
            (Some("MODERATE"), false),
            (Some("TRIAGE"), false),
            (Some("READ"), false),
            (Some("SOMETHING_NEW"), false),
            (None, false),
        ] {
            assert_eq!(merge_visible(perm), want, "{perm:?}");
        }
    }

    #[test]
    fn close_visible_follows_the_server_flag() {
        assert!(close_visible(true));
        assert!(!close_visible(false));
    }

    #[test]
    fn close_enabled_table() {
        assert!(close_enabled(Some("OPEN"), false, false));
        assert!(!close_enabled(Some("CLOSED"), false, false));
        assert!(!close_enabled(Some("MERGED"), false, false));
        assert!(!close_enabled(None, false, false));
        assert!(!close_enabled(Some("OPEN"), true, false));
        assert!(!close_enabled(Some("OPEN"), false, true));
    }

    #[test]
    fn ready_and_convert_are_mirrors_on_an_open_pr() {
        // (state, draft, can_update, id_empty, busy) -> (ready, convert)
        for (args, ready, convert) in [
            ((Some("OPEN"), true, true, false, false), true, false),
            ((Some("OPEN"), false, true, false, false), false, true),
            ((Some("OPEN"), true, false, false, false), false, false),
            ((Some("OPEN"), false, false, false, false), false, false),
            ((Some("OPEN"), true, true, true, false), false, false),
            ((Some("OPEN"), false, true, true, false), false, false),
            ((Some("OPEN"), true, true, false, true), false, false),
            ((Some("OPEN"), false, true, false, true), false, false),
            ((Some("CLOSED"), true, true, false, false), false, false),
            ((Some("CLOSED"), false, true, false, false), false, false),
            ((Some("MERGED"), false, true, false, false), false, false),
            ((None, true, true, false, false), false, false),
        ] {
            let (s, d, u, e, b) = args;
            assert_eq!(ready_enabled(s, d, u, e, b), ready, "{args:?}");
            assert_eq!(convert_enabled(s, d, u, e, b), convert, "{args:?}");
        }
    }

    #[test]
    fn reopen_enabled_table() {
        assert!(reopen_enabled(Some("CLOSED"), true, false, false));
        assert!(!reopen_enabled(Some("CLOSED"), false, false, false));
        assert!(!reopen_enabled(Some("OPEN"), true, false, false));
        assert!(!reopen_enabled(Some("MERGED"), true, false, false));
        assert!(!reopen_enabled(None, true, false, false));
        assert!(!reopen_enabled(Some("CLOSED"), true, true, false));
        assert!(!reopen_enabled(Some("CLOSED"), true, false, true));
    }

    #[test]
    fn delete_branch_enabled_table() {
        use crate::gql::HeadBranch;
        let head = |perm: &str, name: &str| HeadBranch {
            id: "REF_1".into(),
            name: name.into(),
            repo_permission: Some(perm.into()),
            default_branch: Some("main".into()),
        };
        let ok = head("WRITE", "feat");
        assert!(delete_branch_enabled(Some("MERGED"), Some(&ok), false));
        assert!(delete_branch_enabled(Some("CLOSED"), Some(&ok), false));
        // Open PRs keep their branch; draft or not.
        assert!(!delete_branch_enabled(Some("OPEN"), Some(&ok), false));
        assert!(!delete_branch_enabled(None, Some(&ok), false));
        // Gone, read-only, the default branch, busy.
        assert!(!delete_branch_enabled(Some("MERGED"), None, false));
        assert!(!delete_branch_enabled(
            Some("MERGED"),
            Some(&head("READ", "feat")),
            false
        ));
        assert!(!delete_branch_enabled(
            Some("MERGED"),
            Some(&head("ADMIN", "main")),
            false
        ));
        assert!(!delete_branch_enabled(Some("MERGED"), Some(&ok), true));
    }

    #[test]
    fn merge_state_allows_table() {
        for (status, want) in [
            (Some("CLEAN"), true),
            (Some("HAS_HOOKS"), true),
            (Some("UNSTABLE"), true),
            (Some("BLOCKED"), false),
            (Some("BEHIND"), false),
            (Some("DIRTY"), false),
            (Some("UNKNOWN"), false),
            (Some("DRAFT"), false),
            (Some("SOMETHING_NEW"), false),
            (None, false),
        ] {
            assert_eq!(merge_state_allows(status), want, "{status:?}");
        }
    }

    fn enabled(
        state: &str,
        draft: bool,
        mergeable: &str,
        status: &str,
        busy: bool,
        no_methods: bool,
    ) -> bool {
        merge_enabled(
            Some(state),
            draft,
            Some(mergeable),
            Some(status),
            busy,
            no_methods,
        )
    }

    #[test]
    fn merge_enabled_table() {
        assert!(enabled("OPEN", false, "MERGEABLE", "CLEAN", false, false));
        assert!(enabled(
            "OPEN",
            false,
            "MERGEABLE",
            "UNSTABLE",
            false,
            false
        ));
        // Each disqualifier alone flips it off.
        assert!(!enabled(
            "MERGED",
            false,
            "MERGEABLE",
            "CLEAN",
            false,
            false
        ));
        assert!(!enabled(
            "CLOSED",
            false,
            "MERGEABLE",
            "CLEAN",
            false,
            false
        ));
        assert!(!enabled("OPEN", true, "MERGEABLE", "CLEAN", false, false));
        assert!(!enabled(
            "OPEN",
            false,
            "CONFLICTING",
            "CLEAN",
            false,
            false
        ));
        assert!(!enabled("OPEN", false, "UNKNOWN", "CLEAN", false, false));
        assert!(!enabled(
            "OPEN",
            false,
            "MERGEABLE",
            "BLOCKED",
            false,
            false
        ));
        assert!(!enabled("OPEN", false, "MERGEABLE", "BEHIND", false, false));
        assert!(!enabled("OPEN", false, "MERGEABLE", "DIRTY", false, false));
        assert!(!enabled(
            "OPEN",
            false,
            "MERGEABLE",
            "UNKNOWN",
            false,
            false
        ));
        assert!(!enabled("OPEN", false, "MERGEABLE", "CLEAN", true, false));
        assert!(!enabled("OPEN", false, "MERGEABLE", "CLEAN", false, true));
        assert!(!merge_enabled(
            Some("OPEN"),
            false,
            None,
            Some("CLEAN"),
            false,
            false
        ));
        assert!(!merge_enabled(
            Some("OPEN"),
            false,
            Some("MERGEABLE"),
            None,
            false,
            false
        ));
    }
}
