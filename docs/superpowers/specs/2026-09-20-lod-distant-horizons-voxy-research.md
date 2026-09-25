# How Distant Horizons and Voxy do LOD — and what it means for ours

2026-09-20. Research only, no code changed. Follows
`2026-09-20-lod-seams-and-texture-scale-findings.md`. Goal stated by the user: far LOD must be
indistinguishable from real chunks at the distance it is shown.

Sources: both mods' source, cloned and read by two research agents (Distant Horizons core @
56be986, Voxy @ 534d58e). The load-bearing claims — Voxy's mip rule and subdivision threshold,
DH's detail formula, default quality and datapoint bit layout — were re-checked by hand in the
clones. Everything else is as reported from source by the agents. Units below: 1 block = 16 of
our voxels; our LOD "factor" is in voxels, so factor 16 = 1-block cells = Minecraft's level 0.

## The one-paragraph answer

The two mods reach "looks the same" by opposite means, and we currently do neither. **Voxy** uses
exactly our downsample (true-3D, any-solid, rounds surfaces up a cell) and makes it invisible by
only ever drawing a level while its voxels are **~1–2 px** on screen. **Distant Horizons** draws
much coarser columns (2–4.5 px wide) but **never coarsens height**: Y stays at 1-block precision
at every level, so silhouettes, sea level and terraces are exact and only X/Z detail is lost. We
coarsen all three axes like Voxy *and* draw cells at 3 / 6 / 13 px like nobody. That, not the
downsample rule, is the core defect.

## Side by side

| | Distant Horizons | Voxy | Ours today |
|---|---|---|---|
| Data | per-column RLE of vertical spans (id, top, bottom, light), 64×64 columns/section | 32³ voxel sections, levels 0–4 (1–16 blocks) | 32³ voxel grids, factors 2/4/16/64/256 voxels |
| Vertical precision | **exact (1 block) at every level** | halves per level | coarsened by the full factor |
| Downsample | union of the 4 children's Y transitions; per-interval majority id with **air excluded**; top = max of children | any-non-air; material = child with max `(opacity<<4 \| corner)`, top-ish corner preferred | any-solid; material = volume majority of solids, ties → **lowest BlockId** |
| Level selection | `level = log2(dist / 192 blocks)` (default MEDIUM): 1-block < 384, 2-block < 768, 4-block < 1536, 8-block < 3072 | GPU octree; subdivide a 32³ node when its projected area > 64×64 px² → voxels ≈ 1–2 px; L0→L1 ≈ 770 blocks, then doubling | fixed region rings; cell size ×4 per ×2 distance |
| px per cell where a level starts (our 60° FOV, ~1008 px tall) | ~4.5 px wide, **0 px tall error** | ~2 px | 1.1 / 1.4 / **3.0 / 6.4 / 13.2** |
| Texture / colour | flat linear-space average of the top texture × biome tint + faint world-locked noise; real textures only to level 2, repeating once per *block* | one texture per coarse voxel from a mipped, flood-filled per-face atlas; converges to average colour because voxels are ~1 px | one texture per coarse cell, nearest, **no mips** |
| Seams | side faces = own span minus neighbour's spans (always watertight); border faces kept as overdraw; no skirts | cull against same-level neighbour; superset property of any-solid; parent drawn until all 8 children uploaded; no skirts | 1-cell skirt on border side quads; roots retired once replacement is meshed |
| Real-chunk ↔ LOD join | Bayer-dither discard of LOD near the vanilla edge + fade pass | stencil/depth against vanilla, fog + fade in last 10 % | hard edge |
| Shading | per-face constants, stored light, **SSAO**, no baked AO | same: lightmap + face tint + SSAO | baked per-vertex AO at every factor (an AO gradient spans a whole coarse cell) |
| Far data | real worldgen off-thread, progressive; server plugin streams full data, client resolves colour | client-seen chunks only | server-built; real generation for ≤16, `coarse_tile` point samples for 64/256 |

## What this says about our three visible defects

1. **Walls at ring boundaries / blocky silhouettes.** A surface rounds up by up to one cell. At
   ≤2 px per cell (Voxy) that is sub-noticeable; with exact Y (DH) it does not exist. At our 6
   and 13 px it is a wall. Voxy has no fix for it either — its author just never draws a level
   that coarse that close (and Minecraft's sea happens to sit on a 16-aligned boundary).
2. **Material flips.** Both mods pick what is *visible from outside*: DH votes per Y interval so
   the top interval stays grass; Voxy prefers opaque and upper children. Ours takes a volume
   majority and breaks ties to the lowest BlockId — and dirt (1) < grass (3). Once cells span
   more than one block (factor ≥ 32, i.e. today's 64/256 via `downsample_chunks`), a grass block
   over a dirt block is a 4–4 tie that resolves to **dirt**, depending only on whether the
   surface block lands in the upper or lower half of the pair. Derived from `downsample2`'s rule,
   not yet measured on real terrain.
3. **Texture scale.** Both converge to the block's average colour at distance — DH by
   construction, Voxy through mips. Neither magnifies a texture across a coarse cell at a size
   you can see. World-scale UVs + a mip chain (previous findings doc) gives the same result.

## Recommendation

Three changes, in order of value per effort. They are independent; 1 alone removes most of what
the screenshot shows.

### 1. Re-cut the ring table so a cell is ≤ ~2 px (Voxy's lever)

With focal length 873 px, a cell of `f` voxels is 2 px at `27.3 × f` blocks. Inside today's
2080-block horizon that needs the missing power-of-two factors, and factor 256 never qualifies
(it would start at ~7000 blocks):

| factor | cell | ring (Chebyshev regions) | starts at | px/cell at start |
|---|---|---|---|---|
| full-res | 1/16 blk | ≤ 1 | — | — |
| 2 | 1/8 blk | ≤ 2 | 96 blk | 1.1 |
| 4 | 1/4 blk | ≤ 3 | 160 blk | 1.4 |
| **8** | 1/2 blk | ≤ 6 | 224 blk | 1.9 |
| 16 | 1 blk | ≤ 13 | 416 blk | 2.1 |
| **32** | 2 blk | ≤ 27 | 864 blk | 2.0 |
| 64 | 4 blk | ≤ 32 | 1760 blk | 2.0 |

Because cell size now tracks distance, every ring holds a similar number of grids: ≈ 12.4 k
grids per vertical layer against ≈ 8.8 k today — **about 1.4× the client cells** for ≤ 2 px
everywhere. Factor 8 is one more `downsample_cells` pass (per-region key, 4×4 grids); factor 32
is block-scale + one pass and is exactly one grid per region. The ring table should really be
derived from FOV and resolution (a named px-per-cell tunable) rather than hard-coded.

The real cost is on the server, not the client: factor 16 would cover 729 regions instead of 289
and factor 32 another ~2300, and factors ≤ 16 currently come from full `generate_region`. That
needs measuring before committing; the likely answer is extending `coarse_tile`-style direct
profile sampling down to factors 16/32 for pristine regions (which is also *more* accurate —
see 2), keeping full generation only for regions that have authoritative edits.

### 2. Stop coarsening Y as hard as X/Z (DH's lever)

DH's exact heights come from a column-span data model. We cannot adopt that wholesale — our
world is true 3D with player builds and sub-block chiselling — but the insight transfers as
**anisotropic cells**: coarsen X/Z by `f`, Y by `min(f, f_y_max)` with `f_y_max` = 16 voxels
(1 block) or less. The grid stays 32³, the mesher and records are untouched, and the chunk
transform becomes a non-uniform axis-aligned scale (face normals come from a table, so nothing
else changes; UV scaling needs the per-axis scale). `coarse_tile` already knows the exact
surface per column and only loses it when writing into the cubic grid, so pristine far terrain
becomes exact for free. Cost: a grid covers less height, so tall terrain needs more vertical
layers of grids (RLE-packed, surface-only in practice).

With lever 1 in place this matters less (a one-cell error is ≤ 2 px); it matters a lot if the
horizon is ever pushed out to where 64/256-voxel cells are needed, because it lets those rings
be DH-grade (wide but exact in height) instead of requiring Voxy-grade distances.

### 3. Make the coarse material the visible one

Replace "volume majority, ties → lowest id" with a surface-first rule: among the solid children,
prefer the upper layer, then opacity (opaque > cutout > fluid), then majority, with a
deterministic y-first tie-break. Both mods do a version of this. Any-solid occupancy should
stay — it is what keeps coarse levels a superset of fine ones and therefore crack-free without
skirts (both mods rely on that).

### Smaller items, all cheap

- Sea level on the coarsest lattice (option A of the previous doc) — still worth it, one line
  of worldgen semantics.
- World-scale UVs + mip chain (flood-fill transparent texels before mipping, as Voxy's
  `MipGen.solidify` does, or cutout textures darken).
- Drop or fade baked AO from factor 16 up; both mods use SSAO instead, and our AO gradient
  otherwise stretches across a whole coarse cell.
- Dither the near-window ↔ LOD edge (DH's 4×4 Bayer discard) once the above are in; today the
  hard edge is hidden only by the rings matching well at factors 2/4.

## What neither mod solves (so we should not expect to)

Thin features fatten under any-solid/max-of-children (trees, pillars, overhangs); DH caps spans
per column and loses overhangs entirely; Voxy has no LOD-to-LOD blending, so level swaps pop.
"Indistinguishable" in both is a statement about pixel error, not about the data being faithful.

## Implementation status (2026-09-20, same day)

Landed on `develop` (uncommitted at time of writing):

- **Ring table** `[(2,2),(3,4),(6,8),(13,16),(27,32),(32,64)]` with new factors 8 and 32;
  `LOD_MAX_CELL_PX` / `LOD_REFERENCE_FOCAL_PX` name the budget and
  `ring_table_keeps_cells_within_the_pixel_budget` holds the table to it (worst case, viewer on
  their region's edge: ≤ 2.43 px at 1080p). Factor 256 stays streamable but unused.
- **Server cost went down, not up.** The feared cost was measured: `generate_region` ≈ 100 ms,
  `coarse_tile` ≈ 2.5 ms at factor 16 and ≈ 0.6 ms at 32 (debug-opt test profile). Pristine
  regions at `COARSE_MIN_FACTOR` (16) and coarser now take the coarse + overlay path even when
  region-keyed, so full generation covers 13² = 169 regions (factors 2/4/8) instead of 17² = 289.
  Coarse factor 16 reproduces downsampled full generation in 4096/4096 columns (height and
  material); factor 32 in 98.7 %.
- **Visible-first material** (`game::lod::merge_group`): per x/z column the topmost solid voxel,
  then majority, ties to lowest id; occupancy still any-solid. `coarse_tile` uses the matching
  rule (material of the cell's topmost terrain block).
- **Sea on the lattice**: water fills through `SEA_TOP_BLOCK` (63), surface = `SEA_LEVEL` = 1024
  voxels; exact at every factor (`coarse_sea_surface_is_exact_at_every_factor`). World output
  changed: ocean and beach band one block lower.
- **Shading**: UVs in world voxels in all three terrain shaders, full mip chain on the block
  array (linear-light, alpha-weighted box filter; nearest mag, linear min/mip), baked AO faded out
  by factor 16.

Deferred, deliberately:

- **Anisotropic Y.** With every ring at ≤ ~2.4 px the residual height round-up is already at the
  pixel budget; capping Y would halve it in the factor-32/64 rings only. It touches every
  downsampler's group math, the client transform/Aabb/skirt bias and the brick raymarcher's
  scalar-cell DDA. Worth doing when the horizon is pushed past 32 regions, not before.
- **Dithered near-window ↔ LOD edge.** The first ring is 1.1–1.8 px per cell against full-res
  chunks; revisit if the hard edge is ever visible in play.
- Client cost: ~1.4× the LOD grids per vertical layer and ~3,300 keys (was ~570). The client was
  already CPU-bound on `ChunkQuads` extraction with a full ring table — re-measure with
  `just perf-capture`.
