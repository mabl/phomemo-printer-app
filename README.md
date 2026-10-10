# Phomemo Printer App for Linux

Print labels from Linux to a Phomemo Bluetooth printer. Set up your printer
and label size in a web browser, then print images directly or use CUPS/IPP
to print from other applications without a vendor driver.

Create your label designs in another application; this app handles printer
setup and printing, not label design. It uses
[PAPPL](https://www.msweet.org/pappl/) and Bluetooth Classic SPP over RFCOMM
to present printers as IPP Everywhere printers.

## Supported printers

**The M220 is hardware-tested.** All [listed model profiles](docs/models.md)
are expected to work based on the vendor's commands and shared protocol;
reports from other printers are welcome. The list covers the D30 family
(12 mm print heads), M110/M120 families (48 mm), and M200/M220 families
(72 mm). Select the actual loaded label size, not the print-head width.

## Install

Choose an installation for your Linux system:

| Installation | How it runs | Instructions |
| --- | --- | --- |
| Snap (AMD64/ARM64) | Persistent system service | [Snap installation](docs/packages.md#snap-installation) |
| Flatpak (experimental, AMD64/ARM64) | User-session server; launcher opens your browser | [Flatpak installation](docs/packages.md#flatpak-installation) |
| NixOS | System service managed by a NixOS module | [NixOS installation](docs/install.md#nixos) |
| Nix or source build | Foreground server or native systemd service | [Build and install](docs/install.md#nix-or-source-build) |

Download versioned packages and SHA-256 checksums from
[GitHub Releases](https://github.com/mabl/phomemo-printer-app/releases).
Development packages are also available as
[CI workflow artifacts](https://github.com/mabl/phomemo-printer-app/actions/workflows/packaging.yml).
See [downloads and checksums](docs/packages.md#downloads-and-checksums).

Bluetooth printing from the Snap and Flatpak packages has **not yet been
hardware-tested**. The [package guide](docs/packages.md) provides setup
instructions; installation and printing feedback is welcome.

## Print your first label

1. **Power on the printer and load labels.** Create a label-sized PNG or
   JPEG in your preferred design application.
2. **Pair and trust the printer on the Linux host**, using Bluetooth
   settings or `bluetoothctl` (`scan on`, `pair ADDRESS`, `trust ADDRESS`).
   Close any vendor app that might hold its Bluetooth connection.
3. **Start your installed app or service and open its web interface:**

   | Installation | Start | Default web interface |
   | --- | --- | --- |
   | Snap | Service starts automatically after installation | `http://localhost:8000/` |
   | Flatpak | `flatpak run io.github.mabl.phomemo-printer-app` | `http://127.0.0.1:8631/` |
   | NixOS / native systemd | Enabled service starts automatically; native setup uses `sudo systemctl enable --now phomemo-printer-app` | `http://localhost:8000/` |
   | Nix / source, foreground | [Start the built binary](docs/install.md#run-a-foreground-server) with a fixed port | `http://localhost:8000/` with the guide's command |

   Use the configured port if you changed it. Keep a foreground or Flatpak
   server running while you print.
4. **Add the printer if it was not added automatically.** Choose its model
   and select **Loaded Media** and the appropriate **Tracking** setting on
   its **Media Setup** page. A server without
   saved printers automatically adds supported printers it finds at startup.
5. **Submit the image through the web interface.** Print PDFs through CUPS
   instead, as described below.

Use one application queue per physical printer: it accepts only one
Bluetooth connection at a time. Stop other installations using it.
[Package-specific CLI commands](docs/packages.md#first-printer-and-cups-setup)
and [native CLI examples](docs/configuration.md#printer-and-job-commands)
are available if you prefer the terminal.

## Print through CUPS

After adding your printer, add its IPP queue to the host's CUPS scheduler
to print from desktop applications or use `lp`, including for PDFs.
[Snap and Flatpak CUPS setup](docs/packages.md#first-printer-and-cups-setup)
has different commands: Snap administration uses `sudo`; Flatpak uses host
printer settings or host `lpadmin`.

For a **native installation**, with the server running and the CUPS client
tools (`lpstat`, `lpadmin`) installed:

```sh
phomemo-printer-app register-cups --queue phomemo --port 8000
lp -d phomemo -o media=Custom.40x30mm label.pdf
# To remove the CUPS queue:
phomemo-printer-app unregister-cups --queue phomemo
```

Replace the media size with your loaded labels and the port with your
server's fixed port. CUPS administration requires the appropriate host
permissions. `register-cups` reads the caller's environment, not the
service's configuration; pass matching host/TLS settings if changed. See
[CUPS configuration](docs/configuration.md#cups-registration).

## Edge-to-edge labels

On the M220 with 40 × 30 mm labels, a 44 × 34 mm design with 2 mm of bleed
can extend backgrounds beyond the label edges. The driver prints at 1:1
and drops the unprinted bleed. See the [overprint guide](docs/overprint.md)
for design instructions, a template and hardware validation. Older CUPS
queues need `register-cups --replace` to offer the overprint size.

## Help and configuration

- **Printer missing?** It must be paired with BlueZ and have a recognized
  model name/alias, serial number or a name containing the word `Phomemo`. See
  [troubleshooting](docs/configuration.md#troubleshooting).
- **Continuous roll?** Direct image printing needs a label size with a
  definite length; use CUPS to render pages for continuous media. See
  [known limitations](docs/configuration.md#known-limitations).
- **Change ports, logging, Bluetooth channels or remote access:** see
  [configuration](docs/configuration.md),
  [native service setup](docs/install.md#the-native-systemd-service) or
  [NixOS options](docs/install.md#nixos-options). For package configuration,
  use the [package guide](docs/packages.md).

Please [report successes or problems](https://github.com/mabl/phomemo-printer-app/issues/new)
with your printer model, Linux distribution, installation/package version,
loaded label dimensions and print results. Include firmware if known.

## Upgrading

See the [upgrade and migration guide](docs/upgrading.md) for native service
state migration, darkness defaults and overprint queues, or
[package upgrades](docs/packages.md#upgrade-stop-and-remove) for Snap/Flatpak.

## Development

```sh
nix develop
make check     # fmt-check, clippy, tests, C with -Werror, and the build
```

See [development checks and media catalog maintenance](docs/install.md#development-and-maintenance)
for individual targets and Nix runtime/module checks, and
[package build and release checks](docs/packages.md#building-and-releasing).
Measured M220 positioning and leading-edge bleed experiments are recorded
in [docs/m220-positioning.md](docs/m220-positioning.md).
