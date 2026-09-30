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

### Catppuccin themes (home-manager)

Whiskers-generated Catppuccin palettes (Latte, Frappé, Macchiato, Mocha),
built by the flake and linked into the app's user themes dir — the running
app hot-reloads them, no restart needed. Themes land in the Appearance
pickers (Latte is a light palette, the rest are dark).

```nix
programs.my-dashboard.catppuccin.enable = true;
programs.my-dashboard.catppuccin.flavors = [ "latte" "frappe" "macchiato" "mocha" ];  # default: all
```

Works with `programs.my-dashboard.enable` or standalone (binary managed
elsewhere). Hand-dropped theme files in `~/.config/my-dashboard/themes/`
coexist — don't let home-manager manage that directory as a whole.

The generated files are also exposed for non-HM setups:
`packages.<system>.my-dashboard-catppuccin-themes` (a directory with the
four ThemeSet JSON files).

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
own. No desktop entry is installed; launch `my-dashboard` from a terminal or
your session autostart.

## Development

```console
$ nix develop            # pinned toolchain (rustfmt, clippy, rust-analyzer),
$ cargo run              #   gpui's dynamic libs, AT-SPI daemons on PATH
```

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
`My Dashboard.app` (ad-hoc signed), and installs it into `~/Applications`;
`SIGN_IDENTITY=<id>` re-signs with a stable identity so rebuilds don't
re-prompt for notification authorization.

Gates before pushing (see [AGENTS.md](AGENTS.md)):
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets` (zero
warnings), `cargo test --workspace`.

Non-Nix: a stock Rust 1.85+ toolchain plus the usual Linux GUI dev packages
(wayland, vulkan, xcb, xkbcommon, atk, fontconfig, glib/gtk3) works — the
flake's `dynamicLibraries` list is the exact set.

**Catppuccin themes without Nix:** install
[Whiskers](https://github.com/catppuccin/whiskers) (`cargo install
catppuccin-whiskers`), then from a clone:

```console
$ whiskers nix/catppuccin.tera        # renders catppuccin-{latte,frappe,macchiato,mocha}.json
$ cp themes/catppuccin-mocha.json ~/.config/my-dashboard/themes/
```

They appear in the Appearance pickers immediately (hot reload).

## License

[MIT](LICENSE)
