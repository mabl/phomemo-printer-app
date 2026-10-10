#!/bin/sh
# Run on a Flatpak-capable Linux CI runner. Output must be outside the checkout.
set -eu
if [ "$#" -ne 1 ]; then
    printf '%s\n' 'Usage: sh flatpak/build-bundle.sh /absolute/output-directory' >&2
    exit 2
fi
case "$1" in /*) ;; *) printf '%s\n' 'Output directory must be absolute.' >&2; exit 2;; esac
root=$(realpath "$(dirname "$0")/..")
output=$(realpath -m "$1")
case "$output/" in "$root/"*)
    printf '%s\n' 'Use an output directory outside the source checkout.' >&2
    exit 2;;
esac
manifest="$root/flatpak/io.github.mabl.phomemo-printer-app.json"
python3 "$root/flatpak/update-sources.py" --check
mkdir -p "$output"
if [ -e "$output/build" ]; then
    printf '%s\n' 'Output build directory already exists; select a fresh output directory.' >&2
    exit 2
fi
# Stage all checksummed downloads first; compilation has no network permission
# and --disable-download ensures a missing source fails rather than fetching.
flatpak-builder --user --state-dir="$output/builder-state" --download-only \
    "$output/build" "$manifest"
flatpak-builder --user --disable-rofiles-fuse --state-dir="$output/builder-state" --disable-download \
    --repo="$output/repo" "$output/build" "$manifest"
arch=$(flatpak --default-arch)
case "$arch" in
    x86_64) package_arch=amd64;;
    aarch64) package_arch=arm64;;
    *) printf '%s\n' "Unsupported release architecture: $arch" >&2; exit 2;;
esac
version=$(python3 -c 'import sys, tomllib; print(tomllib.load(open(sys.argv[1], "rb"))["package"]["version"])' "$root/phomemo-pappl/Cargo.toml")
bundle="$output/phomemo-printer-app_${version}_${package_arch}.flatpak"
flatpak build-bundle --arch="$arch" \
    --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo \
    "$output/repo" "$bundle" io.github.mabl.phomemo-printer-app experimental
sha256sum "$bundle"
printf '%s\n' "GitHub release artifact: $bundle"
