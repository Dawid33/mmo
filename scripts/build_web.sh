#!/bin/bash

# cargo build -p client --profile wasm-release --target wasm32-unknown-unknown --color=always
# wasm-bindgen --out-name client --out-dir public --target web target/wasm32-unknown-unknown/wasm-release/client.wasm
# wasm-opt -O -ol 100 -s 100 -o public/client_bg.wasm public/client_bg.wasm

cargo build -p client --target wasm32-unknown-unknown --color=always
wasm-bindgen --out-name client --out-dir public --target web target/wasm32-unknown-unknown/debug/client.wasm | less
