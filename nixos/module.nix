# services.phomemo-printer-app: the package's own unit, from
# systemd.packages, with the options below in a drop-in, so that the module
# and `make install-systemd` run the same service.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  inherit (lib)
    literalExpression
    literalMD
    mkDefault
    mkEnableOption
    mkIf
    mkOption
    optional
    optionalAttrs
    types
    ;

  cfg = config.services.phomemo-printer-app;

  # Whether the server serves this host only, as the application decides
  # (is_local_listener in c/main.c): the loopback interface or a domain
  # socket.
  local =
    lib.hasPrefix "/" cfg.listenHostname
    || lib.elem (lib.toLower cfg.listenHostname) [
      "localhost"
      "127.0.0.1"
      "[::1]"
      "::1"
    ];

  # The PAM service the module declares for remote logins.
  defaultAuthService = "phomemo-printer-app";

  # The typed options, as the PHOMEMO_* settings (`phomemo-printer-app
  # --help`); null ones keep the application's defaults.
  settings = {
    PHOMEMO_SERVER_PORT = toString cfg.port;
    PHOMEMO_LISTEN_HOSTNAME = cfg.listenHostname;
    PHOMEMO_AUTH_SERVICE = cfg.authService;
    PHOMEMO_ADMIN_GROUP = cfg.adminGroup;
    PHOMEMO_LOG_FILE = cfg.logFile;
    PHOMEMO_LOG_LEVEL = cfg.logLevel;
    PHOMEMO_TLS_ONLY = if cfg.tlsOnly then "1" else "0";
    PHOMEMO_BT_CHANNELS = lib.concatMapStringsSep "," toString cfg.bluetoothChannels;
  };
  settingsEnvironment = lib.filterAttrs (_: value: value != null) settings;

  # The package's root drop-in, where systemd.packages finds drop-ins. A
  # package without one fails here, at build time: an assertion would have
  # to build the package during evaluation.
  rootDropIn =
    pkgs.runCommandLocal "phomemo-printer-app-root-drop-in"
      { dropIn = "${cfg.package}/share/phomemo-printer-app/root.conf"; }
      ''
        if [ ! -f "$dropIn" ]; then
          echo "services.phomemo-printer-app.runAsRoot: $dropIn does not exist." >&2
          exit 1
        fi
        install -D -m 0644 "$dropIn" \
          $out/lib/systemd/system/phomemo-printer-app.service.d/root.conf
      '';
in
{
  options.services.phomemo-printer-app = {
    enable = mkEnableOption "the Phomemo Printer Application, which serves Phomemo Bluetooth label printers over IPP";

    package = mkOption {
      type = types.package;
      default = pkgs.phomemo-printer-app or (pkgs.callPackage ../package.nix { });
      defaultText = literalMD ''
        `pkgs.phomemo-printer-app`, from the flake's overlay, or else the flake's
        package built with the system's `pkgs`
      '';
      example = literalExpression "phomemo-printer-app.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      description = ''
        The package whose unit and sub-commands the module installs. The
        default shares the system's PAPPL, CUPS and C library; the flake's
        own package is built with the nixpkgs its lock pins.
      '';
    };

    port = mkOption {
      type = types.ints.between 1 65535;
      default = 8000;
      description = ''
        The port of IPP and the web interface. A port below 1024 gives the
        service the capability to bind it.
      '';
    };

    listenHostname = mkOption {
      type = types.nonEmptyStr;
      default = "localhost";
      example = "*";
      description = ''
        Where the server listens: `localhost` serves this host only. A host
        name, an IP address (`[...]` for IPv6), or `*` for every address also
        serves the network, where the web interface asks for a login through
        PAM, see {option}`services.phomemo-printer-app.authService`.
      '';
    };

    authService = mkOption {
      type = types.nullOr types.nonEmptyStr;
      default = if local then null else defaultAuthService;
      defaultText = literalMD ''`null` when listening on localhost only, `"${defaultAuthService}"` otherwise'';
      description = ''
        The PAM service web interface logins are checked with, which the
        module declares in {option}`security.pam.services`: with its defaults,
        or extending another module's definition of the same service. Set,
        even the local web interface asks for a login. PAM's `pam_unix` checks other accounts' passwords only
        for a root server, see
        {option}`services.phomemo-printer-app.runAsRoot`.
      '';
    };

    adminGroup = mkOption {
      type = types.nullOr types.nonEmptyStr;
      default = null;
      example = "wheel";
      description = "The group a web interface login must belong to; `null` for any account.";
    };

    logLevel = mkOption {
      type = types.enum [
        "debug"
        "info"
        "warn"
        "error"
        "fatal"
      ];
      default = "info";
      description = "The least severe messages logged.";
    };

    logFile = mkOption {
      type = types.either (types.enum [
        "-"
        "syslog"
      ]) (types.strMatching "/var/lib/phomemo-printer-app/[^/]*[^/.][^/]*");
      default = "-";
      example = "/var/lib/phomemo-printer-app/server.log";
      description = ''
        Where the server logs: `-` (standard error) and `syslog` reach the
        journal; a file must be directly in the state directory,
        {file}`/var/lib/phomemo-printer-app`, the only one the service may
        write.
      '';
    };

    tlsOnly = mkOption {
      type = types.bool;
      default = false;
      description = ''
        Whether to advertise only `ipps` and `https` URIs to other hosts.
        Plain connections are still answered. The server makes itself a
        certificate, kept in its state directory.
      '';
    };

    bluetoothChannels = mkOption {
      type = types.nonEmptyListOf (types.ints.between 1 30);
      default = [ 1 ];
      description = ''
        The RFCOMM channels to try, in order; a device URI can name one with
        `?channel=N`.
      '';
    };

    environment = mkOption {
      type = types.attrsOf types.str;
      default = { };
      example = {
        PHOMEMO_SPOOL_DIRECTORY = "/var/lib/phomemo-printer-app/jobs";
      };
      description = ''
        Further environment variables of the service: settings without an
        option here. The options' own variables are refused.
      '';
    };

    openFirewall = mkOption {
      type = types.bool;
      default = false;
      description = ''
        Whether to open {option}`services.phomemo-printer-app.port` in the
        firewall. Only takes effect when listening beyond localhost.
      '';
    };

    runAsRoot = mkOption {
      type = types.bool;
      default = false;
      description = ''
        Whether to run the service as root, through the package's drop-in
        {file}`root.conf`, instead of as a dynamic user. Web interface
        logins need it when PAM's `pam_unix` checks them, which it does only
        for root. The sandbox stays, but for root it limits accidents, not a
        compromised server. The drop-in mounts a tmpfs over
        {file}`/etc/cups`, to keep the server's TLS credentials apart from
        CUPS'; without CUPS, the module provides an empty one.
      '';
    };
  };

  config = mkIf cfg.enable {
    assertions = [
      {
        assertion = local || cfg.authService != null;
        message = ''
          services.phomemo-printer-app.authService is null, but the server listens on
          ${cfg.listenHostname}: the remote web interface needs logins.
        '';
      }
      (
        let
          clashes = lib.attrNames (lib.intersectAttrs settings cfg.environment);
        in
        {
          assertion = clashes == [ ];
          message = ''
            services.phomemo-printer-app.environment sets ${lib.concatStringsSep ", " clashes}:
            use the services.phomemo-printer-app options instead.
          '';
        }
      )
    ];

    warnings =
      optional (cfg.openFirewall && local) ''
        services.phomemo-printer-app.openFirewall has no effect: the server listens on
        ${cfg.listenHostname} only.
      ''
      ++
        optional
          (
            cfg.authService != null
            && !cfg.runAsRoot
            && config.security.pam.services.${cfg.authService}.unixAuth
          )
          ''
            services.phomemo-printer-app checks logins with PAM service ${cfg.authService}, whose
            pam_unix checks passwords only for a root server: set
            services.phomemo-printer-app.runAsRoot.
          '';

    # Printers are found and reached through BlueZ, whose D-Bus policy lets
    # any account, the dynamic user's too, ask it for the paired devices.
    # Without it the service runs, finding no Bluetooth printers.
    hardware.bluetooth.enable = mkDefault true;

    # The sub-commands, which find the service at
    # /run/phomemo-printer-app/phomemo-printer-app.sock.
    environment.systemPackages = [ cfg.package ];

    security.pam.services = mkIf (cfg.authService != null) { ${cfg.authService} = { }; };

    networking.firewall.allowedTCPPorts = mkIf (cfg.openFirewall && !local) [ cfg.port ];

    systemd.packages = [ cfg.package ] ++ optional cfg.runAsRoot rootDropIn;

    # The drop-in's tmpfs needs a mount point, which systemd cannot create in
    # an immutable /etc. CUPS' module defines its own.
    environment.etc.cups = mkIf cfg.runAsRoot { source = mkDefault pkgs.emptyDirectory; };

    systemd.services.phomemo-printer-app = {
      wantedBy = [ "multi-user.target" ];
      environment = settingsEnvironment // cfg.environment;
      serviceConfig = {
        # The options are the configuration: the unit's file in /etc/default,
        # which would take precedence, is not read.
        EnvironmentFile = "";
      }
      // optionalAttrs (cfg.port < 1024) {
        AmbientCapabilities = [ "CAP_NET_BIND_SERVICE" ];
        CapabilityBoundingSet = [ "CAP_NET_BIND_SERVICE" ];
      };
    };
  };
}
