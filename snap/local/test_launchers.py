#!/usr/bin/env python3
"""Exercise the snap command chain and hook without a running snapd."""

import argparse
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import unittest


class LauncherTests(unittest.TestCase):
    snap_root = None

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="pm-snap-", dir="/tmp")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.snap = self.snap_root or self.root / "snap"
        local = Path(__file__).resolve().parent
        if not self.snap_root:
            installed = self.snap / "usr/libexec/phomemo"
            installed.mkdir(parents=True)
            for name in ("runtime-setup", "config.sh"):
                shutil.copy2(local / name, installed / name)
        self.launcher = self.snap / "usr/libexec/phomemo/runtime-setup"
        self.common = self.root / "common"
        self.common.mkdir()
        self.tools = self.root / "tools"
        self.tools.mkdir()
        self.snapctl = self.tools / "snapctl"
        self.snapctl.write_text(
            "#!/usr/bin/python3\n"
            "import json, os, sys\n"
            "if os.environ.get('TEST_SNAPCTL_FAIL'): sys.exit(1)\n"
            "if sys.argv[1] == 'get':\n"
            "    print(os.environ.get('TEST_' + sys.argv[2].upper().replace('-', '_'), ''))\n"
            "elif sys.argv[1] == 'set':\n"
            "    print(json.dumps(sys.argv[2:]))\n"
            "else: sys.exit(2)\n"
        )
        self.snapctl.chmod(0o755)
        self.env = {
            key: value for key, value in os.environ.items()
            if not key.startswith(("PHOMEMO_", "TEST_"))
        }
        self.env.update(SNAP=str(self.snap), SNAP_COMMON=str(self.common),
                        PATH=str(self.tools) + ":" + os.environ["PATH"])

    def run_setup(self, *, env=None, ok=True):
        if os.getuid():
            self.skipTest("writable system-daemon setup requires uid 0")
        result = subprocess.run([
            "sh", str(self.launcher), sys.executable, "-c",
            "import json,os; print(json.dumps(dict(os.environ)))",
        ], env=env or self.env, capture_output=True, text=True, timeout=10)
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(result.stdout)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        return result

    def test_defaults_and_private_persistent_paths(self):
        env = self.run_setup()
        self.assertEqual(env["SNAP_COMMON"], str(self.common))
        self.assertEqual(env["PHOMEMO_SERVER_PORT"], "8000")
        self.assertEqual(env["PHOMEMO_LISTEN_HOSTNAME"], "localhost")
        self.assertEqual(env["PHOMEMO_RUNTIME_DIRECTORY"], str(self.common / "runtime"))
        self.assertEqual(env["PHOMEMO_STATE_FILE"],
                         str(self.common / "state/phomemo-printer-app.state"))
        self.assertEqual(env["PHOMEMO_SPOOL_DIRECTORY"], str(self.common / "spool"))
        for name in ("runtime", "state", "spool", "ssl"):
            self.assertEqual((self.common / name).stat().st_mode & 0o777, 0o700)

    def test_config_overrides_inherited_environment(self):
        env = self.run_setup(env=dict(self.env, TEST_PORT="12345",
                                     TEST_LISTEN_HOSTNAME="127.0.0.1",
                                     PHOMEMO_SERVER_PORT="99", PHOMEMO_RUNTIME_DIRECTORY="/bad"))
        self.assertEqual(env["PHOMEMO_SERVER_PORT"], "12345")
        self.assertEqual(env["PHOMEMO_LISTEN_HOSTNAME"], "127.0.0.1")
        self.assertEqual(env["PHOMEMO_RUNTIME_DIRECTORY"], str(self.common / "runtime"))

    def test_invalid_ports_and_remote_listeners(self):
        for port in ("0", "08000", "-1", "65536", "1e3", "80 00", "9" * 100):
            with self.subTest(port=port):
                self.assertIn("port must", self.run_setup(
                    env=dict(self.env, TEST_PORT=port), ok=False).stderr)
        for hostname in ("*", "0.0.0.0", "example.com", "/tmp/server.sock"):
            with self.subTest(hostname=hostname):
                self.assertIn("listen-hostname must", self.run_setup(
                    env=dict(self.env, TEST_LISTEN_HOSTNAME=hostname), ok=False).stderr)
        self.assertFalse((self.common / "runtime").exists())

    def test_config_read_failure_is_fatal(self):
        self.run_setup(env=dict(self.env, TEST_SNAPCTL_FAIL="1"), ok=False)
        self.assertFalse((self.common / "runtime").exists())

    def test_unsafe_existing_directory_rejected(self):
        runtime = self.common / "runtime"
        runtime.mkdir(mode=0o755)
        self.assertIn("mode 0700", self.run_setup(ok=False).stderr)
        self.assertEqual(runtime.stat().st_mode & 0o777, 0o755)

    def test_symlink_directory_rejected(self):
        (self.common / "runtime").symlink_to(self.root)
        self.assertIn("symlink directory", self.run_setup(ok=False).stderr)

    def test_layout_certificate_directory_is_made_private(self):
        (self.common / "ssl").mkdir(mode=0o755)
        self.run_setup()
        self.assertEqual((self.common / "ssl").stat().st_mode & 0o777, 0o700)

    def test_configure_defaults_and_validation(self):
        hook = Path(__file__).resolve().parent.parent / "hooks/configure"
        result = subprocess.run(["sh", str(hook)], env=self.env,
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), ["port=8000", "listen-hostname=localhost"])
        result = subprocess.run(["sh", str(hook)], env=dict(self.env, TEST_PORT="0"),
                                capture_output=True, text=True, timeout=10)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")

    def test_unprivileged_cli_has_explicit_error(self):
        self.root.chmod(0o755)
        # Every directory needed to read the launcher is public; runtime stays private.
        if self.snap_root:
            launcher = self.launcher
        else:
            launcher = self.root / "launcher"
            shutil.copy2(self.launcher, launcher)
        kwargs = {"preexec_fn": lambda: os.setuid(65534)} if os.getuid() == 0 else {}
        result = subprocess.run(["sh", str(launcher), "/bin/true", "status"],
                                env=self.env, capture_output=True, text=True,
                                timeout=10, **kwargs)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("sudo phomemo-printer-app", result.stderr)
        result = subprocess.run(["sh", str(launcher), "/bin/true", "--help"],
                                env=self.env, capture_output=True, timeout=10, **kwargs)
        self.assertEqual(result.returncode, 0)

    def test_actual_packaged_command_chain(self):
        if not self.snap_root or os.getuid():
            self.skipTest("requires a primed snap and uid 0")
        binary = self.snap / "usr/bin/phomemo-printer-app"
        self.run_setup()
        state = self.common / "state/phomemo-printer-app.state"
        state.write_text("NextPrinterID 1\n")  # no physical device autoadd
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            self.env["TEST_PORT"] = str(reservation.getsockname()[1])

        def cli(*args):
            return subprocess.run([str(self.launcher), str(binary), *args],
                                  env=self.env, capture_output=True, text=True, timeout=20)

        for iteration in range(2):
            with tempfile.TemporaryFile(mode="w+") as log:
                server = subprocess.Popen([str(self.launcher), str(binary), "server"],
                                          env=self.env, stdout=log, stderr=log)
                try:
                    deadline = time.monotonic() + 10
                    while time.monotonic() < deadline:
                        if (self.common / "runtime/phomemo-printer-app.sock").exists():
                            status = cli("status")
                            if "Running," in status.stdout:
                                break
                        if server.poll() is not None:
                            log.seek(0)
                            self.fail(log.read())
                        time.sleep(0.05)
                    else:
                        self.fail("Packaged command chain did not start the server")
                    if iteration == 0:
                        result = cli("add", "-d", "Dummy", "-m", "phomemo_m220",
                                     "-v", "socket://127.0.0.1:9")
                        self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn("Dummy", cli("printers").stdout)
                    self.assertFalse((self.common / "phomemo-printer-app.sock").exists())
                finally:
                    server.terminate()  # exec must deliver SIGTERM straight to PAPPL
                    try:
                        server.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        server.kill()
                        server.wait()
                        self.fail("Packaged daemon failed to stop on SIGTERM")
                    self.assertEqual(server.returncode, 0)
        self.assertIn("Dummy", state.read_text())


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--snap-root", type=Path)
    args = parser.parse_args()
    if args.snap_root:
        LauncherTests.snap_root = args.snap_root.resolve(strict=True)
    unittest.main(argv=[__file__], verbosity=2)
