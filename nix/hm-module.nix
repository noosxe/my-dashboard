# User-level module (issue #147): the natural fit for a per-user GUI
# app — installs the package into home.packages. The app manages its own
# state under `~/.config/my-dashboard/` (docs/0004) and
# `~/.config/my-dashboard/themes/` (docs/0025); the module adds nothing
# there on purpose. Catppuccin (and every other gpui-component theme)
# ships built into the app as presets (docs/0074).
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.my-dashboard;
  catppuccinRemoved = ''
    Catppuccin Latte, Frappé, Macchiato and Mocha now ship built into
    my-dashboard as presets (docs/0074) — pick them in Appearance.
  '';
in
{
  imports = [
    (lib.mkRemovedOptionModule [
      "programs"
      "my-dashboard"
      "catppuccin"
      "enable"
    ] catppuccinRemoved)
    (lib.mkRemovedOptionModule [
      "programs"
      "my-dashboard"
      "catppuccin"
      "flavors"
    ] catppuccinRemoved)
  ];

  options.programs.my-dashboard = {
    enable = lib.mkEnableOption "my-dashboard, the universal personal dashboard app";

    package = lib.mkPackageOption self.packages.${pkgs.stdenv.hostPlatform.system} "default" { };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];
  };
}
