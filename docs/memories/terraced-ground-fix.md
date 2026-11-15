---
name: terraced-ground-fix
description: FIXED 2026-09-22 (uncommitted) — "generation is not smooth" terraces on gentle slopes since the first worldgen: surface block was picked by block CENTER, dropping the block whenever the surface lay in its lower half; now picked by lowest voxel center (crates/worldgen/src/lib.rs surface_from_profile); live look pending
metadata:
  type: project
---

User (2026-09-22, screenshot): flat shelves with 1-block dirt risers along contour lines on every
gentle slope; "this has always been the case since the first world gen" — NOT forged-specific.

**Cause (measured with a throwaway example):** `surface_from_profile` returned the highest block whose
CENTER had density > 0. When the continuous surface h lay in the lower half of a block, that block became
air instead of a chiseled straddle with its bottom voxels solid → ground = floor(h) for frac(h) < 0.5,
= h otherwise: half of every meter of rise was a flat shelf, then a 0.5 m jump, then a ramp. Mean
|h − ground| was 0.135 m. **Fix:** test density at the block's lowest voxel center (`block_bottom_voxel_m`)
so the straddle is the block the surface passes through; mean error now 0.009 m, no jumps. Same function
feeds `coarse_tile`, so far LOD moved with it (coarse ⊇ fine kept); 37 worldgen lib tests incl. coarse-vs-full,
forged_world (6), player_walk, smoke all pass. Worldgen disk cache self-invalidates (source hash).
**Not done:** live look; the residual walking "1-voxel terrace speed dip" ([[kcc-voxel-seam-jitter]]) is the
separate 1/16 m stair issue, unchanged.

**Follow-up same day (user screenshot: brown dirt wedges along contours):** with the straddle fix, a
column whose surface lies in the bottom 1/32 m of a block gets NO chisel voxels, so the dirt block below
showed its top; dirt sides also showed along every 1/16 m step. Fix: soil depth is now measured from the
CONTINUOUS surface height (`prof.surface_y()`), `depth = floor(h − block_top)`, so the block under the
straddle is grass (depth 0) and dirt starts a whole meter down; `lo -= MAX_SOIL_DEPTH + 1`. Verified: every
top-exposed voxel in regions (0,0) and (3,−2) is grass. Suites + smoke pass. Still uncommitted; live look pending.
