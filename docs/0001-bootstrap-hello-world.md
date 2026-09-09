# 0001 — Phase 0: Bootstrap — Hello World gpui-component Application

- **Date:** 2026-08-28
- **Revised:** 2026-08-28 — Nix flake + direnv dev environment (NixOS primary)
- **Phase:** 0 (Bootstrap)
- **Supersedes:** —
- **Implementation target:** end of Phase 0
- **Implementation record:** rustc 1.98.0 (rust-overlay stable); zed@`8cfa848c`, gpui-component@`1081d2b2` (first `Cargo.lock`); first build 41s; all §9 gates passed

---

## 1. Context

`my-dashboard` will be a **native desktop application: a universal, multi-modular
personal dashboard** — a single cross-platform (macOS / Windows / Linux) app whose
surface is composed of independent modules (widgets, monitors, tools) that the user
can arrange and extend.

The UI stack is fixed by prior decision:

- **[GPUI](https://github.com/zed-industries/zed)** — Zed's GPU-accelerated UI
  framework (crates `gpui` + `gpui_platform`).
- **[gpui-component](https://github.com/longbridge/gpui-component)** (Longbridge,
  currently `0.5.x`) — the component library on top of GPUI: 60+ components,
  theming, dock layout, tables, charts, markdown. Production-proven (Longbridge Pro)
  and cross-platform.

This document designs the **smallest vertical slice**: a Cargo workspace and a
runnable hello-world window built with gpui-component. Its purpose is to (a) prove
the toolchain and dependencies work on every target OS, and (b) establish the repo
structure and dev workflow all later phases build on.

> API facts in this doc were verified against the upstream `main` branch
> (`examples/hello_world`, `website/docs/getting-started.md`, `README.md`, the
> upstream `flake.nix` files of both gpui-component and zed) and Context7
> (`/longbridge/gpui-component`) on 2026-08-28. See §11 References.

## 2. Goals / Non-goals

**Goals**

1. A Cargo **workspace** with one binary crate, structured for the multi-modular
   future (no restructuring churn later).
2. A windowed hello-world app built with gpui-component: text + a themed primary
   `Button` that responds to clicks.
3. Reproducible builds: committed `Cargo.lock`, pinned toolchain (via the flake's
   lockfile), and a **flake-based devshell** — no global Rust or language tools.
4. Standard Rust dev workflow: `cargo fmt`, `cargo clippy -D warnings`, documented
   build/run commands.
5. Documented environment prerequisites.

**Non-goals** (deferred to later docs, see docs/README.md roadmap)

- Module system, plugin architecture, real dashboard UI (0003).
- Window chrome / title bar / sizing / theming customization (0002).
- Settings & persistence (0004); packaging & installers (0005).
- Building the app *as a Nix package* (crane/buildRustPackage with vendored git
  deps) — Phase 0 needs only the devshell; a hermetic package build is revisited
  in 0005.
- Tests beyond "it compiles and runs" — GPUI test strategy is designed with the
  module architecture.

## 3. Key upstream facts this design relies on

These are the load-bearing API details (current as of verification date):

1. Entry point is **`gpui_platform::application()`** (not the older `App::new()`).
2. **`gpui_component::init(cx)` MUST be called first** inside `.run(...)` before any
   gpui-component feature is used.
3. **Every window's first-level child MUST be `Root`** — it hosts themes, dialogs,
   sheets, notifications and other overlay layers.
4. Recommended dependency set (upstream README / getting-started):
   ```toml
   gpui = { git = "https://github.com/zed-industries/zed" }
   gpui_platform = { git = "https://github.com/zed-industries/zed", features = ["font-kit"] }
   gpui-component = { git = "https://github.com/longbridge/gpui-component" }
   ```
   plus optional `gpui-component-assets` for bundled default assets (icons, fonts):
   ```toml
   gpui-component-assets = { git = "https://github.com/longbridge/gpui-component" }
   ```

   > **Amended 2026-09-06 (#114):** the snippets above reflect the original
   > bootstrap and no longer resolve — at upstream v0.6.0 (the "GPUI Kit"
   > rebrand) the gpui family is republished to crates.io as `gpui-pre-*`, and
   > `gpui-component-assets` was renamed `gpui-kit-assets`. The authoritative
   > dependency set is the root `Cargo.toml`.
   >
   > **Amended 2026-09-09 (#126):** the `gpui-pre-*` deps are exact-pinned
   > (`=0.3.3`) — each release is a snapshot of a different zed rev, so a
   > caret would let any lockfile regen silently change UI behavior
   > (upstream pins its own family with `=` too). Bumps are deliberate PRs
   > re-running the gates plus the #125 upstream-quirk checklist; the policy
   > comment lives at the pin site in `Cargo.toml`.
5. Current `Render` signature takes `(&mut self, &mut Window, &mut Context<Self>)`.
6. OS support: macOS **15+** (Xcode CLT), Windows **10+** (MSVC), Linux (X11 &
   Wayland). `gpui-component` is also published on crates.io (`"0.5"` line); we use
   the git dependency per upstream recommendation, see §6.
7. Both zed and gpui-component maintain **Nix flake devshells**
   (`github:NixOS/nixpkgs/nixos-unstable` + oxalica `rust-overlay`).
   gpui-component's package set — `wayland`, `vulkan-headers`/`vulkan-loader`,
   `libxcb`, `libxkbcommon`, `fontconfig`, glib/GTK stack, plus a hermetic
   `FONTCONFIG_FILE` and `LD_LIBRARY_PATH` — is the proven baseline our flake
   mirrors (§7.3).

## 4. Project structure

**Decision:** Cargo workspace from day one, even though Phase 0 has a single crate.
Rationale: the product is explicitly multi-modular; starting with a workspace avoids
a painful restructure at Phase 1+ and gives all future crates one shared `target/`
dir and one lockfile (which also pins the git revisions of gpui/gpui-component
consistently across crates).

**Decision:** the Rust toolchain and all dev tools live **only in the flake
devshell** (single source of truth: `flake.lock`). There is no `rust-toolchain.toml`
and no rustup on the dev machine.

```
my-dashboard/
├── Cargo.toml              # workspace manifest only (no [package])
├── Cargo.lock              # committed — pins gpui & gpui-component git revs
├── flake.nix               # Nix dev environment: toolchain + GUI/system libs
├── flake.lock              # committed — pins nixpkgs-unstable & rust-overlay
├── .envrc                  # direnv: `use flake`
├── .gitignore              # /target, .direnv
├── crates/
│   ├── app/                # bin crate: dashboard shell (this phase)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs
│   ├── core/               # (future 0003) module trait, registry, app state
│   └── modules/            # (future 0003+) one crate per dashboard module
└── docs/
```

**Naming:** binary crate `dashboard-app`, binary name **`dashboard`**. Future module
crates follow `dashboard-module-<name>`; shared code lives in `dashboard-core`.

## 5. Development environment (NixOS, flake-based)

**Decision:** all development tooling comes from the project's Nix flake dev
shell — **no rustup, no globally installed Rust or language-specific tools**.
Whatever we need, we add to the devshell. The primary dev machine runs NixOS on
nixpkgs-unstable; the flake uses the same channel.

**Committed to the repo (project side):**

- `flake.nix` — devshell with the Rust toolchain (rust-overlay `stable` profile:
  `rustc`, `cargo`, `clippy`, `rustfmt`, + `rust-src`), `rust-analyzer`,
  `pkg-config`, and every system library gpui links or loads on Linux
  (mirrors upstream's own flake; full listing in §7.3).
- `flake.lock` — committed; pins the exact nixpkgs-unstable and rust-overlay
  snapshots. Toolchain and system libs only move when we deliberately run
  `nix flake update`.
- `.envrc` (`use flake`) — direnv loads the devshell automatically when entering
  the directory.

**Host side (once, user-level NixOS/Home Manager config):**

- Nix with flakes enabled (default on modern NixOS).
- `direnv` + `nix-direnv` (cached, near-instant shell activation) —
  `programs.direnv.enable = true;` plus the nix-direnv integration; then
  `direnv allow` in the repo once.

**Notes:**

- **First build is heavy.** Two layers, each slow only once: (1) `nix` builds the
  devshell, (2) the first `cargo build` fetches the large GPUI dependency tree
  **including the two git repositories** (network required once). Afterwards:
  nix-direnv caches the shell, cargo builds incrementally.
- Windowing/runtime: the app runs on the active Wayland or X11 session; GPU
  drivers come from the host; fonts resolve through the devshell's hermetic
  `FONTCONFIG_FILE`.
- macOS / Windows remain *build targets* (0005). Developing from non-Nix machines
  can follow upstream's rustup-based instructions (§11) — out of scope here.

## 6. Dependency strategy

**Decision:** use **git dependencies exactly as upstream recommends**, with the
committed `Cargo.lock` providing reproducibility (Cargo records the resolved
commits in the lockfile).

```toml
# Workspace root Cargo.toml — [workspace.dependencies]
[workspace.dependencies]
anyhow = "1"
gpui = { git = "https://github.com/zed-industries/zed" }
gpui_platform = { git = "https://github.com/zed-industries/zed", features = ["font-kit"] }
gpui-component = { git = "https://github.com/longbridge/gpui-component" }
gpui-component-assets = { git = "https://github.com/longbridge/gpui-component" }
```

Rules (MUST):

- `gpui` and `gpui_platform` resolve from the **same zed commit** (same git source)
  — they are always upgraded together with `gpui-component`.
- `Cargo.lock` is **committed** (this is a binary project, not a library).
- The first `cargo build` inside the devshell downloads crates **and the two git
  repositories**; everything is cached afterwards.
- Upgrades happen deliberately: bump all three git deps in one change
  (`cargo update`), then re-run the acceptance checklist (§9).
- We include `gpui-component-assets` from the start so the default icon set and
  fonts are available; a dashboard will certainly need icons.

MAY, later: pin explicit `rev = "..."` for stricter control if drift becomes a
problem, or switch to crates.io (`gpui-component = "0.5"`) if/when its release
cadence makes that the better default.

## 7. Application design

### 7.1 Bootstrap sequence

The hello world follows upstream's canonical bootstrap:

1. `gpui_platform::application()` — platform init (fonts, windowing backend).
2. `.with_assets(gpui_component_assets::Assets)` — bundled default assets.
3. `gpui_component::init(cx)` — **first** call inside `.run()`.
4. `cx.spawn(...)` → `cx.open_window(WindowOptions::default(), ...)` — spawn is
   the upstream pattern; window creation happens in the async context.
5. Wrap our view in `Root::new(view, window, cx)` — required first-level child;
   we additionally set `.bg(cx.theme().background)` as upstream's example does.

`WindowOptions::default()` is intentionally kept for Phase 0 (identical to the
upstream reference example, minimizing deviation risk). Sizing, centering and
titlebar design are Phase 0002.

### 7.2 The view

A unit struct `HelloWorld` implementing `Render`: a centered vertical flex with
"Hello, World!" text and one primary `Button` that logs to stdout on click. The
`gpui_component::*` glob import provides layout helpers (`v_flex`) alongside
component exports.

### 7.3 Full listings

**`flake.nix`** — devshell (modeled on upstream gpui-component's own flake;
divergences noted below)

```nix
{
  description = "my-dashboard — universal personal dashboard (Rust + gpui-component)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Hermetic font config for the GUI (mirrors upstream).
        uiFont = pkgs.ibm-plex.override { families = [ "sans" ]; };
        fontsConf = pkgs.makeFontsConf { fontDirectories = [ uiFont ]; };

        # "default" profile = rustc, cargo, clippy, rustfmt;
        # rust-src added for rust-analyzer. Pinned by flake.lock.
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" ];
        };

        buildTools = with pkgs; [
          rustToolchain
          rust-analyzer
          pkg-config
        ];

        # Dynamic libraries gpui links/loads on Linux — mirrors upstream's flake.
        dynamicLibraries = with pkgs; [
          wayland
          vulkan-headers
          vulkan-loader
          libxcb
          libxkbcommon
          atk
          fontconfig
          glib
          gtk3
        ];
      in
      {
        devShells.default = with pkgs; mkShell {
          packages = buildTools ++ dynamicLibraries;
          env = {
            RUST_BACKTRACE = "1";
            FONTCONFIG_FILE = fontsConf;
            LD_LIBRARY_PATH = lib.makeLibraryPath dynamicLibraries;
          };
        };
      }
    );
}
```

Divergences from upstream's flake (deliberate): we use the `stable` toolchain
(upstream uses `beta` — switch to `rust-bin.beta.latest.default` if upstream's
MSRV ever requires it); we add `rust-analyzer`; we drop `gio-sharp` (C# GDK
bindings, not used by a Rust app) and `makeWrapper` (only needed for upstream's
wrapped *package* build, not a devshell).

**`.envrc`**

```bash
# Requires host-side direnv + nix-direnv.
# Run `direnv allow` once per clone; the devshell then loads automatically.
use flake
```

**`crates/app/Cargo.toml`**

```toml
[package]
name = "dashboard-app"
description = "Universal personal dashboard — application shell"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
publish = false

[[bin]]
name = "dashboard"
path = "src/main.rs"


> Historical excerpt: the binary was later renamed to `my-dashboard` when
> the installable flake landed (#147); `name = "dashboard"` above is the
> bootstrap-era value.
[dependencies]
anyhow.workspace = true
gpui.workspace = true
gpui_platform.workspace = true
gpui-component.workspace = true
gpui-component-assets.workspace = true
```

**`crates/app/src/main.rs`**

```rust
use gpui::*;
use gpui_component::{button::*, *};

pub struct HelloWorld;

impl Render for HelloWorld {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .v_flex()
            .gap_2()
            .size_full()
            .items_center()
            .justify_center()
            .child("Hello, World!")
            .child(
                Button::new("click-me")
                    .primary()
                    .label("Click me!")
                    .on_click(|_, _, _| println!("Clicked!")),
            )
    }
}

fn main() {
    gpui_platform::application()
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx| {
            // MUST be called before using any gpui-component features.
            gpui_component::init(cx);

            cx.spawn(async move |cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    let view = cx.new(|_| HelloWorld);
                    // The first level in every window MUST be a Root: it hosts
                    // theme, dialogs, sheets, notifications, overlays.
                    cx.new(|cx| {
                        Root::new(view, window, cx).bg(cx.theme().background)
                    })
                })
                .expect("Failed to open window");
            })
            .detach();
        });
}
```

**Workspace root `Cargo.toml`** (complete)

```toml
[workspace]
resolver = "3"
members = ["crates/app"]

[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "1.85"

[workspace.dependencies]
anyhow = "1"
gpui = { git = "https://github.com/zed-industries/zed" }
gpui_platform = { git = "https://github.com/zed-industries/zed", features = ["font-kit"] }
gpui-component = { git = "https://github.com/longbridge/gpui-component" }
gpui-component-assets = { git = "https://github.com/longbridge/gpui-component" }

# Keep debug info cheap in dev builds.
[profile.dev]
debug = "limited"

# Optimize hot-path crates even in dev builds — mirrors upstream gpui-component's
# own workspace tuning; makes dev builds render smoothly instead of stuttering.
[profile.dev.package]
gpui = { opt-level = 3 }
gpui_platform = { opt-level = 3 }
gpui_macros = { opt-level = 3 }
taffy = { opt-level = 3 }
rustybuzz = { opt-level = 3 }
ttf-parser = { opt-level = 3 }
resvg = { opt-level = 3 }
smol = { opt-level = 3 }
```

**`.gitignore`**

```gitignore
/target
/.direnv
```

## 8. Dev workflow

```bash
direnv allow                            # once per clone; devshell auto-loads after
# (explicit alternative: nix develop)

cargo run -p dashboard-app            # build & launch (debug)
cargo run -p dashboard-app --release  # smooth-animating release build
cargo fmt --all -- --check            # formatting gate
cargo clippy --workspace -- -D warnings  # lint gate
cargo doc --workspace --no-deps --open   # API docs
```

- All code MUST pass `fmt` and `clippy -D warnings` before merge.
- `cargo update` (Rust dependency refresh) is its own change + manual smoke test.
- `nix flake update` bumps the pinned nixpkgs-unstable + rust-overlay snapshots
  (i.e. the Rust toolchain and system libs) — treat it like `cargo update`:
  deliberate, one change, then re-run §9.
- Editors (VS Code, Helix, …) MUST resolve `rust-analyzer` from the direnv
  environment, not a global install.
- Optional: `sccache` MAY be added to the devshell later; not required.
- For AI-assisted development, upstream ships skills with component catalog and
  usage patterns: `npx skills add longbridge/gpui-component`.

## 9. Acceptance criteria

Phase 0 is done when, on each target OS we develop on:

- [ ] With **no global Rust on the machine**: `direnv allow` (or `nix develop`)
      yields a shell where `cargo`, `rustc`, `clippy`, `rustfmt` and
      `rust-analyzer` are on PATH.
- [ ] `nix flake check` evaluates the flake without errors.
- [ ] `cargo build --workspace` succeeds inside the devshell (first build may
      take several minutes).
- [ ] `cargo run -p dashboard-app` opens a window.
- [ ] The window shows "Hello, World!" and a primary "Click me!" button, centered,
      rendered with the default gpui-component theme, with correct fonts under
      the active Wayland/X11 session.
- [ ] Clicking the button prints `Clicked!` to the terminal.
- [ ] `cargo fmt --all -- --check` and `cargo clippy --workspace -- -D warnings`
      pass.
- [ ] `Cargo.lock` and `flake.lock` are committed; a fresh clone +
      `direnv allow` reproduces the full environment without surprises.
- [ ] Subsequent incremental builds complete in seconds, not minutes.

## 10. Risks & mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Upstream API churn (gpui is pre-1.0, moves fast with Zed) | Build breakage on update | Committed lockfile; update all 3 deps together as one reviewed change; follow upstream `CHANGELOG`/examples |
| Devshell lib set drifts from what gpui needs upstream | Linker/runtime errors on Linux | `flake.lock` pins everything; mirror upstream's flake package list (verified, §11); update libs together with git deps |
| Upstream devshell uses `beta` toolchain; we use `stable` | Occasional missing compiler features | One-line switch to `rust-bin.beta.latest.default` if required; watch upstream's MSRV |
| Long first build discourages experimentation | Dev friction | Set expectation (nix shell + cargo fetch, minutes each, once); incremental builds after; optional `sccache` |
| macOS 15+ floor, Windows 10+ floor | Excludes older OS users | Documented requirement; acceptable for personal tool; revisit at packaging phase (0005) |
| Dev-build runtime stutter | Misleading "slow framework" impression | `[profile.dev.package]` opt-level overrides from day one (§7.3) |
| Doc/example drift vs reality | Wasted debugging | Verified listings (§3); upstream `examples/hello_world` remains the canonical cross-check; Context7 MCP for up-to-date docs |
| Single-crate workspace looks over-engineered today | Mild | Accepted cost; avoids restructure at 0003 when `dashboard-core` + module crates land |

## 11. References

- gpui-component repo: <https://github.com/longbridge/gpui-component>
  - Getting started: `website/docs/getting-started.md`
  - Installation / system requirements: `website/docs/installation.md`
  - `Root` docs: `website/docs/root.md`
  - Reference example: `examples/hello_world` (`cargo run -p hello_world` upstream)
  - Upstream `flake.nix` (devshell this design mirrors): <https://github.com/longbridge/gpui-component/blob/main/flake.nix>
- GPUI (zed): <https://github.com/zed-industries/zed>
  - zed `flake.nix` (nixpkgs-unstable + rust-overlay reference): <https://github.com/zed-industries/zed/blob/main/flake.nix>
- oxalica rust-overlay: <https://github.com/oxalica/rust-overlay>
- nix-direnv: <https://github.com/nix-community/nix-direnv>
- Context7 library ID: `/longbridge/gpui-component` (used for verification)
