#!/usr/bin/env python3
"""Build-time checks of the primed snap; never installed as a runtime tool."""

import argparse
import os
from pathlib import Path
import re
import subprocess
import sys


def output(*args, env=None):
    return subprocess.check_output(args, text=True, env=env)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prime", required=True, type=Path)
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--triplet", required=True)
    args = parser.parse_args()
    prime = args.prime.resolve(strict=True)
    source = args.source.resolve(strict=True)
    binary = prime / "usr/bin/phomemo-printer-app"
    pappl = prime / "usr/lib" / args.triplet / "libpappl.so.1"
    env = dict(os.environ, LD_LIBRARY_PATH=":".join(
        str(prime / directory / args.triplet) for directory in ("usr/lib", "lib")
    ))

    dynamic = output("readelf", "-dW", str(binary))
    if "[libpappl.so.1]" not in dynamic:
        raise RuntimeError("Executable must depend on shared PAPPL 1.x")
    symbol = "_papplMainloopGetServerPath"
    symbols = output("readelf", "--dyn-syms", "-W", str(binary))
    if not re.search(r"GLOBAL\s+DEFAULT\s+(?!UND\b)\S+\s+" + symbol + r"\b", symbols):
        raise RuntimeError("Executable must export the runtime-path override")
    if "SYMBOLIC" in output("readelf", "-dW", str(pappl)):
        raise RuntimeError("PAPPL must not be linked with -Bsymbolic")
    if symbol not in output("readelf", "-rW", str(pappl)):
        raise RuntimeError("PAPPL's runtime-path helper must remain interposable")
    libraries = output("ldd", str(binary), env=env)
    if str(pappl) not in libraries or "not found" in libraries or "/nix/store" in libraries:
        raise RuntimeError("Invalid packaged library resolution:\n" + libraries)
    for relative in ("usr/bin/lpstat", "usr/sbin/lpadmin"):
        client = prime / relative
        if not os.access(client, os.X_OK):
            raise RuntimeError("Missing bundled CUPS client: " + str(client))
        if "not found" in output("ldd", str(client), env=env):
            raise RuntimeError("Missing CUPS client libraries: " + str(client))

    subprocess.run([
        "shellcheck", "-x", str(source / "snap/local/runtime-setup"),
        str(source / "snap/hooks/configure"),
    ], cwd=source, check=True)
    subprocess.run([
        sys.executable, str(source / "snap/local/test_launchers.py"),
        "--snap-root", str(prime),
    ], env=env, check=True)
    subprocess.run([
        sys.executable, str(source / "scripts/test_packaged_runtime.py"),
        "--binary", str(binary),
    ], env=env, check=True)
    print("Primed snap: shared PAPPL interposition, CUPS clients and runtime verified.")


if __name__ == "__main__":
    main()
