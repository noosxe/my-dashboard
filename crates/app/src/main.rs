use gpui::*;
use gpui_component::{Root, TitleBar};

use dashboard_core::{AppPaths, DashboardShell, ModuleRegistry, Navigator, ShellState, load_json};
use dashboard_module_github::GithubModule;
mod menu;
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
        gpui_kit_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<gpui::SharedString>> {
        gpui_kit_assets::Assets.list(path)
    }
}

fn main() {
    gpui_platform::application()
        .with_assets(AppAssets)
        // Avatar rows fetch remote images through gpui's asset loader
        // (docs/0005 §6.3, doc 0012 D1); without a client gpui falls back
        // to NullHttpClient and logs an error per image.
        .with_http_client(std::sync::Arc::new(
            reqwest_client::ReqwestClient::user_agent(concat!(
                "my-dashboard/",
                env!("CARGO_PKG_VERSION"),
            ))
            .expect("user agent is valid"),
        ))
        .run(move |cx| {
            // MUST be called before using any gpui-component features.

            // Storage roots + the app logger (docs/0008 D5) — logging
            // starts before anything else can emit, so every later
            // event (settings restore, modules, fetches) lands in the
            // file. The handle is leaked: dropping it mid-run would shut
            // the writers down while the app is still going.
            let paths = AppPaths::init();
            // The log ring (#47, docs/0010 D8) is created before
            // `logging::init` so capture starts with the very first line,
            // then handed to the debug server below.
            #[cfg(feature = "debug-tools")]
            let debug_ring = dashboard_debug::logs::RingLog::new();
            #[cfg(feature = "debug-tools")]
            let extra_writers = vec![debug_ring.writer()];
            #[cfg(not(feature = "debug-tools"))]
            let extra_writers: Vec<Box<dyn dashboard_core::logging::LogWriter>> = Vec::new();
            std::mem::forget(dashboard_core::logging::init(paths, extra_writers));

            // Agent Debug Bridge (docs/0010, #45): Unix-socket debug
            // server — starts with the app; failures are logged and
            // non-fatal by design (the bridge must never take the app
            // down with it). Off entirely when the feature is absent.
            #[cfg(feature = "debug-tools")]
            if let Some(socket) = dashboard_debug::start(cx, debug_ring) {
                log::info!(
                    target: "dashboard_app",
                    "debug bridge listening on {}",
                    socket.display()
                );
            }

            // MUST be called before using any gpui-component features.
            gpui_component::init(cx);
            // Menu-bar bindings (docs/0002 D12) — empty off macOS.
            cx.bind_keys(menu::bindings());

            // Global in-flight request tracker (docs/0013 D2) — before
            // module init, so every module's fetch loops can report into it.
            dashboard_core::status::init(cx);
            // Toast hub (docs/0058 D2): beside status::init and before
            // module init — raisers may exist before the window does.
            dashboard_core::toasts::init(cx);
            // Usage records (docs/0053 D3): the module-global flight log.
            // SQLite is the startup default (docs/0057 D1/D9); an unusable
            // data dir degrades to in-memory capture (docs/0057 D6).
            let usage_store = dashboard_core::app_db::SQLiteStore::open_or_fallback(paths);
            // Cross-thread degrade toasts (docs/0058 D6/D7): the store
            // raises from the writer thread through the hub's spill; the
            // shell's pump surfaces it once the window exists. A startup
            // fallback raises before this attach and is latched (D7).
            usage_store.attach_toast_spill(dashboard_core::toasts::spill(cx));
            dashboard_core::usage_records::init_with(cx, usage_store.clone());
            // Notification log persistence (docs/0063 D5/D10): the hub
            // attaches the app DB's `NotificationLog` impl and restores
            // the persisted unread set here — before any window exists,
            // so the badge's first frame reflects what survived the
            // restart. The restore read is the one blocking call on this
            // path (the docs/0054 §8 precedent); a degraded store
            // restores nothing and the hub runs ephemeral (0063 D8).
            dashboard_core::toasts::hub(cx).update(cx, |hub, cx| {
                hub.attach_log_and_restore(usage_store.clone(), cx);
            });

            // Flush debounced state writes at quit (docs/0045 §9, #283):
            // a quit landing inside a 500 ms quiet window would otherwise
            // lose the tail — shell state, settings, and open draft
            // composers land synchronously here. Log-and-swallow (0004
            // §4.7): a failed flush must never block quitting.
            cx.on_app_quit(move |_| {
                let store = usage_store.clone();
                async move {
                    dashboard_core::store::flush_pending_on_quit();
                    // Usage tail: drain + commit the record buffer before
                    // the process exits (docs/0057 D9) — same log-and-
                    // swallow rule: a failed flush must never block
                    // quitting.
                    store.flush_blocking();
                }
            })
            .detach();

            // Persisted app settings (theme restore — docs/0006 §4.4)
            // before any window or module exists.
            dashboard_core::settings::init(
                Some(paths.settings_file()),
                Some(paths.themes_dir()),
                cx,
            );
            dashboard_core::settings::set_app_info("Dashboard", env!("CARGO_PKG_VERSION"));
            // Toast geometry (docs/0058 D5): keep the upstream TopRight +
            // title-bar-aware margins; tighten the stack to the issue's
            // bound. `Theme::change` only recolors — it never resets this
            // field — so setting it once here survives appearance flips.
            cx.global_mut::<gpui_component::Theme>()
                .notification
                .max_items = 3;
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
                    // The menu keeps its own handle — nav moves into the
                    // shell below.
                    let nav_handle = nav.clone();
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
                    // Menu bar + action handlers (docs/0002 D12): needs
                    // both entity handles, so it runs after the shell
                    // exists.
                    menu::install(
                        cx,
                        nav_handle.downgrade(),
                        shell.downgrade(),
                        window.window_handle(),
                    );
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
