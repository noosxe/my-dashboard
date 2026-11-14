//! The status block's verdict (docs/0083 D2): one icon and a word or two
//! that say whether the pull request can merge. Pure: one arm per row of
//! the table, first match wins. The words are never tinted, only the icon
//! (#797); the full sentence is the tooltip and the a11y description.

use crate::gql::PrDetail;

/// Which tone an icon takes. `Merged` is the header badge's tone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Success,
    Danger,
    Warning,
    Muted,
    Merged,
}

/// The icon slots the rail uses, theme icons except `Merged` (the vendored
/// `git-merge` octicon) and `Spinner` (an animated `LoaderCircle`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Glyph {
    Merged,
    CircleX,
    CircleCheck,
    CircleAlert,
    TriangleAlert,
    Dash,
    Spinner,
}

/// One row of D2's table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    Merged,
    Closed,
    Draft,
    Queued,
    AutoMerge,
    Conflicts,
    Checking,
    Blocked,
    OutOfDate,
    ChecksFailing,
    Ready,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Verdict {
    pub row: Row,
    pub glyph: Glyph,
    pub tone: Tone,
    pub word: &'static str,
    /// The full docs/0016 D5 sentence: tooltip and a11y description.
    pub sentence: &'static str,
}

const fn v(
    row: Row,
    glyph: Glyph,
    tone: Tone,
    word: &'static str,
    sentence: &'static str,
) -> Verdict {
    Verdict {
        row,
        glyph,
        tone,
        word,
        sentence,
    }
}

/// `true` for the states D2 rows 6 and 7 name; shared with the gate rows so
/// both read the same facts.
pub(super) fn conflicts(d: &PrDetail) -> bool {
    d.mergeable.as_deref() == Some("CONFLICTING")
        || d.merge_state_status.as_deref() == Some("DIRTY")
}

/// GitHub is still computing mergeability (docs/0016 §15 re-polls it).
pub(super) fn checking(d: &PrDetail) -> bool {
    d.merge_state_status
        .as_deref()
        .is_none_or(|s| s == "UNKNOWN")
        || d.mergeable.as_deref().is_none_or(|m| m == "UNKNOWN")
}

pub(crate) fn verdict(d: &PrDetail) -> Verdict {
    if d.state.as_deref() == Some("MERGED") || d.ending.merged {
        return v(
            Row::Merged,
            Glyph::Merged,
            Tone::Merged,
            "Merged",
            "This pull request was merged",
        );
    }
    if d.state.as_deref() == Some("CLOSED") {
        return v(
            Row::Closed,
            Glyph::CircleX,
            Tone::Danger,
            "Closed",
            "This pull request was closed without merging",
        );
    }
    if d.draft {
        return v(
            Row::Draft,
            Glyph::Dash,
            Tone::Muted,
            "Draft",
            "Draft pull requests can't be merged until marked ready",
        );
    }
    if d.queueing.in_merge_queue {
        return v(
            Row::Queued,
            Glyph::Spinner,
            Tone::Warning,
            "Queued to merge",
            "This pull request is in the merge queue",
        );
    }
    if d.queueing.auto_merge.is_some() {
        return v(
            Row::AutoMerge,
            Glyph::Spinner,
            Tone::Warning,
            "Auto-merge enabled",
            "This pull request merges once its requirements pass",
        );
    }
    if conflicts(d) {
        return v(
            Row::Conflicts,
            Glyph::CircleX,
            Tone::Danger,
            "Conflicts",
            "This branch has conflicts that must be resolved",
        );
    }
    if checking(d) {
        return v(
            Row::Checking,
            Glyph::Spinner,
            Tone::Muted,
            "Checking…",
            "GitHub is still calculating merge status",
        );
    }
    match d.merge_state_status.as_deref() {
        Some("BLOCKED") => v(
            Row::Blocked,
            Glyph::CircleX,
            Tone::Danger,
            "Merging is blocked",
            "Merging is blocked by required reviews or status checks",
        ),
        Some("BEHIND") => v(
            Row::OutOfDate,
            Glyph::TriangleAlert,
            Tone::Warning,
            "Out of date",
            "This branch is out-of-date with the base branch",
        ),
        Some("UNSTABLE") => v(
            Row::ChecksFailing,
            Glyph::TriangleAlert,
            Tone::Warning,
            "Mergeable, checks failing",
            "Mergeable, but some checks are not passing",
        ),
        Some("CLEAN" | "HAS_HOOKS") => v(
            Row::Ready,
            Glyph::CircleCheck,
            Tone::Success,
            "Ready to merge",
            "This branch has no conflicts with the base branch",
        ),
        _ => v(
            Row::Unknown,
            Glyph::CircleAlert,
            Tone::Muted,
            "Merge status unknown",
            "GitHub reported a merge status this version doesn't know",
        ),
    }
}

impl Verdict {
    /// The disabled Merge button's tooltip (D2, #708): the verdict joined
    /// with the gates that block it ("Merging is blocked: changes
    /// requested, 1 check failed"); the sentence when no gate names a
    /// reason.
    pub(crate) fn blocked_tooltip(&self, reasons: &[String]) -> String {
        if reasons.is_empty() {
            self.sentence.to_string()
        } else {
            format!("{}: {}", self.word, reasons.join(", "))
        }
    }
}

#[cfg(test)]
mod tests;
