# Upgrading and migration

For Snap and experimental Flatpak, follow
[package upgrades, stopping and removal](packages.md#upgrade-stop-and-remove).
This guide covers native service migration and printer settings that may
need updating after an application upgrade. For NixOS, update the flake
input and rebuild your system; service settings remain in the
[NixOS module options](install.md#nixos-options).

## Native service upgrades

Build as yourself before installing. For a current source-installed
service, stop it, install the updated binary/unit and restart it:

```sh
make
sudo systemctl stop phomemo-printer-app
sudo make install-systemd
sudo systemctl daemon-reload
sudo systemctl start phomemo-printer-app
```

Use the same installation directory settings as before. The configuration
file `/etc/default/phomemo-printer-app` is kept. If you have an earlier
unit in `/etc/systemd/system`, use the migration below instead.

## Native service migration

Earlier versions installed the unit as
`/etc/systemd/system/phomemo-printer-app.service`, which overrides the one
`install-systemd` now installs (`make install-systemd` warns about it), and
kept state as root in `/var/lib/phomemo-printer-app.state`.

For that older layout, upgrade in this order so the old unit cannot save
over the state while it moves. Systemd then hands the directory to the
service's dynamic user:

```sh
make
sudo systemctl disable --now phomemo-printer-app
sudo rm /etc/systemd/system/phomemo-printer-app.service
sudo make install-systemd
sudo install -d -m 0700 /var/lib/phomemo-printer-app
sudo mv /var/lib/phomemo-printer-app.state /var/lib/phomemo-printer-app/
sudo systemctl daemon-reload
sudo systemctl enable --now phomemo-printer-app
```

The configuration file `/etc/default/phomemo-printer-app` is kept. With
remote logins configured in it, install the
[root drop-in](install.md#remote-logins-and-the-root-drop-in) as well.

## Print darkness and speed

Print darkness now follows PAPPL's semantics: the printer's darkness
(`printer-darkness-configured`, 0–100%, set in the web interface) plus a
per-job offset (`print-darkness`, -100 to 100, default 0), mapped onto the
printer's 15 density levels.

Earlier builds used `print-darkness` as the density itself and saved a
default of 8, which now reads as an offset of +8%. Printers created by an
earlier build therefore print darker than intended. Reset each one once:
choose the darkness in its web interface and clear the saved offset,
which the web interface does not show:

```sh
phomemo-printer-app modify -d PRINTER -o print-darkness-default=0
```

Replace `PRINTER` with the application's printer queue name. This example
uses the native CLI; use the
[package-specific invocation](packages.md) for Snap or Flatpak.

Print speed now defaults to the printer's own setting ("Auto") instead of
level 3.

## Refresh CUPS queues for overprint

A CUPS queue created before overprint support must be recreated to offer
the larger page size. For a native installation:

```sh
phomemo-printer-app register-cups --queue phomemo --port 8000 --replace
```

Use your queue name, the server's fixed port and matching host/TLS
environment, as explained in
[CUPS registration](configuration.md#cups-registration). For packaged
installations, follow [package CUPS setup](packages.md#first-printer-and-cups-setup)
to recreate the host queue (Snap can use `--replace`; Flatpak uses host
printer settings or `lpadmin`). See the [overprint guide](overprint.md)
for label design instructions and hardware validation.
