//! The D5 threshold ladder as drawn (docs/0082 D5, Q1): the state's
//! token colours marks only (the state icon, the `Progress` fill and
//! chart marks); its word stays in `foreground`, and an icon goes with
//! every non-normal state, so colour is never the only signal.

use gpui::{App, Hsla};
use gpui_component::{ActiveTheme, IconName};

use crate::usage_analytics::surfaces::ThresholdState;

/// The state's word, shown beside its icon; `None` for Normal, which
/// shows no badge (D4).
pub(super) fn word(state: ThresholdState) -> Option<&'static str> {
    match state {
        ThresholdState::Normal => None,
        ThresholdState::Low => Some("Low"),
        ThresholdState::Critical => Some("Critical"),
        ThresholdState::Exhausted => Some("Exhausted"),
    }
}

/// The state's icon, from gpui-component's default bundle (Lucide): an
/// `IconName`, so a glyph the bundle lacks can't silently draw nothing.
pub(super) fn icon(state: ThresholdState) -> IconName {
    match state {
        ThresholdState::Normal | ThresholdState::Low => IconName::TriangleAlert,
        ThresholdState::Critical => IconName::CircleAlert,
        ThresholdState::Exhausted => IconName::Pause,
    }
}

/// The mark colour (D5): `primary` for a normal bar, `warning` for
/// Low, `danger` for Critical and Exhausted.
pub(super) fn bar_color(state: Option<ThresholdState>, cx: &App) -> Hsla {
    let theme = cx.theme();
    match state {
        None | Some(ThresholdState::Normal) => theme.primary,
        Some(ThresholdState::Low) => theme.warning,
        Some(ThresholdState::Critical | ThresholdState::Exhausted) => theme.danger,
    }
}

/// A chart mark's colour (D5, D14): `chart_1` for normal columns, the
/// state tokens otherwise.
pub(super) fn mark_color(state: Option<ThresholdState>, cx: &App) -> Hsla {
    match state {
        None | Some(ThresholdState::Normal) => cx.theme().chart_1,
        other => bar_color(other, cx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_carries_no_badge_and_every_other_state_a_word() {
        assert_eq!(word(ThresholdState::Normal), None);
        assert_eq!(word(ThresholdState::Low), Some("Low"));
        assert_eq!(word(ThresholdState::Critical), Some("Critical"));
        assert_eq!(word(ThresholdState::Exhausted), Some("Exhausted"));
    }
}
