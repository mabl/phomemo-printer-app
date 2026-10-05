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

The driver reads a normalized media catalog generated from reference media
definition JSON files.

Regenerate it after updating those inputs:

```bash
python scripts/generate_media_catalog.py \
  --local-paper ../reference-data/localPaper.json \
  --default-printer ../reference-data/DefaultPrinter.json \
  --default-type-group ../reference-data/DefaultTypeGroup.json
```

Generated file:

- `phomemo-protocol/data/media_catalog.json`

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
