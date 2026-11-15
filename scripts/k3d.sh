#!/usr/bin/env bash
# k3d (k3s in docker), the tool behind the twin (`just twin-up`, `twin-images`,
# `twin-down`): runs a pinned release binary from target/tools/k3d-<version>/,
# fetched on first use and checked against its SHA-256 (the release's own
# checksums.txt lists it). No installer script, no system install. To bump: the new
# version and the k3d-linux-amd64 line of that release's checksums.txt.
set -euo pipefail
version="v5.9.0"
sha256="06d8f25bc3a971c4eb29e0ff08429b180402db0f4dec838c9eac427e296800a0"
root="$(cd "$(dirname "$0")/.." && pwd)"
dir="$root/target/tools/k3d-$version"
bin="$dir/k3d"
if [ ! -x "$bin" ]; then
    mkdir -p "$dir"
    echo "k3d: fetching $version into $dir" >&2
    curl -fsSL -o "$bin.part" "https://github.com/k3d-io/k3d/releases/download/$version/k3d-linux-amd64"
    if ! echo "$sha256  $bin.part" | sha256sum -c --status -; then
        rm -f "$bin.part"
        echo "k3d: the download does not match the pinned SHA-256; nothing installed" >&2
        exit 1
    fi
    chmod +x "$bin.part"
    mv "$bin.part" "$bin"
fi
exec "$bin" "$@"
