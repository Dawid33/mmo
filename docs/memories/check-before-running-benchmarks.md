---
name: check-before-running-benchmarks
description: "The user runs their own benchmarks on this box — ask before running a timing benchmark/capture so they don't collide"
metadata:
  node_type: memory
  type: feedback
  originSessionId: 3cfb14fc-0941-435a-95e1-acad3e91fdb4
  modified: 2026-09-22T18:24:22.083Z
---

On 2026-09-22 the user rejected my benchmark run and paused the task because "there are other benchmarks running".

**Why:** concurrent CPU-heavy runs corrupt both sets of timings, and the user often has their own measurements going on the shared machine.

**How to apply:** before running any timing-sensitive benchmark, perf capture or long release run, ask the user (or check `ps` for running benches) instead of launching it directly. Building is less sensitive but still competes for cores. Related: [[laptop-power-clamp]], [[cap-build-test-parallelism]].
