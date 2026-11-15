---
name: egpu-adapter-selection
description: "MEASURED 4.2x: unpinned adapter = NVK T600 = 14.6 fps; WGPU_ADAPTER_NAME=6700 = 61 fps (vsync cap). Always pin, and always check AdapterInfo before trusting any perf number"
metadata: 
  node_type: memory
  type: user
  originSessionId: 53828178-a245-4802-99ff-8647b2a56282
---

Dawid's dev machine exposes four Vulkan devices: Intel Iris Xe (integrated),
NVIDIA T600 Laptop GPU (NVK/nouveau), an AMD Radeon RX 6700 XT (RADV) when the
eGPU is plugged in, and llvmpipe (software). Both the T600 and the eGPU are
discrete, so Bevy/wgpu's default high-performance preference picks the T600
(first enumerated), not the eGPU.

**Why it matters (measured 2026-07-11, release build, identical spawn scene,
44.6k entities):** the T600-on-NVK default renders the game at **14.6 fps**
(frame_time 68.6ms, 32ms of it GPU) — NVK is an immature open-source NVIDIA
driver and is drastically slower. Pinned to the 6700 XT the same scene runs at
**61.3 fps** (frame_time 16.7ms, 2.6ms GPU) — i.e. vsync-capped, GPU 12x
faster. A "the game is slow" report on this box is the adapter until proven
otherwise.

**How to apply:** launch with `WGPU_ADAPTER_NAME="6700"` (case-insensitive
substring; `just adapter=6700 <recipe>` threads it through). Safe to leave set
when the eGPU is unplugged — Bevy warns and falls back. Vulkan enumerates at
startup, so restart the client after connecting the eGPU. **Before trusting any
frame-rate measurement, read the `AdapterInfo` line in the log** and confirm
which GPU you actually got; `just diag` prints it first for this reason. See
[[client-perf-text-diagnostics]].

**2026-09-20 update:** a bare `just run` (debug, unpinned) measured GPU-bound at 4.7 fps on the NVK T600 — `main_opaque_pass_3d` 162 ms vs ~5 ms on the Intel iGPU for the same scene — while the debug build itself was fine (genesis ≤1.2 s/region, client ticks ~1 ms). The justfile `adapter` now defaults to `$VOXEL_ADAPTER`; suggest `export VOXEL_ADAPTER=Intel` (or `6700`) in the shell profile.

**Update 2026-09-22 (Bevy 0.19.1 / wgpu 29):** NVK advertises `EXPERIMENTAL_COOPERATIVE_MATRIX` but then fails `vkCreateDevice` when it's requested. Because Bevy requests every advertised feature, an unpinned launch panicked. `renderer::render_plugin()` now disables that feature, and the T600 works again. When the eGPU is unplugged, `WGPU_ADAPTER_NAME=6700` falls back silently (to NVK); use `iris` for the iGPU.
