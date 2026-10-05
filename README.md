# Phomemo Printer Application

A [PAPPL](https://www.msweet.org/pappl/) printer application for Phomemo
Bluetooth label printers. It drives the printers over Bluetooth RFCOMM and
presents each one as an IPP Everywhere printer, so CUPS and other IPP
clients print to it without a vendor driver; a web interface sets up
printers and their media.

## Supported printers

Phomemo's label printers with 12 mm heads (the D30 and its relatives), and
with 48 mm (M110, M120 and relatives) and 72 mm heads (M200, M220 and
relatives): [docs/models.md](docs/models.md) lists every model.

## Requirements

- Linux with BlueZ, and the printer paired with it
- PAPPL 1.x, 1.4 or later
- To build: Rust 1.87 or later, cbindgen, a C compiler, GNU make,
  pkg-config and PAPPL's development files - or Nix, which provides them
- For `register-cups`: the CUPS client tools (`lpstat`, `lpadmin`)

## Build

With Nix:

```bash
nix build                # ./result/bin/phomemo-printer-app
nix develop --command make
```

Without Nix, with the requirements installed:

```bash
make                     # ./phomemo-printer-app
```

## Install

As a system service, from a source build:

```bash
make
sudo make install-systemd
sudo systemctl daemon-reload
sudo systemctl enable --now phomemo-printer-app
```

The install targets copy what `make` built and never build anything, so
run `make` first, as yourself. They are:

| Target            | Installs                                              |
| ----------------- | ----------------------------------------------------- |
| `install`         | the binary, to `$(BINDIR)`                            |
| `install-unit`    | the systemd unit, to `$(UNITDIR)`, and its root       |
|                   | drop-in, to `$(DATADIR)/phomemo-printer-app`          |
| `install-systemd` | both, and `$(ENVFILE)` unless it exists               |

`PREFIX` (`/usr/local`) sets `BINDIR` (`$(PREFIX)/bin`), `DATADIR`
(`$(PREFIX)/share`) and `UNITDIR` (`$(PREFIX)/lib/systemd/system`);
`ENVFILE` is
`/etc/default/phomemo-printer-app`. `DESTDIR` stages an installation, e.g.
for a distribution package. To remove the service, disable it and run
`sudo make uninstall-systemd`, which keeps the configuration file, the
service's state, and the root drop-in if you installed it in
`/etc/systemd/system/phomemo-printer-app.service.d/`.

The Nix package contains the binary, the unit in `lib/systemd/system`, the
drop-in in `share/phomemo-printer-app` and the example configuration in
`share/doc/phomemo-printer-app`. On NixOS,
with `phomemo-printer-app` bound to the package:

```nix
systemd.packages = [ phomemo-printer-app ];
systemd.services.phomemo-printer-app.wantedBy = [ "multi-user.target" ];
```

### The service

The service runs as a dynamic, unprivileged user (`DynamicUser=yes`). It
keeps its printers, spool and TLS certificate in
`/var/lib/phomemo-printer-app`, listens for the sub-commands of every
account at `/run/phomemo-printer-app/phomemo-printer-app.sock`, and logs to
the journal (`journalctl -u phomemo-printer-app`). Its sandbox lets it
write nowhere else but its private `/tmp`, `/var/tmp` and `/dev/shm`; the
unit explains each setting. Like every PAPPL server, it lets any local
account administer it through the sub-commands, and without an auth
service through the web interface on localhost.

Remote web interface logins through PAM need a root server, because
`pam_unix` checks other users' passwords only for root. The drop-in
`root.conf`, installed in `$(DATADIR)/phomemo-printer-app`, switches the
service to root, keeping the sandbox:

```bash
DATADIR=/usr/local/share    # as installed: $(PREFIX)/share by default
sudo install -D -m 0644 "$DATADIR/phomemo-printer-app/root.conf" \
  /etc/systemd/system/phomemo-printer-app.service.d/root.conf
sudo systemctl daemon-reload
sudo systemctl restart phomemo-printer-app
```

For root the sandbox limits accidents, not a compromised server: root on
the system bus can still ask systemd for anything. The drop-in mounts a
tmpfs over `/etc/cups`, which therefore has to exist: without CUPS, systemd
creates it, empty, and it stays; with a read-only `/etc` the service fails
to start. With the Nix package, the drop-in is in
`share/phomemo-printer-app`; the NixOS module will expose it as an option.

Configure it in `/etc/default/phomemo-printer-app`, then
`sudo systemctl restart phomemo-printer-app`. Its web interface is at
`http://localhost:8000/` unless `PHOMEMO_SERVER_PORT` says otherwise.

## Configuration

`phomemo-printer-app --help` lists the settings. Each can be set in the
environment, or as a server option (`server -o NAME=VALUE`, or a
`NAME=VALUE` line in PAPPL's configuration file, e.g.
`/etc/phomemo-printer-app.conf`), which takes precedence:

| Variable                  | Option            | Default                                         |
| ------------------------- | ----------------- | ----------------------------------------------- |
| `PHOMEMO_SERVER_PORT`     | `server-port`     | `0`: PAPPL takes a free port (below)            |
| `PHOMEMO_LISTEN_HOSTNAME` | `listen-hostname` | `localhost`                                     |
| `PHOMEMO_AUTH_SERVICE`    | `auth-service`    | none; `cups` when listening beyond localhost    |
| `PHOMEMO_ADMIN_GROUP`     | `admin-group`     | none: any user who logs in                      |
| `PHOMEMO_LOG_FILE`        | `log-file`        | `-`, standard error                             |
| `PHOMEMO_LOG_LEVEL`       | `log-level`       | `info`                                          |
| `PHOMEMO_SPOOL_DIRECTORY` | `spool-directory` | a temporary directory                           |
| `PHOMEMO_STATE_FILE`      | `state-file`      | PAPPL's choice (below)                          |
| `PHOMEMO_TLS_ONLY`        | `tls-only`        | `0`                                             |

`listen-hostname` takes a host name, an IPv4 address, an IPv6 address in
brackets, `*` for every address, or a domain socket path. Listening beyond
localhost enables the remote web interface, which asks for a login through
PAM. With port 0, PAPPL takes the first free port from 8000, or for a user
other than root from 8000 + UID % 1000. The state file holds the printers
and their settings; PAPPL keeps it in `/var/lib/phomemo-printer-app.state`
for root, and otherwise in `$XDG_CONFIG_HOME` or `~/.config`. An empty value restores the default, so
an empty server option undoes the environment; an invalid value is reported
on standard error and ignored.

`PHOMEMO_BT_CHANNELS` lists the RFCOMM channels to try, comma-separated
(default `1`); a device URI can name one with `?channel=N`.

## Adding a printer

Pair the printer once (`bluetoothctl`, then `scan on`, `pair ADDRESS`,
`trust ADDRESS`). With the server running, list what it finds, and add a
queue for the printer with its driver, or `auto` to pick it from the
printer's name:

```bash
phomemo-printer-app devices
phomemo-printer-app add -d m220 -v btspp://27-A6-4F-5D-03-99 -m phomemo_m220
```

Or use the web interface, at `http://localhost:PORT/`, which also adds
printers. A server without saved printers adds those it finds when it
starts. Set the loaded labels in the web interface's Media page, or with
`-o media=...` per job:

```bash
phomemo-printer-app submit -d m220 -o media=om_40x30mm_40x30mm label.png
```

## Printing through CUPS

`register-cups` adds an IPP Everywhere queue in the local CUPS scheduler
for the server, and `unregister-cups` removes it:

```bash
phomemo-printer-app register-cups --queue phomemo --port 8000
phomemo-printer-app unregister-cups --queue phomemo
```

The queue reaches the server at `PHOMEMO_LISTEN_HOSTNAME` (`localhost`
when it listens on every address), over `ipps` with `PHOMEMO_TLS_ONLY`.
These sub-commands read the environment only, not server options, and need
a fixed port: `--port`, or `PHOMEMO_SERVER_PORT`. `--replace` recreates an
existing queue. They exit with 0 on success, 2 for invalid arguments, and 1
on any other failure, including when `lpstat` or `lpadmin` is missing.

## Troubleshooting

- **The printer is not listed.** Only printers paired with BlueZ are, and
  only if their name or alias starts with their model (`M110`,
  `D30_1234`) or is their serial number (`Q198G5949230062`).
  BlueZ older than 5.51 answers only root, the `lp` group and console
  users over D-Bus: give the service the group with
  `SupplementaryGroups=lp` in a drop-in
  (`sudo systemctl edit phomemo-printer-app`).
- **"is another queue using the same printer?"** A printer takes one
  Bluetooth connection at a time, so give each printer one queue. A second
  queue for the same address waits up to 5 seconds for the first one's
  connection, stalling its web and IPP requests, and then fails.
- **Remote logins fail.** They need the root drop-in (see The service).
  Where `/etc/shadow` is mode 0000, as Fedora and RHEL ship it, PAM's
  `unix_chkpwd` also needs the capability the drop-in names.
- **A port below 1024** needs `AmbientCapabilities=CAP_NET_BIND_SERVICE`
  and `CapabilityBoundingSet=CAP_NET_BIND_SERVICE` in a drop-in.
- **`shutdown` returns before the server exits**, which happens when PAPPL's
  main loop next wakes, up to 30 seconds later. `SIGTERM` (`systemctl
  stop`) stops it at once.

## Known limitations

- Images (PNG, JPEG) submitted directly cannot be printed on a continuous
  roll (a media size 0 mm long): PAPPL rasterizes an image onto the media
  size and refuses a page without length ("Invalid media size"). Choose a
  label size for such jobs, or print through CUPS, which renders pages of a
  definite length.
- Raw jobs (`application/vnd.phomemo-raw`) end as soon as they are sent:
  only raster jobs wait for the printer to report each page printed.

## Upgrading

Earlier versions installed the unit as
`/etc/systemd/system/phomemo-printer-app.service`, which overrides the one
`install-systemd` now installs (`make install-systemd` warns about it), and
kept its state, as root, in `/var/lib/phomemo-printer-app.state`. Upgrade
in this order, so the old unit cannot save over the state while it moves;
systemd then hands the directory to the service's dynamic user:

```bash
make
sudo systemctl disable --now phomemo-printer-app
sudo rm /etc/systemd/system/phomemo-printer-app.service
sudo make install-systemd
sudo install -d -m 0700 /var/lib/phomemo-printer-app
sudo mv /var/lib/phomemo-printer-app.state /var/lib/phomemo-printer-app/
sudo systemctl daemon-reload
sudo systemctl enable --now phomemo-printer-app
```

The configuration file, `/etc/default/phomemo-printer-app`, is kept. With
remote logins configured in it, install the root drop-in as well.

Print darkness now follows PAPPL's semantics: the printer's darkness
(`printer-darkness-configured`, 0-100 %, set in the web interface) plus a
per-job offset (`print-darkness`, -100 to 100, default 0), mapped onto the
printer's 15 density levels. Earlier builds used `print-darkness` as the
density itself and saved a default of 8, which now reads as an offset of
+8 %, so printers created by an earlier build print darker than intended.
Reset each one once: choose the darkness in its web interface, and clear
the saved offset, which the web interface does not show:

```bash
phomemo-printer-app modify -d PRINTER -o print-darkness-default=0
```

Print speed now defaults to the printer's own setting ("Auto") instead of
level 3.

## Development

```bash
nix develop
make check     # fmt-check, clippy, tests, C with -Werror, and the build
```

Other targets: `make fmt`, `make lint` (clippy), `make c-lint`,
`make test`, `make clean`. CI runs `make ci`, which is `make check` but
the build, and `nix flake check`, which builds the package (and runs the
tests again, in release mode) instead.

The driver's media catalog, `phomemo-protocol/data/media_catalog.json`, is
generated from the media definitions in the Print Master Android app:
`localPaper.json`, `DefaultPrinter.json` and `DefaultTypeGroup.json` from
the APK's `assets/` directory. Regenerate it after updating them:

```bash
python3 scripts/generate_media_catalog.py --reference-dir path/to/assets
```

`--out` writes elsewhere; the catalog records each input's file name and
SHA-256, not its local path.
