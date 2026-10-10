# NixOS, Nix and source installation

Start with the [README's installation choices](../README.md#install) if you
have not chosen a format. Snap and experimental Flatpak have their own
[package installation guide](packages.md). This guide covers native builds
and services; after installation, follow
[Print your first label](../README.md#print-your-first-label).

## Requirements

- Linux with BlueZ, and the printer paired and trusted on the host
- PAPPL 1.x, 1.4 or later
- To build: Rust 1.87 or later, cbindgen, a C compiler, GNU make,
  pkg-config and PAPPL's development files — or Nix, which provides them
- For `register-cups`: the host's CUPS scheduler and client tools
  (`lpstat`, `lpadmin`)

## Nix or source build

Run these commands from the repository checkout. With Nix:

```sh
nix build                # ./result/bin/phomemo-printer-app
nix develop --command make  # ./phomemo-printer-app
```

Without Nix, with the build requirements installed:

```sh
make                     # ./phomemo-printer-app
```

### Run a foreground server

For a Nix package build:

```sh
PHOMEMO_SERVER_PORT=8000 ./result/bin/phomemo-printer-app server
```

For a source build:

```sh
PHOMEMO_SERVER_PORT=8000 ./phomemo-printer-app server
```

Open `http://localhost:8000/` and keep the terminal/server running while you
print. Use the same binary path for CLI commands in a second terminal, or
use `phomemo-printer-app` if installed on your command search path. A
fixed port also makes CUPS registration predictable. See
[configuration defaults](configuration.md#server-settings) for state and
spool locations when running outside the system service.

## Native systemd installation

From a source build, install and enable a persistent service:

```sh
make
sudo make install-systemd
sudo systemctl daemon-reload
sudo systemctl enable --now phomemo-printer-app
```

Build as yourself first: the install targets copy what `make` built and
never build anything. If replacing an older installation, read
[native service migration](upgrading.md#native-service-migration) before
starting the new service.

| Target | Installs |
| --- | --- |
| `install` | The binary, to `$(BINDIR)` |
| `install-unit` | The systemd unit, to `$(UNITDIR)`, and its root drop-in, to `$(DATADIR)/phomemo-printer-app` |
| `install-systemd` | Both, and `$(ENVFILE)` unless it exists |

`PREFIX` (`/usr/local`) sets `BINDIR` (`$(PREFIX)/bin`), `DATADIR`
(`$(PREFIX)/share`) and `UNITDIR` (`$(PREFIX)/lib/systemd/system`).
`ENVFILE` defaults to `/etc/default/phomemo-printer-app`;
`SYSCONFDIR` defaults to `/etc`. `DESTDIR` stages an installation, for
example for a distribution package.

The Nix package contains the binary, the unit in `lib/systemd/system`,
the root drop-in in `share/phomemo-printer-app`, and the example
configuration in `share/doc/phomemo-printer-app`. On NixOS, use the
[module](#nixos).

### The native systemd service

The service runs as a dynamic, unprivileged user (`DynamicUser=yes`). It
keeps its printers, spool and TLS certificate in
`/var/lib/phomemo-printer-app`, listens for the subcommands of every
account at `/run/phomemo-printer-app/phomemo-printer-app.sock`, and logs to
the journal:

```sh
journalctl -u phomemo-printer-app
```

Its sandbox permits writes to its state and runtime directories and its
private `/tmp`, `/var/tmp` and `/dev/shm`; the
[unit template](../systemd/phomemo-printer-app.service.in) explains each
setting. Like every PAPPL server, it lets any local account administer it
through subcommands and, without an authentication service, through the
web interface on localhost.

Configure it in `/etc/default/phomemo-printer-app`, then apply changes:

```sh
sudo systemctl restart phomemo-printer-app
```

The web interface is at `http://localhost:8000/` unless
`PHOMEMO_SERVER_PORT` says otherwise. See the
[configuration reference](configuration.md) for environment variables,
remote access and troubleshooting. NixOS uses module options instead of
this environment file.

### Remote logins and the root drop-in

Remote web interface logins through PAM need a root server when using
`pam_unix`, because it checks other users' passwords only for root. The
`root.conf` drop-in, installed in `$(DATADIR)/phomemo-printer-app`, switches
the service to root while keeping the sandbox:

```sh
DATADIR=/usr/local/share    # as installed: $(PREFIX)/share by default
sudo install -D -m 0644 "$DATADIR/phomemo-printer-app/root.conf" \
  /etc/systemd/system/phomemo-printer-app.service.d/root.conf
sudo systemctl daemon-reload
sudo systemctl restart phomemo-printer-app
```

For root the sandbox limits accidents, not a compromised server: root on
the system bus can still ask systemd for anything. The drop-in mounts a
tmpfs over `/etc/cups`, which therefore has to exist. Without CUPS,
systemd creates it, empty, and it stays; with a read-only `/etc` the service
fails to start. On NixOS, the module's `runAsRoot` installs the drop-in and
provides `/etc/cups` without CUPS. See
[remote-login troubleshooting](configuration.md#troubleshooting) for
Fedora/RHEL's `/etc/shadow` permissions.

### Stop or remove the native service

Remove any CUPS queue you no longer need before stopping the service and
removing the binary:

```sh
phomemo-printer-app unregister-cups --queue phomemo
```

Then, from the source checkout:

```sh
sudo systemctl disable --now phomemo-printer-app
sudo make uninstall-systemd
sudo systemctl daemon-reload
```

Use the same installation directory settings as when installing.
`uninstall-systemd` keeps the configuration file, the service's state and
the root drop-in if you installed it in
`/etc/systemd/system/phomemo-printer-app.service.d/`.

## NixOS

The flake's NixOS module runs the package's unit with its settings as
options. Add the input and module to your system flake:

```nix
{
  inputs.phomemo-printer-app.url = "github:mabl/phomemo-printer-app";

  outputs =
    { nixpkgs, phomemo-printer-app, ... }:
    {
      nixosConfigurations.HOST = nixpkgs.lib.nixosSystem {
        modules = [
          phomemo-printer-app.nixosModules.default
          { services.phomemo-printer-app.enable = true; }
        ];
      };
    };
}
```

Replace `HOST` with your system configuration name and include your usual
host modules. Rebuild your system to enable the service, then open
`http://localhost:8000/`.

The module builds the package with the system's nixpkgs so it shares the
system's PAPPL, CUPS and C library, or takes `pkgs.phomemo-printer-app`,
which the flake's `overlays.default` adds. For the build using the flake's
own lock pins, as `nix build` makes it, set `package` to
`phomemo-printer-app.packages.${pkgs.stdenv.hostPlatform.system}.default`.

The module installs the subcommands and enables BlueZ
(`hardware.bluetooth.enable`) unless that is already set. Without BlueZ
the service runs but finds no printers. It does **not** read
`/etc/default/phomemo-printer-app`.

### NixOS options

The options are under `services.phomemo-printer-app`:

| Option | Default | Sets |
| --- | --- | --- |
| `enable` | `false` | Enables the service |
| `package` | Overlay package or build with system nixpkgs | Package providing the unit and CLI |
| `port` | `8000` | `PHOMEMO_SERVER_PORT` |
| `listenHostname` | `"localhost"` | `PHOMEMO_LISTEN_HOSTNAME` |
| `authService` | `null` locally, `"phomemo-printer-app"` remotely | `PHOMEMO_AUTH_SERVICE` |
| `adminGroup` | `null` | `PHOMEMO_ADMIN_GROUP` |
| `logLevel` | `"info"` | `PHOMEMO_LOG_LEVEL` |
| `logFile` | `"-"` | `PHOMEMO_LOG_FILE` |
| `tlsOnly` | `false` | `PHOMEMO_TLS_ONLY` |
| `bluetoothChannels` | `[ 1 ]` | `PHOMEMO_BT_CHANNELS` |
| `environment` | `{ }` | Further variables |
| `openFirewall` | `false` | Opens `port` if listening beyond localhost |
| `runAsRoot` | `false` | Installs the root drop-in |

A port below 1024 gives the service the capability to bind it. `logFile`
is `-` or `syslog` (both reach the journal), or a file directly in
`/var/lib/phomemo-printer-app`. `tlsOnly` advertises only `ipps` and
`https` URIs to other hosts, while still answering plain connections.
`environment` takes settings without an option, such as
`PHOMEMO_SPOOL_DIRECTORY`, but refuses variables that an option sets.
`openFirewall` has no effect, and warns, while listening on localhost only.

### NixOS remote administration

Listening beyond localhost, the web interface asks for logins checked by
PAM service `phomemo-printer-app`. `authService` names another service; if
set, it makes the local web interface ask for logins too. The module
declares the service in `security.pam.services`, with NixOS defaults or
adding to another module's definition of it. Remote listeners require a
non-null `authService`.

PAM's `pam_unix` checks passwords only for a root server, so logins need
`runAsRoot`: the module warns without it while the PAM service uses
`pam_unix`. A web interface for administrators on the network:

```nix
services.phomemo-printer-app = {
  enable = true;
  listenHostname = "*";
  openFirewall = true;
  runAsRoot = true;
  adminGroup = "wheel";
};
```

### NixOS and CUPS

With CUPS enabled (`services.printing.enable`), add its queue once with
`register-cups` after adding the printer. It reads its caller's
environment, not module settings: give it the same port and, where set,
the listen host name and TLS-only setting. For example:

```sh
sudo PHOMEMO_TLS_ONLY=1 phomemo-printer-app register-cups --port 8000
```

See [CUPS registration](configuration.md#cups-registration) for full
semantics and [the README](../README.md#print-through-cups) for printing.

## Development and maintenance

```sh
nix develop
make check     # fmt-check, clippy, tests, C with -Werror, and the build
```

Other targets: `make fmt`, `make lint` (clippy), `make c-lint`,
`make test`, `make clean`, and `make ci`, which is `make check` without the
build. CI runs `nix flake check`, checking the same hermetically — the
package build runs the tests in release mode — plus Nix formatting
(`nix fmt`), the dev shell and the NixOS module. Its options are evaluated
by `.#checks.x86_64-linux.module-eval`, and its service is tested in VMs:

```sh
nix build -L .#checks.x86_64-linux.nixos  # requires KVM
```

The package build also runs real-server runtime tests, including native
service socket discovery, persistence and packaged socket isolation.
Select this check directly with:

```sh
nix build -L .#checks.x86_64-linux.runtime
```

The [package guide](packages.md#building-and-releasing) describes
Snap/Flatpak builds and release checks. A weekly workflow proposes the
newest nixpkgs in a pull request.

### Regenerate the media catalog

The driver's media catalog, `phomemo-protocol/data/media_catalog.json`, is
generated from the media definitions in the Print Master Android app:
`localPaper.json`, `DefaultPrinter.json` and `DefaultTypeGroup.json` from
the APK's `assets/` directory. Regenerate it after updating them, from the
repository root:

```sh
python3 scripts/generate_media_catalog.py --reference-dir path/to/assets
```

`--out` writes elsewhere; the catalog records each input's file name and
SHA-256, not its local path. Measured M220 paper-positioning behavior and
leading-edge bleed experiments are recorded in
[m220-positioning.md](m220-positioning.md).
