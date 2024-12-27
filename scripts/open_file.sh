#!/bin/bash

kitty @ --to unix:/tmp/pls-kitty focus-tab -m title:code
kitty @ --to unix:/tmp/pls-kitty send-text -m title:pls-helix "\E:open $1\rg${2}g"
