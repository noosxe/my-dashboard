//! The row's glyph: the *source's* state, in the state pill's tokens
//! (docs/0085 D3), plus the words the accessible name uses for it.

use crate::gql::XrefState;
use crate::state_pill::{Glyph, Tone};

pub(super) fn look(is_pr: bool, state: XrefState) -> (Tone, Glyph) {
    match (is_pr, state) {
        (_, XrefState::Open) => (Tone::Success, Glyph::Open),
        (_, XrefState::Draft) => (Tone::Muted, Glyph::Draft),
        (_, XrefState::Merged) => (Tone::Merged, Glyph::Merged),
        (true, XrefState::Closed) => (Tone::Danger, Glyph::Cross),
        (false, XrefState::Closed) => (Tone::Merged, Glyph::Check),
        (_, XrefState::NotPlanned) => (Tone::Muted, Glyph::Cross),
    }
}

/// "open pull request", "closed issue (not planned)", for the accessible
/// name: colour is never the only signal.
pub(super) fn state_words(is_pr: bool, state: XrefState) -> &'static str {
    match (is_pr, state) {
        (true, XrefState::Open) => "open pull request",
        (true, XrefState::Draft) => "draft pull request",
        (true, XrefState::Merged) => "merged pull request",
        (true, _) => "closed pull request",
        (false, XrefState::Merged) => "closed issue",
        (false, XrefState::Open | XrefState::Draft) => "open issue",
        (false, XrefState::Closed) => "closed issue",
        (false, XrefState::NotPlanned) => "closed issue (not planned)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_state_gets_the_documented_tone_and_glyph() {
        assert_eq!(look(false, XrefState::Open), (Tone::Success, Glyph::Open));
        assert_eq!(look(true, XrefState::Open), (Tone::Success, Glyph::Open));
        assert_eq!(look(true, XrefState::Draft), (Tone::Muted, Glyph::Draft));
        assert_eq!(look(true, XrefState::Merged), (Tone::Merged, Glyph::Merged));
        assert_eq!(look(true, XrefState::Closed), (Tone::Danger, Glyph::Cross));
        assert_eq!(look(false, XrefState::Closed), (Tone::Merged, Glyph::Check));
        assert_eq!(
            look(false, XrefState::NotPlanned),
            (Tone::Muted, Glyph::Cross)
        );
    }

    #[test]
    fn the_words_name_kind_and_state() {
        assert_eq!(state_words(true, XrefState::Draft), "draft pull request");
        assert_eq!(state_words(true, XrefState::Closed), "closed pull request");
        assert_eq!(
            state_words(false, XrefState::NotPlanned),
            "closed issue (not planned)"
        );
        assert_eq!(state_words(false, XrefState::Open), "open issue");
    }
}
