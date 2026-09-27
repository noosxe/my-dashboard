//! App menu bar (docs/0002 D12): the macOS native menu bar via gpui's
//! `App::set_menus`. Menu-item clicks arrive as dispatched actions —
//! the platform routes them through `App::dispatch_action` (pinned
//! gpui-pre `platform/app_menu.rs`, `on_app_menu_action`) — so every
//! item is backed by a plain `App::on_action` handler. Only the three
//! window commands need plumbing, via the captured `AnyWindowHandle`.
//! Linux installs nothing yet (the menu call and its bindings are
//! macOS-gated); an in-window `AppMenuBar` fallback is deferred (#373).

use gpui::{AnyWindowHandle, App, KeyBinding, Menu, MenuItem, WeakEntity, actions};
use gpui_component::input;

use dashboard_core::{DashboardShell, Navigator};

actions!(
    dashboard,
    [
        Quit,
        About,
        Hide,
        ToggleSidebar,
        MinimizeWindow,
        ZoomWindow,
        ToggleFullScreen,
        OpenRepository,
    ]
);

/// Keystrokes the macOS menu shows next to its items (looked up from
/// the app keymap at `set_menus` time). Registered only on macOS: on
/// Linux the menu never appears, and the bindings would still be live
/// global keys there (D12).
pub fn bindings() -> Vec<KeyBinding> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    vec![
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("cmd-m", MinimizeWindow, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
    ]
}

/// The menu tree — pure, so its shape is unit-tested below.
pub fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "Dashboard".into(),
            items: vec![
                MenuItem::action("About Dashboard", About),
                MenuItem::separator(),
                MenuItem::action("Hide Dashboard", Hide),
                MenuItem::separator(),
                MenuItem::action("Quit Dashboard", Quit),
            ],
            disabled: false,
        },
        Menu {
            name: "Edit".into(),
            // Component input actions: they dispatch through the focus
            // path, so they land in the focused text input and no-op
            // when nothing has focus.
            items: vec![
                MenuItem::action("Undo", input::Undo),
                MenuItem::action("Redo", input::Redo),
                MenuItem::separator(),
                MenuItem::action("Cut", input::Cut),
                MenuItem::action("Copy", input::Copy),
                MenuItem::action("Paste", input::Paste),
                MenuItem::separator(),
                MenuItem::action("Select All", input::SelectAll),
            ],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Toggle Sidebar", ToggleSidebar),
                MenuItem::action("Toggle Full Screen", ToggleFullScreen),
            ],
            disabled: false,
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Minimize", MinimizeWindow),
                MenuItem::action("Zoom", ZoomWindow),
            ],
            disabled: false,
        },
        Menu {
            name: "Help".into(),
            items: vec![MenuItem::action("Project Repository", OpenRepository)],
            disabled: false,
        },
    ]
}

/// Registers the action handlers and (on macOS) the menu bar. Called
/// once from the window-build closure: `on_action` pushes onto a
/// listener list, so this must not run twice (single window, one
/// build — same lifetime as the window itself).
pub fn install(
    cx: &mut App,
    nav: WeakEntity<Navigator>,
    shell: WeakEntity<DashboardShell>,
    window: AnyWindowHandle,
) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(move |_: &About, cx| {
        let _ = shell.update(cx, DashboardShell::open_about);
    });
    cx.on_action(move |_: &ToggleSidebar, cx| {
        // Same path as the chrome button (nav.toggle_sidebar), so the
        // debounced shell-state save and re-render follow for free.
        let _ = nav.update(cx, Navigator::toggle_sidebar);
    });
    cx.on_action(move |_: &MinimizeWindow, cx| {
        let _ = window.update(cx, |_, window, _| window.minimize_window());
    });
    cx.on_action(move |_: &ZoomWindow, cx| {
        let _ = window.update(cx, |_, window, _| window.zoom_window());
    });
    cx.on_action(move |_: &ToggleFullScreen, cx| {
        let _ = window.update(cx, |_, window, _| window.toggle_fullscreen());
    });
    cx.on_action(|_: &OpenRepository, cx| {
        cx.open_url("https://github.com/noosxe/my-dashboard");
    });

    if cfg!(target_os = "macos") {
        cx.set_menus(menus());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_shape_matches_docs_0002_d12() {
        let menus = menus();
        let names: Vec<_> = menus.iter().map(|m| m.name.as_ref()).collect();
        assert_eq!(names, ["Dashboard", "Edit", "View", "Window", "Help"]);
        assert_eq!(menus[0].items.len(), 5); // About, —, Hide, —, Quit
        assert!(menus.iter().all(|m| !m.items.is_empty()));
    }

    #[test]
    fn bindings_are_macos_only() {
        assert_eq!(bindings().is_empty(), !cfg!(target_os = "macos"));
    }
}
