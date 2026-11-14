//! The merge method the split button preselects (docs/0016 D5, #776).

use super::MergeMethod;

/// The viewer's default (`Repository.viewerDefaultMergeMethod`: their last
/// used method, else the repository's) when the repository still allows
/// it; otherwise the first allowed, merge commit preferred. A missing or
/// unknown value falls back the same way.
pub(crate) fn default_method(
    methods: &[MergeMethod],
    viewer_default: Option<&str>,
) -> Option<MergeMethod> {
    let wanted = match viewer_default {
        Some("MERGE") => Some(MergeMethod::Merge),
        Some("SQUASH") => Some(MergeMethod::Squash),
        Some("REBASE") => Some(MergeMethod::Rebase),
        _ => None,
    };
    wanted
        .filter(|m| methods.contains(m))
        .or_else(|| methods.iter().copied().find(|m| *m == MergeMethod::Merge))
        .or_else(|| methods.first().copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    use MergeMethod::*;

    #[test]
    fn without_a_viewer_default_merge_commit_wins_else_the_first_allowed() {
        assert_eq!(default_method(&[Squash, Merge], None), Some(Merge));
        assert_eq!(default_method(&[Rebase, Squash], None), Some(Rebase));
        assert_eq!(default_method(&[], None), None);
    }

    #[test]
    fn an_allowed_viewer_default_is_preselected() {
        assert_eq!(
            default_method(&[Merge, Squash, Rebase], Some("SQUASH")),
            Some(Squash)
        );
        assert_eq!(
            default_method(&[Merge, Squash, Rebase], Some("REBASE")),
            Some(Rebase)
        );
        assert_eq!(default_method(&[Merge, Squash], Some("MERGE")), Some(Merge));
    }

    #[test]
    fn a_default_the_repo_no_longer_allows_falls_back_to_the_rule() {
        assert_eq!(
            default_method(&[Merge, Rebase], Some("SQUASH")),
            Some(Merge)
        );
        assert_eq!(
            default_method(&[Rebase, Squash], Some("MERGE")),
            Some(Rebase)
        );
        assert_eq!(default_method(&[], Some("SQUASH")), None);
    }

    #[test]
    fn an_unknown_value_is_ignored() {
        assert_eq!(
            default_method(&[Squash, Merge], Some("FAST_FORWARD")),
            Some(Merge)
        );
        assert_eq!(default_method(&[Squash], Some("")), Some(Squash));
    }
}
