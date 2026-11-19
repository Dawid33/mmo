//! LOD cell data model + downsampling, shared by the server (builds streamed
//! LOD) and the client (renders it). Pure data code — no Bevy, no I/O.
//! **Layout contract:** LOD grids are always 32³ `Vec<Voxel>` in
//! `x + 32*y + 32*32*z` order.

use std::collections::BTreeMap;

use crate::{
    brick_coords, BlockId, BrickView, Chunk, ChunkCoords, RegionCoords, Voxel, BLOCK_VOXELS, BRICK_BLOCKS,
    BRICK_VOXELS, BRICK_VOXEL_COUNT, REGION_SIZE,
};

/// Merge up to 2×2×2 source 32³ grids (index `sx + 2*sy + 4*sz`; `None` = all-air) into one 32³ coarse grid at 2× the cell size.
/// Each coarse cell (x,y,z) draws from source grid `(x/16, y/16, z/16)`'s 2×2×2 voxel block at `((x%16)*2, (y%16)*2, (z%16)*2)`.
/// Solid iff any source voxel solid; material per [`merge_group`] (what is visible from above, not the volume majority).
///
/// **Layout contract:** LOD grids are always 32³ `Vec<Voxel>` in the standard `x + 32*y + 32*32*z` order.
pub fn downsample2(sources: &[Option<&[Voxel]>; 8]) -> Vec<Voxel> {
    let mut output = vec![Voxel::default(); BRICK_VOXEL_COUNT];

    for z in 0..32 {
        for y in 0..32 {
            for x in 0..32 {
                // Which source octant, and the 2×2×2 block's origin within it.
                let source_idx = x / 16 + 2 * (y / 16) + 4 * (z / 16);
                let Some(source_grid) = sources[source_idx] else {
                    continue;
                };
                let (ox, oy, oz) = ((x % 16) * 2, (y % 16) * 2, (z % 16) * 2); // 0..=30

                let mut group = [BlockId::AIR; 8];
                for dz in 0..2 {
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let idx = (ox + dx) + 32 * (oy + dy) + 32 * 32 * (oz + dz);
                            group[dx + 2 * dy + 4 * dz] = source_grid[idx].block;
                        }
                    }
                }
                output[x + 32 * y + 32 * 32 * z] = merge_group(&group);
            }
        }
    }

    output
}

/// A block as LOD terrain sees it: liquid is air. Water is not terrain in
/// the LOD grids — it rides beside them as the key's [`LodWater`], taken
/// from the recipe, so the grids show the seabed and lake beds.
#[inline]
pub fn terrain_id(id: BlockId) -> BlockId {
    if id != BlockId::AIR && crate::block_props().is_liquid(id) { BlockId::AIR } else { id }
}

/// The coarse cell for one 2×2×2 source group (index `dx + 2*dy + 4*dz`,
/// `AIR` = empty; liquid counts as `AIR`, [`terrain_id`]). Occupancy is any-solid — that keeps every coarse level a
/// superset of the finer one, so rings of different factors never crack
/// against each other. The MATERIAL is what a viewer sees from above: each of
/// the four x/z columns contributes its topmost solid voxel, the most frequent
/// of those wins, ties go to the lowest `BlockId`. A volume majority instead
/// lets the dirt under a grass surface outvote it as soon as a cell spans more
/// than one block, so distant land changed colour with the ring factor.
pub fn merge_group(group: &[BlockId; 8]) -> Voxel {
    let first = group[0];
    if group.iter().all(|&id| id == first) {
        return Voxel::new(terrain_id(first)); // uniform (incl. all-air): the common case
    }
    let group = group.map(terrain_id);
    let mut tops = [BlockId::AIR; 4];
    let mut n = 0;
    for dz in 0..2 {
        for dx in 0..2 {
            let upper = group[dx + 2 + 4 * dz];
            let top = if upper != BlockId::AIR { upper } else { group[dx + 4 * dz] };
            if top != BlockId::AIR {
                tops[n] = top;
                n += 1;
            }
        }
    }
    let mut winner: Option<(BlockId, usize)> = None;
    for &id in &tops[..n] {
        let count = tops[..n].iter().filter(|&&other| other == id).count();
        if winner.is_none_or(|(best, best_count)| {
            count > best_count || (count == best_count && id < best)
        }) {
            winner = Some((id, count));
        }
    }
    winner.map_or_else(Voxel::default, |(id, _)| Voxel::new(id))
}

/// Block-scale (factor-16) downsample: each source brick contributes its
/// `BRICK_BLOCKS`³ blocks directly as coarse cells. Chiseled blocks count as
/// their block id — chisel detail is invisible at 16×.
/// Output: fills only the `0..2 * BRICK_BLOCKS` corner of the 32³ grid
/// (8 source bricks × `BRICK_BLOCKS`³ blocks each).
///
/// NOTE: production factor-16 goes through `block_scale_cells` (which packs
/// a whole region per grid); this is the test-covered reference
/// implementation of the per-group block-read rule.
///
/// **Layout contract:** LOD grids are always 32³ `Vec<Voxel>` in the standard `x + 32*y + 32*32*z` order.
pub fn downsample_blocks(bricks: &[Option<BrickView<'_>>; 8]) -> Vec<Voxel> {
    let mut output = vec![Voxel::default(); BRICK_VOXEL_COUNT];
    for (brick_idx, brick) in bricks.iter().enumerate() {
        let Some(brick) = brick else { continue };
        // The octant this brick fills.
        let s = [brick_idx % 2, (brick_idx / 2) % 2, brick_idx / 4];
        for bz in 0..BRICK_BLOCKS {
            for by in 0..BRICK_BLOCKS {
                for bx in 0..BRICK_BLOCKS {
                    let id = terrain_id(brick.block_at(bx, by, bz).id);
                    if id != BlockId::AIR {
                        let [x, y, z] = [s[0] * BRICK_BLOCKS + bx, s[1] * BRICK_BLOCKS + by, s[2] * BRICK_BLOCKS + bz];
                        output[x + BRICK_VOXELS * y + BRICK_VOXELS * BRICK_VOXELS * z] = Voxel::new(id);
                    }
                }
            }
        }
    }
    output
}

/// One LOD cell: a downsampled 32³ grid plus the key-local group coordinate
/// it belongs to (at factor 2, chunk coords `div_euclid`'d by 2 on every
/// axis, y included — see `build_far_lod_cells`).
pub struct LodCell {
    pub group: [i32; 3],
    pub grid: Vec<Voxel>,
}

/// Group a region's `(ChunkCoords, Chunk)` list, read as 32³ bricks
/// ([`Chunk::bricks`]), into 2×2×2-brick clusters and downsample each cluster
/// into one `LodCell`, keyed by world brick coords `div_euclid` 2. `coords.x`/
/// `coords.z` are region-local `0..REGION_CHUNKS` (unsigned) but `coords.y` is
/// signed (chunks stack both above and below y=0 as the world grows) — every
/// axis uses `div_euclid`/`rem_euclid` uniformly so the grouping and octant
/// math stay correct regardless of sign, matching `downsample2`'s
/// `sx + 2*sy + 4*sz` octant convention (more negative y lands in the lower
/// octant half, same as positive y — `div_euclid`/`rem_euclid` round toward
/// negative infinity, not toward zero, so this holds across the y=0 boundary).
pub fn build_far_lod_cells(chunks: Vec<(ChunkCoords, Chunk)>) -> Vec<LodCell> {
    let mut groups: BTreeMap<[i32; 3], Vec<Voxel>> = BTreeMap::new();
    for (coords, chunk) in chunks {
        for (b, brick) in chunk.bricks() {
            let w = brick_coords(coords, b);
            let group = w.map(|v| v.div_euclid(2) as i32);
            let octant = w.map(|v| v.rem_euclid(2) as usize);
            let grid = groups
                .entry(group)
                .or_insert_with(|| vec![Voxel::default(); BRICK_VOXEL_COUNT]);
            write_brick_at_half_scale(grid, octant, &brick);
        }
    }
    groups
        .into_iter()
        .map(|(group, grid)| LodCell { group, grid })
        .collect()
}

/// Write `brick`, downsampled 2x, into its octant of a coarse 32^3 grid —
/// exactly what `downsample2` produces from `brick.voxels()`, without ever
/// materializing the dense 32^3 source. That expansion was ~70% of a
/// factor-2/4 server LOD build (690 of ~950 ms per region, 2026-09-19
/// profile), and LOD delivery is what paces the horizon filling after a join.
///
/// It works per block: the 2x2x2 source groups sit on even coordinates, so
/// none straddles a 16-voxel block boundary. An un-chiseled block is uniform,
/// hence so is its 8^3 image (one fill); only chiseled blocks need
/// [`merge_group`], fed from their decoded materials. A uniform brick (e.g.
/// every brick of a `Uniform` chunk) fills its whole octant without visiting
/// blocks. The whole octant is overwritten, air included, so a later brick at
/// the same coordinates replaces an earlier one just as the old octant table did.
fn write_brick_at_half_scale(grid: &mut [Voxel], octant: [usize; 3], brick: &BrickView<'_>) {
    const HALF_GRID: usize = BRICK_VOXELS / 2; // an octant's edge in coarse cells (16)
    let half = BLOCK_VOXELS / 2; // coarse cells per block axis (8)
    let row_at = |x: usize, y: usize, z: usize| x + y * BRICK_VOXELS + z * BRICK_VOXELS * BRICK_VOXELS;
    let origin = octant.map(|o| o * HALF_GRID);
    if let Some(block) = brick.uniform() {
        for z in 0..HALF_GRID {
            for y in 0..HALF_GRID {
                let row = row_at(origin[0], origin[1] + y, origin[2] + z);
                grid[row..row + HALF_GRID].fill(Voxel::new(terrain_id(block.id)));
            }
        }
        return;
    }
    for bz in 0..BRICK_BLOCKS {
        for by in 0..BRICK_BLOCKS {
            for bx in 0..BRICK_BLOCKS {
                let id = terrain_id(brick.block_at(bx, by, bz).id);
                let base = [origin[0] + bx * half, origin[1] + by * half, origin[2] + bz * half];
                // Decoded once per chiseled block: grid order, x fastest.
                let chisel = brick.chisel(bx, by, bz).map(crate::ChiselData::materials);
                for z in 0..half {
                    for y in 0..half {
                        let row = row_at(base[0], base[1] + y, base[2] + z);
                        match &chisel {
                            None => grid[row..row + half].fill(Voxel::new(id)),
                            Some(materials) => {
                                for x in 0..half {
                                    let mut group = [BlockId::AIR; 8];
                                    for dz in 0..2 {
                                        for dy in 0..2 {
                                            for dx in 0..2 {
                                                let (vx, vy, vz) = (2 * x + dx, 2 * y + dy, 2 * z + dz);
                                                group[dx + 2 * dy + 4 * dz] = materials
                                                    [vx + vy * BLOCK_VOXELS + vz * BLOCK_VOXELS * BLOCK_VOXELS];
                                            }
                                        }
                                    }
                                    grid[row + x] = merge_group(&group);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Second `downsample2` pass: merge factor-2 cells (group key = chunk coords
/// `div_euclid` 2) into factor-4 cells (group key = chunk coords `div_euclid`
/// 4). Same grouping/octant math as `build_far_lod_cells`, one level up: the
/// input group keys play the role the chunk coords played there.
pub fn downsample_cells(cells: Vec<LodCell>) -> Vec<LodCell> {
    let mut groups: BTreeMap<[i32; 3], [Option<Vec<Voxel>>; 8]> = BTreeMap::new();
    for LodCell { group, grid } in cells {
        let parent = [group[0].div_euclid(2), group[1].div_euclid(2), group[2].div_euclid(2)];
        let octant = (group[0].rem_euclid(2)
            + 2 * group[1].rem_euclid(2)
            + 4 * group[2].rem_euclid(2)) as usize;
        let entry = groups.entry(parent).or_insert_with(|| std::array::from_fn(|_| None));
        entry[octant] = Some(grid);
    }
    groups
        .into_iter()
        .map(|(group, grids)| {
            let sources: [Option<&[Voxel]>; 8] = std::array::from_fn(|i| grids[i].as_deref());
            LodCell { group, grid: downsample2(&sources) }
        })
        .collect()
}

/// Factor-16 (block scale) packing (liquid counts as air, [`terrain_id`]): ONE 32³ grid per 16×16-brick x/z tile and
/// 16-brick vertical band, group key = world brick coords `div_euclid` 16 —
/// instead of one `downsample_blocks` grid per 2×2×2-brick group. A block cell
/// is 16 world units, so a 16-brick tile fills a grid's 32×32-cell x/z span
/// exactly (the grid's full 512-unit span) and 32 cells of height cover one
/// 16-brick band (512 units). Mapping: world brick `w`, brick-local block
/// `(bx, by, bz)` → cell `(w.rem_euclid(16) * BRICK_BLOCKS + bx, ...)`. With
/// `REGION_BLOCKS = 64` a region spans 2×2 tiles per band; a region smaller
/// than one tile leaves the grid's excess x/z volume as air (the
/// corner-packing trade the plan documents for `downsample_blocks`) —
/// over-draw from the shared LOD `Aabb`, never under-draw. Chiseled blocks
/// count as their block id — chisel detail is invisible at 16×, matching
/// `downsample_blocks`. An all-air uniform brick writes nothing; any other
/// uniform brick fills its cells without visiting blocks.
pub fn block_scale_cells(chunks: Vec<(ChunkCoords, Chunk)>) -> Vec<LodCell> {
    const BRICKS_PER_GRID: i64 = (BRICK_VOXELS / BRICK_BLOCKS) as i64; // 16
    let mut bands: BTreeMap<[i32; 3], Vec<Voxel>> = BTreeMap::new();
    for (coords, chunk) in chunks {
        for (b, brick) in chunk.bricks() {
            let w = brick_coords(coords, b);
            let group = w.map(|v| v.div_euclid(BRICKS_PER_GRID) as i32);
            let band =
                bands.entry(group).or_insert_with(|| vec![Voxel::default(); BRICK_VOXEL_COUNT]);
            let l = w.map(|v| v.rem_euclid(BRICKS_PER_GRID) as usize * BRICK_BLOCKS);
            let uniform = brick.uniform().map(|u| terrain_id(u.id));
            if uniform == Some(BlockId::AIR) {
                continue;
            }
            for bz in 0..BRICK_BLOCKS {
                for by in 0..BRICK_BLOCKS {
                    for bx in 0..BRICK_BLOCKS {
                        let id = uniform.unwrap_or_else(|| terrain_id(brick.block_at(bx, by, bz).id));
                        if id != BlockId::AIR {
                            let idx = (l[0] + bx)
                                + BRICK_VOXELS * (l[1] + by)
                                + BRICK_VOXELS * BRICK_VOXELS * (l[2] + bz);
                            band[idx] = Voxel::new(id);
                        }
                    }
                }
            }
        }
    }
    bands.into_iter().map(|(group, grid)| LodCell { group, grid }).collect()
}

/// Every streamable LOD factor. The server validates request factors
/// against this list; `downsample_chunks` supports exactly these. Each is
/// twice the last from 2 to 512 (128 and 512 added 2026-11-05 for the 15 km
/// horizon), so [`ring_table_for`] can double the cell per doubling of
/// distance. Every one divides the sea surface (`worldgen`'s
/// `sea_surface_sits_on_every_lod_lattice`).
pub const LOD_FACTORS: [u32; 9] = [2, 4, 8, 16, 32, 64, 128, 256, 512];

/// Pristine (never-edited) terrain at this factor and coarser is sampled
/// straight from the worldgen profile (`worldgen::coarse_tile`) instead of
/// downsampled from a fully generated region: a cell is a whole block or
/// more, so the voxel-resolution surface chiselling full generation pays for
/// is invisible, and the wide outer rings hold thousands of regions.
pub const COARSE_MIN_FACTOR: u32 = 16;

/// On-screen size a LOD cell may reach where its ring begins, in pixels, at
/// the reference view below. Far terrain only reads as the real thing while
/// its cells are about this small — the same budget Voxy's screen-space
/// subdivision enforces; coarser and every rounded-up surface is a visible
/// step. `ring_table_keeps_cells_within_the_pixel_budget` holds the table to it.
pub const LOD_MAX_CELL_PX: f32 = 2.5;
/// Reference view for [`LOD_MAX_CELL_PX`]: focal length in pixels of a 1080p
/// frame at the camera's 60° vertical FOV (`540 / tan 30°`).
pub const LOD_REFERENCE_FOCAL_PX: f32 = 935.0;

/// The finest factor a ring table uses. At the play sizes (256 m regions and
/// up) that is block scale ([`COARSE_MIN_FACTOR`]): the client already draws
/// its own near regions at block scale beyond ~96 m (coarse twins,
/// `NEAR_DETAIL_DISTANCE_M`), so a finer first ring showed the regions past
/// them in MORE detail than the terrain in front — 10.2M of the 26.6M far
/// quads at 256 m regions, a third of the frame on an integrated GPU
/// (2026-11-14 capture) — and made the server generate every one of those
/// regions in full to downsample it. The pixel budget therefore does not bind
/// the first ring there (`ring_table_keeps_cells_within_the_pixel_budget`).
/// The 1 km test sizes keep their sub-block rings.
pub const LOD_MIN_RING_FACTOR: u32 = if crate::REGION_BLOCKS >= 256 { COARSE_MIN_FACTOR } else { LOD_FACTORS[0] };

/// Ring table: `(chebyshev_bound, lod_factor)` with ascending bounds. A
/// region at Chebyshev distance `d` from the viewer region renders at the
/// factor of the first entry with `d <= bound`; None past the last bound.
///
/// The factor doubles when the distance doubles, so every ring starts at
/// roughly [`LOD_MAX_CELL_PX`] and holds a similar number of grids. (The
/// previous table quadrupled the factor per doubling and showed 3 / 6 / 13 px
/// cells in its outer rings.)
///
/// Bounds are in REGIONS, so the table belongs to a region size: the pixel
/// budget is about world distance. Each doubling of `REGION_BLOCKS` starts
/// every ring twice as far out, so the same bounds carry twice the factor —
/// the near window already covers what the finest ring used to — and the
/// horizon halves in regions to keep the same [`LOD_HORIZON_UNITS`] (16
/// regions of 1024, 8 of 2048, 4 of 4096, 2 of 8192). `ring_table_keeps_cells_within_the_pixel_budget`
/// checks whichever table the build ships, `ring_table_ends_at_the_horizon`
/// that it stops at the horizon.
///
/// The play sizes (256 / 512) reach 3 km (3,072 m; 624 keys at 256 m
/// regions) since 2026-11-14. They reached 15 km from 2026-11-05 (~4,560
/// keys, factors up to 512): drawing it was nearly free past 3 km, but
/// streaming it took the server's LOD workers over a minute after every join
/// and the 3 km inside it were already a third of an integrated GPU's frame.
/// Their first ring is block scale ([`LOD_MIN_RING_FACTOR`]). The client's
/// haze follows the reach (`atmosphere::VISIBILITY_SHARE_OF_REACH`), and
/// `VOXEL_VIEW_DISTANCE_M` still builds any longer table at run time (up to
/// 16 km). 64 / 128 — tests and `render-bench` — keep 1 km so their key
/// counts and baselines do not move. Every
/// table equals [`ring_table_for`] at its horizon except 64's `(13, 16)` (the
/// rule gives 12), which predates the rule and is pinned by fixtures.
/// The client's `VOXEL_VIEW_DISTANCE_M` builds another one at run time.
pub const LOD_RING_TABLE: &[(i32, u32)] = match crate::REGION_BLOCKS {
    64 => &[(2, 2), (3, 4), (6, 8), (13, 16), (16, 32)],
    128 => &[(2, 4), (3, 8), (6, 16), (8, 32)],
    256 => &[(3, 16), (6, 32), (12, 64)],
    // Factor 32 a region out would be 3.7 px per cell, hence factor 16 to 2.
    512 => &[(2, 16), (3, 32), (6, 64)],
    _ => panic!("no LOD_RING_TABLE for this REGION_BLOCKS"),
};

/// How far the default LOD rings reach, in world units: the last ring's
/// Chebyshev bound × `REGION_SIZE` ([`ring_table_reach_units`]). It is also
/// the NEAREST the horizon edge can be (the viewer may stand on their
/// region's edge), which is where the client's fog goes opaque by default.
/// 1 km (16 384 units) at 64 / 128 m regions, 3,072 m at the play sizes.
pub const LOD_HORIZON_UNITS: f32 = if crate::REGION_BLOCKS >= 256 { 49_152.0 } else { 16_384.0 };

/// Reach of a ring table in world units: its last bound × `REGION_SIZE`
/// (0 for an empty table).
pub fn ring_table_reach_units(table: &[(i32, u32)]) -> f32 {
    table.last().map_or(0.0, |&(bound, _)| bound as f32 * REGION_SIZE)
}

/// The ring table that reaches `horizon_regions` regions while keeping every
/// ring within [`LOD_MAX_CELL_PX`] where it starts, as coarse as that allows:
/// each ring takes the coarsest [`LOD_FACTORS`] entry whose cell is within
/// budget at the ring's start (worst case: the viewer on their region's edge,
/// the first ring starting one region out, as
/// `ring_table_keeps_cells_within_the_pixel_budget` assumes) and lasts until
/// the next coarser factor fits. Factors only grow outward and never go
/// below [`LOD_MIN_RING_FACTOR`] (the first ring may be over budget for
/// that). Empty for `horizon_regions < 1`.
pub fn ring_table_for(horizon_regions: i32) -> Vec<(i32, u32)> {
    // Nearest start distance (regions) at which `factor` is within budget.
    let fits_from = |factor: u32| -> i32 {
        let units = factor as f32 * LOD_REFERENCE_FOCAL_PX / LOD_MAX_CELL_PX;
        (units / REGION_SIZE).ceil() as i32
    };
    let mut table = Vec::new();
    let mut prev = 0;
    let mut i = LOD_FACTORS.iter().position(|&f| f == LOD_MIN_RING_FACTOR).expect("the floor is a LOD factor");
    while prev < horizon_regions {
        let start = prev.max(1);
        while i + 1 < LOD_FACTORS.len() && fits_from(LOD_FACTORS[i + 1]) <= start {
            i += 1;
        }
        let next = LOD_FACTORS.get(i + 1).map_or(horizon_regions, |&f| fits_from(f));
        let bound = next.min(horizon_regions).max(prev + 1);
        table.push((bound, LOD_FACTORS[i]));
        prev = bound;
    }
    table
}

// The `LodKey` tiling contract (`tile_regions`, `cells_per_region`,
// `copy_region_cells_into_tile`): every factor's 32-cell grid (32 * factor
// units) is either a whole number of regions (tile-keyed) or divides a region
// exactly (region-keyed, several grids per key). With `LOD_FACTORS` all
// powers of two that is "REGION_SIZE is a power of two"; `>= 32` keeps the
// factor-2 grid (64 units) inside a chunk pair the way the mesher expects.
const _: () = assert!(crate::REGION_BLOCKS.is_power_of_two() && crate::REGION_BLOCKS >= 64);

/// Hysteresis band for `ring_factor_in` (see that function).
pub const LOD_STICKY_BAND: i32 = 1;

/// Tile edge length in regions for a factor's `LodKey`: factors whose 32³
/// grid fits inside one region key per-region (the integer division
/// truncates to 0 and `max(1)` says "one region, several grids"); coarser
/// ones key per-tile so one key is exactly one grid footprint
/// (32 * factor == span * REGION_SIZE). At the default size that is 64/256
/// per-tile; at `REGION_BLOCKS = 256` factor 64 is per-region too.
pub fn tile_regions(factor: u32) -> i32 {
    ((32 * factor) as i32 / REGION_SIZE as i32).max(1)
}

/// Names one streamable LOD unit: a region, or a tile of
/// `tile_regions(factor)`² regions once a factor's 32-cell grid spans more
/// than one region (from 64 at 64 m regions, 256 at 256 m). `coords` are region coords
/// divided (euclid) by the tile span, so they're plain region coords for
/// the small factors.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, state_hash::StateHash,
    serde::Serialize, serde::Deserialize,
)]
pub struct LodKey {
    pub factor: u32,
    pub coords: [i32; 2],
}

impl LodKey {
    pub fn for_region(factor: u32, rc: RegionCoords) -> Self {
        let span = tile_regions(factor);
        Self { factor, coords: [rc.x.div_euclid(span), rc.z.div_euclid(span)] }
    }

    /// Tile edge in regions (1 for factors <= 16).
    pub fn region_span(&self) -> i32 {
        tile_regions(self.factor)
    }

    /// The minimum-corner region this key covers.
    pub fn origin_region(&self) -> RegionCoords {
        let span = self.region_span();
        RegionCoords::new(self.coords[0] * span, self.coords[1] * span)
    }

    /// Every region this key covers (span² of them).
    pub fn regions(&self) -> Vec<RegionCoords> {
        let o = self.origin_region();
        let span = self.region_span();
        let mut out = Vec::with_capacity((span * span) as usize);
        for dx in 0..span {
            for dz in 0..span {
                out.push(RegionCoords::new(o.x + dx, o.z + dz));
            }
        }
        out
    }

    /// World-space origin of this key's render root.
    pub fn world_offset(&self) -> [f32; 3] {
        self.origin_region().world_offset()
    }
}

/// Run-length encoding of one 32³ LOD grid in the standard layout order.
/// Terrain grids are dominated by air/uniform-rock runs: ~64 KB raw
/// typically packs to ~1 KB. Runs are capped at u16::MAX (< 32768 total
/// cells, so any grid fits in <= BRICK_VOXEL_COUNT runs).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PackedLodGrid(pub Vec<(u16, BlockId)>);

impl PackedLodGrid {
    pub fn encode(grid: &[Voxel]) -> Self {
        debug_assert_eq!(grid.len(), BRICK_VOXEL_COUNT);
        let mut runs: Vec<(u16, BlockId)> = Vec::new();
        for v in grid {
            match runs.last_mut() {
                Some((count, block)) if *block == v.block && *count < u16::MAX => *count += 1,
                _ => runs.push((1, v.block)),
            }
        }
        Self(runs)
    }

    /// Whether the runs add up to exactly one 32^3 grid — i.e. whether
    /// [`decode`](Self::decode) will succeed. O(runs), no allocation: lets the
    /// client validate untrusted wire data on its main thread and leave the
    /// 64 KiB expansion to the mesh task.
    pub fn is_well_formed(&self) -> bool {
        self.0.iter().map(|&(n, _)| n as usize).sum::<usize>() == BRICK_VOXEL_COUNT
    }

    /// None when the run total is not exactly one grid — wire data is
    /// untrusted, so corrupt payloads drop instead of panicking.
    pub fn decode(&self) -> Option<Vec<Voxel>> {
        if !self.is_well_formed() {
            return None;
        }
        let mut grid = Vec::with_capacity(BRICK_VOXEL_COUNT);
        for &(n, block) in &self.0 {
            for _ in 0..n {
                grid.push(Voxel::new(block));
            }
        }
        Some(grid)
    }
}

/// Deepest water depth, in blocks, an [`LodWater`] column ships. The client
/// draws water with alpha `1 − exp(−0.35 × depth_m)` capped at its
/// `WATER_MAX_ALPHA` (0.92), which saturates at ≈ 7.2 m of depth: deeper
/// adds nothing visible and would only break the open sea's RLE runs.
pub const LOD_WATER_MAX_DEPTH_BLOCKS: u8 = 8;

/// A key's water surface layer, streamed beside its terrain grids (water
/// plan Task 8). LOD grids carry no liquid ([`terrain_id`]); this says, per
/// LOD column, the topmost water block (`crate::NO_WATER` when the column
/// is dry or its top is ice) and the water's depth in blocks over the
/// ground (clamped to [`LOD_WATER_MAX_DEPTH_BLOCKS`]). It comes from the recipe on every build path,
/// never from chunks, so a terrain rebuild resends the same water and water
/// never enters the terrain fingerprint.
///
/// Layout: an RLE of `(length, water_top, depth)` runs, row major (x
/// fastest, then z) over the key's `side × side` LOD columns, where
/// `side = region_span() × cells_per_region(factor)` ([`LodWater::side_for`]).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct LodWater {
    pub side: u16,
    pub runs: Vec<(u16, i16, u8)>,
}

impl LodWater {
    /// LOD columns per side of `key`'s water layer.
    pub fn side_for(key: &LodKey) -> u16 {
        let side = key.region_span() as usize * cells_per_region(key.factor);
        u16::try_from(side).expect("a LOD key's columns per side fit a u16")
    }

    /// Encode `tops` (`side²` `(water_top, depth)` entries, row major).
    pub fn encode(tops: &[(i16, u8)], side: u16) -> Self {
        debug_assert_eq!(tops.len(), usize::from(side) * usize::from(side));
        let mut runs: Vec<(u16, i16, u8)> = Vec::new();
        for &(top, depth) in tops {
            match runs.last_mut() {
                Some((n, t, d)) if *t == top && *d == depth && *n < u16::MAX => *n += 1,
                _ => runs.push((1, top, depth)),
            }
        }
        Self { side, runs }
    }

    /// A dry layer for `key`: every column `NO_WATER` (builders and tests
    /// with no water recipe).
    pub fn dry(key: &LodKey) -> Self {
        let side = Self::side_for(key);
        Self::encode(&vec![(crate::NO_WATER, 0); usize::from(side) * usize::from(side)], side)
    }

    /// Whether the runs cover exactly `side²` columns with no empty run,
    /// i.e. whether [`decode`](Self::decode) succeeds. O(runs), no
    /// allocation: wire data is untrusted.
    pub fn is_well_formed(&self) -> bool {
        let side = usize::from(self.side);
        self.runs.iter().all(|&(n, _, _)| n > 0)
            && self.runs.iter().map(|&(n, _, _)| usize::from(n)).sum::<usize>() == side * side
    }

    /// Whether any column holds water.
    pub fn has_water(&self) -> bool {
        self.runs.iter().any(|&(n, top, _)| n > 0 && top != crate::NO_WATER)
    }

    /// The `side²` `(water_top, depth)` entries, row major; `None` when
    /// malformed.
    pub fn decode(&self) -> Option<Vec<(i16, u8)>> {
        if !self.is_well_formed() {
            return None;
        }
        let mut tops = Vec::with_capacity(usize::from(self.side) * usize::from(self.side));
        for &(n, top, depth) in &self.runs {
            tops.extend(std::iter::repeat_n((top, depth), usize::from(n)));
        }
        Some(tops)
    }
}

/// Pure ring assignment: the factor a region at Chebyshev distance `dist`
/// should render at, given the factor it currently renders at (if any).
///
/// Fresh assignment picks the first table entry whose bound covers `dist`
/// (`None` past the last bound). A current assignment is sticky: its ring's
/// distance interval, widened by `band` on both edges, keeps claiming the
/// region — both promote (moved inward) and demote (moved outward) need the
/// distance to clear the boundary by more than `band`. The innermost ring has
/// no inner edge: the near/far exclusion sets bound it, not distance.
pub fn ring_factor_in(
    table: &[(i32, u32)],
    band: i32,
    dist: i32,
    current: Option<u32>,
) -> Option<u32> {
    if let Some(cur) = current {
        if let Some(i) = table.iter().position(|&(_, f)| f == cur) {
            let held_from = if i == 0 { i32::MIN } else { table[i - 1].0 + 1 - band };
            if dist >= held_from && dist <= table[i].0 + band {
                return Some(cur);
            }
        }
    }
    table.iter().find(|&&(bound, _)| dist <= bound).map(|&(_, f)| f)
}

/// x/z cells one region occupies in a grid at `factor` (16 at 64, 4 at 256;
/// 32 or more for the small factors, where a region spans whole grids).
pub fn cells_per_region(factor: u32) -> usize {
    (REGION_SIZE as u32 / factor) as usize
}

/// Downsample one region's chunks (region-local x/z chunk coords, absolute
/// signed y) to `factor` LOD cells with key-local group coords. Factors
/// follow the composition: 2 = voxel 2x2x2 merge; 4/8 = one/two more merge
/// passes; 16 = block scale; 32/64/256 = block scale + 1/2/4 more merge passes
/// (each pass halves the group lattice, so groups stay consistent throughout).
pub fn downsample_chunks(chunks: Vec<(ChunkCoords, Chunk)>, factor: u32) -> Vec<LodCell> {
    match factor {
        2 | 4 | 8 => sub_block_cells_bucketed(chunks, factor),
        16 => block_scale_cells(chunks),
        32 => downsample_cells(block_scale_cells(chunks)),
        64 => downsample_cells(downsample_cells(block_scale_cells(chunks))),
        128 => downsample_cells(downsample_cells(downsample_cells(block_scale_cells(chunks)))),
        256 => downsample_cells(downsample_cells(downsample_cells(downsample_cells(
            block_scale_cells(chunks),
        )))),
        512 => downsample_cells(downsample_cells(downsample_cells(downsample_cells(
            downsample_cells(block_scale_cells(chunks)),
        )))),
        _ => panic!("unsupported LOD factor {factor} (must be one of {LOD_FACTORS:?})"),
    }
}

/// The sub-block factors (2/4/8) of [`downsample_chunks`]: factor 2 cells
/// ([`build_far_lod_cells`]) merged `log2(factor / 2)` times by
/// [`downsample_cells`], run over buckets of chunks instead of the whole
/// region at once. The unbucketed pipeline holds one dense 32³ grid per
/// 2×2×2-brick group of the WHOLE region before the first merge — ~4 GiB for
/// a 256 m region at 16-block chunks, and the LOD pool runs several such builds
/// at once (2026-11-02: the 256 m play default OOM-killed the server at
/// 20 GB). A factor-`f` cell's group is its bricks' coords `div_euclid(f)`, and
/// chunks sit on a `BRICKS_PER_CHUNK_EDGE` lattice, so bucketing chunks by
/// their origin brick `div_euclid(max(f, BRICKS_PER_CHUNK_EDGE))` never splits
/// a group: every output cell comes out of exactly one bucket, identical to the
/// unbucketed one. Sorting by group restores the unbucketed (`BTreeMap`) order.
/// Peak memory is one bucket's factor-2 grids (one chunk's at the default
/// chunk size: 64 grids).
fn sub_block_cells_bucketed(chunks: Vec<(ChunkCoords, Chunk)>, factor: u32) -> Vec<LodCell> {
    debug_assert!(matches!(factor, 2 | 4 | 8));
    let unit = (factor as i64).max(crate::BRICKS_PER_CHUNK_EDGE as i64);
    let mut buckets: BTreeMap<[i64; 3], Vec<(ChunkCoords, Chunk)>> = BTreeMap::new();
    for (coords, chunk) in chunks {
        let key = crate::brick_coords(coords, [0, 0, 0]).map(|v| v.div_euclid(unit));
        buckets.entry(key).or_default().push((coords, chunk));
    }
    let mut out = Vec::new();
    for (_, bucket) in buckets {
        let mut cells = build_far_lod_cells(bucket);
        let mut f = 2;
        while f < factor {
            cells = downsample_cells(cells);
            f *= 2;
        }
        out.extend(cells);
    }
    out.sort_by_key(|cell| cell.group);
    out
}

/// Overlay one region's authoritative cells into a tile's grids: clear the
/// region's x/z cell rect in EVERY band (so terrain the players removed
/// doesn't survive as a coarse-sampled ghost), then write the region's
/// cells. `region_cells` must come from `downsample_chunks(..., key.factor)`
/// — content in the low-corner rect, absolute band indices in `group[1]`.
pub fn copy_region_cells_into_tile(
    tile: &mut BTreeMap<[i32; 3], Vec<Voxel>>,
    region_cells: &[LodCell],
    region: RegionCoords,
    key: &LodKey,
) {
    let per = cells_per_region(key.factor);
    let origin = key.origin_region();
    let x_off = ((region.x - origin.x) as usize) * per;
    let z_off = ((region.z - origin.z) as usize) * per;
    debug_assert!(x_off + per <= 32 && z_off + per <= 32, "region outside tile");

    for grid in tile.values_mut() {
        for lz in 0..per {
            for ly in 0..32 {
                for lx in 0..per {
                    grid[(x_off + lx) + 32 * ly + 32 * 32 * (z_off + lz)] = Voxel::default();
                }
            }
        }
    }
    for cell in region_cells {
        debug_assert_eq!((cell.group[0], cell.group[2]), (0, 0), "region cells are low-corner");
        let grid = tile
            .entry([0, cell.group[1], 0])
            .or_insert_with(|| vec![Voxel::default(); BRICK_VOXEL_COUNT]);
        for lz in 0..per {
            for ly in 0..32 {
                for lx in 0..per {
                    grid[(x_off + lx) + 32 * ly + 32 * 32 * (z_off + lz)] =
                        cell.grid[lx + 32 * ly + 32 * 32 * lz];
                }
            }
        }
    }
}

