---
name: near-window-5x5
description: Production near window raised 3x3 -> 5x5 (DEFAULT_NEAR_RADIUS=2) 2026-09-22 without widening LOD; cull BFS cap + render-bench gate fixed
metadata:
  type: project
---
2026-09-22 (uncommitted): `DEFAULT_NEAR_RADIUS = 2` in crates/client/src/world_scale.rs → `full` = 5x5 simulated regions. LOD_RING_TABLE deliberately NOT widened (user: "don't expand lod to compensate"): desired_lod subtracts the near window, so the factor-2 ring is fully absorbed; factor 4 starts at d=3, horizon unchanged (1,080 → 1,064 keys).
Blockers fixed: CULL_BFS_MAX_CHEBYSHEV was 2*REGION_CHUNKS (outer ring of a 5x5 would stay Hidden) → (MAX_NEAR_RADIUS+1)*REGION_CHUNKS; render-bench gates_ready demanded every ring factor → now only rings with bound > near_radius.
Unmeasured: 25 regions = 2.8x server region threads/genesis + client predicted sims; cull flood covers ~2.8x cells; wasm FULL also 5x5. `VOXEL_NEAR_RADIUS=1` restores 3x3.
**How to apply:** perf numbers before/after this change aren't comparable at `full`; see [[small-world-knobs-investigation]].
