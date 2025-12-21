#!/bin/sh

cargo build --workspace --bins -p server --color=always 2>&1 | less -R +F
# cargo build -p rollback --color=always 2>&1 | less -R +F
# cargo build -p approx --color=always 2>&1 | less -R +F
