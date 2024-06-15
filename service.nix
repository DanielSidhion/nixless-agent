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
      updatePublicKey = lib.mkOption {
        description = ''
          The public key to use when verifying requests made to update the system.
        '';
        type = lib.types.str;
      };
      maxSystemHistoryCount = lib.mkOption {
        description = ''
          How many configurations the agent will keep in the machine (for rollbacks, for example).
        '';
        type = lib.types.ints.positive;
        default = 3;
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

      users.users = lib.optionalAttrs (cfg.user == "nixless-agent") {
        nixless-agent = {
          group = cfg.group;
          isSystemUser = true;
        };
      };

      users.groups = lib.optionalAttrs (cfg.group == "nixless-agent") {
        nixless-agent.members = [ cfg.user ];
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
          NIXLESS_AGENT_UPDATE_PUBLIC_KEY = cfg.updatePublicKey;
          NIXLESS_MAX_SYSTEM_HISTORY_COUNT = builtins.toString cfg.maxSystemHistoryCount;
          RUST_BACKTRACE = "full";
        };

        serviceConfig = {
          ExecStart = lib.getExe cfg.package;
          CapabilityBoundingSet = "CAP_SYS_ADMIN CAP_CHOWN CAP_SETPCAP CAP_FOWNER";
          AmbientCapabilities = "CAP_SYS_ADMIN CAP_CHOWN CAP_SETPCAP CAP_FOWNER";
          StateDirectory = "nixless-agent";
          DynamicUser = false;
          User = cfg.user;
          Group = cfg.group;
          ProtectHome = true;
          ProtectHostname = true;
          ProtectKernelLogs = true;
          ProtectKernelModules = true;
          ProtectKernelTunables = true;
          ProtectProc = "default"; # Required so nixless-agent can check whether the nix daemon is running.
          ProcSubset = "pid";
          ProtectSystem = "strict";
          ReadWritePaths = "/nix";
          # Restart = "on-failure";
          Restart = "no";
          RestartSec = 10;
          RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ]; # AF_UNIX is used by D-Bus.
          RestrictNamespaces = "mnt";
          RestrictRealtime = true;
          RestrictSUIDSGID = true;
        };
      };
    };
}
