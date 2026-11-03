# User-level module (issue #147): the natural fit for a per-user GUI
# app — installs the package into home.packages. The app manages its own
# state under `~/.config/my-dashboard/` (docs/0004) and
# `~/.config/my-dashboard/themes/` (docs/0025); the module adds nothing
# there on purpose — except the opt-in Whiskers-generated Catppuccin
# themes (#143), which are linked file-by-file so hand-dropped user
# themes coexist in the same directory.
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.my-dashboard;
  allFlavors = [
    "latte"
    "frappe"
    "macchiato"
    "mocha"
  ];
  themeLink = flavor: {
    name = "my-dashboard/themes/catppuccin-${flavor}.json";
    value.source = "${
      self.packages.${pkgs.stdenv.hostPlatform.system}.my-dashboard-catppuccin-themes
    }/catppuccin-${flavor}.json";
  };
in
{
  options.programs.my-dashboard = {
    enable = lib.mkEnableOption "my-dashboard, the universal personal dashboard app";

    package = lib.mkPackageOption self.packages.${pkgs.stdenv.hostPlatform.system} "default" { };

    catppuccin = {
      enable = lib.mkEnableOption ''
        Catppuccin color themes for my-dashboard — Whiskers-generated
        ThemeSet files linked into the app's user themes dir, where the
        running app hot-reloads them (docs/0025). Independent of
        `programs.my-dashboard.enable`, so it also works when the app
        binary is managed elsewhere.
      '';

      flavors = lib.mkOption {
        type = lib.types.listOf (lib.types.enum allFlavors);
        default = allFlavors;
        defaultText = "all four flavors";
        description = ''
          Which Catppuccin flavors to install. Each becomes one theme in
          the app's Appearance → palettes: Latte is light, the rest are
          dark.
        '';
      };
    };
  };

  config = lib.mkMerge [
    (lib.mkIf cfg.enable {
      home.packages = [ cfg.package ];
    })
    (lib.mkIf cfg.catppuccin.enable {
      xdg.configFile = builtins.listToAttrs (map themeLink cfg.catppuccin.flavors);
    })
  ];
}
