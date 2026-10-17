# `programs.tinyclient` for Home Manager. flake.nix passes itself in, for the default package.
self: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.programs.tinyclient;
in {
  options.programs.tinyclient = {
    enable = lib.mkEnableOption "tinyclient, Slack, Discord, Telegram, Teams, Signal and Messages in one small native window";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "tinyclient.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      example = lib.literalExpression ''
        tinyclient.packages.''${pkgs.stdenv.hostPlatform.system}.default.override {
          telegramApiId = "12345";
          telegramApiHash = "0123456789abcdef0123456789abcdef";
        }
      '';
      description = "The tinyclient to install. Telegram needs a build with your own app id.";
    };

    systemd.enable = lib.mkEnableOption ''
      the `tinyclient-chatd` user service. systemd then starts chatd with the graphical session and
      keeps it receiving after the window quits; the window asks systemd for it instead of starting
      its own. The service needs the session's display variables (`WAYLAND_DISPLAY` or `DISPLAY`)
      in the systemd user manager, for sign-in windows'';
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [
    {
      assertions = [(lib.hm.assertions.assertPlatform "programs.tinyclient" pkgs lib.platforms.linux)];
      home.packages = [cfg.package];
    }

    # packaging/linux/tinyclient-chatd.service, with chatd from the package. The [Install] section
    # is what makes `systemctl --user is-enabled` say "enabled", which the window checks for.
    (lib.mkIf cfg.systemd.enable {
      systemd.user.services.tinyclient-chatd = {
        Unit = {
          Description = "tinyclient background chat service";
          Documentation = "https://github.com/itz4blitz/tinyclient";
          PartOf = ["graphical-session.target"];
          After = ["graphical-session-pre.target"];
          StartLimitIntervalSec = 30;
          StartLimitBurst = 3;
        };
        Service = {
          Type = "exec";
          ExecStart = "${cfg.package}/lib/tinyclient/chatd";
          Restart = "on-failure";
          RestartSec = 2;
          StandardOutput = "journal";
          StandardError = "journal";
        };
        Install.WantedBy = ["graphical-session.target"];
      };
    })
  ]);
}
