"""Exercise the real Flatpak launcher/binary against an isolated test portal.

Needs a C compiler, gio-2.0 development files and dbus-daemon (SDK build tests).
No Bluetooth printer or real desktop/browser is touched.
"""
import argparse
import http.client
import os
import signal
import socket
import struct
import subprocess
import tempfile
import time
import unittest
from pathlib import Path


class LauncherTests(unittest.TestCase):
    launcher: Path
    binary: Path
    dbus_daemon = "dbus-daemon"

    @classmethod
    def setUpClass(cls):
        cls.build = tempfile.TemporaryDirectory(prefix="pf-build-", dir="/tmp")
        cls.portal = Path(cls.build.name) / "test-portal"
        flags = subprocess.check_output(
            ["pkg-config", "--cflags", "--libs", "gio-2.0"], text=True).split()
        subprocess.run(["cc", "-Wall", "-Wextra", "-Werror",
                        str(Path(__file__).with_name("test-portal.c")),
                        "-o", str(cls.portal), *flags], check=True)

    @classmethod
    def tearDownClass(cls):
        cls.build.cleanup()

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="pf-", dir="/tmp")
        self.root = Path(self.temporary.name)
        self.runtime = self.root / "run"
        self.runtime.mkdir(mode=0o700)
        self.data = self.root / "data"
        self.data.mkdir(mode=0o700)
        self.state = self.root / "printer.state"
        self.state.write_text("NextPrinterID 1\n")
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            self.port = reservation.getsockname()[1]
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith("PHOMEMO_") and key not in (
                        "SNAP_COMMON", "RUNTIME_DIRECTORY", "DBUS_SESSION_BUS_ADDRESS")}
        self.env.update(
            XDG_RUNTIME_DIR=str(self.runtime), XDG_DATA_HOME=str(self.data),
            XDG_CONFIG_HOME=str(self.root / "config"),
            PHOMEMO_STATE_FILE=str(self.state), PHOMEMO_SERVER_PORT=str(self.port),
            PHOMEMO_TEST_PORTAL_LOG=str(self.root / "portal.log"),
        )
        config = self.root / "dbus.conf"
        config.write_text(
            '<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>'
            '<auth>EXTERNAL</auth><policy context="default">'
            '<allow send_destination="*"/><allow eavesdrop="true"/>'
            '<allow own="*"/></policy></busconfig>')
        # No service directories: even the "missing portal" case cannot activate
        # the host's actual desktop portal/browser through D-Bus autostart.
        self.dbus = subprocess.Popen(
            [str(self.dbus_daemon), "--config-file=" + str(config), "--nofork", "--print-address=1"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.env["DBUS_SESSION_BUS_ADDRESS"] = self.dbus.stdout.readline().strip()
        self.processes = []
        self.log = self.enterContext(tempfile.TemporaryFile())
        self.portal_process = None

    def tearDown(self):
        for process in reversed(self.processes):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        self.dbus.terminate()
        self.dbus.communicate(timeout=5)
        self.log.close()
        self.temporary.cleanup()

    def start_portal(self, mode="success"):
        process = subprocess.Popen([str(self.portal)], env=dict(
            self.env, PHOMEMO_TEST_PORTAL_MODE=mode), stdout=subprocess.PIPE,
            stderr=self.log, text=True)
        self.processes.append(process)
        self.assertEqual(process.stdout.readline().strip(), "ready")
        process.stdout.close()
        self.portal_process = process

    def launch(self):
        process = subprocess.Popen([str(self.launcher)], env=self.env,
                                   stdout=self.log, stderr=self.log)
        self.processes.append(process)
        return process

    def cli(self, *args):
        return subprocess.run([str(self.launcher), "--cli", *args], env=self.env,
                              capture_output=True, text=True, timeout=15, check=False)

    def wait_for(self, condition, message):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if condition():
                return
            time.sleep(0.05)
        self.log.seek(0)
        self.fail(message + "\n" + self.log.read().decode(errors="replace"))

    def opened(self, count=1):
        path = self.root / "portal.log"
        return path.exists() and len(path.read_text().splitlines()) >= count

    def not_running(self):
        return "not running" in self.cli("status").stdout

    def test_repeated_launch_cli_and_visible_stop(self):
        self.start_portal()
        first = self.launch()
        self.wait_for(self.opened, "Portal was not called")
        others = [self.launch() for _ in range(3)]
        for other in others:
            self.assertEqual(other.wait(timeout=20), 0)
        self.assertIsNone(first.poll())
        self.assertIn("Running,", self.cli("status").stdout)
        self.assertNotEqual(self.cli("server").returncode, 0)
        self.assertEqual(self.cli("add", "-d", "Dummy", "-m", "phomemo_m220",
                                  "-v", "socket://127.0.0.1:9").returncode, 0)
        self.assertIn("Dummy", self.cli("printers").stdout)
        self.assertEqual((self.root / "portal.log").read_text().splitlines(),
                         [f"http://127.0.0.1:{self.port}/"] * 4)
        stopped = subprocess.run([str(self.launcher), "--stop"], env=self.env,
                                 capture_output=True, timeout=15, check=False)
        self.assertEqual(stopped.returncode, 0, stopped.stderr)
        self.assertEqual(first.wait(timeout=15), 0)
        self.assertIn("Dummy", self.state.read_text())

    def test_refused_portal_does_not_orphan_new_server(self):
        self.start_portal("refuse")
        process = self.launch()
        self.assertNotEqual(process.wait(timeout=25), 0)
        self.assertTrue(self.not_running())

    def test_desktop_delegates_to_direct_cli_server(self):
        self.start_portal()
        server = subprocess.Popen([str(self.launcher), "--cli", "server"],
                                  env=self.env, stdout=self.log, stderr=self.log)
        self.processes.append(server)
        self.wait_for(lambda: "Running," in self.cli("status").stdout,
                      "Direct CLI server never started")
        desktop = self.launch()
        self.assertEqual(desktop.wait(timeout=20), 0)
        self.assertTrue(self.opened())
        self.assertIsNone(server.poll())
        self.assertEqual(self.cli("shutdown").returncode, 0)
        self.assertEqual(server.wait(timeout=15), 0)

    def test_visible_stop_terminates_server_without_printers(self):
        self.start_portal()
        process = self.launch()
        self.wait_for(self.opened, "Portal was not called")
        stopped = subprocess.run([str(self.launcher), "--stop"], env=self.env,
                                 capture_output=True, timeout=15, check=False)
        self.assertEqual(stopped.returncode, 0, stopped.stderr)
        self.assertEqual(process.wait(timeout=15), 0)
        self.assertTrue(self.not_running())

    def test_unavailable_portal_does_not_orphan_new_server(self):
        process = self.launch()
        self.assertNotEqual(process.wait(timeout=25), 0)
        self.assertTrue(self.not_running())

    def test_refused_portal_does_not_stop_existing_server(self):
        self.start_portal()
        first = self.launch()
        self.wait_for(self.opened, "Portal was not called")
        self.portal_process.terminate()
        self.portal_process.wait(timeout=5)
        self.start_portal("refuse")
        second = self.launch()
        self.assertNotEqual(second.wait(timeout=25), 0)
        self.assertIsNone(first.poll())
        self.assertIn("Running,", self.cli("status").stdout)

    def test_supervisor_sigterm_and_sigkill_stop_server(self):
        self.start_portal()
        for signo in (signal.SIGTERM, signal.SIGKILL):
            with self.subTest(signal=signo):
                process = self.launch()
                self.wait_for(lambda: "Running," in self.cli("status").stdout,
                              "Server never started")
                os.kill(process.pid, signo)
                process.wait(timeout=15)
                self.wait_for(self.not_running, "Supervisor left an orphan server")

    def test_short_runtime_and_unsafe_override(self):
        self.start_portal()
        # Default must be shared across invocations, not ephemeral /tmp storage.
        result = self.cli("status")
        self.assertEqual(result.returncode, 0, result.stderr)
        runtime = self.runtime / "app/io.github.mabl.phomemo-printer-app/r"
        self.assertEqual(runtime.stat().st_mode & 0o777, 0o700)
        self.assertLessEqual(len(os.fsencode(runtime / "phomemo-printer-app.sock")), 107)
        unsafe = self.root / "unsafe"
        unsafe.mkdir(mode=0o755)
        self.env["PHOMEMO_RUNTIME_DIRECTORY"] = str(unsafe)
        self.assertNotEqual(self.cli("status").returncode, 0)

    def test_port_collision_does_not_open_other_service(self):
        self.start_portal()
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", self.port))
            listener.listen()
            process = self.launch()
            self.assertNotEqual(process.wait(timeout=25), 0)
        self.assertFalse(self.opened())
        self.assertTrue(self.not_running())

    def test_state_and_spool_paths_with_spaces_and_quotes(self):
        self.start_portal()
        self.state = self.root / 'printer "state" with spaces'
        self.state.write_text("NextPrinterID 1\n")
        self.env["PHOMEMO_STATE_FILE"] = str(self.state)
        spool = self.root / 'spool "directory" with spaces'
        self.env["PHOMEMO_SPOOL_DIRECTORY"] = str(spool)
        process = self.launch()
        self.wait_for(self.opened, "Portal was not called")
        self.assertEqual(self.cli("add", "-d", "Dummy", "-m", "phomemo_m220",
                                  "-v", "socket://127.0.0.1:9").returncode, 0)
        self.assertEqual(self.cli("shutdown").returncode, 0)
        self.assertEqual(process.wait(timeout=15), 0)
        self.assertIn("Dummy", self.state.read_text())
        self.assertEqual(spool.stat().st_mode & 0o777, 0o700)

    def test_stop_during_pending_portal_is_prompt(self):
        for mode in ("pending", "pending-method"):
            with self.subTest(mode=mode):
                self.start_portal(mode)
                process = self.launch()
                self.wait_for(self.opened, "Portal was not called")
                started = time.monotonic()
                stopped = subprocess.run([str(self.launcher), "--stop"], env=self.env,
                                         capture_output=True, timeout=5, check=False)
                self.assertEqual(stopped.returncode, 0, stopped.stderr)
                self.assertLess(time.monotonic() - started, 5)
                self.assertEqual(process.wait(timeout=5), 0)
                self.assertTrue(self.not_running())
                self.assertIn("closed", (self.root / "portal.log").read_text())
                self.portal_process.terminate()
                self.portal_process.wait(timeout=5)
                (self.root / "portal.log").unlink()

    def test_noncanonical_lifecycle_arguments_fail_closed(self):
        for args in (("-o", "log-level=debug", "server"),
                     ("-dd", "-a", "server"), ("status", "server"),
                     ("server", "-o", "log-level=debug", "server"),
                     ("server", "-o", "listen-hostname=/tmp/wrong")):
            with self.subTest(args=args):
                self.assertNotEqual(self.cli(*args).returncode, 0)
                self.assertTrue(self.not_running())
        server = subprocess.Popen([str(self.launcher), "--cli", "server", "-o", "log-level=debug"],
                                  env=self.env, stdout=self.log, stderr=self.log)
        self.processes.append(server)
        self.wait_for(lambda: "Running," in self.cli("status").stdout, "Canonical CLI server never started")
        control = self.runtime / "app/io.github.mabl.phomemo-printer-app/r/control.sock"
        self.assertTrue(control.is_socket())
        for args in (("-o", "log-level=debug", "shutdown"),
                     ("shutdown", "-o", "log-level=debug"), ("status", "shutdown")):
            self.assertNotEqual(self.cli(*args).returncode, 0)
            self.assertIsNone(server.poll())
        self.assertEqual(self.cli("shutdown").returncode, 0)
        self.assertEqual(server.wait(timeout=5), 0)

    def test_trailing_slash_symlinks_are_rejected(self):
        target = self.root / "target"
        target.mkdir(mode=0o700)
        link = self.root / "link"
        link.symlink_to(target, target_is_directory=True)
        for variable in ("PHOMEMO_RUNTIME_DIRECTORY", "PHOMEMO_SPOOL_DIRECTORY"):
            for suffix in ("", "/", "///"):
                with self.subTest(variable=variable, suffix=suffix):
                    self.env[variable] = str(link) + suffix
                    result = self.cli("status")
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("without a final symlink", result.stderr)
            del self.env[variable]
        self.assertEqual(list(target.iterdir()), [])

    def test_saved_and_logging_options_cannot_override_binding_contract(self):
        config = self.root / "config"
        config.mkdir(mode=0o700)
        (config / "phomemo-printer-app.conf").write_text(
            f"listen-hostname={self.root}/wrong.sock\nserver-port=1\ntls-only=true\n"
            f"state-file={self.root}/wrong.state\nspool-directory={self.root}/wrong-spool\n")
        server = subprocess.Popen([str(self.launcher), "--cli", "server", "-o",
                                   f"log-level=debug listen-hostname={self.root}/injected.sock"],
                                  env=self.env, stdout=self.log, stderr=self.log)
        self.processes.append(server)
        self.wait_for(lambda: "Running," in self.cli("status").stdout, "Bound server never started")
        self.uuid_from_endpoint("127.0.0.1", self.port)
        self.assertFalse((self.root / "wrong.sock").exists())
        self.assertFalse((self.root / "injected.sock").exists())
        self.assertFalse((self.root / "wrong-spool").exists())
        self.assertEqual(self.cli("shutdown").returncode, 0)
        self.assertEqual(server.wait(timeout=5), 0)

    @staticmethod
    def uuid_from_endpoint(host, port):
        # RFC 8011 operation header plus PWG IPP System Service operation 0x005b.
        request = bytearray(struct.pack("!BBHI", 2, 0, 0x005B, 1) + b"\x01")
        for tag, name, value in ((0x47, b"attributes-charset", b"utf-8"),
                                 (0x48, b"attributes-natural-language", b"en"),
                                 (0x45, b"system-uri", b"ipp://localhost/ipp/system"),
                                 (0x44, b"requested-attributes", b"system-uuid")):
            request.extend(bytes([tag]) + struct.pack("!H", len(name)) + name +
                           struct.pack("!H", len(value)) + value)
        request.append(3)
        if host.startswith("/"):
            connection = http.client.HTTPConnection("localhost", timeout=2)
            connection.sock = socket.socket(socket.AF_UNIX)
            connection.sock.settimeout(2)
            connection.sock.connect(host)
        else:
            connection = http.client.HTTPConnection(host, port, timeout=2)
        try:
            connection.request("POST", "/ipp/system", bytes(request), {"Content-Type": "application/ipp"})
            response = connection.getresponse().read()
        finally:
            connection.close()
        index = response.find(b"urn:uuid:")
        if index < 0:
            raise AssertionError(f"No system UUID in IPP reply: {response!r}")
        return response[index:index + 45]

    def test_deterministic_uuid_collision_cannot_open_foreign_tcp_server(self):
        self.start_portal()
        self.cli("status")  # Create the launcher's default private runtime.
        selected = self.runtime / "app/io.github.mabl.phomemo-printer-app/r"
        selected_socket = selected / "phomemo-printer-app.sock"
        other = self.root / "other-run"
        other.mkdir(mode=0o700)
        other_state = self.root / "other.state"
        other_state.write_text("NextPrinterID 1\n")
        environments = [dict(self.env, PHOMEMO_RUNTIME_DIRECTORY=str(selected),
                             PHOMEMO_LISTEN_HOSTNAME=str(selected_socket)),
                        dict(self.env, PHOMEMO_RUNTIME_DIRECTORY=str(other),
                             PHOMEMO_LISTEN_HOSTNAME="127.0.0.1",
                             PHOMEMO_STATE_FILE=str(other_state),
                             PHOMEMO_SPOOL_DIRECTORY=str(self.root / "other-spool"))]
        for env in environments:
            process = subprocess.Popen([str(self.binary), "server"], env=env,
                                       stdout=self.log, stderr=self.log)
            self.processes.append(process)
        self.wait_for(lambda: "Running," in self.cli("status").stdout, "UNIX-only server not ready")
        self.wait_for(lambda: (other / "phomemo-printer-app.sock").exists(), "TCP server not ready")
        self.assertEqual(self.uuid_from_endpoint(str(selected_socket), 0),
                         self.uuid_from_endpoint("127.0.0.1", self.port))
        process = self.launch()
        self.assertNotEqual(process.wait(timeout=5), 0)
        self.assertFalse(self.opened())
        self.assertFalse((selected / "control.sock").exists())
        self.assertIn("Running,", self.cli("status").stdout)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--launcher", required=True, type=Path)
    parser.add_argument("--binary", required=True, type=Path,
                        help="documents which real binary the launcher was compiled to execute")
    parser.add_argument("--dbus-daemon", type=Path,
                        help="absolute build-test-only daemon (required in SDK 25.08)")
    args, test_args = parser.parse_known_args()
    LauncherTests.launcher = args.launcher.resolve(strict=True)
    LauncherTests.binary = args.binary.resolve(strict=True)
    if args.dbus_daemon:
        LauncherTests.dbus_daemon = args.dbus_daemon.resolve(strict=True)
    unittest.main(argv=[__file__, *test_args], verbosity=2)
