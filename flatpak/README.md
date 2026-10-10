# Experimental GitHub Flatpak

This is the Flatpak half of the GitHub-distributed Snap + Flatpak packaging
experiment. App ID: **`io.github.mabl.phomemo-printer-app`**; branch:
**`experimental`**. This directory is for GitHub builds and releases, with no
Flathub submission or claim of Flathub eligibility. Flathub is used only as a
source of the Freedesktop runtime, SDK and Rust SDK extension.

## Launch, print, stop

Install the downloaded release bundle:

```sh
flatpak install --user /absolute/path/phomemo-printer-app_VERSION_amd64.flatpak
flatpak run io.github.mabl.phomemo-printer-app
```

Replace `VERSION` with the release version (currently `0.1.0`); aarch64 bundles
use the workflow architecture suffix `arm64`.

The desktop entry starts a **foreground, user-session server** and asks the
`org.freedesktop.portal.OpenURI` portal to open `http://127.0.0.1:8631/` in the
host's browser. It waits for the portal's actual response, including cancellation.
The initial launcher remains as the server supervisor. Another launch opens the
existing supervised server's website instead of starting a competing server.
The supervisor forces loopback/port/state/spool options for every packaged server,
then verifies the UNIX endpoint's kernel peer PID belongs to its own child before
attesting the selected port over its private control socket. PAPPL's deterministic
system UUID is **not** an identity proof. An unmanaged UNIX-only server plus an
independent TCP server with an identical UUID cannot satisfy this contract; the
launcher refuses to open a website. Port collisions and contract mismatches fail
closed.

**Closing the browser does not stop the server.** Use **Stop Phomemo Server** in
the desktop launcher's context menu, or:

```sh
flatpak run --command=phomemo-printer-app io.github.mabl.phomemo-printer-app shutdown
```

Stopping the initial terminal launch with Ctrl-C also stops its child server.
Supervisor termination forwards shutdown; Linux parent-death signalling prevents
an orphan if the supervisor is killed. A failed/cancelled first portal launch
stops and reaps only the server it started. A failed portal call on a repeated
launch leaves the existing server running. Emergency stop is
`flatpak kill io.github.mabl.phomemo-printer-app` (abrupt; prefer `shutdown`).
The wrapper supervises direct `server` starts as well. Stop/shutdown uses a
private, same-UID-checked control socket and signals the supervisor's own child,
then acknowledges after it exits. This also stops a newly installed server with
zero queues: PAPPL's raw Shutdown-All-Printers operation can otherwise return
success without terminating an empty server. No PID-file signalling is used.
Stop bypasses the startup lock. The supervisor services control requests during
both the asynchronous OpenURI method call and its pending response, closes the
portal request, and stops promptly even while a chooser is waiting for the user.

There is no autostart or Background portal permission. Launch once each session.
Some desktop portal implementations enforce a policy on applications without
windows; this foreground experiment needs testing on the supported desktops.
Browser/server lifecycle is deliberately explicit rather than coupled to a tab.

Pair the printer using the host's Bluetooth settings first. CLI invocations use
the same runtime, state and spool directories as the desktop launcher:

```sh
flatpak run --command=phomemo-printer-app io.github.mabl.phomemo-printer-app status
flatpak run --command=phomemo-printer-app io.github.mabl.phomemo-printer-app devices
flatpak run --command=phomemo-printer-app io.github.mabl.phomemo-printer-app printers
flatpak run --command=phomemo-printer-app io.github.mabl.phomemo-printer-app submit -d PRINTER - < /absolute/path/label.png
```

Piping a job into stdin avoids broad file access. The browser's normal file
chooser can upload a print job to the localhost web interface. The package does
not grant access to the host home directory. Explicit per-invocation file access
can be granted by the user when necessary.

For CUPS integration, create a printer in the running application's web UI, then
run these commands **on the host**, using the actual printer IPP URI shown by
the app (for example `ipp://127.0.0.1:8631/ipp/print/PRINTER`):

```sh
sudo lpadmin -p phomemo -E -v 'IPP_URI_FROM_THE_APP' -m everywhere
lp -d phomemo /absolute/path/label.png
# Remove the host queue when no longer needed:
sudo lpadmin -x phomemo
```

The application must be running while CUPS prints. Direct IPP clients and direct
`submit` work without a host CUPS queue. No CUPS daemon, `lpadmin`, host execution,
or in-sandbox `register-cups` is needed or supported by this packaging.

## Runtime contract and permissions

Defaults, set in `launcher.c` for **both** CLI and desktop:

| Setting | Default |
| --- | --- |
| `PHOMEMO_RUNTIME_DIRECTORY` | `$XDG_RUNTIME_DIR/app/io.github.mabl.phomemo-printer-app/r` |
| `PHOMEMO_STATE_FILE` | `$XDG_DATA_HOME/phomemo/printer.state` |
| `PHOMEMO_SPOOL_DIRECTORY` | `$XDG_DATA_HOME/phomemo/spool` |
| `PHOMEMO_SERVER_PORT` | `8631` |
| `PHOMEMO_LISTEN_HOSTNAME` | `127.0.0.1` |

Flatpak already maps the per-app runtime directory across invocations; using an
arbitrary directory directly under `$XDG_RUNTIME_DIR` would not achieve that.
Runtime/spool directories are private, owned by the current UID, mode 0700.
The complete runtime socket path must be at most **107 bytes** on Linux. An
explicit runtime override must already exist; invalid, long or final-symlink
directories fail rather than falling back to another server. The application
itself locks the runtime directory inode; the launcher also serializes startup
and portal work with a separate lock, without PID-file-based signalling. Stop
does not acquire that lock. Runtime/spool validation strips trailing slashes
before checking `O_NOFOLLOW`, so `symlink/` is also rejected.

State and spool files live in Flatpak's app-specific writable data area; this
package does not write into native configuration or service directories. Do not
run as root. Overrides for runtime/state/spool/port can be supplied via
`flatpak run --env=NAME=VALUE ...`; keep them identical for server and CLI.
For an interactive server without opening a browser, use:

```sh
flatpak run --command=phomemo-printer-app io.github.mabl.phomemo-printer-app server
```

Desktop launch forces loopback, its chosen port/state/spool and plain local HTTP
with explicit server options so saved PAPPL options cannot redirect the browser.
The CLI wrapper uses the same bound contract. It requires the sub-command first,
single-letter options after the command, and standalone `--help`/`--version`.
`shutdown` accepts no arguments. `server` accepts only `-o log-level=VALUE` and
`-o log-file=VALUE`; binding/state/spool overrides use the environment table above.
Options-before-command, clustered options, and noncanonical server/shutdown forms
are rejected rather than reaching PAPPL's permissive/quirky parser. Use explicit
`submit` for printing, and `submit -- FILENAME` for a final filename matching a
reserved sub-command. Native application parsing is unchanged.

The manifest grants exactly:

- `--allow=bluetooth`: RFCOMM sockets for the Bluetooth transport.
- `--system-talk-name=org.bluez`: paired-device discovery through BlueZ.
- `--share=network`: localhost web/IPP access and network printer transport.
- `--system-talk-name=org.freedesktop.Avahi`: PAPPL/CUPS DNS-SD discovery and
  advertisement through the host Avahi service. Only Avahi client libraries are
  built; no sandbox Avahi daemon or unrestricted system bus is used.

Portals are reachable through Flatpak's default filtered session bus. There is
no blanket host execution, session/system bus socket, home filesystem, CUPS
socket, or all-device grant. USB printing is disabled in these builds.

## Pinned inputs and offline Cargo

| Component | Source |
| --- | --- |
| Freedesktop Platform + SDK | supported `25.08` branch |
| Rust | `org.freedesktop.Sdk.Extension.rust-stable//25.08` |
| PAPPL 1.x | **1.4.12**, upstream release archive, SHA-256 pinned |
| libcups | **2.4.12**, OpenPrinting release archive, SHA-256 pinned |
| Avahi client | **0.8**, upstream release archive, SHA-256 pinned |
| cbindgen | **0.29.2**, crates.io archive, SHA-256 pinned; its own Cargo.lock |
| Isolated test bus | **D-Bus 1.16.2**, checksummed release; daemon only, build-test-only, removed from the bundle |
| Cargo generator | official `flatpak/flatpak-builder-tools` revision **`74697c75b630d7330e77250fc13cb5ea688d9479`**, script SHA-256 pinned |

PAPPL is **shared**, with static installation disabled and no `-Bsymbolic`
binding. `c/main.c` interposes PAPPL 1.x's exported private
`_papplMainloopGetServerPath` function to select the package socket. This is
version-sensitive: real-server runtime tests must run for every package build.
PAPPL 2 is a different ABI and is not a compatible update.

`cargo-sources.json` and `cbindgen-sources.json` contain official-generator
archive sources, lockfile crate checksums, checksum-file descriptions and Cargo
source replacement configuration. They do **not** contain vendored crate code.
`sources-lock.json` records generator provenance and hashes of both lock inputs
and generated outputs. The checker compares every locked registry crate's URL,
version, checksum and vendor destination; changed locks fail the build. New
git/private sources are rejected until explicitly supported.

After changing Cargo.lock or the cbindgen source pin, run with Python >=3.11 and
`uv` installed:

```sh
python3 /absolute/checkout/flatpak/update-sources.py --update
python3 /absolute/checkout/flatpak/update-sources.py --check
```

The update step downloads the checksummed cbindgen crate to recover its lock and
the pinned official generator; it uses fixed Python dependency versions via uv.
The check step needs only Python's standard library and **no network**. Normal
package builds never run the generator or fetch crates through Cargo.

## CI build and GitHub release handoff

Use a Linux runner with working Flatpak/bubblewrap user namespaces and
`flatpak-builder` (1.4.x), Python >=3.11. Install the matching architecture's SDK
and runtime first:

```sh
flatpak remote-add --user --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
flatpak install --user --noninteractive flathub org.freedesktop.Platform//25.08 org.freedesktop.Sdk//25.08 org.freedesktop.Sdk.Extension.rust-stable//25.08
sh /absolute/checkout/flatpak/build-bundle.sh /absolute/fresh/output-directory
```

The script validates Cargo inputs, downloads all manifest sources, then performs
a separate `--disable-download` build with no network build permission. Both
Rust modules use frozen/offline Cargo; the application is built through the
existing Makefile. The build-only `cargo-offline` executable wrapper preserves
frozen Cargo while giving cbindgen a valid executable path in `CARGO`. The build
uses `--disable-rofiles-fuse`, avoiding a dependency on a container's `/dev/fuse`.
Build tests are enabled in the manifest. The offline source
checker first runs regression cases for changed locks, missing crates, modified
inline checksums, changed cbindgen pins and unsupported git dependencies. A linkage
check then verifies the package loads shared PAPPL, CUPS and Avahi from `/app/lib`,
rather than silently selecting SDK/runtime copies (CUPS otherwise defaults to
`lib64` on this platform). Then:

1. `scripts/test_packaged_runtime.py --binary /app/libexec/phomemo-printer-app`:
   real package binary/server/CLI, persistence, locking, invalid paths, duplicate
   listeners and disabled private-server fallback. The optional native systemd
   test is skipped in this sandbox.
2. `flatpak/test-launcher.py --launcher /app/bin/phomemo-launcher --binary /app/libexec/phomemo-printer-app --dbus-daemon /app/libexec/flatpak-tests/dbus-daemon`:
   real launcher plus real server and an isolated test OpenURI service;
   concurrent/repeated launches, CLI queue operations, desktop Stop, denied or
   missing/pending portal, deterministic UUID collision between two real PAPPL
   servers, canonical grammar, trailing-slash symlinks, port collision, and
   SIGTERM/SIGKILL cleanup. SDK 25.08 does not supply `dbus-daemon`: the manifest
   explicitly builds a pinned daemon with service activation disabled, uses it
   for the tests, and removes it plus its debug binary at cleanup. The isolated
   test-bus configuration has no service directories and cannot activate the
   host portal.

Publish the resulting `phomemo-printer-app_VERSION_amd64.flatpak` or
`phomemo-printer-app_VERSION_arm64.flatpak` as a GitHub release
artifact, along with its SHA-256 from the script output. Build x86_64 first;
aarch64 needs a native runner or separately validated emulation. Keep the
Snap job as the other GitHub package; this script does not manage either
workflow or publish anything. Record the installed runtime/SDK/extension OSTree
commits in CI logs if exact toolchain reproducibility is required: supported
runtime branches receive updates and are not immutable source pins.

### Installed default OpenURI test handoff

The workflow owner can compile the existing `flatpak/test-portal.c` on the host
with a host C compiler, `pkg-config`, and GLib/GIO development headers:

```sh
cc /absolute/checkout/flatpak/test-portal.c -o /absolute/test-output/phomemo-test-portal $(pkg-config --cflags --libs gio-2.0)
```

Inside an **isolated** session bus, start that utility with
`--log /absolute/test-output/portal.log --mode success`. Wait for its stdout
`ready` line before invoking the installed application's **default**
`flatpak run --user io.github.mabl.phomemo-printer-app` entry point. The utility
records the exact requested URI and sends the asynchronous portal response; it
does not execute a browser. Its `--mode pending` and `--mode pending-method`
variants permit installed Stop-during-prompt checks, and `--mode refuse` tests
cancellation. It exits rather than impersonating an already-owned portal name.
Keep the utility on the host bus, while the installed launcher runs behind
Flatpak's normal filtered portal access. Repeated launches and CLI shutdown must
use the same private runtime/port overrides; Stop must complete before the
pending request deadline. No test portal or D-Bus daemon belongs in the installed
bundle. The build tests supply their own no-service-directory bus; the installed
test should do likewise and seed an empty state file to avoid hardware discovery.

Before calling the package usable, CI must pass an actual Flatpak build and a
desktop install smoke test; local helper tests are not a substitute. On a real
desktop verify portal browser opening, cross-invocation socket sharing, the Stop
action, Bluetooth discovery/printing, host CUPS printing, and restart persistence.
Host BlueZ/Avahi availability, Bluetooth adapter permissions, portal background
policies and the private PAPPL helper are the main integration risks.
