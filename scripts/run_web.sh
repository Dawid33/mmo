#!/bin/bash

cargo build -p client --profile wasm-release --target wasm32-unknown-unknown --color=always
wasm-bindgen --out-name client --out-dir public --target web target/wasm32-unknown-unknown/wasm-release/client.wasm

basic-http-server public
