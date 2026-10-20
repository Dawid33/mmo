#!/bin/bash

# RUSTFLAGS="-Zproc-macro-backtrace" RUST_BACKTRACE=1 cargo run --color=always 2>&1 | less -R +F 
# RUST_BACKTRACE=1 cargo test --bin pls --color=always basic 2>&1 | less -R +F


# export PYROSCOPE=true
# Play builds use 256 m regions, as the justfile's play recipes (`play_chunks`);
# an exported VOXEL_REGION_CHUNKS wins.
export VOXEL_REGION_CHUNKS="${VOXEL_REGION_CHUNKS:-128}"
{
# Server first: it takes the build lock first and is listening by the time the
# client connects (the client retries its connect either way).
RUST_BACKTRACE=1 cargo run --bin server --color=always 2>&1 &
RUST_BACKTRACE=1 cargo run --bin client --color=always 2>&1
} |& less -R +F 

# RUST_BACKTRACE=1 cargo test -p game --color=always -- --no-capture 2>&1 | less -R +F
