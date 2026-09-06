# User-level module (issue #147): the natural fit for a per-user GUI
# app — installs the package into home.packages. The app manages its own
# state under `~/.config/my-dashboard/` (docs/0004) and
# `~/.config/my-dashboard/themes/` (docs/0025); the module adds nothing
# there on purpose.
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.my-dashboard;
in
{
  options.programs.my-dashboard = {
    enable = lib.mkEnableOption "my-dashboard, the universal personal dashboard app";

    package = lib.mkPackageOption self.packages.${pkgs.system} "default" { };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];
  };
}
