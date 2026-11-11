#!/usr/bin/env bash
# Builds the browser release into a static site directory (`just web-release`,
# also step one of `just deploy`):
#   <out>/index.html            crates/client/index.html
#   <out>/api/wasm.js, wasm.wasm  wasm-bindgen output (the paths index.html loads —
#                               wasm-server-runner's layout, so one page serves both)
#   <out>/api/version           the git revision (index.html polls it and reloads
#                               open tabs when a redeploy changes it)
#   <out>/assets/               the runtime assets (minus the dev cert-hash file)
# Env: OUT (default target/web), CAP (the justfile's core-pinning prefix),
# VOXEL_REGION_BLOCKS (must match the server's build),
# VOXEL_WEB_SERVER (the main menu's prefilled Join server address).
set -euo pipefail
cd "$(dirname "$0")/.."
out="${OUT:-target/web}"
cap="${CAP:-}"

# wasm-bindgen's CLI must match Cargo.lock's wasm-bindgen exactly (same install
# as `just determinism-wasm`).
ver=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/version = |"/, ""); print; exit }' Cargo.lock)
tools="$PWD/target/tools/wasm-bindgen-$ver"
[ -x "$tools/bin/wasm-bindgen" ] || $cap cargo install --locked wasm-bindgen-cli --version "$ver" --root "$tools"

$cap cargo build -p client --profile wasm-release --target wasm32-unknown-unknown

rm -rf "$out.tmp"
mkdir -p "$out.tmp/api"
"$tools/bin/wasm-bindgen" --target web --no-typescript --out-name wasm --out-dir "$out.tmp/api" \
    target/wasm32-unknown-unknown/wasm-release/client.wasm
mv "$out.tmp/api/wasm_bg.wasm" "$out.tmp/api/wasm.wasm"

# wasm-opt from a pinned binaryen release under target/tools (downloaded on first
# use). NOT the distro's: Ubuntu 24.04's binaryen 108 re-points wasm-bindgen's
# externref table export at the funcref table, and the page dies at start-up with
# "RangeError: failed to grow table". BINARYEN=none skips the pass.
binaryen="${BINARYEN:-version_133}"
if [ "$binaryen" != none ]; then
    bdir="$PWD/target/tools/binaryen-$binaryen"
    if [ ! -x "$bdir/bin/wasm-opt" ]; then
        echo "web-release: fetching binaryen $binaryen into $bdir"
        mkdir -p "$bdir.tmp"
        curl -fsSL "https://github.com/WebAssembly/binaryen/releases/download/$binaryen/binaryen-$binaryen-x86_64-linux.tar.gz" |
            tar -xz -C "$bdir.tmp" --strip-components=1
        rm -rf "$bdir" && mv "$bdir.tmp" "$bdir"
    fi
    # -O3: the sim runs in the browser, so speed over size. The feature flags are
    # the ones rustc's wasm32 target emits by default.
    $cap "$bdir/bin/wasm-opt" -O3 --enable-bulk-memory --enable-nontrapping-float-to-int \
        --enable-sign-ext --enable-mutable-globals --enable-reference-types --enable-multivalue \
        -o "$out.tmp/api/wasm.wasm" "$out.tmp/api/wasm.wasm"
fi
# Whatever touched it: wasm-bindgen's externref table must still be one.
python3 -I scripts/wasm_externref_check.py "$out.tmp/api/wasm.wasm"

cp crates/client/index.html "$out.tmp/index.html"
rev=$(git rev-parse --short HEAD)
git diff --quiet HEAD -- || rev="$rev-dirty-$(date -u +%Y%m%dT%H%M%SZ)"
printf '%s' "$rev" > "$out.tmp/api/version"
rsync -a --exclude webtransport-cert-hash.json assets/ "$out.tmp/assets/"

rm -rf "$out"
mv "$out.tmp" "$out"
du -sh "$out" "$out/api/wasm.wasm"
echo "web-release: $out ($rev)"
