#!/usr/bin/env python3
"""Real PAPPL runtime/CLI regression tests; no printer or Bluetooth required.

Run after building: python3 scripts/test_packaged_runtime.py --binary /absolute/binary
On Linux, also run under `unshare --user --map-root-user` to test uid 0 without
changing the host service. Each test uses private directories and its own port.
"""

import argparse
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
import unittest


class RuntimeTests(unittest.TestCase):
    binary: Path
    service_binary = None
    service_directory = None

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="pm-", dir="/tmp")
        self.root = Path(self.temporary.name)
        self.executable = self.root / "pm-runtime"
        binary = self.binary
        if self._testMethodName == "test_native_systemd_discovery_and_manual_server":
            if self.service_binary is None:
                self.skipTest("pass --service-binary and --service-directory for native systemd coverage")
            binary = self.service_binary
        shutil.copy2(binary, self.executable)
        self.runtime = self.root / "run"
        self.runtime.mkdir(mode=0o700)
        self.snap = self.root / "snap-common"
        self.snap.mkdir(mode=0o700)
        self.home = self.root / "home"
        self.home.mkdir(mode=0o700)
        self.config = self.home / ".config"
        self.config.mkdir(mode=0o700)
        self.state = self.root / "printer.state"
        self.socket = self.runtime / "pm-runtime.sock"
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            self.port = reservation.getsockname()[1]
        self.env = {
            key: value for key, value in os.environ.items()
            if not key.startswith("PHOMEMO_")
            and key not in ("RUNTIME_DIRECTORY", "SNAP_COMMON", "XDG_CONFIG_HOME")
        }
        self.env.update(
            HOME=str(self.home), XDG_CONFIG_HOME=str(self.config),
            TMPDIR=str(self.root), SNAP_COMMON=str(self.snap),
            PHOMEMO_RUNTIME_DIRECTORY=str(self.runtime),
            PHOMEMO_STATE_FILE=str(self.state),
            PHOMEMO_SPOOL_DIRECTORY=str(self.root / "spool"),
            PHOMEMO_SERVER_PORT=str(self.port),
            PHOMEMO_LISTEN_HOSTNAME="127.0.0.1",
        )
        self.server = None
        self.log = tempfile.TemporaryFile(mode="w+")

    def tearDown(self):
        try:
            self.stop_server()
        finally:
            self.log.close()
            self.temporary.cleanup()

    def cli(self, *args, env=None, ok=True):
        result = subprocess.run(
            [str(self.executable), *args], env=self.env if env is None else env,
            cwd=str(self.root), stdin=subprocess.DEVNULL, capture_output=True,
            text=True, timeout=20,
        )
        if ok:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def start_server(self, *args):
        # A valid empty state suppresses PAPPL's initial local-device autoadd.
        # Thus even a machine with paired Bluetooth printers only uses our dummy.
        if not self.state.exists():
            self.state.write_text("NextPrinterID 1\n")
        self.server = subprocess.Popen(
            [str(self.executable), "server", *args], env=self.env,
            cwd=str(self.root), stdin=subprocess.DEVNULL,
            stdout=self.log, stderr=self.log, umask=0o077,
        )
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if self.server.poll() is not None:
                self.log.seek(0)
                self.fail("Server exited: " + self.log.read())
            if self.socket.exists():
                result = self.cli("status")
                if "Running," in result.stdout:
                    return
            time.sleep(0.05)
        self.fail("Server did not become ready")

    def stop_server(self):
        if self.server is not None:
            self.server.terminate()
            try:
                self.server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.server.kill()
                self.server.wait()
                self.fail("Server did not terminate cleanly")
            self.server = None

    def add_printer(self):
        # Creating a queue does not open its device. No physical hardware needed.
        self.cli("add", "-d", "Dummy", "-m", "phomemo_m220",
                 "-v", "socket://127.0.0.1:9")
        self.assertIn("Dummy", self.cli("printers").stdout)
        self.cli("default", "-d", "Dummy")
        self.assertEqual(self.cli("default").stdout.strip(), "Dummy")

    def test_shared_socket_persistence_and_snap_environment(self):
        # PAPPL must still read config from the real SNAP_COMMON, not runtime.
        (self.snap / "pm-runtime.conf").write_text("log-level=debug\n")
        self.start_server()
        self.add_printer()
        environ = Path(f"/proc/{self.server.pid}/environ").read_bytes().split(b"\0")
        self.assertIn(os.fsencode("SNAP_COMMON=" + str(self.snap)), environ)
        self.assertFalse((self.snap / "pm-runtime.sock").exists())
        self.assertFalse((self.root / f"pm-runtime{os.getuid()}.sock").exists())
        self.assertFalse((self.runtime / "pm-runtime.state").exists())
        self.stop_server()
        self.assertTrue(self.state.is_file())
        self.log.seek(0)
        self.assertIn("Entering run loop", self.log.read())
        self.start_server()
        self.assertIn("Dummy", self.cli("printers").stdout)
        self.assertEqual(self.cli("default").stdout.strip(), "Dummy")

    def test_flatpak_environment_without_snap_common(self):
        del self.env["SNAP_COMMON"]
        self.start_server()
        self.add_printer()
        self.stop_server()
        self.start_server()
        self.assertIn("Dummy", self.cli("printers").stdout)

    def test_default_snap_state_remains_in_real_snap_common(self):
        del self.env["PHOMEMO_STATE_FILE"]
        self.state = self.snap / "pm-runtime.state"
        self.start_server()
        self.add_printer()
        self.stop_server()
        self.assertTrue(self.state.is_file())
        self.assertIn("Dummy", self.state.read_text())
        self.assertFalse((self.runtime / self.state.name).exists())
        self.start_server()
        self.assertIn("Dummy", self.cli("printers").stdout)
        self.assertEqual(self.cli("default").stdout.strip(), "Dummy")

    def test_second_server_cannot_replace_listener_or_state(self):
        self.start_server()
        self.add_printer()
        inode = self.socket.stat().st_ino
        second = self.cli("server", ok=False)
        self.assertIn("already has a server", second.stderr)
        # PAPPL 1.4 changes argv[i] while walking a clustered option.
        clustered = self.cli("-dd", "-a", "server", ok=False)
        self.assertIn("already has a server", clustered.stderr)
        self.assertEqual(self.socket.stat().st_ino, inode)
        self.assertIsNone(self.server.poll())
        self.assertIn("Dummy", self.cli("printers").stdout)

    def test_listen_hostname_socket_alias_is_not_bound_twice(self):
        alias = self.root / "alias"
        alias.symlink_to(self.runtime, target_is_directory=True)
        self.env["PHOMEMO_LISTEN_HOSTNAME"] = str(alias / self.socket.name)
        self.start_server()
        self.add_printer()
        self.log.seek(0)
        log = self.log.read()
        self.assertEqual(log.count("Listening for connections on '"), 1, log)

    def test_missing_server_does_not_autostart_persistent_server(self):
        self.assertIn("not running", self.cli("status").stdout)
        fallback = self.cli("printers", ok=False)
        self.assertIn("Private-server fallback is disabled", fallback.stderr)
        self.assertFalse(self.socket.exists())
        self.assertFalse(self.state.exists())
        self.assertFalse((self.root / "spool").exists())
        self.cli("server", "-o", "private-server=true", ok=False)

    def test_invalid_directories_fail_closed(self):
        unsafe = self.root / "unsafe"
        unsafe.mkdir(mode=0o755)
        link = self.root / "linked"
        link.symlink_to(self.runtime, target_is_directory=True)
        file = self.root / "file"
        file.write_text("sentinel")
        for value in ("", "relative", "/" + "x" * 150, str(unsafe), str(link),
                      str(link) + "/", str(file), str(self.root / "missing")):
            with self.subTest(value=value):
                env = dict(self.env, PHOMEMO_RUNTIME_DIRECTORY=value)
                result = self.cli("status", env=env, ok=False)
                self.assertIn("PHOMEMO_RUNTIME_DIRECTORY", result.stderr)
                self.assertFalse(self.socket.exists())
        self.assertEqual(file.read_text(), "sentinel")
        help_result = self.cli("--help", env=dict(self.env, PHOMEMO_RUNTIME_DIRECTORY=""))
        self.assertIn("PHOMEMO_RUNTIME_DIRECTORY", help_result.stdout)
        self.assertIn("Private-server auto-start is disabled", help_result.stdout)
        self.cli("server", "-Z", "--version", ok=False)
        self.cli("--unknown", "--help", ok=False)

    def test_socket_obstacles_are_not_unlinked(self):
        target = self.root / "sentinel"
        target.write_text("keep me")
        self.socket.symlink_to(target)
        self.cli("server", ok=False)
        self.cli("-dd", "-a", "server", ok=False)
        self.assertTrue(self.socket.is_symlink())
        self.socket.unlink()
        self.socket.write_text("keep socket filename")
        self.cli("server", ok=False)
        self.assertEqual(self.socket.read_text(), "keep socket filename")
        self.socket.unlink()
        with socket.socket(socket.AF_UNIX) as listener:
            listener.bind(str(self.socket))
            listener.listen()
            inode = self.socket.stat().st_ino
            self.cli("server", ok=False)
            self.assertEqual(self.socket.stat().st_ino, inode)
        # A stale socket is safe to recover after a crash.
        self.start_server()
        self.add_printer()
        self.assertEqual(target.read_text(), "keep me")

    def test_native_discovery_without_runtime_override(self):
        del self.env["PHOMEMO_RUNTIME_DIRECTORY"]
        if os.getuid():
            del self.env["SNAP_COMMON"]
            self.socket = self.root / f"pm-runtime{os.getuid()}.sock"
        else:
            # Exercise PAPPL's native root-Snap path in an unprivileged user namespace.
            self.socket = self.snap / "pm-runtime.sock"
        self.start_server()
        self.add_printer()
        self.stop_server()
        self.start_server()
        self.assertIn("Dummy", self.cli("printers").stdout)

    def test_native_systemd_discovery_and_manual_server(self):
        del self.env["PHOMEMO_RUNTIME_DIRECTORY"]
        del self.env["SNAP_COMMON"]
        self.service_directory.mkdir(mode=0o755, exist_ok=True)
        self.env["RUNTIME_DIRECTORY"] = "/unused:" + str(self.service_directory)
        self.socket = self.service_directory / "pm-runtime.sock"
        self.addCleanup(self.socket.unlink, missing_ok=True)
        self.start_server()
        self.add_printer()
        # libcups deliberately makes native sockets accessible across accounts,
        # even under the systemd unit's restrictive umask.
        self.assertTrue(self.socket.stat().st_mode & 0o002)
        clients = dict(self.env)
        del clients["RUNTIME_DIRECTORY"]
        client_tmp = self.root / "client-temp"
        client_tmp.mkdir(mode=0o700)
        clients["TMPDIR"] = str(client_tmp)
        self.assertIn("Dummy", self.cli("printers", env=clients).stdout)
        inode = self.socket.stat().st_ino

        # A server not authorized by systemd must not take the service socket.
        # Use a second web port to keep TCP bind failure from masking that bug.
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            manual_port = reservation.getsockname()[1]
        clients["PHOMEMO_SERVER_PORT"] = str(manual_port)
        clients["PHOMEMO_STATE_FILE"] = str(self.root / "manual.state")
        (self.root / "manual.state").write_text("NextPrinterID 1\n")
        clients["PHOMEMO_SPOOL_DIRECTORY"] = str(self.root / "manual-spool")
        manual_env = dict(clients, TMPDIR=str(self.root))
        manual = subprocess.Popen(
            [str(self.executable), "-dd", "-a", "server"],
            env=manual_env, cwd=str(self.root), stdin=subprocess.DEVNULL,
            stdout=self.log, stderr=self.log, umask=0o077,
        )
        try:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                with socket.socket() as probe:
                    if probe.connect_ex(("127.0.0.1", manual_port)) == 0:
                        break
                self.assertIsNone(manual.poll())
                time.sleep(0.05)
            else:
                self.fail("Manual server never opened its independent port")
            # A round trip lets the manual server finish its listener setup.
            self.cli("status", "-u", f"ipp://127.0.0.1:{manual_port}/ipp/system")
            self.assertEqual(self.socket.stat().st_ino, inode)
            self.assertIn("Dummy", self.cli("printers", env=clients).stdout)
        finally:
            manual.terminate()
            try:
                manual.wait(timeout=10)
            except subprocess.TimeoutExpired:
                manual.kill()
                manual.wait()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--service-binary", type=Path,
                        help="binary compiled with SERVICE_DIRECTORY set to --service-directory")
    parser.add_argument("--service-directory", type=Path)
    args = parser.parse_args()
    RuntimeTests.binary = args.binary.resolve(strict=True)
    if args.service_binary or args.service_directory:
        if not args.service_binary or not args.service_directory:
            parser.error("--service-binary and --service-directory must be used together")
        RuntimeTests.service_binary = args.service_binary.resolve(strict=True)
        RuntimeTests.service_directory = args.service_directory.resolve()
    unittest.main(argv=[__file__], verbosity=2)
