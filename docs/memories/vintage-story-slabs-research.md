---
name: vintage-story-slabs-research
description: How Vintage Story implements slabs + chiseling (from its C# source) and what it implies for our slab/ChiselData design
metadata:
  type: project
---
RESEARCH DONE 2026-09-22 → docs/superpowers/specs/2026-09-22-vintage-story-slabs-and-chiseling.md (from vssurvivalmod/vsapi source).
VS: slab = shaped JSON block (6 rots, no merging, floating-on-top accepted, shape-accurate raycast w/ HitPosition, 0.6 step with smooth rise ~3 blocks/s); chisel CONVERTS a block to a microblock seeded from its collision box; microblock = greedy uint-cuboid list on 16³ + palette, rebuilt from dense grid each edit; collision = 1 box/cuboid; worldgen = whole cubes + snow layers only.
User concern (2026-09-22): does NOT want slabs as a special case — must be voxel-editable. Open decision: cuboid-list ChiselData (slab = 1 cuboid chisel, BlockShape removed) vs VS-style convert-on-carve.
**Why:** decides whether BlockShape (just landed, see [[block-half-block-worldgen]]) stays.
**How to apply:** don't re-research VS; keep KCC collision on parry Voxels grids (compound cuboids would reintroduce seam jitter, see [[kcc-voxel-seam-jitter]]).
