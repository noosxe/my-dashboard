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
