#!/bin/bash

cargo build -p client --profile wasm-release --target wasm32-unknown-unknown --color=always 2>&1 | less -R +F
