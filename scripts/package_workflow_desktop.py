#!/usr/bin/env python3
"""Installed Flatpak default desktop/CLI contract on an isolated test bus."""

import os
from pathlib import Path
import select
import shlex
import socket
import subprocess
import tempfile
import time


APP = "io.github.mabl.phomemo-printer-app"
PORT = 8631


def clean_environment():
    return {
        key: value for key, value in os.environ.items()
        if not key.startswith("PHOMEMO_") and key not in (
            "SNAP_COMMON", "RUNTIME_DIRECTORY", "DBUS_SESSION_BUS_ADDRESS",
            "DBUS_SESSION_BUS_PID", "DBUS_STARTER_ADDRESS", "DBUS_STARTER_BUS_TYPE",
        )
    }


def ready_line(process):
    if not select.select([process.stdout], [], [], 10)[0]:
        raise RuntimeError("Test D-Bus service did not become ready")
    return process.stdout.readline().strip()


def installed_desktop(portal_source, check_ipp):
    # No PHOMEMO runtime/state/spool/port overrides, command override, or extra
    # sandbox permission on the desktop command. Each invocation is independent.
    env = clean_environment()
    host_runtime = Path(env.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}"))
    if not host_runtime.is_dir():
        raise RuntimeError(f"Create the runner's user runtime directory before testing: {host_runtime}")
    env["XDG_RUNTIME_DIR"] = str(host_runtime)
    runtime = host_runtime / "app" / APP / "r"
    data = Path.home() / ".var/app" / APP / "data/phomemo"
    state = data / "printer.state"
    socket_path = runtime / "phomemo-printer-app.sock"
    if len(os.fsencode(socket_path)) > 107:
        raise RuntimeError("Default installed runtime socket exceeds Linux's socket-path limit")
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", PORT))

    with tempfile.TemporaryDirectory(prefix="pm-desktop-", dir="/tmp") as temporary:
        root = Path(temporary)
        portal = root / "test-portal"
        flags = shlex.split(subprocess.check_output(["pkg-config", "--cflags", "--libs", "gio-2.0"], text=True))
        subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", str(portal_source),
                        "-o", str(portal), *flags], check=True, timeout=60)
        config = root / "dbus.conf"
        # No <servicedir> or <standard_session_servicedirs>: a missing portal
        # cannot activate a real desktop portal or open the user's browser.
        config.write_text(
            '<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>'
            '<auth>EXTERNAL</auth><policy context="default">'
            '<allow send_destination="*"/><allow eavesdrop="true"/>'
            '<allow own="*"/></policy></busconfig>'
        )
        processes = []
        portal_process = None
        dbus = None
        with (root / "desktop.log").open("w+") as log:
            def spawn(command, **kwargs):
                process = subprocess.Popen(command, env=env, stdin=subprocess.DEVNULL,
                                           stderr=log, **kwargs)
                processes.append(process)
                return process

            def command(*args, cli=False):
                prefix = ["flatpak", "run", "--user"]
                if cli:
                    prefix.append("--command=phomemo-printer-app")
                return [*prefix, APP, *args]

            def invoke(*args, cli=False, ok=True):
                result = subprocess.run(command(*args, cli=cli), env=env,
                                        capture_output=True, text=True, timeout=30)
                if (result.returncode == 0) != ok:
                    raise RuntimeError(f"Unexpected launcher result: {result.stdout}\n{result.stderr}")
                return result.stdout

            def cli(*args):
                return invoke(*args, cli=True)

            def wait(condition, message):
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    if condition():
                        return
                    time.sleep(0.1)
                log.flush()
                raise RuntimeError(message + "\n" + (root / "desktop.log").read_text())

            def running():
                return "Running," in cli("status")

            portal_log = root / "portal.log"

            def calls():
                return portal_log.read_text().splitlines() if portal_log.exists() else []

            def start_portal(mode="success"):
                nonlocal portal_process
                portal_env = dict(env, PHOMEMO_TEST_PORTAL_LOG=str(portal_log), PHOMEMO_TEST_PORTAL_MODE=mode)
                portal_process = subprocess.Popen([str(portal)], env=portal_env,
                                                  stdout=subprocess.PIPE, stderr=log, text=True)
                processes.append(portal_process)
                if ready_line(portal_process) != "ready":
                    raise RuntimeError("Isolated test OpenURI service did not acquire its bus name")
                portal_process.stdout.close()

            def stop_portal():
                nonlocal portal_process
                portal_process.terminate()
                portal_process.wait(timeout=5)
                portal_process = None

            def desktop():
                return spawn(command(), stdout=log)

            def stopped(process):
                invoke("--stop")  # Actual installed desktop Stop action.
                if process.wait(timeout=20) != 0:
                    raise RuntimeError("Installed desktop supervisor did not stop cleanly")
                wait(lambda: not running(), "Installed desktop Stop left a running server")

            try:
                dbus = spawn(["dbus-daemon", f"--config-file={config}", "--nofork", "--print-address=1"],
                             stdout=subprocess.PIPE, text=True)
                address = ready_line(dbus)
                if not address.startswith("unix:"):
                    raise RuntimeError("Isolated session bus did not return a Unix address")
                dbus.stdout.close()
                # Flatpak's default session-bus proxy connects to this private
                # bus and exposes its portal to the sandbox. Never bypass it
                # with --socket=session-bus or a filesystem permission.
                env["DBUS_SESSION_BUS_ADDRESS"] = address
                if running():
                    raise RuntimeError("Refusing to replace an existing default-path Flatpak server")
                if state.exists():
                    raise RuntimeError("Default-path desktop tests require a fresh CI installation")
                data.mkdir(parents=True, mode=0o700, exist_ok=True)
                state.write_text("NextPrinterID 1\n")
                start_portal()
                print("Testing installed default Flatpak desktop, independent sandboxes and isolated OpenURI", flush=True)
                first = desktop()
                wait(lambda: len(calls()) == 1 and running(), "Default desktop did not open the test portal")
                check_ipp(PORT)
                if not socket_path.is_socket():
                    raise RuntimeError("Default desktop did not create its shared runtime socket")
                for directory in (runtime, data, data / "spool"):
                    stat = directory.stat()
                    if stat.st_uid != os.getuid() or stat.st_mode & 0o777 != 0o700:
                        raise RuntimeError(f"Default application directory is not private: {directory}")
                inode = socket_path.stat().st_ino
                repeats = [desktop(), desktop()]
                for repeated in repeats:
                    if repeated.wait(timeout=30) != 0:
                        raise RuntimeError("Independent repeated desktop launch failed")
                wait(lambda: len(calls()) == 3, "Repeated desktop launches did not use OpenURI")
                if first.poll() is not None or socket_path.stat().st_ino != inode:
                    raise RuntimeError("Repeated desktop launch replaced the original server")
                if calls() != [f"http://127.0.0.1:{PORT}/"] * 3:
                    raise RuntimeError(f"Portal opened unexpected default URIs: {calls()!r}")
                cli("add", "-d", "Desktop-Dummy", "-m", "phomemo_m220", "-v", "socket://127.0.0.1:9")
                cli("default", "-d", "Desktop-Dummy")
                if "Desktop-Dummy" not in cli("printers"):
                    raise RuntimeError("Independent sandbox CLI cannot see the desktop's queue")
                stopped(first)
                if "Desktop-Dummy" not in state.read_text():
                    raise RuntimeError("Default desktop state file was not persisted")
                restarted = desktop()
                wait(lambda: len(calls()) == 4 and running(), "Default desktop did not restart")
                if "Desktop-Dummy" not in cli("printers") or cli("default").strip() != "Desktop-Dummy":
                    raise RuntimeError("Default-path queue/default did not survive desktop restart")

                stop_portal()
                start_portal("refuse")
                invoke(ok=False)  # Denied repeat leaves the existing server.
                if restarted.poll() is not None or not running():
                    raise RuntimeError("A refused repeated portal request stopped the existing server")
                stopped(restarted)
                invoke(ok=False)  # Denied first launch must reap its new server.
                wait(lambda: not running(), "A refused first portal launch orphaned a server")
                stop_portal()
                count = len(calls())
                invoke(ok=False)  # No activation directories: genuinely absent portal.
                wait(lambda: not running(), "Missing test portal orphaned a server")
                if len(calls()) != count:
                    raise RuntimeError("The absent portal unexpectedly received an OpenURI request")
            finally:
                # Attempt the ordinary Stop action before killing sandbox
                # processes; keep the isolated bus alive throughout cleanup.
                active = [process for process in processes if process is not dbus and process is not portal_process
                          and process.poll() is None]
                if active and dbus is not None and dbus.poll() is None:
                    try:
                        invoke("--stop")
                    except (RuntimeError, subprocess.TimeoutExpired):
                        pass
                for process in reversed(processes):
                    if process.poll() is None:
                        process.terminate()
                        try:
                            process.wait(timeout=10)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait()
