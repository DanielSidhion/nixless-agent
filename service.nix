{ nixless-agent, system-switch-tracker }:
{ lib, config, ... }:
let
  cfg = config.services.nixless-agent;
in
{
  options = {
    services.nixless-agent = {
      enable = lib.mkOption {
        description = ''
          Whether to enable nixless-agent.
        '';
        type = lib.types.bool;
        default = false;
      };
      package = lib.mkOption {
        description = ''
          The package to use.
        '';
        type = lib.types.package;
        default = nixless-agent;
      };
      user = lib.mkOption {
        description = ''
          The user under which nixless-agent runs.
        '';
        type = lib.types.str;
        default = "nixless-agent";
      };
      group = lib.mkOption {
        description = ''
          The group under which attic runs.
        '';
        type = lib.types.str;
        default = "nixless-agent";
      };
      port = lib.mkOption {
        description = ''
          The port on which nixless-agent will listen for requests.
        '';
        type = lib.types.port;
        default = 45567;
      };
      cacheUrl = lib.mkOption {
        description = ''
          The URL of the binary cache to use when downloading a system configuration.
        '';
        type = lib.types.str;
      };
      cachePublicKey = lib.mkOption {
        description = ''
          The public key of the binary cache.
        '';
        type = lib.types.str;
      };
    };
  };

  config = lib.mkIf (cfg.enable)
    {
      assertions = [ ];

      security.polkit = {
        enable = true;
        extraConfig = ''
          polkit.addRule(function(action, subject) {
            if (action.id == "org.freedesktop.systemd1.manage-units" && subject.user == "nixless-agent") {
              if (action.lookup("unit") === undefined && action.lookup("verb") === undefined) {
                return polkit.Result.YES;
              }
            }
          });
        '';
      };

      systemd.services.nixless-agent = {
        wantedBy = [ "multi-user.target" ];
        after = [ "network.target" ];

        environment = {
          NIXLESS_AGENT_LISTEN_PORT = builtins.toString cfg.port;
          NIXLESS_AGENT_TEMP_DOWNLOAD_PATH = "/var/lib/nixless-agent/downloads";
          NIXLESS_AGENT_CACHE_URL = cfg.cacheUrl;
          NIXLESS_AGENT_ABSOLUTE_ACTIVATION_TRACKER_COMMAND = lib.getExe system-switch-tracker;
          NIXLESS_AGENT_CACHE_PUBLIC_KEY = cfg.cachePublicKey;
          RUST_BACKTRACE = "full";
        };

        serviceConfig = {
          ExecStart = lib.getExe cfg.package;
          CapabilityBoundingSet = "CAP_SYS_ADMIN CAP_CHOWN CAP_SETPCAP CAP_FOWNER";
          AmbientCapabilities = "CAP_SYS_ADMIN CAP_CHOWN CAP_SETPCAP CAP_FOWNER";
          StateDirectory = "nixless-agent";
          DynamicUser = true;
          User = cfg.user;
          Group = cfg.group;
          ProtectHome = true;
          ProtectHostname = true;
          ProtectKernelLogs = true;
          ProtectKernelModules = true;
          ProtectKernelTunables = true;
          ProtectProc = "default"; # Required so nixless-agent can check whether the nix daemon is running.
          ProtectSystem = "strict";
          ReadWritePaths = "/nix";
          Restart = "on-failure";
          RestartSec = 10;
          RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ]; # AF_UNIX is used by D-Bus.
          RestrictNamespaces = "mnt";
          RestrictRealtime = true;
          RestrictSUIDSGID = true;
        };
      };
    };
}
