# my-dashboard

A universal personal desktop dashboard — a native GUI app built with Rust and
[gpui-component](https://github.com/longbridge/gpui-component) (the "GPUI Kit"
rebrand of the Zed UI stack). One window, multiple pluggable modules, rendered
entirely on the GPU.

**Modules today:** a GitHub module (repo PR/issue overviews, PR and issue
detail pages with comment editing, reactions, issue creation) and an Appearance
settings module with light/dark modes, per-mode palettes, and hot-reloading
user color themes.

**Design docs** live in [`docs/README.md`](docs/README.md) — numbered, merged
with the PR that ships the feature, and always current with behavior.

## Install (Nix)

Requires [Nix with flakes enabled](https://nixos.wiki/wiki/Flakes)
(`nix.settings.experimental-features = [ "nix-command flakes" ];`).

The flake exposes:

- `packages.<system>.default` / `packages.<system>.my-dashboard` — the app,
- `apps.<system>.default` — `nix run` entry,
- `overlays.default` — `pkgs.my-dashboard` overlay,
- `nixosModules.default` — `programs.my-dashboard.enable`,
- `homeManagerModules.default` — `programs.my-dashboard.enable`,
- `devShells.default` — pinned Rust toolchain + gpui's dynamic libs + AT-SPI
  for the debug bridge.

> The repo is private until its first public release: `github:` forms below
> work once it is public; until then, use a local clone path (`.` after
> `git clone`).

### Run without installing

```console
$ nix run github:noosxe/my-dashboard        # once public
$ nix run .#                                # from a clone
```

### NixOS — flake-input module (recommended)

```nix
# flake.nix
{
  inputs.my-dashboard.url = "github:noosxe/my-dashboard";
  # inputs.my-dashboard.inputs.nixpkgs.follows = "nixpkgs";  # optional

  outputs = { self, nixpkgs, my-dashboard, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        my-dashboard.nixosModules.default
      ];
    };
  };
}
```

```nix
# configuration.nix
programs.my-dashboard.enable = true;
# programs.my-dashboard.package = pkgs.my-dashboard;  # override the build
```

### home-manager — user-level module

Standalone:

```nix
# flake.nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    my-dashboard.url = "github:noosxe/my-dashboard";
  };
  outputs = { self, nixpkgs, home-manager, my-dashboard, ... }: {
    homeConfigurations."me" = home-manager.lib.homeManagerConfiguration {
      pkgs = nixpkgs.legacyPackages.x86_64-linux;
      modules = [
        ./home.nix
        my-dashboard.homeManagerModules.default
      ];
    };
  };
}
```

```nix
# home.nix
programs.my-dashboard.enable = true;
```

Integrated into a NixOS flake: pass `my-dashboard.homeManagerModules.default`
in your `home-manager.nixosModules.home-manager` `modules` list and enable it
in the user's `home.nix` — same option name.

### Themes

Every upstream gpui-component theme — Catppuccin Latte, Frappé, Macchiato
and Mocha among them — ships built into the app as a preset (docs/0074);
pick one under Appearance. No Nix option is needed. To add your own
palettes, drop ThemeSet JSON files into `~/.config/my-dashboard/themes/`
(docs/0025): the running app hot-reloads them, and a file whose theme
name matches a preset shadows it.

The former `programs.my-dashboard.catppuccin.{enable,flavors}` options
are removed; setting them fails evaluation with a pointer to the presets.

### Overlay (any setup)

```nix
nixpkgs.overlays = [
  my-dashboard.overlays.default   # adds pkgs.my-dashboard
];
environment.systemPackages = [ pkgs.my-dashboard ];   # or home.packages
```

### From a clone, without modules

```console
$ nix build .#          # ./result/bin/my-dashboard
$ nix profile install .#   # if that's your thing
```

The build compiles the whole Rust workspace from source (gpui is heavy; the
first build takes a while, then the store cache takes over). Fonts follow your
**system fontconfig** (`fonts.packages` on NixOS) — the app doesn't pin its
own. The package ships a desktop entry
(`share/applications/com.noosxe.my-dashboard.desktop`, named after the app's
Wayland app_id so docks match the window) and a scalable icon in the hicolor
theme, so every install path — NixOS/home-manager modules, overlay,
`nix profile` — puts "My Dashboard" in your launcher.

## Development

```console
$ nix develop            # pinned toolchain (rustfmt, clippy, rust-analyzer),
$ cargo run              #   gpui's dynamic libs, AT-SPI daemons on PATH
```

Bare root commands target the app (`default-members`): `cargo run`
launches `my-dashboard`; reach the other members explicitly with `-p`,
and the gates below always spell `--workspace`, which overrides the
default.

The dev shell pins an IBM Plex fontconfig for hermetic rendering; the dev
shell's AT-SPI setup powers the in-process debug bridge
([docs/0010](docs/0010-agent-debug-bridge.md)) — `cargo run -p dashboard-debug` to
drive the running app.
On macOS the same dev shell evaluates (`nix develop` gives the pinned
toolchain); the GUI runtime above — gpui's dylibs, AT-SPI, hermetic fonts —
is Linux-only (the macOS backend is Cocoa/Metal and needs none of it).

The debug bridge works on macOS in attach mode without that stack
(`logs`, `key`/`type`/`mouse`, `doctor`); tree-backed verbs (`tree`,
`wait-frame`, `click`/`focus <id>`) are gated there with a console
error until the platform populates its a11y tree upstream
([#401](https://github.com/noosxe/my-dashboard/issues/401)) —
`DASHBOARD_DEBUG_A11Y=1` bypasses the gates.

One macOS wrinkle since the gpui-pre 0.3.7 pin (#363): the stack
compiles its Metal shaders **at build time**, which needs (a) Xcode's
downloadable Metal Toolchain —
`/Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild -downloadComponent MetalToolchain`,
run once per Xcode install (the dev shell's `xcodebuild` is a nix shim
that can't; the dev shell warns at entry if it's missing) — and (b)
Apple's `xcrun` for the `metal` tool, because the dev shell's 2019
`xcbuild` xcrun shim cannot resolve the downloaded toolchain. The flake
dev shell handles (b) automatically on macOS (works under `nix develop`
and direnv's `use flake` alike): its shellHook installs
`scripts/xcrun-metal-shadow.sh` as the first `PATH` entry — a wrapper
that routes `metal`/`metallib` to Apple's `xcrun` (`DEVELOPER_DIR` →
Xcode.app, override via `XCODE_DEVELOPER_DIR`) and everything else to the
stock shim, since SDK resolution for the nix cc/ld wrappers must stay
there (nix's ld cannot parse Xcode 27's `.tbd` format). Outside the dev
shell, the same script works standalone:
`PATH="$(dirname scripts/xcrun-metal-shadow.sh):$PATH" cargo build`.

macOS OS-notification delivery (docs/0060) additionally requires a trusted
bundle — plain `cargo run` posts are silently dropped by macOS.
`./scripts/macos-app.sh` builds the release binary, wraps it in
`My Dashboard.app`, and installs it into `~/Applications`; it signs with
your Apple Development identity by default (auto-discovered from the
keychain) so rebuilds don't re-prompt for notification authorization
(docs/0061). `./scripts/macos-app.sh --dev` does the same for the debug
binary as `My Dashboard Dev.app` with its own bundle id.

Raw `cargo run` / `cargo test` binaries live outside that bundle, so every
rebuild changed the binary's code-signing identity and the GitHub
credential's Keychain grant ("Always Allow", docs/0011 §4.1) never
stuck — macOS re-prompted for the login password on every rebuild
(#473). `.cargo/config.toml` now routes macOS `cargo run`/`test`/
`bench` through `.cargo/sign-and-run` (cargo's documented `runner`
hook), which signs the fresh binary with the same Apple Development
identity (`MY_DASHBOARD_CODESIGN_IDENTITY` wins, else the first
"Apple Development" identity — the same discovery as
`scripts/macos-app.sh`) before executing it: allow it once, then
silence. Off macOS, without any codesigning identity, or for bare
`cargo build` (cargo has no post-build hook), the wrapper is a
pass-through and nothing changes; bridge sessions spawn the raw
binary, so sign it once via any `cargo run` and the grant persists
across rebuilds.

Gates before pushing (see [AGENTS.md](AGENTS.md)):
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets` (zero
warnings), `cargo test --workspace`.

Non-Nix: a stock Rust 1.85+ toolchain plus the usual Linux GUI dev packages
(wayland, vulkan, xcb, xkbcommon, atk, fontconfig, glib/gtk3) works — the
flake's `dynamicLibraries` list is the exact set.

## License

[MIT](LICENSE)
