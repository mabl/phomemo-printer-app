#!/usr/bin/env python3
"""Runtime helper boundaries; no installed packages, services or hardware."""

import io
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import package_workflow_desktop as desktop
import package_workflow_runtime as runtime


class RuntimeHelperTests(unittest.TestCase):
    def test_default_desktop_environment_excludes_overrides_and_real_bus(self):
        inherited = dict(PHOMEMO_RUNTIME_DIRECTORY="/bad", PHOMEMO_STATE_FILE="/bad/state",
                         PHOMEMO_SERVER_PORT="9999", SNAP_COMMON="/bad/snap", RUNTIME_DIRECTORY="/bad/run",
                         DBUS_SESSION_BUS_ADDRESS="unix:path=/real/bus", DBUS_STARTER_ADDRESS="unix:path=/real/bus",
                         DBUS_SESSION_BUS_PID="1234", DBUS_STARTER_BUS_TYPE="session", PATH="/usr/bin",
                         XDG_RUNTIME_DIR="/run/user/1001")
        with patch.dict(os.environ, inherited, clear=True):
            self.assertEqual(desktop.clean_environment(), dict(PATH="/usr/bin", XDG_RUNTIME_DIR="/run/user/1001"))

    def test_flatpak_user_bundle_reuses_system_runtime(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as temporary:
            root = Path(temporary)
            app = root / "app-deploy"
            system = root / "system-runtime"
            (app / "files/libexec").mkdir(parents=True)
            (app / "files/lib").mkdir()
            binary = app / "files/libexec/phomemo-printer-app"
            binary.write_bytes(b"ELF")
            (system / "files/lib/x86_64-linux-gnu").mkdir(parents=True)
            loader = system / "files/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2"
            loader.write_bytes(b"ELF")

            def info(*args):
                return str(app) if args == ("--show-location",) else "org.freedesktop.Platform/x86_64/25.08"

            def reply(command, **kwargs):
                return subprocess.CompletedProcess(command, 1 if "--user" in command else 0, str(system), "")

            with patch.object(runtime, "flatpak_info", side_effect=info), \
                 patch.object(runtime.platform, "machine", return_value="x86_64"), \
                 patch.object(runtime.subprocess, "run", side_effect=reply) as execute:
                actual_binary, actual_loader, libraries = runtime.installed_layout("flatpak")
                self.assertEqual(actual_binary, binary)
                self.assertEqual(actual_loader, loader)
                self.assertIn(app / "files/lib", libraries)
                self.assertIn("--system", execute.call_args_list[-1].args[0])

    def test_ipp_rejects_error_status_truncation_and_unrelated_response(self):
        class Response(io.BytesIO):
            status = 200

            class Headers:
                @staticmethod
                def get_content_type():
                    return "application/ipp"

            headers = Headers()

        good = struct.pack("!BBHI", 2, 0, 0, 1) + b"system-uuid"
        for payload in (b"short", struct.pack("!BBHI", 2, 0, 0x0400, 1) + b"system-uuid",
                        struct.pack("!BBHI", 2, 0, 0, 2) + b"system-uuid", good[:8] + b"unrelated"):
            with self.subTest(payload=payload), patch.object(runtime.urllib.request, "urlopen", return_value=Response(payload)), \
                 self.assertRaises(RuntimeError):
                runtime.check_ipp(8631)
        with patch.object(runtime.urllib.request, "urlopen", return_value=Response(good)) as request:
            runtime.check_ipp(8631)
            outgoing = request.call_args.args[0]
            self.assertEqual(outgoing.full_url, "http://127.0.0.1:8631/ipp/system")
            self.assertEqual(outgoing.data[:8], struct.pack("!BBHI", 2, 0, 0x005B, 1))

    def test_cups_uses_exact_uri_and_cleans_only_ci_queues(self):
        queues = {}
        calls = []

        def cli(*args):
            calls.append(args)
            if args[0] == "register-cups":
                queues[args[2]] = "ipp://127.0.0.1:8123/ipp/print"
            else:
                queues.pop(args[2], None)

        def host(command, **kwargs):
            calls.append(command)
            code, output = 0, ""
            if command[:2] == ("lpstat", "-p"):
                code = 0 if command[2] in queues else 1
            elif command[:2] == ("lpstat", "-v"):
                output = "device for queue: " + queues[command[2]] + "\n"
            elif command[:2] == ("lpadmin", "-p"):
                queues[command[2]] = command[command.index("-v") + 1]
            elif command[:2] == ("lpadmin", "-x"):
                queues.pop(command[2], None)
            return subprocess.CompletedProcess(command, code, output, "")

        with patch.object(runtime.subprocess, "run", side_effect=host):
            runtime.snap_cups_smoke(cli, 8123)
        exact = next(call for call in calls if call[:2] == ("lpadmin", "-p"))
        self.assertIn("ipp://127.0.0.1:8123/ipp/print/CI-Dummy", exact)
        self.assertEqual(queues, {})
        self.assertEqual({call[2] for call in calls if call[:2] == ("lpadmin", "-x")},
                         {"package-ci-registered", "package-ci-exact"})
        self.assertTrue(all("submit" not in call and "lp" not in call for call in calls))

    def test_existing_cups_queue_is_not_removed(self):
        with patch.object(runtime.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "", "")) as host:
            with self.assertRaises(RuntimeError):
                runtime.snap_cups_smoke(lambda *args: self.fail("Existing queue was mutated"), 8123)
        self.assertTrue(all(call.args[0][0] != "lpadmin" for call in host.call_args_list))


class LocalDesktopIntegration(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("PACKAGE_WORKFLOW_LOCAL_LAUNCHER"),
                         "optional real local launcher validation; installed sandboxes are tested by package jobs")
    def test_default_desktop_algorithm_with_real_launcher_server_and_portal(self):
        # Adapt only the Flatpak command prefix and its standard XDG data
        # mapping. Everything else is real: supervisor, default directories,
        # isolated bus/OpenURI, web/IPP, independent CLI processes and state.
        launcher = Path(os.environ["PACKAGE_WORKFLOW_LOCAL_LAUNCHER"]).resolve(strict=True)
        original_run, original_popen = subprocess.run, subprocess.Popen
        with tempfile.TemporaryDirectory(prefix="pd-", dir="/tmp") as temporary:
            root = Path(temporary)
            home = root / "home"
            user_runtime = root / "run"
            home.mkdir(mode=0o700)
            user_runtime.mkdir(mode=0o700)

            def adapt(command, kwargs):
                if command[:2] == ["flatpak", "run"]:
                    arguments = command[command.index(desktop.APP) + 1:]
                    cli = "--command=phomemo-printer-app" in command
                    command = [str(launcher), *(["--cli"] if cli else []), *arguments]
                    kwargs["env"] = dict(kwargs["env"], XDG_DATA_HOME=str(home / ".var/app" / desktop.APP / "data"),
                                         XDG_CONFIG_HOME=str(home / ".var/app" / desktop.APP / "config"))
                return command

            def execute(command, **kwargs):
                return original_run(adapt(command, kwargs), **kwargs)

            def start(command, **kwargs):
                return original_popen(adapt(command, kwargs), **kwargs)

            with patch.dict(os.environ, HOME=str(home), XDG_RUNTIME_DIR=str(user_runtime)), \
                 patch.object(subprocess, "run", side_effect=execute), \
                 patch.object(subprocess, "Popen", side_effect=start):
                desktop.installed_desktop(Path(__file__).resolve().parents[1] / "flatpak/test-portal.c", runtime.check_ipp)


if __name__ == "__main__":
    unittest.main(verbosity=2)
