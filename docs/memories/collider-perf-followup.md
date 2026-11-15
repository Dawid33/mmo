---
name: collider-perf-followup
description: RESOLVED — region collider build was ~9-23s/region (O(N^2) snapshot + dense voxel shapes); fixed via unlogged genesis path + full-chunk cuboid collapse
metadata: 
  node_type: memory
  type: project
  originSessionId: 43b7af88-e3ea-4b59-9ea7-eeb3e4aec252
---

**RESOLVED** on branch `region-collider-perf` (from `develop` 8ba8790, 2026-07-07).
Spec: `docs/superpowers/specs/2026-07-07-region-collider-genesis-design.md`;
plan: `docs/superpowers/plans/2026-07-07-region-collider-genesis.md`.

Real terrain materializes ~1088 dense 32³ sub-chunks/region. Profiling
(`crates/worldgen/examples/collider_profile.rs`, `just collider-bench`) found the
~9-15s build was TWO stacked costs, neither worldgen (~51 ms):
1. **O(N²) whole-`PhysicsState` clone**: `attach_voxels_collider_safe`'s
   `snapshot_raw()` cloned the entire growing physics state per sub-chunk.
2. Dense per-voxel parry `Voxels` BVH builds; **960/1088 chunks are fully solid**.

**Fix (both in `crates/game`):**
- `Rollback::seed_terrain_genesis` (`state.rs`) — genesis bulk build that inserts
  bodies/colliders via the rollback macro's raw accessors
  (`self.data.physics.raw_mut().{bodies,colliders}.raw_mut()`), **unlogged**
  (genesis is the base state, never rolled back below itself). Turns O(N²)→O(N).
  `create_mesh`/`attach_voxels_collider_safe` stay for undo-safe runtime edits.
- **Full chunk → single cuboid**: when `solid.len() == ChunkShape::SIZE`, build
  `ColliderBuilder::cuboid(16,16,16)` at body-local (16,16,16) instead of
  `Voxels`. Collision-IDENTICAL (a solid 32³ Voxels culls all interior voxels →
  its surface is exactly the outer cuboid), so zero fidelity change. Partial
  surface chunks keep parry `Voxels`.
- `Region::from_chunks` routes to `seed_terrain_genesis`.

**Result:** one region `from_columns` build ~9-15s → **~1.06s** (release), 19× under
the 20s `REGION_STARTUP_BUDGET`. Independent builds hash-identical (determinism
preserved). Both `threaded_world.rs` tests re-enabled and passing.

**Deferred lever (documented, NOT done):** greedy 3D box-merge of PARTIAL chunks
was measured fastest overall (~0.15s, ~1.2 boxes/chunk) but trades parry's
within-chunk internal-edge handling for merged-box seams on walkable surfaces —
revisit only if per-TICK physics cost (not startup) becomes the bottleneck.

**How to apply:** collider startup is no longer a gate to playing on generated
terrain. See [[tectonic-worldgen-research]] for the worldgen side. A live GPU
playtest for collision *feel* on plateaus/cliffs was NOT done here.
