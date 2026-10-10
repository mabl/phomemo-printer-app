#!/usr/bin/env python3
"""Version, asset, and draft-release gates used only by the package workflow."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = "mabl/phomemo-printer-app"
VERSION_PATTERN = re.compile(
    r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
)


def validate_version(version):
    match = VERSION_PATTERN.fullmatch(version)
    if not match or any(
        part.isdigit() and len(part) > 1 and part.startswith("0")
        for part in (match.group(4) or "").split(".")
    ):
        raise ValueError(f"Unsupported release version: {version!r}")
    # Snap versions are limited to 32 characters. Build metadata is deliberately
    # unsupported so the tag, executable version and both filenames agree.
    if version[-1] in "-.":
        raise ValueError("Snap versions must end with an ASCII letter or digit")
    if len(version) > 32:
        raise ValueError("The application version exceeds Snap's 32-character limit")
    return version


def application_version():
    with (ROOT / "phomemo-pappl/Cargo.toml").open("rb") as source:
        return validate_version(tomllib.load(source)["package"]["version"])


def emit_version():
    version = application_version()
    ref = os.environ.get("GITHUB_REF", "")
    if ref.startswith("refs/tags/") and ref != f"refs/tags/v{version}":
        raise ValueError(f"Tag {ref!r} must exactly match Cargo version v{version}")
    print(f"Application version: {version}", flush=True)
    with Path(os.environ["GITHUB_OUTPUT"]).open("a") as output:
        output.write(f"version={version}\n")


def asset_names(version):
    validate_version(version)
    return sorted(
        f"phomemo-printer-app_{version}_{arch}.{kind}"
        for arch in ("amd64", "arm64")
        for kind in ("snap", "flatpak")
    )


def checked_assets(directory, version, *, with_checksums=False):
    directory = directory.resolve(strict=True)
    expected = set(asset_names(version))
    if with_checksums:
        expected.add("SHA256SUMS")
    actual = {item.name for item in directory.iterdir()}
    if actual != expected:
        raise ValueError(f"Incorrect release assets: missing={expected - actual}, extra={actual - expected}")
    assets = [directory / name for name in sorted(expected)]
    if any(item.is_symlink() or not item.is_file() or item.stat().st_size == 0 for item in assets):
        raise ValueError("Release assets must be nonempty regular files, not symlinks")
    return assets


def checksum_text(assets):
    lines = []
    for asset in assets:
        with asset.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        lines.append(f"{digest}  {asset.name}\n")
    return "".join(lines)


def write_checksums(directory, version):
    assets = checked_assets(directory, version)
    (directory / "SHA256SUMS").write_text(checksum_text(assets))


def gh(*arguments, **kwargs):
    return subprocess.run(["gh", *arguments], check=True, timeout=300, **kwargs)


def api(path):
    return json.loads(gh("api", f"repos/{REPOSITORY}/{path}", capture_output=True, text=True).stdout)


def verify_tag_target(tag, sha):
    target = api(f"git/ref/tags/{tag}")["object"]
    # Annotated tags can themselves point to annotated tags. Bound traversal
    # and require a commit rather than silently accepting a tree/blob.
    for _ in range(16):
        if target["type"] == "commit":
            if target["sha"] != sha:
                raise ValueError("The remote release tag does not point to the tested commit")
            return
        if target["type"] != "tag":
            break
        target = api(f"git/tags/{target['sha']}")["object"]
    raise ValueError("Release tag must resolve to a commit")


def release_info(tag):
    result = subprocess.run(
        ["gh", "api", f"repos/{REPOSITORY}/releases/tags/{tag}"],
        capture_output=True, text=True, timeout=60, check=False,
    )
    if result.returncode:
        if "(HTTP 404)" in result.stderr:
            return None
        raise subprocess.CalledProcessError(result.returncode, result.args, result.stdout, result.stderr)
    return json.loads(result.stdout)


def remote_assets(release, tag, sha, expected, *, complete):
    if release["tag_name"] != tag or release["target_commitish"] != sha:
        raise ValueError("Release tag/target must match the tested tag and commit SHA")
    pages = json.loads(gh(
        "api", "--paginate", "--slurp", f"repos/{REPOSITORY}/releases/{release['id']}/assets",
        capture_output=True, text=True,
    ).stdout)
    assets = [asset for page in pages for asset in page]
    names = [asset["name"] for asset in assets]
    if len(set(names)) != len(names) or not set(names) <= expected or (complete and set(names) != expected):
        raise ValueError(f"Unexpected/incomplete remote release assets: {names!r}")
    # Detect replacement/removal while downloading. These API fields are only
    # a mutation fingerprint; content equality still requires hashing downloads.
    return {asset["name"]: (asset["id"], asset["size"], asset.get("digest"), asset.get("updated_at"))
            for asset in assets}


def verify_remote_bytes(tag, assets, names):
    if not names:
        return
    with tempfile.TemporaryDirectory(prefix="pm-release-", dir="/tmp") as temporary:
        directory = Path(temporary)
        patterns = [argument for name in sorted(names) for argument in ("--pattern", name)]
        gh("release", "download", tag, "--repo", REPOSITORY, "--dir", str(directory), *patterns)
        if {item.name for item in directory.iterdir()} != names:
            raise ValueError("Downloaded release inventory differs from the API inventory")
        for asset in assets:
            if asset.name not in names:
                continue
            remote = directory / asset.name
            if remote.is_symlink() or not remote.is_file():
                raise ValueError("Downloaded assets must be regular files")
            with asset.open("rb") as local, remote.open("rb") as downloaded:
                if hashlib.file_digest(local, "sha256").digest() != hashlib.file_digest(downloaded, "sha256").digest():
                    raise ValueError(f"Remote release asset differs from the tested artifact: {asset.name}")


def publish(directory, tag):
    # These guards are defense in depth against manual/fork publication.
    version = application_version()
    if (
        os.environ.get("GITHUB_EVENT_NAME") != "push"
        or os.environ.get("GITHUB_REPOSITORY") != REPOSITORY
        or os.environ.get("GITHUB_REF") != f"refs/tags/{tag}"
        or tag != f"v{version}"
    ):
        raise ValueError("Publication requires a matching vVERSION push in the upstream repository")
    assets = checked_assets(directory, version, with_checksums=True)
    bundles = [asset for asset in assets if asset.name != "SHA256SUMS"]
    if (directory / "SHA256SUMS").read_text() != checksum_text(bundles):
        raise ValueError("Release checksums do not match the downloaded bundles")

    sha = os.environ.get("GITHUB_SHA", "")
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ValueError("GITHUB_SHA must identify the tested commit")
    verify_tag_target(tag, sha)
    release = release_info(tag)
    expected = {asset.name for asset in assets}
    prerelease = "-" in version
    if release is None:
        gh("release", "create", tag, "--repo", REPOSITORY, "--target", sha,
           "--verify-tag", "--draft", "--generate-notes", "--title", tag)
        release = release_info(tag)
        if release is None or not release["draft"]:
            raise ValueError("New release was not created as a draft")
    release_id = release["id"]
    inventory = remote_assets(release, tag, sha, expected, complete=not release["draft"])
    names = set(inventory)
    verify_remote_bytes(tag, assets, names)
    if not release["draft"]:
        verify_tag_target(tag, sha)
        current = release_info(tag)
        if current is None or current["id"] != release_id or current["draft"]:
            raise ValueError("Published release changed during verification")
        if remote_assets(current, tag, sha, expected, complete=True) != inventory:
            raise ValueError("Published release assets changed during verification")
        # A retry is successful only when every remote byte matches, including
        # SHA256SUMS. Never overwrite an asset of an already-published release.
        if current["prerelease"] != prerelease:
            gh("release", "edit", tag, "--repo", REPOSITORY,
               f"--prerelease={str(prerelease).lower()}", *(["--latest=false"] if prerelease else []))
        print(f"Release {tag} already contains the identical tested artifacts", flush=True)
        return
    missing = [str(asset) for asset in assets if asset.name not in names]
    if missing:
        gh("release", "upload", tag, "--repo", REPOSITORY, *missing)
    # Re-read the complete remote inventory and hash actual downloads after all
    # uploads. Unexpected old assets and corrupted uploads block publication.
    final = release_info(tag)
    if final is None or final["id"] != release_id or not final["draft"]:
        raise ValueError("Draft release changed while uploading")
    inventory = remote_assets(final, tag, sha, expected, complete=True)
    verify_remote_bytes(tag, assets, set(inventory))
    verify_tag_target(tag, sha)
    current = release_info(tag)
    if current is None or current["id"] != release_id or not current["draft"]:
        raise ValueError("Draft release changed during content verification")
    if remote_assets(current, tag, sha, expected, complete=True) != inventory:
        raise ValueError("Draft assets changed during content verification")
    gh("release", "edit", tag, "--repo", REPOSITORY, "--draft=false",
       f"--prerelease={str(prerelease).lower()}", *(["--latest=false"] if prerelease else []))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("version")
    checksum_parser = commands.add_parser("checksums")
    checksum_parser.add_argument("--directory", type=Path, required=True)
    checksum_parser.add_argument("--version", required=True)
    publish_parser = commands.add_parser("publish")
    publish_parser.add_argument("--directory", type=Path, required=True)
    publish_parser.add_argument("--tag", required=True)
    args = parser.parse_args()
    if args.command == "version":
        emit_version()
    elif args.command == "checksums":
        write_checksums(args.directory.resolve(strict=True), args.version)
    else:
        publish(args.directory.resolve(strict=True), args.tag)


if __name__ == "__main__":
    main()
