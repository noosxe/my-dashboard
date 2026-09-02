# 0002 — Phase 1: Application Shell & Window Chrome

- **Implementation record:** rebase-merged as f1c285b; all §6 gates passed (human checks confirmed on Hyprland); API corrections in §3.8
- **Date:** 2026-08-28
- **Phase:** 1 (Shell)
- **Depends on:** 0001 (PR #1)
- **Implementation target:** end of Phase 1

---

## 1. Context

Phase 0 (0001) shipped a default-window hello world. Phase 1 turns it into a real
**application shell**: deliberate window geometry, custom cross-platform title bar
chrome, app identity (title + Wayland `app_id`), and a code structure ready to host
the module system (0003). The shell is the frame the dashboard lives in — it should
be settled before modules land.

## 2. Decisions (owner review 2026-08-28)

All recommendations below were reviewed and accepted; the values feed the §5
listings directly. The one substantive addition from review: a titlebar-less mode
(D7), triggered by the owner running Hyprland titlebar-less everywhere.

| # | Question | Options | My recommendation |
| --- | --- | --- | --- |
| Q1 | **Window chrome style** | (a) custom frameless `TitleBar` (gpui-component); (b) native OS titlebar | **(a)** — one themed look on all 3 OSes; production-proven (Longbridge Pro); **tiling-WM-safe — see note T1**; macOS keeps native traffic lights |
| Q2 | **Default window geometry** | default size / min size | **1280×800 default, centered; min 900×600** (dashboard content wants width) |
| Q3 | **Window title text** | shown in title bar, taskbar, WM lists | **"Dashboard"** (short, human) — repo name stays `my-dashboard` |
| Q4 | **`app_id`** (Wayland/X11 identity) | reverse-DNS string; affects taskbar grouping, future `.desktop` entry | **`com.noosxe.my-dashboard`** (matches your GitHub org) |
| Q5 | **Theme mode default** | (a) follow system dark/light; (b) always dark; (c) always light | **(a)** follow system if the sync hook is cheap to wire (verify during implementation); fallback (b) dark |
| Q6 | **Title bar content** | (a) title only; (b) title + right-side slot (e.g. settings icon placeholder) | **(a)** title only now; reserve the right slot structurally (it's one `.child()`), fill in 0003+ |
| Q7 | **Close button behavior** | (a) close = quit; (b) minimize-to-tray someday | **(a)** plain quit; `TitleBar::on_close_window` hook exists when (b) becomes real |
| Q8 | **Resizable / fullscreen** | defaults: resizable yes, no custom fullscreen handling | Keep defaults (no extra code in 0002) |
| Q9 | **Body placeholder until 0003** | (a) keep centered hello-world content; (b) static wireframe grid of module cards | **(a)** — least code, replaced anyway in 0003; (b) only if you want a visual target now |

**T1 — tiling WMs (Q1):** on compositors that provide no titlebar (Hyprland, sway, i3…)
the app *always* renders its own strip — client-side decorations are part of our window
surface, so we never end up titlebar-less. Upstream's `TitleBar` additionally adapts at
runtime (verified in `title_bar.rs` source): it skips its own min/max/close buttons when the
WM insists on server-side decorations (no duplicates), and on tiling WMs it draws close
always + min/max only if the compositor honors them (`window.window_controls()`), with
right-click → WM window menu and double-click → maximize. The "native" option (b) is the
one that yields *no titlebar at all* on tiling WMs.

**D7 — SUPERSEDED (2026-08-29, owner decision in 0004 PR #13):** the runtime chrome
toggle, the `ctrl-shift-t` binding and the ghost close button are all removed.
Chrome is now **decoration-driven** — resolved per render from
`window.window_decorations()`: `Client` → the app draws the full title bar
(gpui-component renders min/max/close there); `Server` → the app draws nothing at
all, no X button — the server owns decorations (KWin/X11 WM bars provide controls;
native macOS bar provides traffic lights; on decoration-less servers like Hyprland,
window management stays keybind territory). No compositor detection, no guessing
which WM actually draws. Empirically zero boot flash on Hyprland: the decoration
answer arrives with the initial surface configure, before the first frame.
The historical D7 text below is kept for the record.

**D7 — titlebar-less mode (added at review):** the shell carries a runtime
chrome toggle, because the owner runs most apps titlebar-less on Hyprland:

- `ToggleTitleBar` gpui action, bound to **`ctrl-shift-t`** (context-free binding; the
  pattern — `actions!` + `cx.bind_keys` + `on_action(cx.listener(..))` — verified in
  upstream's gpui skill `references/action.md`).
- Default state: **titlebar-less on Linux** (owner's daily driver), full title bar on
  macOS/Windows (`cfg`-selected const; persistence lands in 0004 so the choice is
  remembered).
- In titlebar-less mode a **small ghost close button** overlays the top-right of the
  body (hover-highlighted `Button` + `X` icon → `window.remove_window()`) — the "lazy
  close" affordance; everything else stays WM/keyboard territory.
- Trade-off accepted: no drag strip / right-click WM menu / dbl-click maximize while
  titlebar-less (Hyprland users manage windows by keybind anyway); toggle restores
  the full strip at any time.

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
   supports right-click context menu and double-click maximize/restore. Verified in
   `title_bar.rs`: the component reads `window.window_decorations()` — under
   server-side decorations (X11 w/o compositor; a Wayland compositor granting server
   mode) it skips its own buttons to avoid a duplicate control set, and under
   client-side decorations (tiling WMs) it renders them, gated per-capability via
   `window.window_controls()` ("a tiling compositor may support neither minimize nor
   maximize; close is always ours to offer").
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

8. **API corrections found at implementation** (doc listings updated to match):
   `Context::listener` handlers take 4 args — `(&mut T, &E, &mut Window, &mut
   Context<T>)`; `when()` requires `gpui::prelude::FluentBuilder as _` in scope;
   `IconName` variants are generated from shipped asset filenames (no `X` — use
   `IconName::Close`, backed by `close.svg`); `TitlebarOptions.title` is a
   `SharedString` (`.into()`); `size()` takes `Pixels` (`size(px(..), px(..))`).

## 4. Design

### 4.1 Structure (inside `crates/app` — no new crates yet)

```
crates/app/src/
├── main.rs      # thin bootstrap: assets, HTTP client, init, window options, open window
│                # (HTTP client amended by 0012 impl: `with_http_client` — zed
│                # `reqwest_client` — so gpui's asset loader can fetch remote
│                # avatar images; without it gpui uses NullHttpClient and
│                # errors per image)
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
  **Enforced (2026-09-02):** literal color constructors (`gpui::rgb`/`rgba`/`hsla`)
  are disallowed app-wide in `clippy.toml` — colors come from theme tokens or
  derive from them (`.opacity(..)`).
- **D6 — Root invariant preserved**: `Root` remains the window's first-level view;
  `TitleBar` lives inside the shell view, first child of the top-level `v_flex`.
- **D8 — Window padding & framed panel** (2026-09-02, owner decision in its PR):
  the work area — both main columns (sidebar + content column) — sits inside a
  framed panel: an outer wrapper supplies a 16px (`p_4`) margin from the window
  edges; the inner panel carries `border_1` (`theme.border`), `theme.radius`
  corners and `overflow_hidden` (clips the sidebar/content backgrounds to the
  rounded corners). Verified upstream: gpui/gpui-component add **no automatic
  window content padding** on any platform (`Root`'s Linux `window_border` is a
  20px CSD shadow frame, 0px on macOS/Windows) — apps pad themselves, and this
  is ours. The client-decorated `TitleBar` (D7 note above) stays full-width
  above the padded area.
- **D9 — Typography scale only** (2026-09-02): every text size comes from the
  shared Tailwind-style scale (`text_xs`/`text_sm`/`text_base`/`text_lg`/`text_xl`
  = 12/14/16/18/20px, rem-based so they track the theme font size; note there is
  no `text_md` — the 16px step is `text_base`). No ad-hoc `text_size(px(..))` in
  app code; component sizing goes through the `Size` API so type, icons and
  spacing scale together. Never override `line_height` to tighten rows: inherited
  line heights keep a ~1.3–1.4× ratio that never clips glyph ink, and tighter
  boxes under `overflow_hidden`/`truncate` masks cut descenders (the upstream
  #2775/#2883 mechanism). **Enforced via `clippy.toml`** (`text_size`/
  `line_height` disallowed).
- **D10 — Spacing scale (structural)** (2026-09-02): padding/margin/gap have **no
  raw-value API** — `p_*`/`px_*`/`py_*`/`m_*`/`gap_*` are macro-generated
  (gpui_macros `styles.rs`) and set style fields directly on the fixed 0.25rem
  scale (`0p5`=2 · `1`=4 · `1p5`=6 · `2`=8 · `2p5`=10 · `3`=12 · `4`=16 · `6`=24 ·
  `8`=32px at the default rem), so spacing cannot go ad-hoc through styling.
  Keep it that way: never mutate `style()` fields to dodge the scale. Widget
  **geometry** (`.w(px(..))`, `.h(px(..))` — card sizes, column widths) is
  deliberately exempt: content sizes are layout values, not rhythm; define them
  as named constants. D9's `text_size`/`line_height` and D5's color literals
  are enforced by `clippy.toml` `disallowed-methods`; escape hatch is
  `#[allow(clippy::disallowed_methods)]` + a justifying comment.

## 5. Proposed implementation

**`crates/app/src/main.rs`**

```rust
mod shell;

use gpui::*;
use gpui_component::{ActiveTheme, Root, TitleBar};

use shell::{DashboardShell, ToggleTitleBar};

// Identity & geometry — answers to 0002 §2 (Q2–Q4); single place to tune.
const APP_ID: &str = "com.noosxe.my-dashboard";
const WINDOW_TITLE: &str = "Dashboard";
const DEFAULT_SIZE: (f32, f32) = (1280., 800.);
const MIN_SIZE: (f32, f32) = (900., 600.);

fn main() {
    gpui_platform::application()
        .with_assets(gpui_component_assets::Assets)
        .with_http_client(std::sync::Arc::new(   // 0012 impl — avatar images
            reqwest_client::ReqwestClient::user_agent("my-dashboard/0.1.0")
                .unwrap(),
        ))
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
                app_id: Some(APP_ID.into()), // inner string type per locked gpui rev
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
```

**`crates/app/src/shell.rs`**

```rust
use gpui::{prelude::FluentBuilder as _, *};
use gpui_component::{
    IconName, TitleBar,
    button::{Button, ButtonVariants},
    h_flex, v_flex,
};

actions!(dashboard, [ToggleTitleBar]);

/// Default chrome state (D7): titlebar-less on Linux (owner runs Hyprland),
/// full title bar elsewhere. Persisted choice arrives in 0004.
#[cfg(target_os = "linux")]
const DEFAULT_TITLEBAR: bool = false;
#[cfg(not(target_os = "linux"))]
const DEFAULT_TITLEBAR: bool = true;

pub struct DashboardShell {
    show_titlebar: bool,
}

impl DashboardShell {
    pub fn new() -> Self {
        Self { show_titlebar: DEFAULT_TITLEBAR }
    }

    fn toggle_titlebar(&mut self, _: &ToggleTitleBar, _window: &mut Window, cx: &mut Context<Self>) {
        self.show_titlebar = !self.show_titlebar;
        cx.notify();
    }
}

impl Render for DashboardShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .key_context("Dashboard")
            .on_action(cx.listener(Self::toggle_titlebar))
            .when(self.show_titlebar, |this| {
                this.child(
                    // Custom chrome; per-OS adaptive (traffic lights / buttons,
                    // drag, right-click menu, double-click maximize on Linux).
                    TitleBar::new().child(
                        h_flex()
                            .w_full()
                            .pr_2()
                            .justify_between()
                            .child("Dashboard")
                            // Reserved right-side slot (Q6) — fill in 0003+.
                            .child(div()),
                    ),
                )
            })
            .child(
                // Body placeholder — becomes the module host in 0003.
                h_flex()
                    .id("window-body")
                    .relative()
                    .flex_1()
                    .w_full()
                    .items_center()
                    .justify_center()
                    .child("Hello, World!")
                    // Titlebar-less mode (D7): ghost close button, top-right.
                    .when(!self.show_titlebar, |this| {
                        this.child(
                            div()
                                .absolute()
                                .top_1()
                                .right_1()
                                .child(
                                    Button::new("close")
                                        .ghost()
                                        .icon(IconName::Close)
                                        .on_click(|_, window, _| window.remove_window()),
                                ),
                        )
                    }),
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
- [ ] `ctrl-shift-t` toggles chrome at runtime: full `TitleBar` ⇄ titlebar-less
      (default on Linux is titlebar-less, per D7).
- [ ] Titlebar-less mode shows the ghost close button (top-right, hover
      highlight) and it quits the app; the keybinding restores the full bar.
- [ ] Close button quits the app cleanly (no lingering process).
- [ ] All content colors come from `cx.theme()` — shell renders correctly in the
      default theme mode (and the second mode, if Q5 = follow-system).
- [ ] Usual gates: `cargo fmt --check`, `cargo clippy -D warnings`, incremental
      build only.
- [ ] Doc 0002 updated with any implementation discoveries (e.g. theme sync
      mechanism, exact `app_id` inner type.

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
