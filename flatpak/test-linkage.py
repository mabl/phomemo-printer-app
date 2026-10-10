"""Verify that SDK/package execution uses the pinned shared libraries in /app."""
import argparse
import re
import subprocess
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", required=True, type=Path)
args = parser.parse_args()
result = subprocess.run(["ldd", str(args.binary)], check=True, capture_output=True, text=True)
for library in ("libpappl.so.1", "libcups.so.2", "libavahi-client.so.3", "libavahi-common.so.3"):
    match = re.search(r"(?m)^\s*" + re.escape(library) + r"\s+=>\s+(\S+)", result.stdout)
    if not match or not Path(match[1]).resolve().is_relative_to("/app/lib"):
        parser.exit(1, f"{library} must resolve to the pinned /app/lib build:\n{result.stdout}")
print("Pinned shared PAPPL, libcups and Avahi libraries resolve under /app/lib.")
