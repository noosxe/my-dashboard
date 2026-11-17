//! Label of a reaction picker row (docs/0023 D4). Both pickers, the PR
//! card's and the issue page's, build their rows from this one function.
//!
//! The pinned `PopupMenuItem::new(label)` has one text slot: the label is
//! both what the row shows and what a screen reader announces. So the row
//! reads `<emoji> <name>[, <count>]`, e.g. "👍 Thumbs up, 2". The count is
//! left out while nobody has reacted. Whether the viewer reacted is the
//! item's own checked state, not part of the text.

use crate::gql::ReactionContent;

/// The emoji's plain-words name.
fn name(content: ReactionContent) -> &'static str {
    match content {
        ReactionContent::ThumbsUp => "Thumbs up",
        ReactionContent::ThumbsDown => "Thumbs down",
        ReactionContent::Laugh => "Laugh",
        ReactionContent::Hooray => "Hooray",
        ReactionContent::Confused => "Confused",
        ReactionContent::Heart => "Heart",
        ReactionContent::Rocket => "Rocket",
        ReactionContent::Eyes => "Eyes",
    }
}

/// The picker row's label: emoji, name and the current count (if any).
pub(crate) fn reaction_row_label(content: ReactionContent, count: i64) -> String {
    if count == 0 {
        format!("{} {}", content.glyph(), name(content))
    } else {
        format!("{} {}, {}", content.glyph(), name(content), count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_carries_emoji_name_and_count() {
        assert_eq!(
            reaction_row_label(ReactionContent::ThumbsUp, 2),
            "👍 Thumbs up, 2"
        );
    }

    #[test]
    fn zero_count_is_left_out() {
        assert_eq!(reaction_row_label(ReactionContent::Heart, 0), "❤️ Heart");
    }

    #[test]
    fn every_content_has_a_distinct_name() {
        let mut names: Vec<_> = ReactionContent::ALL.iter().map(|c| name(*c)).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), ReactionContent::ALL.len());
    }
}
