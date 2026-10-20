#!/bin/sh

cargo build --workspace --bins --color=always 2>&1 | less -R +F
# cargo build -p game --color=always 2>&1 | less -R +F
# cargo build -p approx --color=always 2>&1 | less -R +F
