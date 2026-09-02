use gpui::*;
use gpui_component::{Root, TitleBar};

use dashboard_core::{AppPaths, DashboardShell, ModuleRegistry, Navigator, ShellState, load_json};
use dashboard_module_github::GithubModule;
// Identity & geometry — decisions from docs/0002 §2; single place to tune.
const APP_ID: &str = "com.noosxe.my-dashboard";
const WINDOW_TITLE: &str = "Dashboard";
const DEFAULT_SIZE: (f32, f32) = (1280., 800.);
const MIN_SIZE: (f32, f32) = (900., 600.);

/// Asset source: gpui-component's embedded set + our own icons (the GitHub
/// Asset source: gpui-component's embedded set + our own icons (GitHub mark;
/// private-repo lock — both octicons, @primer/octicons 19.33.0).
struct AppAssets;

impl gpui::AssetSource for AppAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        if path == "icons/github.svg" {
            return Ok(Some(std::borrow::Cow::Borrowed(
                include_bytes!("../assets/icons/github.svg",) as &[u8],
            )));
        }
        if path == "icons/lock.svg" {
            return Ok(Some(std::borrow::Cow::Borrowed(
                include_bytes!("../assets/icons/lock.svg",) as &[u8],
            )));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<gpui::SharedString>> {
        gpui_component_assets::Assets.list(path)
    }
}

fn main() {
    gpui_platform::application()
        .with_assets(AppAssets)
        .run(move |cx| {
            // MUST be called before using any gpui-component features.

            // Storage roots + the app logger (docs/0008 D5) — logging
            // starts before anything else can emit, so every later
            // event (settings restore, modules, fetches) lands in the
            // file. The handle is leaked: dropping it mid-run would shut
            // the writers down while the app is still going.
            let paths = AppPaths::init();
            std::mem::forget(dashboard_core::logging::init(paths));

            // Agent Debug Bridge (docs/0010, #45): Unix-socket debug
            // server — starts with the app; failures are logged and
            // non-fatal by design (the bridge must never take the app
            // down with it). Off entirely when the feature is absent.
            #[cfg(feature = "debug-tools")]
            if let Some(socket) = dashboard_debug::start(cx) {
                log::info!(
                    target: "dashboard_app",
                    "debug bridge listening on {}",
                    socket.display()
                );
            }

            // MUST be called before using any gpui-component features.
            gpui_component::init(cx);

            // Persisted app settings (theme restore — docs/0006 §4.4)
            // before any window or module exists.
            dashboard_core::settings::init(Some(paths.settings_file()), cx);
            dashboard_core::settings::set_app_info("Dashboard", env!("CARGO_PKG_VERSION"));
            log::info!(
                target: "dashboard_app",
                "dashboard {} starting: data={}, state={}",
                env!("CARGO_PKG_VERSION"),
                paths.data_dir().display(),
                paths.state_dir().display(),
            );
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
                    // Storage roots were resolved before the window (docs/
                    // 0004); here they seed the shell state path.
                    let shell_state_path = paths.shell_state_file();
                    let shell_state: ShellState = load_json(&shell_state_path)
                        .inspect_err(|err| log::warn!("shell state load failed: {err}"))
                        .ok()
                        .flatten()
                        .unwrap_or_default();

                    // Navigation state, then modules (registry order = sidebar
                    // + home-grid order — docs/0003 §4.2).
                    let nav = cx.new(|_| Navigator::new(shell_state.sidebar_collapsed));
                    let registry =
                        ModuleRegistry::new().with::<GithubModule>(&nav, window, paths, cx);

                    // Live system-appearance follow (docs/0006 §3.6): while
                    // the mode is implicit (System), OS appearance changes
                    // re-apply the theme. The subscription lives as long as
                    // the window — keep it registered for the app lifetime.
                    let appearance_sub = window.observe_window_appearance(
                        dashboard_core::settings::sync_window_appearance,
                    );
                    std::mem::forget(appearance_sub);
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
                    // No .bg() refinement here: Root already paints the live
                    // theme background + foreground every frame; a style
                    // refinement is baked at window construction and would
                    // freeze the pre-portal (light) palette over every later
                    // Theme::change — see gpui-component root.rs render().
                    cx.new(|cx| Root::new(shell, window, cx))
                })
                .expect("Failed to open window");
                log::info!(target: "dashboard_app", "window opened");
            })
            .detach();
        });

    // Debug bridge teardown (docs/0010 D5): restores ScreenReaderEnabled
    // if the bridge auto-enabled it. Best-effort; skipped on crash.
    #[cfg(feature = "debug-tools")]
    dashboard_debug::shutdown();
}
