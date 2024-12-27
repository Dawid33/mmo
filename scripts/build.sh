#!/bin/sh

cargo build --workspace -p client --bins -p server --color=always 2>&1 | less -R +F
