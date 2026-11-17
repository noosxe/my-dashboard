//! The state pill (#740): one coloured, iconed chip for a state word —
//! Open, Merged, Closed, Approved, Pending, a file's change type.
//!
//! Upstream `Badge` is a count/dot overlay for a child: its `color`
//! paints the overlay, and with no count nothing paints, so the state
//! colours never showed. `Tag` is the pinned rev's status chip; this
//! module only fixes the tone → variant mapping and pairs each state
//! with a glyph (docs/0065 Appendix A item 17: colour is never the only
//! signal).

use gpui::{AnyElement, IntoElement as _, ParentElement as _, SharedString, Styled as _};
use gpui_component::{
    Icon, IconName, Sizable as _, h_flex,
    tag::{Tag, TagVariant},
};

/// What the state means; the pill's colour follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    /// Open, approved, added, resolved.
    Success,
    /// Merged: the `info` slot, as the rail's merged glyph (docs/0083 D15).
    Merged,
    /// Closed, changes requested, removed.
    Danger,
    /// Pending, unread: attention without a verdict.
    Warning,
    /// Draft, read, outdated, anything unremarkable.
    Muted,
}

/// The glyph paired with the colour. `None` on a pill is a plain
/// label chip (a notification's kind or reason), not a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Glyph {
    Open,
    Draft,
    Merged,
    Check,
    Cross,
    Alert,
    Plus,
    Minus,
    Right,
    Edit,
    Info,
    Bell,
}

/// The vendored Lucide glyphs the app's asset source serves.
const CIRCLE_DOT: &str = "icons/circle-dot.svg";
const CIRCLE_DASHED: &str = "icons/circle-dashed.svg";
const GIT_MERGE: &str = "icons/git-merge.svg";
/// The octicon pencil the rail already serves.
const PENCIL: &str = "icons/pencil.svg";

pub(crate) fn variant(tone: Tone) -> TagVariant {
    match tone {
        Tone::Success => TagVariant::Success,
        Tone::Merged => TagVariant::Info,
        Tone::Danger => TagVariant::Danger,
        Tone::Warning => TagVariant::Warning,
        Tone::Muted => TagVariant::Secondary,
    }
}

fn icon(glyph: Glyph) -> Icon {
    match glyph {
        Glyph::Open => Icon::empty().path(CIRCLE_DOT),
        Glyph::Draft => Icon::empty().path(CIRCLE_DASHED),
        Glyph::Merged => Icon::empty().path(GIT_MERGE),
        Glyph::Check => Icon::new(IconName::CircleCheck),
        Glyph::Cross => Icon::new(IconName::CircleX),
        Glyph::Alert => Icon::new(IconName::CircleAlert),
        Glyph::Plus => Icon::new(IconName::Plus),
        Glyph::Minus => Icon::new(IconName::Minus),
        Glyph::Right => Icon::new(IconName::ArrowRight),
        Glyph::Edit => Icon::empty().path(PENCIL),
        Glyph::Info => Icon::new(IconName::Info),
        Glyph::Bell => Icon::new(IconName::Bell),
    }
}

/// A state pill: tone colour, a glyph, the label.
pub(crate) fn state_pill(tone: Tone, glyph: Glyph, label: impl Into<SharedString>) -> AnyElement {
    Tag::new()
        .with_variant(variant(tone))
        .xsmall()
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .child(icon(glyph).xsmall())
                .child(label.into()),
        )
        .into_any_element()
}

/// A plain label chip with no state meaning (a notification's kind).
pub(crate) fn label_chip(label: impl Into<SharedString>) -> AnyElement {
    Tag::secondary()
        .xsmall()
        .child(label.into())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tone_has_its_own_variant() {
        let all = [
            Tone::Success,
            Tone::Merged,
            Tone::Danger,
            Tone::Warning,
            Tone::Muted,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(variant(*a), variant(*b), "{a:?} vs {b:?}");
            }
        }
    }
}
