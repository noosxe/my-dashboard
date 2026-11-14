//! The three gate rows under the verdict (docs/0083 D3): Reviews, Checks
//! and Mergeability, each an icon, a short label and an optional muted
//! value. This file is the pure row model; `gate_row.rs` draws it.

use super::verdict::{self, Glyph, Tone};
use crate::gql::{CheckCounts, CheckState, PrDetail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Reviews,
    Checks,
    Merge,
}

impl Kind {
    /// The probe key (docs/0083 D14).
    pub(crate) fn id(self) -> &'static str {
        match self {
            Kind::Reviews => "pr-rail-gate-reviews",
            Kind::Checks => "pr-rail-gate-checks",
            Kind::Merge => "pr-rail-gate-merge",
        }
    }
}

/// Where activating the row goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Go {
    /// A static row.
    Nowhere,
    /// Scroll the timeline to this review node (docs/0026 D9).
    Review(String),
    ChecksTab,
}

/// What sits at the row's trailing edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Trailing {
    None,
    /// "Resolve on GitHub": the PR's conflicts page.
    ResolveOnGithub,
    /// The Update split button (docs/0067), when the server gate is open.
    Update,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Gate {
    pub kind: Kind,
    pub glyph: Glyph,
    pub tone: Tone,
    pub label: String,
    pub value: Option<String>,
    pub go: Go,
    pub trailing: Trailing,
}

impl Gate {
    /// The a11y name: label, value and, for a navigating row, where it
    /// goes ("Checks: 1 failed, 1 running. Open the Checks tab").
    pub(crate) fn spoken(&self) -> String {
        let mut out = self.label.clone();
        if let Some(value) = &self.value {
            out.push_str(": ");
            out.push_str(&value.replace(" · ", ", "));
        }
        match self.go {
            Go::Nowhere => {}
            Go::Review(_) => out.push_str(". Show the review"),
            Go::ChecksTab => out.push_str(". Open the Checks tab"),
        }
        out
    }

    /// The reason this row gives for a blocked merge, if it blocks.
    pub(crate) fn blocking_reason(&self, d: &PrDetail) -> Option<String> {
        match self.kind {
            Kind::Reviews => match d.review_decision.as_deref() {
                Some("CHANGES_REQUESTED") => Some("changes requested".into()),
                Some("REVIEW_REQUIRED") => Some("review required".into()),
                _ => None,
            },
            Kind::Checks => {
                let failed = checks(d).failed;
                (failed > 0).then(|| {
                    format!(
                        "{failed} check{} failed",
                        if failed == 1 { "" } else { "s" }
                    )
                })
            }
            Kind::Merge => verdict::conflicts(d).then(|| "conflicts".into()),
        }
    }
}

/// The rows to show for this PR, in order. Merged PRs drop Mergeability;
/// closed PRs drop Checks and keep Mergeability only for conflicts (D6).
pub(crate) fn gates(d: &PrDetail, waiting: usize) -> Vec<Gate> {
    let terminal_merged = d.state.as_deref() == Some("MERGED") || d.ending.merged;
    let closed = d.state.as_deref() == Some("CLOSED");
    let mut rows = vec![reviews(d)];
    if !closed {
        rows.push(checks_row(d, waiting));
    }
    if !terminal_merged && let Some(row) = merge_row(d, closed) {
        rows.push(row);
    }
    rows
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

// ---- Reviews -------------------------------------------------------------

fn reviews(d: &PrDetail) -> Gate {
    let changes: Vec<_> = d
        .latest_opinionated_reviews
        .iter()
        .filter(|r| r.state == "CHANGES_REQUESTED")
        .collect();
    let approvals = d
        .latest_opinionated_reviews
        .iter()
        .filter(|r| r.state == "APPROVED")
        .count();
    let pending = d.review_requests_total.max(d.review_requests.len() as i64) as usize;
    let pending_value = (pending > 0).then(|| format!("{pending} pending"));
    let changes_value = match changes.as_slice() {
        [] => None,
        [one] => Some(one.login.clone()),
        many => Some(format!("{} reviewers", many.len())),
    };
    let approved_value = (approvals > 0).then(|| plural(approvals, "approval", "approvals"));

    // The decision first; GitHub's `null` (no review rule) falls back to
    // what the latest reviews show.
    let (glyph, tone, label, value) = match d.review_decision.as_deref() {
        Some("CHANGES_REQUESTED") => (
            Glyph::CircleX,
            Tone::Danger,
            "Changes requested",
            changes_value,
        ),
        Some("REVIEW_REQUIRED") => (
            Glyph::TriangleAlert,
            Tone::Warning,
            "Review required",
            pending_value,
        ),
        Some("APPROVED") => (
            Glyph::CircleCheck,
            Tone::Success,
            "Approved",
            approved_value,
        ),
        _ if !changes.is_empty() => (
            Glyph::TriangleAlert,
            Tone::Warning,
            "Changes requested",
            changes_value,
        ),
        _ if approvals > 0 => (
            Glyph::CircleCheck,
            Tone::Success,
            "Approved",
            approved_value,
        ),
        _ => (Glyph::Dash, Tone::Muted, "No reviews yet", pending_value),
    };
    // The newest change request, else the newest approval.
    let target = if !changes.is_empty() {
        newest(d, "CHANGES_REQUESTED")
    } else {
        newest(d, "APPROVED")
    };
    Gate {
        kind: Kind::Reviews,
        glyph,
        tone,
        label: label.into(),
        value,
        go: target.map_or(Go::Nowhere, Go::Review),
        trailing: Trailing::None,
    }
}

/// The node id of the newest review in `state` among the latest per
/// reviewer: the later one in the timeline's order when it is loaded,
/// else the first listed (the anchor then gives up quietly when the
/// review is outside the newest-100 window, docs/0016 §14).
fn newest(d: &PrDetail, state: &str) -> Option<String> {
    let ids: Vec<&str> = d
        .latest_opinionated_reviews
        .iter()
        .filter(|r| r.state == state && !r.id.is_empty())
        .map(|r| r.id.as_str())
        .collect();
    d.reviews
        .nodes
        .iter()
        .rev()
        .find(|r| ids.contains(&r.id.as_str()))
        .map(|r| r.id.clone())
        .or_else(|| ids.first().map(|id| id.to_string()))
}

// ---- Checks --------------------------------------------------------------

/// The counts behind the row: the query's exact counts, or, for a payload
/// persisted before they existed, the listed rows.
pub(crate) fn checks(d: &PrDetail) -> CheckCounts {
    if d.checks.counts.total() > 0 {
        return d.checks.counts;
    }
    let mut counts = CheckCounts::default();
    for c in &d.checks.nodes {
        match c.state {
            CheckState::Failure => counts.failed += 1,
            CheckState::Pending | CheckState::Waiting => counts.running += 1,
            CheckState::Success | CheckState::Neutral | CheckState::Cancelled => counts.passed += 1,
        }
    }
    counts
}

fn checks_row(d: &PrDetail, waiting: usize) -> Gate {
    let counts = checks(d);
    let waiting = waiting as u32;
    let rollup = d.rollup_state.as_deref();
    let nothing = counts.total() == 0 && waiting == 0;
    let (glyph, tone, label) = if counts.failed > 0 {
        (Glyph::CircleX, Tone::Danger, "Checks")
    } else if counts.running > 0 || waiting > 0 {
        (Glyph::Spinner, Tone::Warning, "Checks")
    } else if !nothing {
        (Glyph::CircleCheck, Tone::Success, "Checks passed")
    } else {
        // No counts at all: fall back to the rollup state, the signal a
        // payload without the lists still carries.
        match rollup {
            None => (Glyph::Dash, Tone::Muted, "No checks on this branch"),
            Some("SUCCESS") => (Glyph::CircleCheck, Tone::Success, "Checks passed"),
            Some("FAILURE" | "ERROR") => (Glyph::CircleX, Tone::Danger, "Checks"),
            Some("PENDING" | "EXPECTED") => (Glyph::Spinner, Tone::Warning, "Checks"),
            Some(_) => (Glyph::CircleAlert, Tone::Muted, "Checks status unknown"),
        }
    };
    let mut parts = Vec::new();
    for (n, word) in [
        (counts.failed, "failed"),
        (counts.running, "running"),
        (waiting, "waiting"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {word}"));
        }
    }
    let value = if !parts.is_empty() {
        Some(parts.join(" · "))
    } else if counts.total() > 0 {
        Some(format!("{} of {}", counts.passed, counts.total()))
    } else {
        None
    };
    Gate {
        kind: Kind::Checks,
        glyph,
        tone,
        label: label.into(),
        value,
        go: Go::ChecksTab,
        trailing: Trailing::None,
    }
}

// ---- Mergeability --------------------------------------------------------

fn merge_row(d: &PrDetail, closed: bool) -> Option<Gate> {
    let base = d.base_ref.as_deref().unwrap_or("the base branch");
    let can_update = d.viewer_can_update_branch && d.state.as_deref() == Some("OPEN");
    let row = |glyph, tone, label: String, trailing| Gate {
        kind: Kind::Merge,
        glyph,
        tone,
        label,
        value: None,
        go: Go::Nowhere,
        trailing,
    };
    if verdict::conflicts(d) {
        return Some(row(
            Glyph::CircleX,
            Tone::Danger,
            format!("Conflicts with {base}"),
            Trailing::ResolveOnGithub,
        ));
    }
    if closed {
        return None;
    }
    let update = if can_update {
        Trailing::Update
    } else {
        Trailing::None
    };
    Some(if verdict::checking(d) {
        row(
            Glyph::Spinner,
            Tone::Muted,
            "Checking for conflicts".into(),
            Trailing::None,
        )
    } else if d.merge_state_status.as_deref() == Some("BEHIND") {
        row(
            Glyph::TriangleAlert,
            Tone::Warning,
            format!("Out of date with {base}"),
            update,
        )
    } else if can_update {
        row(
            Glyph::CircleCheck,
            Tone::Success,
            format!("No conflicts, behind {base}"),
            update,
        )
    } else {
        row(
            Glyph::CircleCheck,
            Tone::Success,
            "No conflicts".into(),
            Trailing::None,
        )
    })
}

#[cfg(test)]
mod tests;
