"""Regression checks for stale/tampered offline Cargo source descriptions."""
import contextlib
import importlib.util
import io
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.dont_write_bytecode = True


class SourceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="pf-sources-")
        root = Path(self.temporary.name)
        packaging = root / "flatpak"
        packaging.mkdir()
        shutil.copy2(HERE.parent / "Cargo.lock", root / "Cargo.lock")
        for name in ("cargo-sources.json", "cbindgen-sources.json", "cbindgen-Cargo.lock",
                     "sources-lock.json", "io.github.mabl.phomemo-printer-app.json"):
            shutil.copy2(HERE / name, packaging / name)
        spec = importlib.util.spec_from_file_location("source_checker", HERE / "update-sources.py")
        self.checker = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.checker)
        self.checker.ROOT = root
        self.checker.PACKAGING = packaging
        self.checker.MANIFEST = packaging / "io.github.mabl.phomemo-printer-app.json"
        self.packaging = packaging
        self.lock = root / "Cargo.lock"

    def tearDown(self):
        self.temporary.cleanup()

    def test_current_sources_pass_without_network(self):
        # The stdlib-only check must not accidentally call the update downloader.
        def forbidden(*args):
            self.fail("Offline check attempted a download")
        self.checker.download = forbidden
        with contextlib.redirect_stdout(io.StringIO()):
            self.checker.check()

    def test_lockfile_only_change_is_detected(self):
        self.lock.write_text(self.lock.read_text() + "\n# changed lock input\n")
        with self.assertRaisesRegex(ValueError, "input/output changed"):
            self.checker.check()

    def test_changed_locked_checksum_is_detected(self):
        lines = self.lock.read_text().splitlines()
        index = next(i for i, line in enumerate(lines) if line.startswith("checksum = "))
        lines[index] = 'checksum = "' + "0" * 64 + '"'
        self.lock.write_text("\n".join(lines) + "\n")
        with self.assertRaisesRegex(ValueError, "exactly match"):
            self.checker.check()

    def test_missing_crate_is_detected(self):
        path = self.packaging / "cargo-sources.json"
        entries = json.loads(path.read_text())
        del entries[0]
        path.write_text(json.dumps(entries))
        with self.assertRaisesRegex(ValueError, "exactly match"):
            self.checker.check()

    def test_altered_inline_checksum_is_detected(self):
        path = self.packaging / "cbindgen-sources.json"
        entries = json.loads(path.read_text())
        entries[1]["contents"] = '{"package": null, "files": {}}'
        path.write_text(json.dumps(entries))
        with self.assertRaisesRegex(ValueError, "input/output changed"):
            self.checker.check()

    def test_cbindgen_pin_change_needs_update(self):
        path = self.checker.MANIFEST
        manifest = json.loads(path.read_text())
        module = next(item for item in manifest["modules"] if item["name"] == "cbindgen")
        module["sources"][0]["sha256"] = "0" * 64
        path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "input/output changed"):
            self.checker.check()

    def test_new_git_source_is_rejected(self):
        text = self.lock.read_text().replace(
            "registry+https://github.com/rust-lang/crates.io-index",
            "git+https://example.invalid/dependency#" + "0" * 40, 1)
        self.lock.write_text(text)
        with self.assertRaisesRegex(ValueError, "Unsupported locked source"):
            self.checker.check()


if __name__ == "__main__":
    unittest.main(verbosity=2)
