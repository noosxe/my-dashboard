//! #740: the PR page's state pills — the header's Open / Draft / Merged /
//! Closed and a review card's Approved / Changes requested.

use gpui::{AnyElement, SharedString};

use super::{PrStateTone, ReviewTone};
use crate::state_pill::{Glyph, Tone, state_pill};

/// Tone and glyph for the header's state word.
pub(super) fn pr_state_look(tone: PrStateTone) -> (Tone, Glyph, &'static str) {
    match tone {
        PrStateTone::Open => (Tone::Success, Glyph::Open, "Open"),
        PrStateTone::Draft => (Tone::Muted, Glyph::Draft, "Draft"),
        PrStateTone::Merged => (Tone::Merged, Glyph::Merged, "Merged"),
        PrStateTone::Closed => (Tone::Danger, Glyph::Cross, "Closed"),
        PrStateTone::Unknown => (Tone::Muted, Glyph::Info, "Unknown"),
    }
}

pub(super) fn pr_state_pill(tone: PrStateTone) -> AnyElement {
    let (tone, glyph, label) = pr_state_look(tone);
    state_pill(tone, glyph, label)
}

/// Tone and glyph for a review card's state.
pub(super) fn review_look(tone: ReviewTone) -> (Tone, Glyph) {
    match tone {
        ReviewTone::Approved => (Tone::Success, Glyph::Check),
        ReviewTone::ChangesRequested => (Tone::Danger, Glyph::Alert),
        ReviewTone::Muted => (Tone::Muted, Glyph::Info),
    }
}

pub(super) fn review_pill(tone: ReviewTone, label: SharedString) -> AnyElement {
    let (tone, glyph) = review_look(tone);
    state_pill(tone, glyph, label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_state_pairs_a_tone_with_its_own_glyph_and_word() {
        let all = [
            PrStateTone::Open,
            PrStateTone::Draft,
            PrStateTone::Merged,
            PrStateTone::Closed,
            PrStateTone::Unknown,
        ];
        let looks: Vec<_> = all.iter().map(|t| pr_state_look(*t)).collect();
        assert_eq!(looks[0], (Tone::Success, Glyph::Open, "Open"));
        assert_eq!(looks[2], (Tone::Merged, Glyph::Merged, "Merged"));
        assert_eq!(looks[3], (Tone::Danger, Glyph::Cross, "Closed"));
        // No two states share a glyph: colour is never the only signal.
        for (i, a) in looks.iter().enumerate() {
            for b in &looks[i + 1..] {
                assert_ne!(a.1, b.1);
            }
        }
    }

    #[test]
    fn review_states_split_on_tone_and_glyph() {
        assert_eq!(
            review_look(ReviewTone::Approved),
            (Tone::Success, Glyph::Check)
        );
        assert_eq!(
            review_look(ReviewTone::ChangesRequested),
            (Tone::Danger, Glyph::Alert)
        );
    }
}
