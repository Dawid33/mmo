---
name: perf-at-256-regions
description: perf measurements must use 256-block regions (the play size); tests stay at 64
metadata:
  type: feedback
---
Perf runs/benchmarks/profiles must be built and run with `VOXEL_REGION_BLOCKS=256` (the play recipes' `play_blocks`), not the 64-block test default; correctness tests (`smoke`, `test-one`) stay at 64.

**Why:** the user plays at 256 m regions; 64 m numbers don't capture real-world performance (every per-region cost scales with area).

**How to apply:** build perf binaries via `just perf-capture` / `gpu-measure` / `trace-chrome` (they pass play_blocks) or export VOXEL_REGION_BLOCKS=256 for raw cargo builds; state the region size when reporting numbers. Measure WINDOWED (never fullscreen — user rule 2026-11-05); a window is capped ~60 fps by the Cinnamon compositor, so to get load above the cap build `region_blocks=512` (bigger region = more near terrain) instead of flying the camera, which outruns the LOD distance and renders nothing. Related: [[intel-gpu-shadow-cull]].
