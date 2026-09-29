{
  description = "my-dashboard — universal personal dashboard (Rust + gpui-component)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
    home-manager = {
      url = "github:nix-community/home-manager";
      # Track the same nixpkgs as this flake (issue #147): one lock step
      # for everything the modules consume.
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      flake-utils,
      home-manager,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        inherit (pkgs) lib;

        isLinux = pkgs.stdenv.hostPlatform.isLinux;
        isDarwin = pkgs.stdenv.hostPlatform.isDarwin;

        # Hermetic font config for the GUI in the *dev shell* (mirrors
        # upstream). The installed package deliberately does NOT pin
        # fonts: it uses the system fontconfig so user font settings and
        # NixOS `fonts.packages` are respected (#147, owner decision).
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
        # The macOS backend (Cocoa/Metal) needs none of these, and wayland et al.
        # refuse to evaluate on aarch64-darwin, so only pull them on Linux.
        dynamicLibraries = with pkgs; lib.optionals isLinux [
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

        # Whiskers template generating the Catppuccin ThemeSet files (#143).
        catppuccinTemplate = ./nix/catppuccin.tera;

        # AT-SPI daemons for the debug bridge (docs/0010 D5): provides
        # at-spi-bus-launcher/registryd and the DBus service files so
        # org.a11y.Bus can be activated from inside a dev shell without a
        # system-wide services.gnome.at-spi2.enable.
        atSpi = with pkgs; at-spi2-core;
        # Live debug sessions (docs/0055 D2/D10 §5): headless compositor +
        # screenshots. No GPU, no real input stack needed.
        liveDebug = with pkgs; [ sway grim ];

        # Workspace version stays the single source of truth.
        version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

        # The installable app (issue #147): source build on the same
        # pinned toolchain family, linked against the gpui dynamic libs,
        # wrapped with a runtime LD_LIBRARY_PATH. System fontconfig — no
        # FONTCONFIG_FILE here on purpose. `doCheck` stays off: the test
        # gates (cargo fmt/clippy/test) run in the dev shell/CI, and the
        # Nix sandbox lacks the display/runtime-dir the GUI harness wants.
        my-dashboard = pkgs.rustPlatform.buildRustPackage {
          pname = "my-dashboard";
          inherit version;

          src = lib.cleanSource ./.;
          cargoLock = {
            lockFile = ./Cargo.lock;
            outputHashes = {
              "gpui-base-0.7.0" = "sha256-Ii3wGy0gLfT2PiSkrqNWPCT2SdSAz5CruC46UrW/55M=";
              "gpui-component-0.7.0" = "sha256-Ii3wGy0gLfT2PiSkrqNWPCT2SdSAz5CruC46UrW/55M=";
              "gpui-component-macros-0.7.0" = "sha256-Ii3wGy0gLfT2PiSkrqNWPCT2SdSAz5CruC46UrW/55M=";
              "gpui-kit-assets-0.7.0" = "sha256-Ii3wGy0gLfT2PiSkrqNWPCT2SdSAz5CruC46UrW/55M=";
            };
          };

          nativeBuildInputs = with pkgs; [
            pkg-config
            makeWrapper
          ];
          buildInputs = dynamicLibraries;

          postInstall = ''
            wrapProgram $out/bin/my-dashboard \
              --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath dynamicLibraries}
          '';

          meta = with lib; {
            description = "Universal personal dashboard (Rust + gpui-component)";
            license = licenses.mit;
            mainProgram = "my-dashboard";
            platforms = platforms.linux;
          };
        };

        # Catppuccin theme files for the user themes dir (docs/0025),
        # generated with Whiskers from in-repo nix/catppuccin.tera — one
        # ThemeSet JSON per flavor (#143). Hermetic: whiskers has the
        # palette built in, the template is a flake source file. The jq
        # gates mirror the app's degrade contract: nothing half-valid
        # ships — the build fails instead.
        my-dashboard-catppuccin-themes =
          pkgs.runCommand "my-dashboard-catppuccin-themes"
            {
              nativeBuildInputs = with pkgs; [
                catppuccin-whiskers
                jq
              ];
              meta = with lib; {
                description = "Catppuccin ThemeSet files for my-dashboard (Whiskers-generated)";
                license = licenses.mit;
                platforms = platforms.linux;
              };
            }
            ''
              mkdir -p $out work
              cd work
              whiskers ${catppuccinTemplate}
              for f in themes/catppuccin-*.json; do
                jq -e '.themes[0].name | startswith("Catppuccin ")' "$f" > /dev/null
                jq -e '.themes[0].mode == "dark" or .themes[0].mode == "light"' "$f" > /dev/null
                jq -e '.themes[0].colors | length == 100' "$f" > /dev/null
              done
              test "$(ls themes/catppuccin-*.json | wc -l)" -eq 4
              cp themes/catppuccin-*.json $out/
            '';
      in
      {
        packages.default = my-dashboard;
        packages.my-dashboard = my-dashboard;
        packages.my-dashboard-catppuccin-themes = my-dashboard-catppuccin-themes;

        apps.default = {
          type = "app";
          program = "${my-dashboard}/bin/my-dashboard";
          meta = my-dashboard.meta;
        };

        checks.my-dashboard-catppuccin-themes = my-dashboard-catppuccin-themes;

        devShells.default =
          with pkgs;
          mkShell {
            packages =
              buildTools
              ++ dynamicLibraries
              ++ lib.optionals isLinux ([ atSpi ] ++ liveDebug);
            env = {
              RUST_BACKTRACE = "1";
            }
            # The Linux GUI runtime: hermetic fonts, gpui dylibs and the
            # a11y bridge. macOS has no equivalent (Cocoa/Metal), so
            # keep it out of the darwin shell entirely.
            // lib.optionalAttrs isLinux {
              FONTCONFIG_FILE = fontsConf;
              LD_LIBRARY_PATH = lib.makeLibraryPath dynamicLibraries;
              # Lets a session bus started *inside* the shell DBus-activate
              # org.a11y.Bus. The already-running session bus scans service
              # dirs only at its own startup, so the normal dev flow is:
              #   at-spi-bus-launcher --launch-immediately &
              #   cargo run
              # (the launcher is on PATH via the shellHook below).
              XDG_DATA_DIRS = lib.concatStringsSep ":" (
                [
                  "${atSpi}/share"
                ]
                ++ lib.optionals (builtins.getEnv "XDG_DATA_DIRS" != "") [
                  (builtins.getEnv "XDG_DATA_DIRS")
                ]
              );
            };
            # at-spi2-core ships its daemons in libexec/, not bin/.
            shellHook =
              lib.optionalString isLinux ''
                export PATH="${atSpi}/libexec:$PATH"
              ''
              # gpui-pre >= 0.3.7 compiles Metal shaders at build time
              # (README "Development"): shadow xcrun with the delegating
              # wrapper so `metal`/`metallib` resolve Xcode's downloadable
              # Metal Toolchain while SDK queries stay on the stock shim
              # (nix ld can't parse Xcode 27 .tbd files, so SDK resolution
              # must not move).
              + lib.optionalString isDarwin ''
                # Stable per-user dir (direnv/nix-direnv evaluates this
                # hook ONCE and caches the env: a per-session $TMPDIR
                # install would leave a dead PATH entry behind).
                shadow="''${XDG_CACHE_HOME:-$HOME/.cache}/my-dashboard-xcrun-shadow"
                mkdir -p "$shadow"
                install -m 0755 ${./scripts/xcrun-metal-shadow.sh} "$shadow/xcrun"
                export PATH="$shadow:$PATH"
                # The Metal Toolchain is a per-Xcode-install download; fail
                # loudly at shell entry instead of deep inside a build.
                if env DEVELOPER_DIR="''${XCODE_DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}" \
                    /usr/bin/xcrun metal --version 2>&1 | grep -q 'missing Metal Toolchain'; then
                  echo "warning: Xcode's Metal Toolchain is missing — Metal-shader builds will fail. Fix once per Xcode install:" >&2
                  echo "  /Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild -downloadComponent MetalToolchain" >&2
                fi
              '';
          };
      }
    )
    // {
      # final.system: nixpkgs package sets expose the host system.
      overlays.default = final: _: {
        my-dashboard = self.packages.${final.system}.default;
      };

      nixosModules.default = import ./nix/nixos-module.nix self;
      homeManagerModules.default = import ./nix/hm-module.nix self;
    };
}
