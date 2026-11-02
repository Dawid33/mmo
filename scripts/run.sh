#!/bin/bash

# RUSTFLAGS="-Zproc-macro-backtrace" RUST_BACKTRACE=1 cargo run --color=always 2>&1 | less -R +F 
# RUST_BACKTRACE=1 cargo test --bin pls --color=always basic 2>&1 | less -R +F


# export PYROSCOPE=true
# Play builds use 256 m regions, as the justfile's play recipes (`play_blocks`);
# an exported VOXEL_REGION_BLOCKS wins.
export VOXEL_REGION_BLOCKS="${VOXEL_REGION_BLOCKS:-256}"
# Pin the client to the RX 6700 XT eGPU (unpinned, wgpu picks the ~30x slower
# NVK T600). An exported WGPU_ADAPTER_NAME or VOXEL_ADAPTER wins.
export WGPU_ADAPTER_NAME="${WGPU_ADAPTER_NAME:-${VOXEL_ADAPTER:-6700}}"
# The `embarked` debug launch, like `just run`: the client spawns the world's
# server itself (both binaries are built first, so it finds the server next to
# it). Extra arguments are passed on, e.g. `scripts/run.sh --reset`.
{
cargo build --bin client --bin server --color=always 2>&1 \
  && RUST_BACKTRACE=1 ./target/debug/client debug embarked "$@" 2>&1
} |& less -R +F 

# RUST_BACKTRACE=1 cargo test -p game --color=always -- --no-capture 2>&1 | less -R +F
