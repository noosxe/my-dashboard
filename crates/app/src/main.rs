mod shell;

use gpui::*;
use gpui_component::{ActiveTheme, Root, TitleBar};

use shell::{DashboardShell, ToggleTitleBar};

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

            // Chrome toggle (D7): ctrl-shift-t switches titlebar-less ⇄ full bar.
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
                    let view = cx.new(|_| DashboardShell::new());
                    cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
                })
                .expect("Failed to open window");
            })
            .detach();
        });
}
