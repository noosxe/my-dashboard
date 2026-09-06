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

Gates before pushing (see [AGENTS.md](AGENTS.md)):
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets` (zero
warnings), `cargo test --workspace`.

Non-Nix: a stock Rust 1.85+ toolchain plus the usual Linux GUI dev packages
(wayland, vulkan, xcb, xkbcommon, atk, fontconfig, glib/gtk3) works — the
flake's `dynamicLibraries` list is the exact set.

## License

[MIT](LICENSE)
