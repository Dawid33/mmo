#!/bin/sh

# CARGO_PROFILE_DEV_CODEGEN_BACKEND=cranelift cargo +nightly build -Zcodegen-backend --workspace --bins -p server --color=always 2>&1 | less -R +F
~/Software/rustc_codegen_cranelift/dist/cargo-clif build --workspace --bins -p server --color=always 2>&1 | less -R +F
# cargo build -p rollback --color=always 2>&1 | less -R +F
# cargo build -p approx --color=always 2>&1 | less -R +F
