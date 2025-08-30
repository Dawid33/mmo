#!/bin/sh

# RUSTFLAGS="-Zproc-macro-backtrace" RUST_BACKTRACE=1 cargo run --color=always 2>&1 | less -R +F 
# RUST_BACKTRACE=1 cargo test --bin pls --color=always basic 2>&1 | less -R +F
 
{
RUST_BACKTRACE=1 cargo run --bin client --color=always 2>&1 &
RUST_BACKTRACE=1 cargo run --bin server --color=always 2>&1
} | less -R +F 

# RUST_BACKTRACE=1 cargo test -p rollback --color=always -- --no-capture 2>&1 | less -R +F 
