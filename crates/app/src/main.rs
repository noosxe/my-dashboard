use gpui::*;
use gpui_component::{ActiveTheme, Root, TitleBar};

use dashboard_core::{DashboardShell, ModuleRegistry, Navigator, ToggleTitleBar};
use dashboard_module_clock::ClockModule;
use dashboard_module_notes::NotesModule;

// Identity & geometry — decisions from docs/0002 §2; single place to tune.
const APP_ID: &str = "com.noosxe.my-dashboard";
const WINDOW_TITLE: &str = "Dashboard";
const DEFAULT_SIZE: (f32, f32) = (1280., 800.);
const MIN_SIZE: (f32, f32) = (900., 600.);

fn main() {
    gpui_platform::application()
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx| {
            // MUST be called before using any gpui-component features.
            gpui_component::init(cx);

            // Chrome toggle (0002 D7): ctrl-shift-t switches titlebar-less ⇄ full bar.
            cx.bind_keys([KeyBinding::new("ctrl-shift-t", ToggleTitleBar, None)]);

            let window_options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(DEFAULT_SIZE.0), px(DEFAULT_SIZE.1)),
                    cx,
                ))),
                window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
                app_id: Some(APP_ID.into()),
                titlebar: Some(TitlebarOptions {
                    title: Some(WINDOW_TITLE.into()),
                    ..TitleBar::title_bar_options()
                }),
                // Required on macOS for correct custom-titlebar behavior.
                app_owns_titlebar_drag: true,
                ..Default::default()
            };

            cx.spawn(async move |cx| {
                cx.open_window(window_options, |window, cx| {
                    // Navigation state, then modules (registry order = sidebar
                    // + home-grid order — docs/0003 §4.2).
                    let nav = cx.new(|_| Navigator::new());
                    let registry = ModuleRegistry::new()
                        .with::<ClockModule>(&nav, window, cx)
                        .with::<NotesModule>(&nav, window, cx);

                    let shell = cx.new(|cx| DashboardShell::new(registry, nav, cx));
                    cx.new(|cx| Root::new(shell, window, cx).bg(cx.theme().background))
                })
                .expect("Failed to open window");
            })
            .detach();
        });
}
