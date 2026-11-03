# Thin system-level module (issue #147): installs the package into the
# system profile. my-dashboard is a per-user GUI app — no service, no
# sockets, nothing to configure at the system level.
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

    package = lib.mkPackageOption self.packages.${pkgs.stdenv.hostPlatform.system} "default" { };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
  };
}
