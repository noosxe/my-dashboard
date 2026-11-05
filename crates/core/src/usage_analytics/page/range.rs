//! The D11 range presets (docs/0082 D11): which presets each view
//! offers, what the active view actually reads, and the frozen bounds
//! every view derives from.

use super::UsageView;
use crate::usage_analytics::surfaces::{Bounds, Budget, BudgetScope, Preset, Span};
use crate::usage_records::UsageRecord;

/// The D11 presets in header order. `Current window` is Budget-only:
/// the Requests view's switch starts at `1h`.
pub(super) const BUDGET_PRESETS: [Preset; 6] = [
    Preset::CurrentWindow,
    Preset::Trailing(Span::H1),
    Preset::Trailing(Span::H24),
    Preset::Trailing(Span::D7),
    Preset::Trailing(Span::D30),
    Preset::Trailing(Span::D90),
];

/// The D11 default: 24h.
pub(super) const DEFAULT_PRESET: Preset = Preset::Trailing(Span::H24);

/// What the Requests view reads while the page-global preset is
/// `Current window` (Budget-only, D11): the trailing hour, the one
/// span that always covers a budget window. Picking a span there
/// replaces the preset; switching back to Budget with the preset
/// untouched restores `Current window`.
const REQUESTS_CURRENT_WINDOW_SPAN: Span = Span::H1;

/// The read bound (D11): the whole 90-day retention (docs/0057 D7), so
/// every preset is a re-slice of one read.
pub(super) const READ_SPAN: Span = Span::D90;

/// The module whose budgets the header names (`github` is the only
/// module that reports budgets today, docs/0082 §12).
pub(super) const BUDGET_MODULE: &str = "github";

pub(super) fn preset_label(p: Preset) -> &'static str {
    match p {
        Preset::CurrentWindow => "Current window",
        Preset::Trailing(Span::H1) => "1h",
        Preset::Trailing(Span::H24) => "24h",
        Preset::Trailing(Span::D7) => "7d",
        Preset::Trailing(Span::D30) => "30d",
        Preset::Trailing(Span::D90) => "90d",
    }
}

pub(super) fn budget_label(b: Budget) -> &'static str {
    match b {
        Budget::Rest => "REST",
        Budget::Graphql => "GraphQL",
    }
}

/// The presets `view` offers (D11).
pub(super) fn presets(view: UsageView) -> &'static [Preset] {
    match view {
        UsageView::Budget => &BUDGET_PRESETS,
        UsageView::Requests => &BUDGET_PRESETS[1..],
    }
}

/// The preset `view` reads under the page-global `preset`: `Current
/// window` is Budget-only (D11), so Requests reads the trailing hour.
pub(super) fn effective_preset(view: UsageView, preset: Preset) -> Preset {
    match (view, preset) {
        (UsageView::Requests, Preset::CurrentWindow) => {
            Preset::Trailing(REQUESTS_CURRENT_WINDOW_SPAN)
        }
        (_, p) => p,
    }
}

/// The frozen interval a view derives from (0054 D10): bounds from
/// `as_of`, never wall-clock. `None` only for `Current window` without
/// an open window — empty rather than guessed (0082 D15a). Resolved by
/// the same [`BudgetScope`] the Budget view's bands read, so the
/// header and the view can't disagree.
pub(super) fn bounds(
    rows: &[UsageRecord],
    preset: Preset,
    budget: Budget,
    as_of_ms: u64,
) -> Option<Bounds> {
    BudgetScope {
        module: BUDGET_MODULE.into(),
        budget,
        as_of_ms,
        preset,
    }
    .bounds(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_window_is_budget_only() {
        assert_eq!(presets(UsageView::Budget).len(), 6);
        assert_eq!(
            presets(UsageView::Budget).first(),
            Some(&Preset::CurrentWindow)
        );
        assert!(!presets(UsageView::Requests).contains(&Preset::CurrentWindow));
        assert_eq!(
            effective_preset(UsageView::Requests, Preset::CurrentWindow),
            Preset::Trailing(Span::H1)
        );
        // Spans pass through on either view; the page-global preset is
        // untouched, so Budget gets Current window back.
        assert_eq!(
            effective_preset(UsageView::Requests, Preset::Trailing(Span::D7)),
            Preset::Trailing(Span::D7)
        );
        assert_eq!(
            effective_preset(UsageView::Budget, Preset::CurrentWindow),
            Preset::CurrentWindow
        );
    }

    #[test]
    fn every_preset_has_a_label_and_the_default_is_24h() {
        let labels: Vec<_> = BUDGET_PRESETS.iter().map(|p| preset_label(*p)).collect();
        assert_eq!(labels, ["Current window", "1h", "24h", "7d", "30d", "90d"]);
        assert_eq!(preset_label(DEFAULT_PRESET), "24h");
        // D11: the read covers the widest preset.
        assert_eq!(READ_SPAN, Span::D90);
    }

    #[test]
    fn trailing_bounds_derive_from_the_frozen_as_of() {
        let as_of = 10_000_000_000;
        let b = bounds(&[], Preset::Trailing(Span::H24), Budget::Rest, as_of).unwrap();
        assert_eq!((b.from_ms, b.to_ms), (as_of - Span::H24.ms(), as_of));
        // No snapshot, no open window: empty, not guessed.
        assert_eq!(
            bounds(&[], Preset::CurrentWindow, Budget::Rest, as_of),
            None
        );
    }
}
