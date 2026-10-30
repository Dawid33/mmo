#!/bin/bash

# RUSTFLAGS="-Zproc-macro-backtrace" RUST_BACKTRACE=1 cargo run --color=always 2>&1 | less -R +F 
# RUST_BACKTRACE=1 cargo test --bin pls --color=always basic 2>&1 | less -R +F


# export PYROSCOPE=true
# Play builds use 512 m regions, as the justfile's play recipes (`play_blocks`);
# an exported VOXEL_REGION_BLOCKS wins.
export VOXEL_REGION_BLOCKS="${VOXEL_REGION_BLOCKS:-512}"
# Pin the client to the RX 6700 XT eGPU (unpinned, wgpu picks the ~30x slower
# NVK T600). An exported WGPU_ADAPTER_NAME or VOXEL_ADAPTER wins.
export WGPU_ADAPTER_NAME="${WGPU_ADAPTER_NAME:-${VOXEL_ADAPTER:-6700}}"
{
# Server first: it takes the build lock first and is listening by the time the
# client connects (the client retries its connect either way).
RUST_BACKTRACE=1 cargo run --bin server --color=always 2>&1 &
RUST_BACKTRACE=1 cargo run --bin client --color=always 2>&1
} |& less -R +F 

# RUST_BACKTRACE=1 cargo test -p game --color=always -- --no-capture 2>&1 | less -R +F
