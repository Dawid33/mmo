> **Superseded by [2026-11-06-terrain-slopes-design.md](2026-11-06-terrain-slopes-design.md)** — worldgen now emits whole blocks + slopes; half blocks are gone.

# Worldgen: whole blocks and half blocks only (investigation)

2026-09-22. Question: can worldgen emit only whole blocks (1 m) or half blocks
(0.5 m bottom slabs), never individual 1/16 m voxels?

## Where voxels come from today

Exactly one place: `place_straddle` in `crates/worldgen/src/lib.rs`. Every block
below the surface is already whole. The one block per column that the surface
passes through (the "straddle") gets a dense 16³ `ChiselData`, filled from the
interpolated density field at every voxel center (256 profile samples × 16
heights). So **every surface column in the world is a chiseled block**. That is
4096 per region at the default size. Nothing else in worldgen makes sub-block
detail. The coarse LOD path (`coarse_tile`) already works in whole blocks.

## Measured (`examples/quantize_probe.rs`)

The probe re-quantizes each generated straddle by its mean fill height: round
to 0/8/16 voxel layers for half blocks, 0/16 for whole blocks. It then builds
the real sim `Region` from the result. The half-block variant stores its slabs
as `ChiselData` (the representation available today with no engine change).
Numbers are for 4 regions (spawn (0,0), coast (5,-3), inland (0,14), (0,16)),
REGION_CHUNKS = 32.

| per region                 | voxel (today) | half-block as ChiselData | whole-block |
|----------------------------|---------------|--------------------------|-------------|
| chiseled blocks            | 4096          | 1.9k–2.3k                | 0           |
| chisel data                | 18.1 MiB      | 8.5–10.4 MiB             | ~0          |
| voxel-scale collider cells | 8.8–9.4 M     | 3.9–4.8 M                | 0           |
| snapshot (bincode)         | 36–41 MiB     | 16–23 MiB                | 2.8–7.0 MiB |
| colliders in snapshot      | 16.6–18.8 MiB | 6.1–8.6 MiB              | 1.2–2.7 MiB |
| wire (lz4)                 | 1.7–3.6 MiB   | 0.5–1.3 MiB              | 0.3–1.0 MiB |
| `Region::from_chunks`      | 181–193 ms    | 68–89 ms                 | 4–13 ms     |

Walkability measures the rise between neighbouring block columns. `STEP_HEIGHT` is
0.6 m, so autostep covers 0.5 m, and a 1 m rise needs a jump:

| rises needing a jump | voxel (today) | half-block | whole-block |
|----------------------|---------------|------------|-------------|
| spawn (0,0)          | 0%            | 0%         | 3.8%        |
| inland (0,14)        | 0%            | 0%         | 5.2%        |
| (0,16)               | 0%            | 0%         | 13.6%       |
| coast (5,-3)         | 15.2%         | 16.1%      | 26.7%       |

(The coast's 15% at voxel resolution are real cliffs; half blocks add almost
none.) With half blocks, 7–27% of neighbour pairs become a 0.5 m step. The
remainder are flat.

## Reading

- **The chisel straddle is the dominant cost of a region.** It accounts for about half the
  snapshot (the chisel data) plus most of the rest (the 9 M voxel collider cells). Going
  whole-block removes both: the snapshot shrinks 6–13×, and building the sim region gets
  ~15–45× faster. This probably cuts `generate_region` time too, because `place_straddle` is
  ~1 M profile interpolations per region. That is an estimate; I did not time generation alone.
- **Half blocks stored as `ChiselData` only halve the cost.** A slab is still a dense
  4.6 KB bitset+palette and 2048 voxel collider cells. To get whole-block-like cost,
  half blocks need a native representation (next section).
- **Whole blocks break walking on slopes.** 4–27% of steps need a jump unless
  `STEP_HEIGHT` goes above 1 m or the game auto-jumps. Half blocks keep ground walkable
  everywhere the voxel world is.

## What each option takes

**A. Whole blocks only.** This is a worldgen-only change. Delete `place_straddle`. Pick
the surface block by rounding, i.e. test the density at the block *center*. That is the
rule the 2026-09-22 terraced-ground fix moved away from, but terraces are now the
intent. Fill a straddle below `water_top` with water instead of leaving it air.
Coarse LOD becomes exact at factor 16 and stays cheap. The cost is gameplay:
raise `STEP_HEIGHT`, or accept Minecraft-style jumping.

**B. Half blocks as ChiselData slabs.** Also worldgen-only (~30 lines). Sample the
profile once at the block-column center, and test density at `by + 0.25` (slab)
and `by + 0.75` (full). `surface_from_profile` must use the same lower
test point so coarse LOD agrees. You get about half of A's savings.

**C. Half blocks as a native block shape (recommended if half blocks are wanted).**
Put a shape in the reserved `Block.data` bytes, e.g. `data[0] = BOTTOM_SLAB`, and
store no chisel. Touch points, all Bevy-free and deterministic:
- `game::block::derive_voxels`: fill the lower 8 layers. The client mesher and
  bridge consume `derive_voxels`, so rendering needs no other change.
- `game::block::collider_cells`: slabs need a collider. The choices are a third
  collider scale (an 8-voxel "half-block" grid, 4 cells per slab), or 2048
  voxel-scale cells, which is no better than today. The multi-scale collider
  code already has the two-scale pattern, so a third grid is the clean fit.
- `game::lod::write_chunk_at_half_scale`: add a slab fast path (half-scale image
  = lower 4 of 8 layers filled).
- `game::edit`: breaking a slab clears it, and placing stays whole-block. Raycast
  already hits derived voxels.
- Worldgen as in B, writing the shape instead of a chisel.
Expected cost: close to A, since ~2k extra slab blocks/region are just `Block`s,
plus a few thousand half-scale collider cells.

## Hazards found

- **Underwater straddles are air today.** `place_straddle` only writes solid or
  air, so the top block of every sea/lake floor has an air pocket above its
  voxels, under water. A and C must decide what fills the empty part of a
  submerged straddle: water for A; for C, a "waterlogged" slab or a water block
  above it. B can use a two-entry palette (material + water).
- **Forged worlds.** River levees and lake berms are carved per point and are
  sometimes only a fraction of a meter above the water surface. Quantized to
  0.5 m or 1 m, they can round down and open a lake side. Re-run the forged containment
  checks (`forged.rs`, the "0.01% open sides" measurement) after the change.
  The carve may need to round levees *up*.
- **Look.** Gentle slopes become 0.5 m (or 1 m) terraces, the shelf look the
  terraced-ground fix removed. This time it is by design, but it is the same
  visual.
- **Movement feel.** Every 0.5 m terrace is an autostep, an instant 0.5 m camera
  pop, where today the KCC glides over sub-0.5 m rises. Minecraft slabs do the same.
  A camera-height smoothing pass may be wanted.
- **Chisel stays** for player edits (the `ChiselData` machinery is not removed,
  just no longer produced by worldgen).

## Implemented (option C), 2026-09-22

- `game::block`: `BlockShape { Full, BottomSlab }` in `Block.data[0]`,
  `Block::bottom_slab`, `Block::solid_layers`, `HALF_BLOCK_VOXELS = 8`.
  `derive_voxels` fills a slab's lower 8 layers. `ColliderCells` gained `halves`
  (half-block grid, 4 cells per slab), and `ColliderCells::grids()` is the single
  list that `create_mesh`, `seed_terrain_genesis` and `edit.rs`'s collider rebuild
  all walk (block, half, voxel scale, in that order).
- `game::lod::write_chunk_at_half_scale`: slab fast path. The bit-exact
  dense-reference test now mixes in random slabs. Block-scale LOD (factor 16+),
  the block raycast and editing treat a slab as a solid cell (any-solid).
- `worldgen`: `place_straddle` is gone. `surface_from_profile` tests density at
  `by + 0.25`; `surface_is_full` tests it at `by + 0.75`. A submerged surface block
  is always whole. Soil depth is an integer count from the surface block (the block
  under a slab is dirt, not grass).
- Measured on the real output: spawn snapshot 36.5 → 3.1 MiB, coast 41 → 6.5 MiB;
  `generate_region` ~75 → 2–6 ms; `Region::from_chunks` ~185 → 7–13 ms.
  No voxel-scale collider cells; 3–10k half-block cells per region.
- Verified: `game` block/lod lib tests, worldgen lib (incl. both slow
  coarse-vs-full LOD tests), `forged_world` (incl. slow containment),
  `player_walk`, `just test-threads`, `just smoke`.
- Not verified: live look/feel in the client; `edit.rs` has no unit tests for
  breaking a slab + undo.

## Revised: half blocks are ordinary chisel data (2026-09-22, same day)

**Why the change.** The user did not want slabs to be a special case that can't be edited
at the voxel level. Vintage Story research
(`2026-09-22-vintage-story-slabs-and-chiseling.md`) showed that VS stores chiseled
blocks compactly as a greedy cuboid list. So `BlockShape` was removed. `ChiselData`
now uses that representation:

- **Storage.** `ChiselData = { cuboids: Vec<u32>, palette: Vec<BlockId> }`, packed the
  VS way (4 bits each for min x/y/z and max−1 x/y/z, 8 bits of palette index).
  - The list is **canonical**: every construction and edit goes through the dense
    `ChiselGrid` and one fixed greedy merge (`from_grid`). Equal voxels therefore
    give equal bytes and equal hashes. This is pinned by `chisel_data_is_canonical`
    and `a_packed_cuboid_is_its_own_canonical_form`.
  - Undo replaces the whole chunk value, so no per-voxel inverse is needed.
  - API: `cuboid`, `from_fn`, `from_grid`, `grid`, `materials`, `cuboids`. The old
    `set` / `is_solid` / `material_at` still exist, but `set` re-merges the whole block
    on every call.
- **Colliders.** `collider_cells` takes each chisel at its coarsest aligned solid cell:
  - a chisel that fills its block → one block cell;
  - each solid 8³ octant → a half-block cell;
  - the rest → voxel cells.

  A half block therefore gets 4 half cells, the same as the slab version, with no
  special case. Collision stays on parry `Voxels` grids and is never one collider per
  cuboid, which protects the KCC seam fixes. A side effect: `flat_floor` partial layers
  and full chisels also collide coarser now. `walking_smoothness`'s `chiselled_floor`
  gained hidden voids to keep its surface at voxel scale.
- **Worldgen.** A half-block surface is `Block::new(mat)` plus
  `ChiselData::cuboid(mat, [0,0,0], [16,8,16])`. The half-block rule, the underwater
  rule and soil depth are unchanged. The LOD half-scale fast path decodes each
  chisel once.
- **Measured.** Spawn: snapshot 3.11 MiB, `generate_region` ~5 ms, `Region::from_chunks`
  ~18 ms, 2,377 half-block chisels = 0.07 MiB (~31 B each). Coast: 6.53 MiB and ~17 ms.
  Same size as the slab-shape version; region build ~5–10 ms slower (collider/derive
  decode).
- **Verified.**
  - `game`: lib (101 tests), `chunk_blocks`, `walking_smoothness`, `shape_cast_grazing`,
    `jump_arc`.
  - `worldgen`: lib incl. slow, `player_walk`, `forged_world` incl. slow.
  - `just smoke`, `just test-threads`.
- **Not done.** No live look. No VS-style editing UX yet (shape-accurate raycast with
  hit position, placement by face region, smooth step-up).
