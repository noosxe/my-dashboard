# 0002 — Phase 1: Application Shell & Window Chrome

- **Status:** Draft — **awaiting review of §2 open questions**
- **Date:** 2026-08-28
- **Phase:** 1 (Shell)
- **Depends on:** 0001 (Implemented, PR #1)
- **Implementation target:** end of Phase 1

---

## 1. Context

Phase 0 (0001) shipped a default-window hello world. Phase 1 turns it into a real
**application shell**: deliberate window geometry, custom cross-platform title bar
chrome, app identity (title + Wayland `app_id`), and a code structure ready to host
the module system (0003). The shell is the frame the dashboard lives in — it should
be settled before modules land.

## 2. Open questions — NEEDS YOUR INPUT

Each item has a recommendation; defaults are used in §5 listings and are trivial to
change while the doc is Draft.

| # | Question | Options | My recommendation |
| --- | --- | --- | --- |
| Q1 | **Window chrome style** | (a) custom frameless `TitleBar` (gpui-component); (b) native OS titlebar | **(a)** — one themed look on all 3 OSes; production-proven (Longbridge Pro); Linux gets drag/right-click menu/double-click-maximize for free; macOS keeps native traffic lights |
| Q2 | **Default window geometry** | default size / min size | **1280×800 default, centered; min 900×600** (dashboard content wants width) |
| Q3 | **Window title text** | shown in title bar, taskbar, WM lists | **"Dashboard"** (short, human) — repo name stays `my-dashboard` |
| Q4 | **`app_id`** (Wayland/X11 identity) | reverse-DNS string; affects taskbar grouping, future `.desktop` entry | **`com.noosxe.my-dashboard`** (matches your GitHub org) |
| Q5 | **Theme mode default** | (a) follow system dark/light; (b) always dark; (c) always light | **(a)** follow system if the sync hook is cheap to wire (verify during implementation); fallback (b) dark |
| Q6 | **Title bar content** | (a) title only; (b) title + right-side slot (e.g. settings icon placeholder) | **(a)** title only now; reserve the right slot structurally (it's one `.child()`), fill in 0003+ |
| Q7 | **Close button behavior** | (a) close = quit; (b) minimize-to-tray someday | **(a)** plain quit; `TitleBar::on_close_window` hook exists when (b) becomes real |
| Q8 | **Resizable / fullscreen** | defaults: resizable yes, no custom fullscreen handling | Keep defaults (no extra code in 0002) |
| Q9 | **Body placeholder until 0003** | (a) keep centered hello-world content; (b) static wireframe grid of module cards | **(a)** — least code, replaced anyway in 0003; (b) only if you want a visual target now |

## 3. Verified upstream facts (2026-08-28)

Sources: upstream `examples/window_title` (fetched in full), gpui-component website
docs `components/title-bar.md` (via Context7), zed `crates/gpui/src/window.rs`
(fetched + indexed).

1. **Custom title bar setup** — canonical pattern from the `window_title` example:
   ```rust
   // in main(), before open_window:
   let window_options = TitleBar::window_options();
   // equivalent explicit form (website docs):
   WindowOptions {
       titlebar: Some(TitleBar::title_bar_options()),
       // Required on macOS, otherwise the system also handles title bar double
       // clicks and delays title bar clicks to disambiguate double clicks.
       app_owns_titlebar_drag: true,
       ..Default::default()
   }
   ```
   Adding custom bounds on top: `WindowOptions { window_bounds: Some(bounds), ..TitleBar::window_options() }`.
2. **Rendering pattern** — `TitleBar` is rendered as the first child *inside* the
   Root view (the Root stays the window's first-level view):
   ```rust
   v_flex().size_full()
       .child(TitleBar::new().child(h_flex().w_full().pr_2().justify_between()
           .child("title").child("right item")))
       .child(/* window body */)
   ```
3. **TitleBar behavior is per-OS adaptive**: macOS native traffic lights; Windows
   and Linux custom control buttons with manual event handling; Linux additionally
   supports right-click context menu and double-click maximize/restore.
4. **Custom close behavior** hook exists:
   `TitleBar::new().on_close_window(|_, window, cx| { …; window.remove_window(); })`
   — primarily for Linux (Q7 future use).
5. **`WindowOptions` fields** (from the struct destructure in zed source):
   `window_bounds`, `titlebar` (`TitlebarOptions { title, … }`), `focus`, `show`,
   `kind`, `is_movable`, `app_owns_titlebar_drag`, `inactive_frame_interval`,
   `is_resizable`, `is_minimizable`, `display_id`, `window_background`, `app_id`,
   `window_min_size`, `window_decorations`, `icon`, (macOS) `tabbing_identifier`.
6. **Runtime setters** exist for later phases: `window.set_window_title(&str)`,
   `window.set_app_id(&str)`; centering helper `Bounds::centered(None, size(w, h), cx)`.
7. **Theming**: `Theme` is a GPUI global; `cx.update_global::<Theme, _>(|t, cx| t.toggle_mode(cx))`
   switches light/dark; semantic tokens via `ActiveTheme` — `cx.theme().background`,
   `.surface`, `.foreground`, `.border`, `.primary`, `.muted`, `.destructive`.
   (System-appearance sync mechanism to be confirmed at implementation — Q5.)

## 4. Design

### 4.1 Structure (inside `crates/app` — no new crates yet)

```
crates/app/src/
├── main.rs      # thin bootstrap: assets, init, window options, open window
└── shell.rs     # DashboardShell view: TitleBar + body placeholder
```

`main.rs` owns identity + geometry constants (single obvious place to change Q2–Q4
answers later); `shell.rs` owns the chrome. In 0003 the body becomes the module
host and new files/crates grow from there.

### 4.2 Decisions

- **D1 — Custom `TitleBar` chrome** (pending Q1): frameless look, consistent
  theming; `TitleBar::window_options()` as the base, extended with bounds, min
  size, `app_id`, and explicit `TitlebarOptions` title.
- **D2 — Geometry**: centered default bounds via `Bounds::centered`, plus
  `window_min_size` so the layout never collapses (pending Q2 values).
- **D3 — Identity**: window title from `TitlebarOptions.title` (also what the WM
  shows) + `app_id` for Wayland taskbar grouping (pending Q3/Q4 values).
- **D4 — Close = quit** (pending Q7): no `on_close_window` override in 0002; the
  hook is documented for future tray behavior.
- **D5 — Theme**: rely on `gpui_component::init` defaults + `cx.theme()` tokens
  everywhere (no hardcoded colors in the shell). Mode strategy per Q5.
- **D6 — Root invariant preserved**: `Root` remains the window's first-level view;
  `TitleBar` lives inside the shell view, first child of the top-level `v_flex`.

## 5. Proposed implementation

**`crates/app/src/main.rs`**

```rust
mod shell;

use gpui::*;
use gpui_component::{Root, TitleBar};

use shell::DashboardShell;

// Identity & geometry — answers to 0002 §2 (Q2–Q4); single place to tune.
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
                    size(DEFAULT_SIZE.0, DEFAULT_SIZE.1),
                    cx,
                ))),
                window_min_size: Some(size(MIN_SIZE.0, MIN_SIZE.1)),
                app_id: Some(APP_ID.into()), // inner string type per locked gpui rev
                titlebar: Some(TitlebarOptions {
                    title: Some(WINDOW_TITLE.to_owned()),
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
```

**`crates/app/src/shell.rs`**

```rust
use gpui::*;
use gpui_component::{TitleBar, h_flex, v_flex};

pub struct DashboardShell;

impl DashboardShell {
    pub fn new() -> Self {
        Self
    }
}

impl Render for DashboardShell {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .child(
                // Custom chrome; per-OS adaptive (traffic lights / buttons,
                // drag, right-click menu, double-click maximize on Linux).
                TitleBar::new().child(
                    h_flex()
                        .w_full()
                        .pr_2()
                        .justify_between()
                        .child("Dashboard")
                        // Reserved right-side slot (0002 §2 Q6) — fill in 0003+.
                        .child(div()),
                ),
            )
            .child(
                // Body placeholder — becomes the module host in 0003.
                h_flex()
                    .id("window-body")
                    .size_full()
                    .items_center()
                    .justify_center()
                    .child("Hello, World!"),
            )
    }
}
```

No `Cargo.toml` changes: everything used here comes from the existing deps.

## 6. Acceptance criteria

- [ ] Window opens centered at 1280×800 and cannot be resized below 900×600.
- [ ] Custom title bar renders: title "Dashboard", working drag, correct OS-native
      controls (Linux: custom buttons incl. right-click menu + double-click
      maximize).
- [ ] WM/taskbar shows title "Dashboard"; Wayland taskbar groups by
      `com.noosxe.my-dashboard` (verify with `hyprctl`/`kdotool`/taskbar tooltip).
- [ ] Close button quits the app cleanly (no lingering process).
- [ ] All content colors come from `cx.theme()` — shell renders correctly in the
      default theme mode (and the second mode, if Q5 = follow-system).
- [ ] Usual gates: `cargo fmt --check`, `cargo clippy -D warnings`, incremental
      build only.
- [ ] Doc 0002 updated with any implementation discoveries (e.g. theme sync
      mechanism, exact `app_id` inner type) and flipped to Implemented.

## 7. Risks & mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Minor API signature drift in `WindowOptions` fields (inner types) vs this doc | Compile error at implementation | Doc marks the one uncertain spot (`app_id` inner type); lockfile pins the exact gpui rev — adjust to what compiles, record in doc |
| macOS-specific titlebar behavior (`app_owns_titlebar_drag`) untestable from Linux | Regressions when building for macOS later | Pattern copied verbatim from upstream docs incl. the macOS-required flag; revisit at 0005 packaging |
| `TitleBar` interplay with future dock layout (0003) | Rework when modules land | TitleBar stays a fixed top strip; body is fully owned by the shell view — orthogonal to module layout |
| Theme mode sync semantics (Q5) unclear until implementation | Small rework | Decision is isolated to one init-time call; (b) fixed-dark is a safe fallback |

## 8. References

- Upstream example (fetched in full, basis for §5):
  `examples/window_title/src/main.rs` — <https://github.com/longbridge/gpui-component/blob/main/examples/window_title/src/main.rs>
- TitleBar docs: `website/docs/components/title-bar.md` (via Context7
  `/longbridge/gpui-component`)
- gpui `WindowOptions` source: `crates/gpui/src/window.rs` — <https://github.com/zed-industries/zed/blob/main/crates/gpui/src/window.rs>
- Theme docs: `website/docs/theme.md`, skills `references/usage.md` (via Context7)
