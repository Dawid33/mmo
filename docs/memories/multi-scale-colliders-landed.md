---
name: multi-scale-colliders-landed
description: "Multi-scale chunk colliders merged to develop (abb988d, 2026-07-08); scale-seam handling deliberately deferred until observed"
metadata: 
  node_type: memory
  type: project
  originSessionId: 697b817e-f0a8-4505-a950-be7b640f2834
---

Chunk physics colliders are now two-scale (merged into develop at abb988d, 2026-07-08): `collider_cells` in crates/game/src/block.rs decomposes a chunk into whole solid blocks (one `Voxels` collider, voxel_size 16) + chisel detail (second `Voxels` collider, voxel_size 1) on the same fixed body; full-cube chunks still collapse to a cuboid. Plan doc: docs/superpowers/plans/2026-07-08-multi-scale-chunk-colliders.md (untracked). Don't re-plan.

Open decisions/known gaps:
- **Seams between the two collider scales are deliberately deferred** (user decision): parry's internal-edge classification stops at collider borders, so phantom-edge catching at block/chisel boundaries is possible. Deal with it only when observed in play. Live smoke run showed 0 mispredicts/206 reconciles.
- A chunk fully solid purely via fully-occupied chisel no longer collapses to a cuboid (collision-identical, documented in seed_terrain_genesis doc).
- `create_mesh` deliberately never cuboid-collapses (asymmetry vs genesis is hash-relevant; documented on create_mesh).
- Pre-existing gap noticed during this work: dead `HALF_VOXEL_SIZE` const warning in crates/game/src/voxel.rs:9. (`just test-full` originally missed `-p server`; fixed since — justfile now runs it.)

Related: [[collider-perf-followup]], [[first-tick-massprops-spike]]
