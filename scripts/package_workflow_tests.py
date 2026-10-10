#!/usr/bin/env python3
"""Adversarial workflow gates. All GitHub calls are mocked; never publishes."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import package_workflow_metadata as metadata


SHA = "a" * 40


class GitHub:
    """Byte-backed fake GitHub API and gh uploads/downloads, including races."""

    def __init__(self, tag, *, draft=True, present=True):
        self.calls = []
        self.assets = {}
        self.release = dict(id=123, tag_name=tag, target_commitish=SHA,
                            draft=draft, prerelease=False) if present else None
        self.target = dict(type="commit", sha=SHA)
        self.annotations = {}
        self.upload_extra = False
        self.corrupt_upload = False
        self.api_failure = False
        self.move_tag_on_download = False
        self.extra_on_download = False

    def run(self, command, **kwargs):
        self.calls.append(command)
        self.assert_command(command)
        args = command[1:]
        stdout = ""
        if args[0] == "api":
            endpoint = args[-1]
            if "/git/ref/" in endpoint:
                stdout = json.dumps(dict(object=self.target))
            elif "/git/tags/" in endpoint:
                stdout = json.dumps(dict(object=self.annotations[endpoint.rsplit("/", 1)[1]]))
            elif endpoint.endswith("/assets"):
                # Keep pagination visible to catch code assuming a single page.
                rows = [dict(id=index, name=name, size=len(data), digest="sha256:untrusted")
                        for index, (name, data) in enumerate(self.assets.items())]
                stdout = json.dumps([rows[:2], rows[2:]])
            elif "/releases/tags/" in endpoint:
                if self.api_failure:
                    return subprocess.CompletedProcess(command, 1, "", "gh: unavailable (HTTP 503)")
                if self.release is None:
                    return subprocess.CompletedProcess(command, 1, "", "gh: Not Found (HTTP 404)")
                stdout = json.dumps(self.release)
            else:
                raise AssertionError(endpoint)
        elif args[:2] == ["release", "create"]:
            self.release = dict(id=123, tag_name=args[2], target_commitish=args[args.index("--target") + 1],
                                draft=True, prerelease=False)
        elif args[:2] == ["release", "upload"]:
            paths = [Path(item) for item in args if item.startswith("/")]
            for path in paths:
                if path.name in self.assets:
                    raise AssertionError("Attempted overwrite")
                self.assets[path.name] = b"corrupt" if self.corrupt_upload else path.read_bytes()
            if self.upload_extra:
                self.assets["obsolete.snap"] = b"unexpected"
        elif args[:2] == ["release", "download"]:
            directory = Path(args[args.index("--dir") + 1])
            for index, argument in enumerate(args):
                if argument == "--pattern":
                    name = args[index + 1]
                    (directory / name).write_bytes(self.assets[name])
            if self.move_tag_on_download:
                self.target["sha"] = "b" * 40
            if self.extra_on_download:
                self.assets["obsolete.snap"] = b"unexpected"
        elif args[:2] == ["release", "edit"]:
            if "--draft=false" in args:
                self.release["draft"] = False
            self.release["prerelease"] = "--prerelease=true" in args
        else:
            raise AssertionError(command)
        return subprocess.CompletedProcess(command, 0, stdout, "")

    @staticmethod
    def assert_command(command):
        if command[0] != "gh" or "--clobber" in command:
            raise AssertionError(command)
        if command[1] == "release" and command[command.index("--repo") + 1] != metadata.REPOSITORY:
            raise AssertionError("Repository was not explicitly bound")

    def mutations(self):
        return [call[2] for call in self.calls if call[1] == "release" and call[2] != "download"]


class WorkflowTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="pm-workflow-tests-", dir="/tmp")
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.version = "0.1.0"
        self.tag = "v" + self.version
        self.env = dict(GITHUB_REF="refs/tags/" + self.tag, GITHUB_EVENT_NAME="push",
                        GITHUB_REPOSITORY=metadata.REPOSITORY, GITHUB_SHA=SHA)
        for name in metadata.asset_names(self.version):
            (self.directory / name).write_bytes(("tested:" + name).encode())
        metadata.write_checksums(self.directory, self.version)
        self.bytes = {path.name: path.read_bytes() for path in self.directory.iterdir()}

    def publish(self, github, **env):
        with patch.dict(os.environ, dict(self.env, **env), clear=True), \
             patch.object(metadata, "application_version", return_value=self.version), \
             patch.object(metadata.subprocess, "run", side_effect=github.run):
            metadata.publish(self.directory, self.tag)

    def test_version_intersection(self):
        for version in ("0.1.0", "1.2.3-rc.1", "1.0.0-alpha-beta", "1.0.0-0"):
            self.assertEqual(metadata.validate_version(version), version)
        for version in ("01.2.3", "1.2", "1.2.3+local", "1.2.3-01", "1.2.3-rc..1",
                        "1.2.3-rc-", "1.2.3--", "1.2.3-rc.", "1.2.3-" + "a" * 32, "../../oops"):
            with self.subTest(version=version), self.assertRaises(ValueError):
                metadata.validate_version(version)

    def test_version_tag_match(self):
        output = self.directory / "output"
        with patch.dict(os.environ, dict(self.env, GITHUB_OUTPUT=str(output)), clear=True), \
             patch.object(metadata, "application_version", return_value=self.version):
            metadata.emit_version()
            self.assertEqual(output.read_text(), "version=0.1.0\n")
            with patch.dict(os.environ, GITHUB_REF="refs/tags/v999.0.0"), self.assertRaises(ValueError):
                metadata.emit_version()

    def test_checksums_and_local_inventory(self):
        text = (self.directory / "SHA256SUMS").read_text()
        self.assertEqual(len(text.splitlines()), 4)
        for name in metadata.asset_names(self.version):
            self.assertIn(hashlib.sha256(self.bytes[name]).hexdigest() + "  " + name, text)
        metadata.checked_assets(self.directory, self.version, with_checksums=True)
        path = self.directory / metadata.asset_names(self.version)[0]
        path.unlink()
        with self.assertRaises(ValueError):
            metadata.checked_assets(self.directory, self.version, with_checksums=True)
        path.write_bytes(b"")
        with self.assertRaises(ValueError):
            metadata.checked_assets(self.directory, self.version, with_checksums=True)
        path.unlink()
        path.symlink_to(self.directory / "SHA256SUMS")
        with self.assertRaises(ValueError):
            metadata.checked_assets(self.directory, self.version, with_checksums=True)
        path.unlink()
        path.write_bytes(self.bytes[path.name])
        (self.directory / "extra").write_bytes(b"extra")
        with self.assertRaises(ValueError):
            metadata.checked_assets(self.directory, self.version, with_checksums=True)

    def test_new_release_is_staged_verified_then_published(self):
        github = GitHub(self.tag, present=False)
        self.publish(github)
        self.assertEqual(github.mutations(), ["create", "upload", "edit"])
        self.assertEqual(github.assets, self.bytes)
        self.assertFalse(github.release["draft"])
        self.assertFalse(github.release["prerelease"])
        creation = next(call for call in github.calls if call[1:3] == ["release", "create"])
        self.assertIn("--verify-tag", creation)
        self.assertIn("--draft", creation)
        self.assertIn(SHA, creation)

    def test_published_identical_retry_is_read_only(self):
        github = GitHub(self.tag, draft=False)
        github.assets = dict(self.bytes)
        self.publish(github)
        self.assertEqual(github.mutations(), [])

    def test_published_changed_content_or_checksum_rejected(self):
        for name in self.bytes:
            with self.subTest(name=name):
                github = GitHub(self.tag, draft=False)
                github.assets = dict(self.bytes, **{name: b"tampered"})
                with self.assertRaises(ValueError):
                    self.publish(github)
                self.assertEqual(github.mutations(), [])

    def test_published_missing_extra_and_duplicate_inventory_rejected(self):
        github = GitHub(self.tag, draft=False)
        for inventory in ({}, dict(self.bytes, obsolete=b"old")):
            github.assets = inventory
            with self.assertRaises(ValueError):
                self.publish(github)
        self.assertEqual(github.mutations(), [])
        # API names must be unique, even when a misleading response lists five.
        with patch.object(metadata, "gh", return_value=subprocess.CompletedProcess(
            [], 0, json.dumps([[dict(name="SHA256SUMS"), dict(name="SHA256SUMS")]]), "")), \
             self.assertRaises(ValueError):
            metadata.remote_assets(github.release, self.tag, SHA, set(self.bytes), complete=False)

    def test_draft_resume_uploads_only_missing_and_normalizes_stable(self):
        github = GitHub(self.tag)
        name = next(iter(self.bytes))
        github.assets[name] = self.bytes[name]
        github.release["prerelease"] = True
        self.publish(github)
        self.assertEqual(github.mutations(), ["upload", "edit"])
        self.assertFalse(github.release["prerelease"])
        upload = next(call for call in github.calls if call[1:3] == ["release", "upload"])
        self.assertNotIn(str(self.directory / name), upload)
        self.assertEqual(github.assets, self.bytes)
        self.assertEqual(sum(call[1:3] == ["release", "download"] for call in github.calls), 2)

    def test_draft_unexpected_or_changed_existing_assets_fail_before_upload(self):
        for inventory in ({"obsolete.snap": b"old"}, {"SHA256SUMS": b"tampered"}):
            github = GitHub(self.tag)
            github.assets = inventory
            with self.assertRaises(ValueError):
                self.publish(github)
            self.assertEqual(github.mutations(), [])

    def test_post_upload_extra_and_corruption_block_publish(self):
        for option in ("upload_extra", "corrupt_upload"):
            github = GitHub(self.tag)
            setattr(github, option, True)
            with self.assertRaises(ValueError):
                self.publish(github)
            self.assertEqual(github.mutations(), ["upload"])
            self.assertTrue(github.release["draft"])

    def test_wrong_tag_target_or_release_target_rejected(self):
        for draft in (True, False):
            github = GitHub(self.tag, draft=draft)
            github.assets = dict(self.bytes)
            github.target["sha"] = "b" * 40
            with self.assertRaises(ValueError):
                self.publish(github)
            self.assertEqual(github.mutations(), [])
            github.target["sha"] = SHA
            for field, value in (("tag_name", "v99.0.0"), ("target_commitish", "main")):
                original = github.release[field]
                github.release[field] = value
                with self.assertRaises(ValueError):
                    self.publish(github)
                github.release[field] = original
            self.assertEqual(github.mutations(), [])

    def test_annotated_tag_peeling(self):
        github = GitHub(self.tag, draft=False)
        github.assets = dict(self.bytes)
        github.target = dict(type="tag", sha="b" * 40)
        github.annotations["b" * 40] = dict(type="tag", sha="c" * 40)
        github.annotations["c" * 40] = dict(type="commit", sha=SHA)
        self.publish(github)
        github.annotations["c" * 40] = dict(type="tree", sha=SHA)
        with self.assertRaises(ValueError):
            self.publish(github)

    def test_published_stable_prerelease_flag_normalized_without_upload(self):
        github = GitHub(self.tag, draft=False)
        github.assets = dict(self.bytes)
        github.release["prerelease"] = True
        self.publish(github)
        self.assertEqual(github.mutations(), ["edit"])
        self.assertFalse(github.release["prerelease"])

    def test_prerelease_flag_and_latest_policy(self):
        self.version, self.tag = "0.1.0-rc.1", "v0.1.0-rc.1"
        self.env["GITHUB_REF"] = "refs/tags/" + self.tag
        for path in self.directory.iterdir():
            path.unlink()
        for name in metadata.asset_names(self.version):
            (self.directory / name).write_bytes(b"prerelease")
        metadata.write_checksums(self.directory, self.version)
        github = GitHub(self.tag)
        self.publish(github)
        self.assertTrue(github.release["prerelease"])
        self.assertIn("--latest=false", github.calls[-1])

    def test_publication_guards_and_local_tampering_precede_github(self):
        for invalid in (dict(GITHUB_EVENT_NAME="pull_request"), dict(GITHUB_EVENT_NAME="workflow_dispatch"),
                        dict(GITHUB_REPOSITORY="fork/app"), dict(GITHUB_REF="refs/heads/main"), dict(GITHUB_SHA="bad")):
            github = GitHub(self.tag)
            with self.assertRaises(ValueError):
                self.publish(github, **invalid)
            self.assertEqual(github.calls, [])
        (self.directory / "SHA256SUMS").write_bytes(b"tampered")
        github = GitHub(self.tag)
        with self.assertRaises(ValueError):
            self.publish(github)
        self.assertEqual(github.calls, [])

    def test_api_errors_do_not_create_a_release(self):
        github = GitHub(self.tag)
        github.api_failure = True
        with self.assertRaises(subprocess.CalledProcessError):
            self.publish(github)
        self.assertEqual(github.mutations(), [])

    def test_mutations_during_download_block_publish_and_retry(self):
        for draft in (True, False):
            for option in ("move_tag_on_download", "extra_on_download"):
                with self.subTest(draft=draft, mutation=option):
                    github = GitHub(self.tag, draft=draft)
                    github.assets = dict(self.bytes)
                    setattr(github, option, True)
                    with self.assertRaises(ValueError):
                        self.publish(github)
                    self.assertNotIn("edit", github.mutations())


if __name__ == "__main__":
    unittest.main(verbosity=2)
