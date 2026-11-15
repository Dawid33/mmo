---
name: near-cost-coarse-twins
description: "2026-11-05 — embarked 3×3 neighbourhood was the frame cost (22.8M quads, geometry-bound); coarse twins halved GPU; benchmark must be embarked + horizon camera"
metadata:
  node_type: memory
  type: project
  originSessionId: 146a984b-c25e-445b-bf57-02cc3ed645c3
  modified: 2026-11-05T16:04:14.019Z
---

Perf work 2026-11-05 (findings doc `docs/superpowers/specs/2026-11-05-view-distance-perf.md`):

- Benchmark the way the game is played: EMBARKED (`client debug embarked` with
  `VOXEL_DEBUG_WORLD=sweep64 VOXEL_DEBUG_EMBARK_AT=0,0`) so the 3×3 fortress neighbourhood is
  simulated, and a HORIZON camera (`VOXEL_CAMERA_PITCH_DEG=15 VOXEL_CAMERA_DISTANCE_M=150`).
  The user corrected both: a skip-menu client simulates one region, and the default pitch
  looks at the ground (far LOD off screen).
- **Why:** user called out that earlier sweeps were un-embarked and ground-facing.
- **How to apply:** any client frame/GPU measurement uses that setup; the user watches the
  benchmark windows (don't run two clients at once — it contaminated one GPU measure).
- Result: near terrain 22.8M quads → GPU 47.8 ms (geometry-bound); coarse twins
  (`meshing/coarse.rs`, 96 m switch) → 23.6 ms. At 15 km view the LOD rings' 21.3M quads are
  the next lever (main pass 22 ms). Embarked `world_ready` never fires (gate expects 80 keys).

Related: [[egpu-adapter-selection]], [[live-test-intel-adapter]], [[check-before-running-benchmarks]].
