---
name: live-test-intel-adapter
description: "Live/functional test runs use the Intel iGPU (adapter=Intel), not the RX 6700 eGPU (often unplugged) nor the unpinned NVK T600"
metadata:
  node_type: memory
  type: feedback
  originSessionId: 931bfd5a-22b3-4f34-9237-f044048dd952
  modified: 2026-09-27T17:46:42.840Z
---

For live game test runs (functional checks: movement, rollback, mispredicts), launch with `just adapter=Intel ...`.

**Why:** 2026-09-27 H10 live run asked for `adapter=6700`, the eGPU was not attached, wgpu silently fell back to the NVK T600 (~15 fps in a debug build) and the user could not complete the test. User: "you should be using Intel adapter, that is sufficient to run tests".

**How to apply:** default to `adapter=Intel` for any run the user plays or I drive; check `lspci`/the `AdapterInfo` log line before trusting the run. Reserve the 6700 for perf measurements, and only when it is attached. Related: [[egpu-adapter-selection]], [[testing-one-client]].
