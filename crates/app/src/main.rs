use gpui::*;
use gpui_component::{ActiveTheme, Root, TitleBar};

use dashboard_core::{AppPaths, DashboardShell, ModuleRegistry, Navigator, ShellState, load_json};
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
                    // Storage roots first (docs/0004): one resolution,
                    // before any entity exists.
                    let paths = AppPaths::init();
                    let shell_state_path = paths.shell_state_file();
                    let shell_state: ShellState = load_json(&shell_state_path)
                        .inspect_err(|err| eprintln!("dashboard: shell state load failed: {err}"))
                        .ok()
                        .flatten()
                        .unwrap_or_default();

                    // Navigation state, then modules (registry order = sidebar
                    // + home-grid order — docs/0003 §4.2).
                    let nav = cx.new(|_| Navigator::new(shell_state.sidebar_collapsed));
                    let registry = ModuleRegistry::new()
                        .with::<ClockModule>(&nav, window, paths, cx)
                        .with::<NotesModule>(&nav, window, paths, cx);

                    let shell = cx.new(|cx| {
                        DashboardShell::new(
                            registry,
                            nav,
                            shell_state,
                            Some(shell_state_path),
                            window,
                            cx,
                        )
                    });
                    cx.new(|cx| Root::new(shell, window, cx).bg(cx.theme().background))
                })
                .expect("Failed to open window");
            })
            .detach();
        });
}
