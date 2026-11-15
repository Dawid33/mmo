---
name: chunk-size-two-meters
description: Sim chunk is now 16³ blocks (16 m) — LANDED 2026-10-29 (develop 4ac725b..a3fc21e); 32³-voxel render brick; VOXEL_CHUNK_BLOCKS dial {2,16}; perf measurement (plan Task 12) pending Dawid's go-ahead
metadata:
  type: project
---
LANDED 2026-10-29 via plan docs/superpowers/plans/2026-10-29-chunk-16-blocks.md (handoff + §7 addendum in …-chunk-size-increase-handoff.md), commits 4ac725b..a3fc21e on develop.
- Sim `Chunk` = 16³ blocks (one entity/body/collider set); render/LOD unit = 32³-voxel brick (`BRICK_VOXELS`); client fans chunks out to brick entities (bricks.rs, SkippedBricks for buried).
- Dials in blocks: VOXEL_REGION_BLOCKS (test 64 / play 256), VOXEL_CELL_BLOCKS; VOXEL_CHUNK_BLOCKS build dial, only 2 or 16 legal (goldens pinned for both). Old VOXEL_REGION_CHUNKS/CELL_CHUNKS fail the build.
- Edits: SetBlocks per-block deltas + per-block undo; colliders patched via parry fork `Voxels::set_voxels`.
**Why:** 2 m chunks were a 52f37d3 accident (~50k entities per 256 m region); Dawid wanted ≥16 blocks.
**How to apply:** don't re-plan. OPEN (TODO.md): Task 12 measurement needs Dawid's go-ahead — client brick landing (whole chunk ≈200-300 bricks/frame vs cap 160), replay churn when a predicted break materialises ground, solid chunk's first-edit full collider rebuild. `just pull-smoke` red since 9aed311 (OverlayTexture; not this work). Full HASH_MODE ≈4.4 ms/edit at 16. Related: [[region-size-dial]], [[trees-plant-life-investigation]].
