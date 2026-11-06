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
