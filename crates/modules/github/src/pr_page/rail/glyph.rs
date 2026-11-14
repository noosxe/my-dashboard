//! Drawing the verdict's and the gate rows' icons (docs/0083 D15): tone
//! on the icon only, never on the words.

use gpui::{AnyElement, App, Hsla, IntoElement as _, ParentElement as _, Styled as _, div};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable as _, spinner::Spinner};

use super::verdict::{Glyph, Tone};

/// The vendored octicon `git-merge-16`, served by the app's asset source
/// like the pencil.
const GIT_MERGE: &str = "icons/git-merge.svg";

pub(super) fn tone_color(tone: Tone, cx: &App) -> Hsla {
    let theme = cx.theme();
    match tone {
        Tone::Success => theme.success,
        Tone::Danger => theme.danger,
        Tone::Warning => theme.warning,
        Tone::Muted => theme.muted_foreground,
        // The header badge's Merged tone is `accent`, a near-neutral in
        // the default themes: as a thin icon it would vanish, so the
        // merged glyph takes the `info` slot instead.
        Tone::Merged => theme.info,
    }
}

/// A 16 px icon slot, so spinners and glyphs align in one column.
pub(super) fn glyph(glyph: Glyph, tone: Tone, cx: &App) -> AnyElement {
    let color = tone_color(tone, cx);
    let icon = match glyph {
        Glyph::Spinner => {
            return div()
                .flex_none()
                .child(
                    Spinner::new()
                        .icon(IconName::LoaderCircle)
                        .small()
                        .color(color),
                )
                .into_any_element();
        }
        Glyph::Merged => Icon::empty().path(GIT_MERGE),
        Glyph::CircleX => Icon::new(IconName::CircleX),
        Glyph::CircleCheck => Icon::new(IconName::CircleCheck),
        Glyph::CircleAlert => Icon::new(IconName::CircleAlert),
        Glyph::TriangleAlert => Icon::new(IconName::TriangleAlert),
        Glyph::Dash => Icon::new(IconName::Dash),
    };
    div()
        .flex_none()
        .child(icon.small().text_color(color))
        .into_any_element()
}
