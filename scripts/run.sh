#!/bin/bash

# RUSTFLAGS="-Zproc-macro-backtrace" RUST_BACKTRACE=1 cargo run --color=always 2>&1 | less -R +F 
# RUST_BACKTRACE=1 cargo test --bin pls --color=always basic 2>&1 | less -R +F


# export PYROSCOPE=true
{
RUST_BACKTRACE=1 ~/Software/rustc_codegen_cranelift/dist/cargo-clif run --bin client --color=always 2>&1 &
RUST_BACKTRACE=1 ~/Software/rustc_codegen_cranelift/dist/cargo-clif run --bin server --color=always 2>&1
} |& less -R +F 

# RUST_BACKTRACE=1 cargo test -p rollback --color=always -- --no-capture 2>&1 | less -R +F 
 
# RUST_BACKTRACE=1 CARGO_PROFILE_DEV_CODEGEN_BACKEND=cranelift cargo +nightly run -Zcodegen-backend --bin worldgen --color=always 2>&1 |& less -R +F 
 
# RUST_BACKTRACE=1 CARGO_PROFILE_DEV_CODEGEN_BACKEND=cranelift cargo +nightly run -Zcodegen-backend --bin client --color=always 2>&1 &
# RUST_BACKTRACE=1 CARGO_PROFILE_DEV_CODEGEN_BACKEND=cranelift cargo +nightly run -Zcodegen-backend --bin server --color=always 2>&1
 
# RUST_BACKTRACE=1 ~/Software/rustc_codegen_cranelift/dist/cargo-clif run --bin client --color=always 2>&1 &
# RUST_BACKTRACE=1 ~/Software/rustc_codegen_cranelift/dist/cargo-clif run --bin server --color=always 2>&1
