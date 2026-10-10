#!/usr/bin/env python3
"""Test installed package bytes and real package launchers without hardware.

The shared regression suite needs a copyable executable named pm-runtime. Run
it through the installed package's ELF loader/libraries on the host, then check
web, IPP, CLI and restart/persistence through the actual confined package entry
point. Desktop portal tests use an isolated test service, never a real browser.
No physical Bluetooth printer or printing is required.
"""

import argparse
import os
from pathlib import Path
import platform
import shlex
import socket
import struct
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

from package_workflow_desktop import installed_desktop


SNAP = "phomemo-printer-app"
FLATPAK = "io.github.mabl.phomemo-printer-app"


def run(command, **kwargs):
    kwargs.setdefault("timeout", 60)
    return subprocess.run(command, check=True, **kwargs)


def flatpak_info(*args):
    return run(["flatpak", "info", "--user", *args, FLATPAK], capture_output=True, text=True).stdout.strip()


def check_ipp(port):
    def attribute(tag, name, value):
        name, value = name.encode(), value.encode()
        return bytes([tag]) + struct.pack("!H", len(name)) + name + struct.pack("!H", len(value)) + value

    # IPP/2.0 Get-System-Attributes (0x005b), operation attributes, end tag.
    body = struct.pack("!BBHI", 2, 0, 0x005B, 1) + b"\x01"
    body += attribute(0x47, "attributes-charset", "utf-8")
    body += attribute(0x48, "attributes-natural-language", "en")
    body += attribute(0x45, "system-uri", f"ipp://127.0.0.1:{port}/ipp/system")
    body += attribute(0x44, "requested-attributes", "system-uuid") + b"\x03"
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}/ipp/system", data=body,
        headers={"Content-Type": "application/ipp"}, method="POST",
    )
    with urllib.request.urlopen(request, timeout=5) as response:
        payload = response.read()
        if response.status != 200 or response.headers.get_content_type() != "application/ipp":
            raise RuntimeError("The confined server did not return an IPP response")
    if len(payload) < 8:
        raise RuntimeError("Truncated IPP response")
    _, _, status, request_id = struct.unpack("!BBHI", payload[:8])
    if status > 0x0002 or request_id != 1 or b"system-uuid" not in payload:
        raise RuntimeError(f"IPP Get-System-Attributes failed: status={status:#06x}")


def installed_layout(kind):
    machine = platform.machine()
    triplets = {"x86_64": "x86_64-linux-gnu", "aarch64": "aarch64-linux-gnu"}
    loaders = {"x86_64": "ld-linux-x86-64.so.2", "aarch64": "ld-linux-aarch64.so.1"}
    if machine not in triplets:
        raise ValueError(f"Unsupported native architecture: {machine}")
    triplet = triplets[machine]
    if kind == "snap":
        if os.geteuid() != 0:
            raise ValueError("Run Snap package tests as root, like the packaged service")
        app = Path(f"/snap/{SNAP}/current")
        runtime = Path("/snap/core24/current")
        binary = app / "usr/bin/phomemo-printer-app"
        roots = [app / "usr", app, runtime / "usr", runtime]
    else:
        app = Path(flatpak_info("--show-location")) / "files"
        runtime_ref = flatpak_info("--show-runtime")
        runtime_result = subprocess.run(
            ["flatpak", "info", "--user", "--show-location", runtime_ref],
            capture_output=True, text=True, timeout=60, check=False,
        )
        # A user-installed bundle can reuse the system runtime that the build
        # action installed. Prefer a user deployment when present, like Flatpak.
        if runtime_result.returncode:
            runtime_result = run(
                ["flatpak", "info", "--system", "--show-location", runtime_ref],
                capture_output=True, text=True,
            )
        runtime = Path(runtime_result.stdout.strip()) / "files"
        binary = app / "libexec/phomemo-printer-app"
        roots = [app, runtime]
    binary = binary.resolve(strict=True)
    library_paths = [
        root / suffix
        for root in roots
        for suffix in (f"lib/{triplet}", "lib", "lib64")
        if (root / suffix).is_dir()
    ]
    # Always use the package runtime's libc and loader, even when it is newer
    # than the host Ubuntu. Avoid accidentally testing the host's libpappl.
    loader = next(
        (directory / loaders[machine] for directory in library_paths
         if str(directory).startswith(str(runtime)) and (directory / loaders[machine]).is_file()),
        None,
    )
    if loader is None:
        raise ValueError(f"No {machine} ELF loader in installed runtime {runtime}")
    return binary, loader, library_paths


def regression_suite(kind, suite, version):
    binary, loader, library_paths = installed_layout(kind)
    prefix = [str(loader), "--library-path", ":".join(map(str, library_paths))]
    result = run([*prefix, str(binary), "--version"], capture_output=True, text=True)
    if result.stdout.strip() != version:
        raise ValueError(f"Installed {kind} binary version {result.stdout!r} != {version!r}")
    with tempfile.TemporaryDirectory(prefix="pm-package-", dir="/tmp") as temporary:
        wrapper = Path(temporary) / "pm-runtime"
        command = [*prefix, "--argv0", "pm-runtime", str(binary)]
        wrapper.write_text("#!/bin/sh\nexec " + shlex.join(command) + ' "$@"\n')
        wrapper.chmod(0o755)
        print(f"Running shared regression suite against installed {kind}: {binary}", flush=True)
        run(["python3", str(suite), "--binary", str(wrapper)], timeout=300)


def package_smoke(kind, version):
    if kind == "snap":
        data = Path(f"/var/snap/{SNAP}/common")
        prefix = ["snap", "run", SNAP]
    else:
        data = Path.home() / ".var/app" / FLATPAK / "data"
        prefix = ["flatpak", "run", "--user", "--command=phomemo-printer-app"]
    data.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="ci-", dir=data) as temporary:
        root = Path(temporary)
        runtime = root / "r"
        runtime.mkdir(mode=0o700)
        state = root / "printer.state"
        state.write_text("NextPrinterID 1\n")
        spool = root / "spool"
        spool.mkdir(mode=0o700)
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        overrides = {
            "PHOMEMO_RUNTIME_DIRECTORY": str(runtime),
            "PHOMEMO_STATE_FILE": str(state),
            "PHOMEMO_SPOOL_DIRECTORY": str(spool),
            "PHOMEMO_SERVER_PORT": str(port),
            "PHOMEMO_LISTEN_HOSTNAME": "127.0.0.1",
            "PHOMEMO_TLS_ONLY": "false",
            "PHOMEMO_LOG_FILE": "-",
        }
        env = dict(os.environ, **overrides)
        if kind == "flatpak":
            prefix += [f"--env={key}={value}" for key, value in overrides.items()]
            prefix += [FLATPAK]
        else:
            # Snap's command chain deliberately ignores runtime/state/port
            # environment overrides. Exercise its real system daemon and
            # snapctl configuration, with the service stopped before seeding.
            state = data / "state/phomemo-printer-app.state"
            state.parent.mkdir(mode=0o700, exist_ok=True)
            state.write_text("NextPrinterID 1\n")
            run(["snap", "set", SNAP, f"port={port}", "listen-hostname=127.0.0.1"])

        def cli(*args):
            return run([*prefix, *args], env=env, capture_output=True, text=True).stdout

        if cli("--version").strip() != version:
            raise ValueError("The confined package entry point reports an incorrect version")
        log_path = root / "server.log"
        server = None

        def stop_server():
            nonlocal server
            if server is None:
                return
            if kind == "snap":
                run(["snap", "stop", "--disable", f"{SNAP}.daemon"], timeout=120)
                server = None
                return
            try:
                cli("shutdown")
                server.wait(timeout=20)
                if server.returncode != 0:
                    raise RuntimeError(f"Packaged server shutdown failed: {log_path.read_text()}")
            finally:
                if server.poll() is None:
                    server.terminate()
                    try:
                        server.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        server.kill()
                        server.wait()
                server = None

        def start_server():
            nonlocal server
            if kind == "snap":
                run(["snap", "start", "--enable", f"{SNAP}.daemon"])
                server = True
            else:
                with log_path.open("a") as log:
                    server = subprocess.Popen(
                        [*prefix, "server"], env=env, stdin=subprocess.DEVNULL,
                        stdout=log, stderr=log, umask=0o077,
                    )
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                if kind == "flatpak" and server.poll() is not None:
                    raise RuntimeError(f"Packaged server exited: {log_path.read_text()}")
                try:
                    with urllib.request.urlopen(f"http://127.0.0.1:{port}/", timeout=1) as response:
                        page = response.read()
                        if response.status == 200 and b"<html" in page.lower():
                            break
                except (urllib.error.URLError, TimeoutError):
                    pass
                time.sleep(0.2)
            else:
                if kind == "snap":
                    run(["snap", "logs", "-n", "100", f"{SNAP}.daemon"])
                    raise RuntimeError("Packaged Snap web UI did not become ready (see daemon logs)")
                raise RuntimeError(f"Packaged web UI did not become ready: {log_path.read_text()}")
            # Independently verify TCP IPP and the shared local CLI socket.
            check_ipp(port)
            if "Running," not in cli("status"):
                raise RuntimeError("Separate package CLI invocation cannot find its server")

        try:
            print(f"Testing actual {kind} launcher: web, IPP, CLI, restart/persistence", flush=True)
            start_server()
            cli("add", "-d", "CI-Dummy", "-m", "phomemo_m220", "-v", "socket://127.0.0.1:9")
            cli("default", "-d", "CI-Dummy")
            if "CI-Dummy" not in cli("printers") or cli("default").strip() != "CI-Dummy":
                raise RuntimeError("Dummy queue/default was not created")
            if kind == "snap":
                snap_cups_smoke(cli, port)
            stop_server()
            if not state.is_file() or "CI-Dummy" not in state.read_text():
                raise RuntimeError("Confined server did not persist queue state in the requested directory")
            start_server()
            if "CI-Dummy" not in cli("printers") or cli("default").strip() != "CI-Dummy":
                raise RuntimeError("Confined server did not restore its queue/default after restart")
        finally:
            stop_server()


def snap_cups_smoke(cli, port):
    """Real host CUPS queues, one through the Snap CLI, with no print jobs."""
    registered, exact = "package-ci-registered", "package-ci-exact"
    env = dict(os.environ, CUPS_SERVER="/run/cups/cups.sock", LC_ALL="C")

    def host(*args, check=True):
        return subprocess.run(args, env=env, check=check, capture_output=True, text=True, timeout=30)

    host("lpstat", "-r")
    if any(host("lpstat", "-p", queue, check=False).returncode == 0 for queue in (registered, exact)):
        raise RuntimeError("Refusing to replace existing host CUPS test queues")
    try:
        cli("register-cups", "--queue", registered, "--port", str(port))
        alias = f"ipp://127.0.0.1:{port}/ipp/print"
        if host("lpstat", "-v", registered).stdout.strip().rsplit(" ", 1)[-1] != alias:
            raise RuntimeError("Snap register-cups did not configure the intended host CUPS URI")
        uri = f"ipp://127.0.0.1:{port}/ipp/print/CI-Dummy"
        host("lpadmin", "-p", exact, "-E", "-v", uri, "-m", "everywhere")
        if host("lpstat", "-v", exact).stdout.strip().rsplit(" ", 1)[-1] != uri:
            raise RuntimeError("Host CUPS did not retain the exact dummy printer IPP URI")
        cli("unregister-cups", "--queue", registered)
        if host("lpstat", "-p", registered, check=False).returncode == 0:
            raise RuntimeError("Snap unregister-cups did not remove its host CUPS queue")
    finally:
        # Only the two CI-owned queues, never the host's default queue.
        host("lpadmin", "-x", registered, check=False)
        host("lpadmin", "-x", exact, check=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind", choices=("snap", "flatpak"))
    parser.add_argument("--suite", type=Path, required=True)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    regression_suite(args.kind, args.suite.resolve(strict=True), args.version)
    package_smoke(args.kind, args.version)
    if args.kind == "flatpak":
        installed_desktop(Path(__file__).resolve().parents[1] / "flatpak/test-portal.c", check_ipp)


if __name__ == "__main__":
    main()
