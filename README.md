# Phomemo Printer Application

Native PAPPL printer application for Phomemo Bluetooth label printers.

## Build

The Nix flake provides a dev shell with the full toolchain (Rust, clippy,
rustfmt, cbindgen, PAPPL 1.x, a C compiler):

```bash
nix develop
make            # build ./phomemo-printer-app
make check      # fmt-check, clippy, tests, C -Werror, build (what CI runs)
```

Other targets: `make fmt`, `make fmt-check`, `make lint` (clippy),
`make c-lint` (C sources with `-Werror`), `make test`, `make clean`.

Without Nix you need:

- Rust >= 1.87 (cargo, rustc; plus clippy and rustfmt for `make check`)
- cbindgen
- a C compiler, GNU make and pkg-config
- PAPPL 1.x development files (found via `pkg-config pappl`)

## Media catalog generation

The driver reads a normalized media catalog generated from the media
definitions bundled with the Print Master Android app: `localPaper.json`,
`DefaultPrinter.json` and `DefaultTypeGroup.json` from the APK's `assets/`
directory.

Regenerate it after updating those inputs, passing the directory that holds
them:

```bash
python3 scripts/generate_media_catalog.py --reference-dir path/to/assets
```

The output defaults to `phomemo-protocol/data/media_catalog.json` (override
with `--out`); it records each input's file name and SHA-256, not its local
path.

## Run locally

```bash
./phomemo-printer-app server
```

Default runtime behavior:

- listen host: `localhost`
- port: auto-selected free port (PAPPL default)
- no PAM auth in local mode

Override runtime values with environment variables:

- `PHOMEMO_SERVER_PORT`
- `PHOMEMO_LISTEN_HOSTNAME`
- `PHOMEMO_AUTH_SERVICE`
- `PHOMEMO_ADMIN_GROUP`
- `PHOMEMO_LOG_FILE`
- `PHOMEMO_LOG_LEVEL` (`debug|info|warn|error|fatal`)
- `PHOMEMO_SPOOL_DIRECTORY`
- `PHOMEMO_TLS_ONLY` (`0|1|true|false|yes|no`)
- `PHOMEMO_BT_CHANNELS`: the RFCOMM channels to try, comma-separated
  (default `1`); a device URI can name one with `?channel=N`

## Register with local CUPS

Create an IPP Everywhere queue pointed at the local app:

```bash
./phomemo-printer-app register-cups --queue phomemo --port 8000
```

Note: when server port is auto-selected, `register-cups` requires an explicit
`--port` value (or `PHOMEMO_SERVER_PORT`) so the queue URI is stable.

Recreate an existing queue:

```bash
./phomemo-printer-app register-cups --queue phomemo --port 8000 --replace
```

Remove the queue:

```bash
./phomemo-printer-app unregister-cups --queue phomemo
```

## Install binary and systemd unit

Install binary:

```bash
sudo make install PREFIX=/usr/local
```

Install unit and default env file:

```bash
sudo make install-systemd PREFIX=/usr/local
```

If you install to a different prefix, update `ExecStart` in
`systemd/phomemo-printer-app.service` before installing the unit.

Enable and start service:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now phomemo-printer-app.service
```

Default service config file path:

- `/etc/default/phomemo-printer-app`

You can edit that file and restart the service to change listen/auth/log/spool settings.

## Known limitations

- Images (PNG, JPEG) submitted directly cannot be printed on a continuous
  roll (a media size 0 mm long): PAPPL rasterizes an image onto the media
  size and refuses a page without length ("Invalid media size"). Choose a
  label size for such jobs, e.g. `-o media=om_40x30mm_40x30mm`, or print
  through CUPS, which renders pages of a definite length.
- `shutdown` returns at once, but the server exits only when PAPPL's main
  loop next wakes, up to 30 seconds later. `SIGTERM` stops it at once.
- Use one queue per printer. A Bluetooth printer takes one connection at a
  time, so a second queue for the same address waits for the first one's
  device for up to 5 seconds, stalling its web and IPP requests, and then
  fails with "is another queue using the same printer?".
- Raw jobs (`application/vnd.phomemo-raw`) end as soon as they are sent:
  only raster jobs wait for the printer to report each page printed.

## Upgrading

Print darkness now follows PAPPL's semantics: the printer's darkness
(`printer-darkness-configured`, 0-100 %, set in the web interface) plus a
per-job offset (`print-darkness`, -100 to 100, default 0), mapped onto the
printer's 15 density levels. Earlier builds used `print-darkness` as the
density itself and saved a default of 8, which now reads as an offset of
+8 %, so printers created by an earlier build print darker than intended.
Reset each one once: choose the darkness in its web interface, and clear
the saved offset, which the web interface does not show:

```bash
./phomemo-printer-app modify -d PRINTER -o print-darkness-default=0
```

Print speed now defaults to the printer's own setting ("Auto") instead of
level 3.
