#!/usr/bin/env bash
# Seeds the fuzz targets' corpora under quest/fuzz/corpus/<target>/ from quest/corpus/ (git-ignored; re-run
# after the corpus or the bytecode changes): the corpus's sources for `compile`, its people and script files
# for `stub_inputs`, and — through quest-corpus's `examples/dump_seeds.rs` — each run case's program bytes
# (`program_bytes`), a saved state (`vm_load`) and wire values (`wire_decode`). Files already there (what
# libFuzzer found) are kept; a seed of the same name is overwritten. `invariants` and `conversation` get a few
# byte strings written here: 40 bytes each (seed, fuel, latency, rule, k, j, world case, flags) for
# `invariants`; for `conversation` a case byte, then talk, tick, pick the offer, checkpoint, pick, rollback,
# tick, edit, pick, leave, done (the ops of its module docs).
set -euo pipefail
quest="$(cd "$(dirname "$0")/.." && pwd)"
out="$quest/fuzz/corpus"
mkdir -p "$out/compile" "$out/stub_inputs" "$out/invariants" "$out/conversation"
bytes() { local b; for b in "$@"; do printf "\\x$(printf %02x "$b")"; done; }
for n in $(seq 0 23); do
    seed=()
    for i in $(seq 0 31); do seed+=($(( (n * 37 + i * 11) % 256 ))); done
    bytes "${seed[@]}" $(( n * 53 % 256 )) $(( n % 20 )) $(( n % 9 )) $(( n % 15 )) $(( n * 3 )) $(( n * 7 % 40 )) \
        "$n" $(( n % 2 )) > "$out/invariants/seed-$n"
done
for n in $(seq 0 23); do
    bytes "$n" 0 $(( n / 3 )) $(( n )) 255 4 1 0 1 0 5 1 0 1 1 6 4 7 $(( n % 3 )) 4 1 0 1 2 2 0 3 0 4 4 \
        > "$out/conversation/seed-$n"
done
# Every source of a case, its sub-directories' too (`mod` files, an `edit` target), named by its path.
(cd "$quest/corpus" && find run fail -name '*.qs') | while read -r f; do
    cp "$quest/corpus/$f" "$out/compile/${f//\//-}"
done
for f in "$quest"/corpus/run/*/{people,script}.txt; do
    [ -f "$f" ] || continue
    cp "$f" "$out/stub_inputs/$(basename "$(dirname "$f")")-$(basename "$f")"
done
cargo run -q --manifest-path "$quest/Cargo.toml" -p quest-corpus --example dump_seeds -- "$out"
