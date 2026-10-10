"""Generate/check Flatpak Cargo inputs; --check is stdlib-only and offline.

--update uses uv and the official, revision- and checksum-pinned generator.
Only JSON source descriptions and cbindgen's lockfile are kept, never vendors.
"""
import argparse
import hashlib
import io
import json
import subprocess
import tarfile
import tempfile
import tomllib
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PACKAGING = ROOT / "flatpak"
GENERATOR_REVISION = "74697c75b630d7330e77250fc13cb5ea688d9479"
GENERATOR_SHA256 = "0a2db6be87d75910facef28ab46d4d6460802e8419ab850d0caa6a364d26b380"
GENERATOR_URL = (
    "https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/"
    f"{GENERATOR_REVISION}/cargo/flatpak-cargo-generator.py"
)
MANIFEST = PACKAGING / "io.github.mabl.phomemo-printer-app.json"


def digest(data):
    return hashlib.sha256(data).hexdigest()


def download(url, sha256):
    with urllib.request.urlopen(url, timeout=60) as response:
        data = response.read()
    if digest(data) != sha256:
        raise ValueError(f"Checksum mismatch: {url}")
    return data


def inputs():
    return [
        (ROOT / "Cargo.lock", PACKAGING / "cargo-sources.json"),
        (PACKAGING / "cbindgen-Cargo.lock", PACKAGING / "cbindgen-sources.json"),
    ]


def validate(lock, sources):
    packages = tomllib.loads(lock.read_text())["package"]
    expected = {}
    for package in packages:
        if "source" not in package:
            continue
        # Fail closed rather than silently omitting new git/private dependencies.
        if package["source"] != "registry+https://github.com/rust-lang/crates.io-index":
            raise ValueError(f"Unsupported locked source: {package['source']}")
        name, version = package["name"], package["version"]
        expected[f"https://static.crates.io/crates/{name}/{name}-{version}.crate"] = (
            package["checksum"], f"cargo/vendor/{name}-{version}"
        )
    entries = json.loads(sources.read_text())
    actual = {}
    for source in entries:
        if source["type"] == "archive":
            if source["url"] in actual:
                raise ValueError("Duplicate Cargo archive")
            actual[source["url"]] = (source["sha256"], source["dest"])
        elif source["type"] != "inline":
            raise ValueError("Unexpected generated source type")
    if expected != actual:
        raise ValueError(f"{sources.name} does not exactly match {lock.name}")
    config = entries[-1]
    if config.get("dest") != "cargo" or config.get("dest-filename") != "config":
        raise ValueError("Missing Cargo vendor configuration")
    parsed = tomllib.loads(config["contents"])["source"]
    if parsed != {"vendored-sources": {"directory": "cargo/vendor"},
                  "crates-io": {"replace-with": "vendored-sources"}}:
        raise ValueError("Unexpected Cargo vendor configuration")


def metadata():
    files = {str(path.relative_to(ROOT)): digest(path.read_bytes())
             for pair in inputs() for path in pair}
    cbindgen = next(module for module in json.loads(MANIFEST.read_text())["modules"]
                    if module["name"] == "cbindgen")["sources"][0]
    return {"generator_revision": GENERATOR_REVISION,
            "generator_sha256": GENERATOR_SHA256, "inputs_and_outputs_sha256": files,
            "cbindgen_source": cbindgen}


def update():
    cbindgen = next(module for module in json.loads(MANIFEST.read_text())["modules"]
                    if module["name"] == "cbindgen")["sources"][0]
    data = download(cbindgen["url"], cbindgen["sha256"])
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        locks = [member for member in archive.getmembers()
                 if member.name.endswith("/Cargo.lock") and member.isfile()]
        if len(locks) != 1:
            raise ValueError("cbindgen crate must include one Cargo.lock")
        (PACKAGING / "cbindgen-Cargo.lock").write_bytes(archive.extractfile(locks[0]).read())
    with tempfile.TemporaryDirectory(prefix="phomemo-flatpak-generator-") as temporary:
        script = Path(temporary) / "flatpak-cargo-generator.py"
        script.write_bytes(download(GENERATOR_URL, GENERATOR_SHA256))
        for lock, sources in inputs():
            subprocess.run([
                "uv", "run", "--no-project", "--no-config",
                "--with", "aiohttp==3.12.15", "--with", "tomlkit==0.13.3",
                "--with", "PyYAML==6.0.2", str(script), str(lock), "-o", str(sources),
            ], check=True, cwd=ROOT)
            validate(lock, sources)
    (PACKAGING / "sources-lock.json").write_text(json.dumps(metadata(), indent=2) + "\n")


def check():
    for lock, sources in inputs():
        validate(lock, sources)
    saved = json.loads((PACKAGING / "sources-lock.json").read_text())
    if saved != metadata():
        raise ValueError("Cargo input/output changed; run python3 flatpak/update-sources.py --update")
    print("Flatpak Cargo inputs and checksummed sources are current (offline check).")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--update", action="store_true")
    mode.add_argument("--check", action="store_true")
    args = parser.parse_args()
    try:
        update() if args.update else check()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"{error}\n")
