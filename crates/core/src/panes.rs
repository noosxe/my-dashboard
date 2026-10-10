//! Pane seam state (docs/0069): resizable-pane widths shared across
//! pages and the shell through one global. Pages seed their pane from
//! it at creation and write it back when a resize settles (drag end,
//! keyboard step); the shell observes the global and funnels changes
//! into the same debounced `shell.json` save as every other chrome
//! state (docs/0004 §4.3).

use gpui::{App, Global, Pixels, px};

/// Files-tab sidebar: never narrower than this (0018 D2 controls stay
/// usable), never wider than ~40% of the tab area at default geometry
/// — the primary (diff pane) keeps the surplus (0069 D2/D3).
pub const FILES_SIDEBAR_MIN: f32 = 220.0;
pub const FILES_SIDEBAR_MAX: f32 = 480.0;
/// Today's `w_75` — the pre-seam width, kept as the default (0069 D3).
pub const FILES_SIDEBAR_DEFAULT: f32 = 300.0;
/// Keyboard resize step (docs/0069 D5).
pub const RESIZE_KEYBOARD_STEP: f32 = 16.0;

fn clamp_files_sidebar(width: f32) -> f32 {
    width.clamp(FILES_SIDEBAR_MIN, FILES_SIDEBAR_MAX)
}

/// One field per persisted pane group (docs/0069 D4). Seeded at boot
/// from the loaded [`crate::shell::ShellState`], clamped on restore —
/// the window may have changed since the save.
#[derive(Debug, Clone, Copy)]
pub struct PaneWidths {
    pub files_sidebar_px: f32,
}

impl Default for PaneWidths {
    fn default() -> Self {
        Self {
            files_sidebar_px: FILES_SIDEBAR_DEFAULT,
        }
    }
}

impl Global for PaneWidths {}

impl PaneWidths {
    /// The seeded Files sidebar width, clamped into its declared range.
    pub fn files_sidebar(&self) -> Pixels {
        px(clamp_files_sidebar(self.files_sidebar_px))
    }
}

/// Boot seeding (docs/0069 D4): call once after `shell.json` loads,
/// before any page entity exists.
pub fn seed(cx: &mut App, files_sidebar_px: f32) {
    cx.set_global(PaneWidths {
        files_sidebar_px: clamp_files_sidebar(files_sidebar_px),
    });
}

/// Record a settled Files sidebar width (drag end or keyboard step,
/// docs/0069 D4/D5). No-op when the global was never seeded (tests) or
/// the value is unchanged — `global_mut` notifies observers
/// unconditionally, and each notification schedules a debounced save.
pub fn set_files_sidebar(width: Pixels, cx: &mut App) {
    let clamped = clamp_files_sidebar(width.as_f32());
    let Some(panes) = cx.try_global::<PaneWidths>() else {
        return;
    };
    if panes.files_sidebar_px == clamped {
        return;
    }
    cx.global_mut::<PaneWidths>().files_sidebar_px = clamped;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn seed_clamps_on_restore(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            seed(cx, 9_000.0);
            let panes = cx.try_global::<PaneWidths>().expect("seeded");
            assert_eq!(panes.files_sidebar_px, FILES_SIDEBAR_MAX);
        });
        cx.update(|cx| {
            seed(cx, 10.0);
            let panes = cx.try_global::<PaneWidths>().expect("seeded");
            assert_eq!(panes.files_sidebar_px, FILES_SIDEBAR_MIN);
        });
    }

    #[gpui::test]
    fn seed_default_lands_in_range(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            seed(cx, FILES_SIDEBAR_DEFAULT);
            let panes = cx.try_global::<PaneWidths>().expect("seeded");
            assert_eq!(panes.files_sidebar().as_f32(), FILES_SIDEBAR_DEFAULT);
        });
    }

    #[gpui::test]
    fn set_ignores_unseeded_app(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            set_files_sidebar(px(400.0), cx);
            assert!(cx.try_global::<PaneWidths>().is_none());
        });
    }

    #[gpui::test]
    fn set_clamps_and_skips_unchanged(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            seed(cx, FILES_SIDEBAR_DEFAULT);
            set_files_sidebar(px(50.0), cx);
            assert_eq!(
                cx.global::<PaneWidths>().files_sidebar_px,
                FILES_SIDEBAR_MIN
            );
            set_files_sidebar(px(1_000.0), cx);
            assert_eq!(
                cx.global::<PaneWidths>().files_sidebar_px,
                FILES_SIDEBAR_MAX
            );
            // Unchanged writes stay no-ops (each real one schedules a save).
            set_files_sidebar(px(FILES_SIDEBAR_MAX), cx);
            assert_eq!(
                cx.global::<PaneWidths>().files_sidebar_px,
                FILES_SIDEBAR_MAX
            );
        });
    }
}
