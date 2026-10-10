# Snap and experimental Flatpak packages

Both packages build the same Phomemo driver from source and bundle PAPPL
1.4.12. **The packaged Bluetooth printing path has not yet been validated
on hardware.** Successful package tests demonstrate startup, local IPP,
CLI access and saved configuration, not physical printing.

| | Snap | Flatpak |
| --- | --- | --- |
| Architecture suffix | `amd64`, `arm64` | `amd64`, `arm64` |
| Runs as | Root system daemon, confined by snapd | Current user, confined by Flatpak |
| Starts at boot | Yes | No; launch each session |
| CLI | `sudo phomemo-printer-app …` | `flatpak run --command=phomemo-printer-app io.github.mabl.phomemo-printer-app …` |
| Default web interface | `http://localhost:8000/` | `http://127.0.0.1:8631/` |
| Host CUPS setup | Packaged `register-cups` command | Host printer settings or `lpadmin` |

The Flatpak launcher uses the host desktop's OpenURI portal. A host with
Flatpak, a graphical session and a working desktop portal is required for
browser launch. Background-policy behavior still needs real-desktop
testing. Closing the browser does not stop the server.

## Downloads and checksums

Download a matching package from
[GitHub Releases](https://github.com/mabl/phomemo-printer-app/releases),
along with `SHA256SUMS`. Tags must match the application version, such as
`v0.1.0`. Filenames are:

```text
phomemo-printer-app_0.1.0_amd64.snap
phomemo-printer-app_0.1.0_arm64.snap
phomemo-printer-app_0.1.0_amd64.flatpak
phomemo-printer-app_0.1.0_arm64.flatpak
```

Use `amd64` for an x86-64 PC and `arm64` for a 64-bit ARM system. In the
download directory, verify the package you downloaded:

```sh
sha256sum --check --ignore-missing SHA256SUMS
```

All available package lines must report `OK`. An absent release means no
version has been published yet. For testing, the
[Packages workflow](https://github.com/mabl/phomemo-printer-app/actions/workflows/packaging.yml)
provides artifacts after its package checks pass. Extract the artifact ZIP
to obtain the package; tagged releases provide the combined checksums.

## Snap installation

Install `snapd` using your distribution's instructions first. A downloaded
Snap lacks the Snap Store's assertion, hence `--dangerous` for local
installation:

```sh
sudo snap install --dangerous ./phomemo-printer-app_0.1.0_amd64.snap
sudo snap connect phomemo-printer-app:bluez :bluez
sudo snap connect phomemo-printer-app:avahi-control
# Required only for register-cups/unregister-cups:
sudo snap connect phomemo-printer-app:cups-control
sudo snap restart phomemo-printer-app.daemon
```

The package uses the host's BlueZ service and Avahi; its CUPS commands
manage the host's CUPS scheduler. Install/enable these services as needed.
`snap connections phomemo-printer-app` shows granted interfaces. A
connection grants access; it does not install a Bluetooth adapter or host
service.

The daemon starts automatically. Its runtime socket is private and
root-owned, so administration commands require `sudo`:

```sh
sudo phomemo-printer-app status
sudo phomemo-printer-app devices
sudo phomemo-printer-app printers
sudo snap logs phomemo-printer-app.daemon
```

If `/snap/bin` is absent from your command search path, use
`sudo snap run phomemo-printer-app …` instead. Standalone `--help` and
`--version` also work without root.

Open `http://localhost:8000/` to add a printer and select the loaded label
size. The Snap supports local administration only; remote PAM logins and
USB printing are disabled in this package.

Configuration is shared by the daemon and CLI through `snapctl`:

```sh
sudo snap set phomemo-printer-app port=8001 listen-hostname=127.0.0.1
sudo snap restart phomemo-printer-app.daemon
```

The default port is `8000`. `listen-hostname` accepts only loopback values:
`localhost`, `127.0.0.1`, `[::1]`, or `::1`. With a changed port, give that
same port when registering CUPS.

## Flatpak installation

Install Flatpak and the desktop portal through your distribution first.
The application bundle comes from GitHub; Flathub supplies its Freedesktop
runtime, not this application:

```sh
flatpak remote-add --user --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
flatpak install --user ./phomemo-printer-app_0.1.0_amd64.flatpak
flatpak run io.github.mabl.phomemo-printer-app
```

The launcher starts the server at `http://127.0.0.1:8631/` and requests
browser opening through the portal. Repeated launches open the same
supervised server. A rejected initial portal request stops the server it
started. The first terminal launch remains running; Ctrl-C stops it.

For a server without browser opening:

```sh
flatpak run --command=phomemo-printer-app io.github.mabl.phomemo-printer-app server
```

For status or discovery, use a second terminal:

```sh
flatpak run --command=phomemo-printer-app io.github.mabl.phomemo-printer-app status
flatpak run --command=phomemo-printer-app io.github.mabl.phomemo-printer-app devices
```

Stop using the desktop entry's **Stop Phomemo Server** action, or:

```sh
flatpak run --command=phomemo-printer-app io.github.mabl.phomemo-printer-app shutdown
```

The server does not autostart at login and is not a machine-wide service.
Do not run this package as root. Bluetooth pairing happens on the host;
the manifest allows RFCOMM sockets and scoped BlueZ discovery without
granting access to all devices or the full system bus. USB printing is
disabled.

The package has no general host-file access. To submit a PNG, pipe it:

```sh
flatpak run --command=phomemo-printer-app io.github.mabl.phomemo-printer-app submit -d PRINTER - < label.png
```

Alternatively upload it through the web interface. For runtime overrides,
launcher behavior, offline dependencies and local build instructions, see
[flatpak/README.md](../flatpak/README.md).

## First printer and CUPS setup

1. Power on the printer and load labels.
2. Pair and trust it in the host's Bluetooth settings. Close any vendor
   app that might hold its Bluetooth connection.
3. Start the selected package and open its web interface.
4. Add the printer if it was not added automatically; choose its model
   and the actual loaded label size.
5. Submit a label-sized PNG through the web interface.

The [model list](models.md) distinguishes hardware-tested M220 from other
model profiles. Report your package/version, distribution, model, firmware,
loaded labels and actual print results when testing another model.

For Snap, after creating a printer:

```sh
sudo phomemo-printer-app register-cups --queue phomemo --port 8000
lp -d phomemo -o media=Custom.40x30mm label.png
```

The example media is for 40 × 30 mm labels; replace it with your loaded
size. Register only after starting the server, and use its configured port.

For Flatpak, add an IPP printer in the host's printer settings or run
`lpadmin` **on the host** with the printer URI displayed by the application:

```sh
sudo lpadmin -p phomemo -E -v 'ipp://127.0.0.1:8631/ipp/print/PRINTER' -m everywhere
lp -d phomemo -o media=Custom.40x30mm label.png
```

Replace `PRINTER` and the media with the actual values. The Flatpak must
remain running while CUPS prints. Its CLI does not support
`register-cups`/`unregister-cups`; it bundles libcups, not host administration
utilities. PDFs should be printed through CUPS.

Snap, Flatpak and native installations have independent saved printers.
Configure one queue per physical printer: the printer accepts only one
Bluetooth connection at a time. Stop other installations before using it.

## Upgrade, stop and remove

GitHub-downloaded bundles have manual upgrades. Store-based automatic
application updates are not configured.

For Snap, download and verify the new bundle, then:

```sh
sudo snap install --dangerous ./phomemo-printer-app_NEWVERSION_amd64.snap
sudo snap restart phomemo-printer-app.daemon
```

Configuration, printers, spool and certificates are retained under
`/var/snap/phomemo-printer-app/common`. To stop without uninstalling:

```sh
sudo snap stop --disable phomemo-printer-app.daemon
# Start it again:
sudo snap start --enable phomemo-printer-app.daemon
```

For Flatpak, stop the application, download and verify the new bundle,
then reinstall it:

```sh
flatpak run --command=phomemo-printer-app io.github.mabl.phomemo-printer-app shutdown
flatpak install --user --reinstall ./phomemo-printer-app_NEWVERSION_amd64.flatpak
flatpak run io.github.mabl.phomemo-printer-app
```

Flatpak data is retained in `~/.var/app/io.github.mabl.phomemo-printer-app/`.

Before uninstalling, remove any host CUPS queue you created:

```sh
sudo lpadmin -x phomemo
# Choose the installed format:
sudo snap remove phomemo-printer-app
flatpak uninstall --user io.github.mabl.phomemo-printer-app
```

Snap removal normally creates a data snapshot; `--purge` removes without
one. Flatpak removal keeps app data unless `--delete-data` is requested.

## Building and releasing

The [Packages workflow](../.github/workflows/packaging.yml) builds natively
on `ubuntu-24.04` (AMD64) and `ubuntu-24.04-arm` (ARM64), on pull requests,
`main`/`packaging/**` pushes, manual runs and `v*` tags. Builds include:

- Source-built application and shared PAPPL 1.4.12; no host/Nix executable
  is copied into the package.
- Runtime tests against the packaged binary, plus tests through the
  installed, sandboxed entry points.
- Flatpak desktop portal, repeated launch, Stop and persistence tests at
  the default paths, using an isolated fake portal service.
- Snap registration/unregistration against a host CUPS scheduler with a
  dummy printer; no physical print jobs.
- Locked, offline Cargo inputs for Flatpak, checked against `Cargo.lock`.

PAPPL 1.x has no public CLI socket-path callback. `c/main.c` interposes its
exported `_papplMainloopGetServerPath` helper to separate the runtime socket
from state/configuration. Builds need shared PAPPL with interposable
symbols; static or `-Bsymbolic` PAPPL builds are unsupported. This is why
real-server tests run for each package build. The native fallback follows
ordinary Linux PAPPL paths, rather than a custom `PAPPL_SOCKDIR` build.

Relevant local checks:

```sh
nix develop --command make check
nix build -L .#checks.x86_64-linux.runtime
python3 flatpak/update-sources.py --check
python3 flatpak/test-sources.py
python3 scripts/package_workflow_tests.py
python3 scripts/package_workflow_runtime_tests.py
```

For Snap, run `snapcraft --platform=amd64` on a Snapcraft-capable AMD64
Linux host. For Flatpak, follow the SDK installation instructions in
[flatpak/README.md](../flatpak/README.md) and build into a fresh directory
outside the checkout with `flatpak/build-bundle.sh`.

To publish, first update the application version in
`phomemo-pappl/Cargo.toml` and the matching Cargo lock entry, validate the
packages, then push a matching `vVERSION` tag. The tag workflow also runs
the native Nix checks. It publishes the four tested bundles and
`SHA256SUMS` only after every required job passes. Publication starts as a
draft; retrying verifies remote assets and never overwrites a mismatched
published release. No Snap Store or Flathub application publication is
performed.

Hardware printing, real desktop background behavior and package upgrades
must be validated separately before recommending the packages broadly.
