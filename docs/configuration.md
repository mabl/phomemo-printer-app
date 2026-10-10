# Configuration and troubleshooting

For first setup, follow [Print your first label](../README.md#print-your-first-label).
This reference describes native server settings and CLI behavior. Snap and
Flatpak supply their own launchers, configuration and runtime directories;
use the [package guide](packages.md) for those installations. Native
systemd and NixOS configuration are described in the
[installation guide](install.md).

## Server settings

`phomemo-printer-app --help` lists the settings. Each setting in the table
can be set in the environment, or as a server option
(`server -o NAME=VALUE`, or a `NAME=VALUE` line in PAPPL's configuration
file, for example `/etc/phomemo-printer-app.conf`). Server options take
precedence over the environment.

| Variable | Option | Default outside a service/package |
| --- | --- | --- |
| `PHOMEMO_SERVER_PORT` | `server-port` | `0`: PAPPL takes a free port (below) |
| `PHOMEMO_LISTEN_HOSTNAME` | `listen-hostname` | `localhost` |
| `PHOMEMO_AUTH_SERVICE` | `auth-service` | None; `cups` when listening beyond localhost |
| `PHOMEMO_ADMIN_GROUP` | `admin-group` | None: any user who logs in |
| `PHOMEMO_LOG_FILE` | `log-file` | `-`, standard error |
| `PHOMEMO_LOG_LEVEL` | `log-level` | `info` |
| `PHOMEMO_SPOOL_DIRECTORY` | `spool-directory` | A temporary directory |
| `PHOMEMO_STATE_FILE` | `state-file` | PAPPL's choice (below) |
| `PHOMEMO_TLS_ONLY` | `tls-only` | `0` |

`listen-hostname` takes a host name, an IPv4 address, an IPv6 address in
brackets, `*` for every address, or a domain socket path. Listening beyond
localhost enables the remote web interface, which asks for a login through
PAM. An explicitly configured authentication service also requires logins
locally. With port 0, PAPPL takes the first free port from 8000, or for a
user other than root from 8000 + UID % 1000.

The state file holds printers and their settings. PAPPL keeps it in
`/var/lib/phomemo-printer-app.state` for root, and otherwise in
`$XDG_CONFIG_HOME` or `~/.config`. The native systemd unit overrides these
defaults: it fixes the port at 8000 and keeps state, spool and certificates
in `/var/lib/phomemo-printer-app`.

An empty value restores the default, so an empty server option undoes the
environment. An invalid value is reported on standard error and ignored.
`tls-only` advertises secure URIs to other hosts; it does not disable plain
connections.

For a source-installed service, edit `/etc/default/phomemo-printer-app`
and restart it. For NixOS, use
[module options](install.md#nixos-options); that environment file is not
read. Remote PAM logins using `pam_unix` need the
[root drop-in](install.md#remote-logins-and-the-root-drop-in) or NixOS
`runAsRoot`.

## Bluetooth channels

`PHOMEMO_BT_CHANNELS` lists the RFCOMM channels to try, comma-separated
(default `1`). A device URI can select one with `?channel=N`.

## Runtime socket

Package launchers set `PHOMEMO_RUNTIME_DIRECTORY` for the server and CLI:
an existing writable directory owned by their user, mode `0700`, with an
absolute path and no final symlink. Both server and CLI must use the same
directory and executable name. The complete `DIRECTORY/NAME.sock` path
must fit a UNIX socket (107 bytes on Linux).

With this setting the server must be started explicitly; automatic
private-server startup is disabled. Leave it unset for native
systemd/PAPPL socket discovery. Snap's private socket requires root CLI
administration; Flatpak's launcher and CLI use the current user's private
socket. See [package commands](packages.md).

## Printer and job commands

These examples use an installed **native** binary and a running server.
For a checkout build, use `./phomemo-printer-app` or
`./result/bin/phomemo-printer-app` instead. Snap requires `sudo` and Flatpak
requires its launcher command; see
[package-specific CLI instructions](packages.md#first-printer-and-cups-setup).

Pair the printer once on the host (`bluetoothctl`, then `scan on`,
`pair ADDRESS`, `trust ADDRESS`). List discovered devices and add a queue
using its driver, or `auto` to choose from the printer's name:

```sh
phomemo-printer-app devices
phomemo-printer-app add -d m220 -v btspp://27-A6-4F-5D-03-99 -m phomemo_m220
```

Replace the sample Bluetooth address with your printer's address, and the
model/queue with yours. The web interface at `http://localhost:PORT/` can
also add printers. A server without saved printers adds supported ones it
finds at startup. Select **Loaded Media** and the appropriate **Tracking**
setting on its **Media Setup** page, or select media per job:

```sh
phomemo-printer-app submit -d m220 -o media=om_40x30mm_40x30mm label.png
```

The media example is for 40 × 30 mm labels. Use your actual loaded size;
print PDFs through CUPS.

## CUPS registration

`register-cups` adds an IPP Everywhere queue in the local CUPS scheduler
for the server, and `unregister-cups` removes it. Start the server and add
your printer first; the CUPS scheduler and client tools (`lpstat`,
`lpadmin`) must be installed, and the caller needs permission to administer
CUPS.

```sh
phomemo-printer-app register-cups --queue phomemo --port 8000
phomemo-printer-app unregister-cups --queue phomemo
```

The queue reaches the server at `PHOMEMO_LISTEN_HOSTNAME` (`localhost`
when it listens on every address), over `ipps` with `PHOMEMO_TLS_ONLY`.
These subcommands read their **caller's environment only** — not server
options or the service's configuration. Give them the service's
`PHOMEMO_LISTEN_HOSTNAME` and `PHOMEMO_TLS_ONLY` where it sets them, and its
fixed port through `--port` or `PHOMEMO_SERVER_PORT`. For example:

```sh
sudo PHOMEMO_LISTEN_HOSTNAME=localhost PHOMEMO_TLS_ONLY=1 \
  phomemo-printer-app register-cups --queue phomemo --port 8000
```

`--replace` recreates an existing queue. The commands exit with 0 on
success, 2 for invalid arguments, and 1 on any other failure, including
missing `lpstat` or `lpadmin`.

Snap's command requires `sudo` and the CUPS interface connection. Flatpak
does not support these administration commands: use the host's printer
settings or `lpadmin`. See
[package CUPS setup](packages.md#first-printer-and-cups-setup) and
[printing examples](../README.md#print-through-cups).

## Troubleshooting

- **The printer is not listed.** Only printers paired with BlueZ are
  listed. Their name or alias must identify a recognized model (`M110`,
  `D30_1234`, `Phomemo M220S`), look like a recognized printer serial number
  (`Q198G5949230062`), or contain the standalone word `Phomemo`. A device
  discovered from `Phomemo` alone may need its model selected manually.
  BlueZ older
  than 5.51 answers only root, the `lp` group and console users over D-Bus.
  For the native service, give it the group with `SupplementaryGroups=lp`
  in a drop-in (`sudo systemctl edit phomemo-printer-app`). For packaged
  Bluetooth permissions, see [the package guide](packages.md).
- **"is another queue using the same printer?"** A printer takes one
  Bluetooth connection at a time, so give each printer one application
  queue and stop other installations or vendor apps using it. A second
  queue for the same address waits up to 5 seconds for the first one's
  connection, stalling its web and IPP requests, then fails.
- **Remote logins fail.** With `pam_unix`, they need the
  [root drop-in](install.md#remote-logins-and-the-root-drop-in), or NixOS
  `runAsRoot`. Where `/etc/shadow` is mode 0000, as Fedora and RHEL ship
  it, PAM's `unix_chkpwd` also needs the capability the drop-in names.
  The Snap supports local administration only.
- **The web interface answers "Bad Request" from the network.** PAPPL
  takes requests only for `localhost`, an address, any `.local` name, or
  its own host name: `PHOMEMO_LISTEN_HOSTNAME` when that names a host,
  otherwise the system's, with `.local` added if it has no domain. Thus
  `http://HOST:PORT/` fails for a bare host name; use the address or
  `http://HOST.local:PORT/`. The latter needs mDNS on both ends: Avahi
  publishing the host and a client that resolves `.local` names. On
  NixOS, enable publishing with:

  ```nix
  services.avahi = {
    enable = true;
    publish = { enable = true; addresses = true; userServices = true; };
  };
  ```

  This also lets the server announce its printers. On a NixOS client,
  `services.avahi.nssmdns4 = true` enables `.local` resolution.
- **A port below 1024** needs `AmbientCapabilities=CAP_NET_BIND_SERVICE`
  and `CapabilityBoundingSet=CAP_NET_BIND_SERVICE` in a native systemd
  drop-in. The NixOS module supplies these automatically.
- **`shutdown` returns before the server exits.** It exits when PAPPL's
  main loop next wakes, up to 30 seconds later. `SIGTERM` (`systemctl stop`
  for the native service) stops it at once. Use the
  [package stop commands](packages.md#upgrade-stop-and-remove) for Snap
  or Flatpak.

## Known limitations

- Images (PNG, JPEG) submitted directly cannot be printed on a continuous
  roll (a media size 0 mm long). PAPPL rasterizes an image onto the media
  size and refuses a page without length ("Invalid media size"). Choose a
  label size for these jobs, or print through CUPS, which renders pages of
  a definite length.
- Raw jobs (`application/vnd.phomemo-raw`) end as soon as they are sent;
  only raster jobs wait for the printer to report each page printed.
- Bluetooth printing from Snap and Flatpak has not yet been validated on
  hardware. See [package validation and feedback](packages.md#feedback).
