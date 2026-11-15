---
name: lod-dh-voxy-research
description: IMPLEMENTED 2026-09-20 (uncommitted on develop) — LOD re-cut after Distant Horizons/Voxy research; ≤2.5 px cells, factors 8+32, coarse sampling ≥16, visible-first material, sea on lattice, world UVs + mips; anisotropic Y + edge dither deferred
metadata:
  type: project
---

IMPLEMENTED 2026-09-20 (user confirmed "visually looked good" in a live run; uncommitted on develop at the time). Don't re-research or re-plan. Report + implementation status: docs/superpowers/specs/2026-09-20-lod-distant-horizons-voxy-research.md. User's bar: far LOD must be indistinguishable from real chunks at the distance shown ("the whole point of LOD").

- Voxy = our exact downsample (true-3D any-solid, rounds up) but only draws a level while voxels are ~1–2 px (subdivide 32³ node when projected area > 64² px; L0→L1 ≈ 770 blocks). DH = coarse columns (2–4.5 px wide) but Y is NEVER coarsened (per-column RLE spans, 12-bit exact heights; level = log2(dist/192 blocks)).
- Ours coarsens all 3 axes AND draws cells at 1.1/1.4/3.0/6.4/13.2 px (factor ×4 per distance ×2) — the core defect, not the downsample rule.
- Recommended: (1) ring table [(2,2),(3,4),(6,8),(13,16),(27,32),(32,64)] → ≤2.1 px/cell everywhere for ~1.4× client grids (12.4k vs 8.8k per layer); needs new factors 8 and 32; real cost is server worldgen (729 regions at full generation vs 289) → extend coarse_tile-style sampling to 16/32, MUST MEASURE. (2) anisotropic cells: coarsen Y by min(f, 16 voxels) — DH's insight without its column data model. (3) material = visible-first (upper layer, opacity) not volume-majority-ties-to-lowest-id (dirt 1 beats grass 3 on 4–4 ties at factor ≥32; derived, not measured). Plus: mips + world-scale UVs, no baked AO from factor 16, dithered near↔LOD edge.

**Why:** the user rejected "just fix texture scale / sea level" as missing the point; the LOD design itself has to change.
**How to apply:** start any LOD-quality work from the report; see [[lod-sea-walls-and-texture-scale]] for the symptom diagnosis and [[streamed-lod-rings-design]] for the current ring/streaming implementation.

What landed: LOD_RING_TABLE [(2,2),(3,4),(6,8),(13,16),(27,32),(32,64)] + LOD_MAX_CELL_PX test; COARSE_MIN_FACTOR=16 routes pristine 16/32 keys through coarse_tile (MEASURED: generate_region ~100 ms vs coarse 2.5 ms @16 / 0.6 ms @32; factor 16 coarse == downsampled full-gen in 4096/4096 columns) so full generation covers 169 regions, not 289 — server cost went DOWN; merge_group visible-first material; SEA_TOP_BLOCK=63 (ocean + beach band one block lower, world output changed); world-voxel UVs + AO fade in all 3 terrain shaders; mip chain in block_textures::mip_chain. Live run (NVK T600, debug): 3376 LOD keys received by 15.9 s, world_ready 41.7 s, no shader errors.
Deferred on purpose: anisotropic Y (only pays past a 32-region horizon; touches brick raymarcher DDA), dithered near↔LOD edge. Unmeasured: client frame cost at ~1.4x LOD grids / 3376 roots — re-run `just perf-capture` on the 6700.

**2026-09-22 horizon halved** (user: "reduce lod distance and make fog appear further away"): new `game::lod::LOD_HORIZON_UNITS` = 16 384 (1 km, was 2 km); tables drop the outermost ring (32: …(16,32); 64: …(8,32); 128: …(4,32)), test `ring_table_ends_at_the_horizon`. Fog in client atmosphere.rs is now LINEAR: clear to `FOG_START_FRACTION` (0.6) × horizon, opaque at the horizon (was exponential 2 km visibility, hazed from the camera). Live look not done.
