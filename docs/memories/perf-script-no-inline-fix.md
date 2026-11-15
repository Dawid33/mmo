---
name: perf-script-no-inline-fix
description: "perf script livelocks with \"could not read first record\" on this project's Rust binaries; fix is --no-inline, NOT switching off Cranelift"
metadata: 
  node_type: memory
  type: project
  originSessionId: a83add69-0712-4735-913b-05ea2716f643
---

`perf script` (used by `just perf-client`/`perf-server` to make folded stacks)
livelocks emitting `addr2line ... could not read first record` repeatedly — blocked
in `do_poll`, ~0% CPU, never finishes. Root cause: perf's **inline-frame expansion**
spawns persistent `addr2line -a -i -f` coprocesses, and binutils addr2line (6.8-era)
can't drive that streaming mode on these Rust binaries.

**Fix: `perf script --no-inline`** — skips the addr2line coprocess. Then it runs in
<1s; function-level symbols still resolve from the ELF symtab (all folded stacks
need). Verified: 0.8s, 0 errors, real frames (`bevy_app::app::App::run`,
`parking::Inner::park`, `client`/`game`). Already wired into the perf-* recipes.

**Misdiagnosis to avoid:** this is INDEPENDENT of Cranelift vs stable cargo — the
error reproduces identically on both toolchains. A one-shot `addr2line -e BIN -f
0xADDR` test "works" and misleads you; the failure is only in perf's coprocess
(`-i -f` streaming) path. The stable-cargo default (see [[vertical-chunk-stacking-landed]]
sibling justfile changes) is the user's preference, not an addr2line fix.

**pkill footgun hit while debugging:** `pkill -f 'inferno-collapse-perf'` matched my
own shell (the string was in my command line) and killed it. Kill perf/inferno
pipelines by numeric PID, or `pkill -x <name>` — never `pkill -f` with a string that
appears in the running command.
