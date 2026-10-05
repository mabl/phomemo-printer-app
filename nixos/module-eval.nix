# The NixOS module's assertions, warnings and option types, from evaluating
# configurations: what the VM test leaves out. Fails listing every
# expectation that does not hold.
{
  lib,
  pkgs,
  module,
}:

let
  # The configuration with the service and `config`.
  eval =
    config:
    (lib.nixosSystem {
      modules = [
        module
        {
          nixpkgs.pkgs = pkgs;
          boot.loader.grub.enable = false;
          fileSystems."/" = {
            device = "none";
            fsType = "tmpfs";
          };
          system.stateVersion = lib.trivial.release;
          services.phomemo-printer-app.enable = true;
        }
        config
      ];
    }).config;

  # The service's settings; `{ }` for the defaults.
  evalService = settings: eval { services.phomemo-printer-app = settings; };

  failedAssertions = config: map (a: a.message) (lib.filter (a: !a.assertion) config.assertions);
  service = config: config.systemd.services.phomemo-printer-app;
  mentions = text: lib.any (lib.hasInfix text);

  # Whether the service's settings pass their options' types.
  accepted =
    settings:
    let
      cfg = (evalService settings).services.phomemo-printer-app;
    in
    (builtins.tryEval (builtins.deepSeq (lib.mapAttrs (name: _: cfg.${name}) settings) true)).success;

  defaults = evalService { };
  remote = evalService {
    listenHostname = "*";
    openFirewall = true;
  };
  root = evalService { runAsRoot = true; };
  rootWithCups = eval {
    services.phomemo-printer-app.runAsRoot = true;
    services.printing.enable = true;
  };

  expectations = {
    "the defaults evaluate cleanly" = failedAssertions defaults == [ ] && defaults.warnings == [ ];
    "the defaults set the settings" =
      lib.getAttrs [
        "PHOMEMO_SERVER_PORT"
        "PHOMEMO_LISTEN_HOSTNAME"
        "PHOMEMO_LOG_FILE"
        "PHOMEMO_LOG_LEVEL"
        "PHOMEMO_TLS_ONLY"
        "PHOMEMO_BT_CHANNELS"
      ] (service defaults).environment == {
        PHOMEMO_SERVER_PORT = "8000";
        PHOMEMO_LISTEN_HOSTNAME = "localhost";
        PHOMEMO_LOG_FILE = "-";
        PHOMEMO_LOG_LEVEL = "info";
        PHOMEMO_TLS_ONLY = "0";
        PHOMEMO_BT_CHANNELS = "1";
      };
    "the defaults leave the unset settings out" =
      !(service defaults).environment ? PHOMEMO_AUTH_SERVICE
      && !(service defaults).environment ? PHOMEMO_ADMIN_GROUP;
    "the defaults read no environment file" = (service defaults).serviceConfig.EnvironmentFile == "";
    "the defaults open no port" = defaults.networking.firewall.allowedTCPPorts == [ ];
    "the defaults enable BlueZ" = defaults.hardware.bluetooth.enable;
    "the defaults declare no PAM service" = !defaults.security.pam.services ? phomemo-printer-app;
    "the defaults add no capability" = !(service defaults).serviceConfig ? AmbientCapabilities;
    "the defaults leave /etc/cups alone" = !defaults.environment.etc ? cups;

    "a remote listener checks logins with the module's PAM service" =
      (service remote).environment.PHOMEMO_AUTH_SERVICE == "phomemo-printer-app"
      && remote.security.pam.services ? phomemo-printer-app;
    "a remote listener opens the port" = remote.networking.firewall.allowedTCPPorts == [ 8000 ];
    "pam_unix logins without root warn" = mentions "runAsRoot" remote.warnings;
    "pam_unix logins without root warn on localhost too" =
      mentions "runAsRoot"
        (evalService { authService = "login"; }).warnings;
    "logins without pam_unix do not warn" =
      (eval {
        services.phomemo-printer-app.authService = "phomemo-printer-app";
        security.pam.services.phomemo-printer-app.unixAuth = false;
      }).warnings == [ ];
    "a remote listener without an auth service fails" = mentions "authService" (
      failedAssertions (evalService {
        listenHostname = "*";
        authService = null;
      })
    );
    "openFirewall on localhost warns, opening nothing" =
      let
        config = evalService { openFirewall = true; };
      in
      mentions "openFirewall" config.warnings && config.networking.firewall.allowedTCPPorts == [ ];

    "the environment takes other variables" =
      let
        config = evalService {
          environment = {
            PHOMEMO_SPOOL_DIRECTORY = "/var/lib/phomemo-printer-app/jobs";
          };
        };
      in
      failedAssertions config == [ ]
      && (service config).environment.PHOMEMO_SPOOL_DIRECTORY == "/var/lib/phomemo-printer-app/jobs";
    "the environment refuses an option's variable" = mentions "PHOMEMO_SERVER_PORT" (
      failedAssertions (evalService {
        environment.PHOMEMO_SERVER_PORT = "9000";
      })
    );
    "the environment refuses an unset option's variable" = mentions "PHOMEMO_AUTH_SERVICE" (
      failedAssertions (evalService {
        environment.PHOMEMO_AUTH_SERVICE = "login";
      })
    );

    "a port below 1024 adds the capability" =
      (service (evalService {
        port = 631;
      })).serviceConfig.AmbientCapabilities == [
        "CAP_NET_BIND_SERVICE"
      ];

    "runAsRoot installs the drop-in" = lib.any (
      package: package.name == "phomemo-printer-app-root-drop-in"
    ) root.systemd.packages;
    "runAsRoot provides /etc/cups" =
      root.environment.etc ? cups && "${root.environment.etc.cups.source}" == "${pkgs.emptyDirectory}";
    "runAsRoot leaves CUPS its /etc/cups" =
      failedAssertions rootWithCups == [ ] && rootWithCups.environment.etc.cups.source == "/var/lib/cups";

    "empty strings are refused" =
      !accepted { listenHostname = ""; }
      && !accepted { authService = ""; }
      && !accepted { adminGroup = ""; };
    "log files directly in the state directory are accepted" =
      accepted { logFile = "/var/lib/phomemo-printer-app/server.log"; }
      && accepted { logFile = "/var/lib/phomemo-printer-app/.log"; }
      && accepted { logFile = "syslog"; };
    "other log files are refused" =
      !accepted { logFile = "/var/lib/phomemo-printer-app/.."; }
      && !accepted { logFile = "/var/lib/phomemo-printer-app/logs/server.log"; }
      && !accepted { logFile = "/var/lib/phomemo-printer-app/"; }
      && !accepted { logFile = "/var/log/phomemo-printer-app.log"; };
    "invalid ports and channels are refused" =
      !accepted { port = 0; }
      && !accepted { bluetoothChannels = [ ]; }
      && !accepted { bluetoothChannels = [ 31 ]; };
  };

  failures = lib.concatLines (lib.attrNames (lib.filterAttrs (_: holds: !holds) expectations));
in
pkgs.runCommandLocal "phomemo-printer-app-module-eval" { inherit failures; } ''
  if [ -n "$failures" ]; then
    printf 'Does not hold:\n%s' "$failures" >&2
    exit 1
  fi
  touch $out
''
