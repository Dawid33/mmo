---
name: intel-gpu-shadow-cull
description: 2026-11-05 Intel iGPU 60 fps work — diag misses shadow cascades; GPU cull now face-culls ortho views + honours the cut; `just gpu-measure`
metadata:
  type: project
---
2026-11-05 (uncommitted at the time): Iris Xe default commander view went 26–31 ms → 16.3 ms (60 fps vsync).
`diag` called it CPU-bound but the GPU was 85% busy: Bevy 0.19's shadow pass has no GPU timestamp; the 4 sun
cascades were 17.2 of 22.5 ms GPU. Fix in `voxel_pull_cull.wgsl` + `gpu.rs` (camera.w = 2 directional face cull,
`cut` vec4 → `cut_state`); verified pixel-identical by screenshot diff. Measure with `just gpu-measure` (Mesa
INTEL_MEASURE). Remaining GPU levers if needed: cascade count / SHADOW_MAX_DISTANCE_M in atmosphere.rs.
Related: [[live-test-intel-adapter]], [[render-perf-investigation-0922]].
