#!/usr/bin/env bash
# Seeds the fuzz targets' corpora under quest/fuzz/corpus/<target>/ from quest/corpus/ (git-ignored; re-run
# after the corpus or the bytecode changes): the corpus's sources for `compile`, its people and script files
# for `stub_inputs`, and — through quest-corpus's `examples/dump_seeds.rs` — each run case's program bytes
# (`program_bytes`), a saved state (`vm_load`) and wire values (`wire_decode`). Files already there (what
# libFuzzer found) are kept; a seed of the same name is overwritten.
set -euo pipefail
quest="$(cd "$(dirname "$0")/.." && pwd)"
out="$quest/fuzz/corpus"
mkdir -p "$out/compile" "$out/stub_inputs"
# Every source of a case, its sub-directories' too (`mod` files, an `edit` target), named by its path.
(cd "$quest/corpus" && find run fail -name '*.qs') | while read -r f; do
    cp "$quest/corpus/$f" "$out/compile/${f//\//-}"
done
for f in "$quest"/corpus/run/*/{people,script}.txt; do
    [ -f "$f" ] || continue
    cp "$f" "$out/stub_inputs/$(basename "$(dirname "$f")")-$(basename "$f")"
done
cargo run -q --manifest-path "$quest/Cargo.toml" -p quest-corpus --example dump_seeds -- "$out"
