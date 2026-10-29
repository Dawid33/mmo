//! LOD cell data model + downsampling, shared by the server (builds streamed
//! LOD) and the client (renders it). Pure data code — no Bevy, no I/O.
//! **Layout contract:** LOD grids are always 32³ `Vec<Voxel>` in
//! `x + 32*y + 32*32*z` order.

use std::collections::BTreeMap;

use crate::{BlockId, Chunk, ChunkCoords, RegionCoords, Voxel, BRICK_VOXEL_COUNT, REGION_SIZE};

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

/// The coarse cell for one 2×2×2 source group (index `dx + 2*dy + 4*dz`,
/// `AIR` = empty). Occupancy is any-solid — that keeps every coarse level a
/// superset of the finer one, so rings of different factors never crack
/// against each other. The MATERIAL is what a viewer sees from above: each of
/// the four x/z columns contributes its topmost solid voxel, the most frequent
/// of those wins, ties go to the lowest `BlockId`. A volume majority instead
/// lets the dirt under a grass surface outvote it as soon as a cell spans more
/// than one block, so distant land changed colour with the ring factor.
pub fn merge_group(group: &[BlockId; 8]) -> Voxel {
    let first = group[0];
    if group.iter().all(|&id| id == first) {
        return Voxel::new(first); // uniform (incl. all-air): the common case
    }
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

/// Block-scale (factor-16) downsample: each source chunk contributes its
/// 2×2×2 `blocks` directly as coarse cells. Chiseled blocks count as their
/// block id — chisel detail is invisible at 16×.
/// Output: fills only the 0..4 corner of the 32³ grid (4×4×4 cells = 8 source chunks × 8 blocks each).
///
/// NOTE: production factor-16 goes through `block_scale_cells` (which packs
/// a whole region per grid); this is the test-covered reference
/// implementation of the per-group block-read rule.
///
/// **Layout contract:** LOD grids are always 32³ `Vec<Voxel>` in the standard `x + 32*y + 32*32*z` order.
pub fn downsample_blocks(chunks: &[Option<&Chunk>; 8]) -> Vec<Voxel> {
    let mut output = vec![Voxel::default(); BRICK_VOXEL_COUNT];

    for (chunk_idx, chunk_opt) in chunks.iter().enumerate() {
        if let Some(chunk) = chunk_opt {
            // Determine which octant this chunk belongs to
            let sx = chunk_idx % 2;
            let sy = (chunk_idx / 2) % 2;
            let sz = (chunk_idx / 4) % 2;

            // For each of the 8 blocks in this chunk
            for bz in 0..2 {
                for by in 0..2 {
                    for bx in 0..2 {
                        let block = chunk.block_at(bx, by, bz);

                        if block.id != BlockId::AIR {
                            // Map to coarse cell position
                            let coarse_x = sx * 2 + bx;
                            let coarse_y = sy * 2 + by;
                            let coarse_z = sz * 2 + bz;

                            let idx = coarse_x + 32 * coarse_y + 32 * 32 * coarse_z;
                            output[idx] = Voxel::new(block.id);
                        }
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

/// Group a region's `(ChunkCoords, Chunk)` list into 2×2×2 clusters and
/// downsample each cluster into one `LodCell`. `coords.x`/`coords.z` are
/// region-local `0..REGION_CHUNKS` (unsigned) but `coords.y` is signed (chunks stack both
/// above and below y=0 as the world grows) — every axis uses
/// `div_euclid`/`rem_euclid` uniformly so the grouping and octant math stay
/// correct regardless of sign, matching `downsample2`'s `sx + 2*sy + 4*sz`
/// octant convention (more negative y lands in the lower octant half, same as
/// positive y — `div_euclid`/`rem_euclid` round toward negative infinity, not
/// toward zero, so this holds across the y=0 boundary).
pub fn build_far_lod_cells(chunks: Vec<(ChunkCoords, Chunk)>) -> Vec<LodCell> {
    let mut groups: BTreeMap<[i32; 3], Vec<Voxel>> = BTreeMap::new();
    for (coords, chunk) in chunks {
        let cx = coords.x as i32;
        let cz = coords.z as i32;
        let group = [cx.div_euclid(2), coords.y.div_euclid(2), cz.div_euclid(2)];
        let octant = [
            cx.rem_euclid(2) as usize,
            coords.y.rem_euclid(2) as usize,
            cz.rem_euclid(2) as usize,
        ];
        let grid = groups
            .entry(group)
            .or_insert_with(|| vec![Voxel::default(); BRICK_VOXEL_COUNT]);
        write_chunk_at_half_scale(grid, octant, &chunk);
    }
    groups
        .into_iter()
        .map(|(group, grid)| LodCell { group, grid })
        .collect()
}

/// Write `chunk`, downsampled 2x, into its octant of a coarse 32^3 grid —
/// exactly what `downsample2` produces from `derive_voxels(chunk)`, without
/// ever materializing the dense 32^3 source. That expansion was ~70% of a
/// factor-2/4 server LOD build (690 of ~950 ms per region, 2026-09-19
/// profile), and LOD delivery is what paces the horizon filling after a join.
///
/// It works per block: the 2x2x2 source groups sit on even coordinates, so
/// none straddles a 16-voxel block boundary. An un-chiseled block is uniform,
/// hence so is its 8^3 image (one fill); only chiseled blocks need
/// [`merge_group`], fed from their decoded materials. The whole octant
/// is overwritten, air included, so a later chunk at the same coordinates
/// replaces an earlier one just as the old octant table did.
fn write_chunk_at_half_scale(grid: &mut [Voxel], octant: [usize; 3], chunk: &Chunk) {
    use crate::BLOCK_VOXELS;
    let half = BLOCK_VOXELS / 2; // coarse cells per block axis (8)
    for (bi, block) in chunk.blocks.iter() {
        let (bx, by, bz) = bi.xyz();
        let base = [
            octant[0] * 16 + bx * half,
            octant[1] * 16 + by * half,
            octant[2] * 16 + bz * half,
        ];
        // Decoded once per chiseled block: grid order, x fastest.
        let chisel = chunk.chisel.get(&bi).map(crate::ChiselData::materials);
        for z in 0..half {
            for y in 0..half {
                let row = (base[1] + y) * 32 + (base[2] + z) * 32 * 32 + base[0];
                match &chisel {
                    None => grid[row..row + half].fill(Voxel::new(block.id)),
                    Some(materials) => {
                        for x in 0..half {
                            let mut group = [BlockId::AIR; 8];
                            for dz in 0..2 {
                                for dy in 0..2 {
                                    for dx in 0..2 {
                                        let (vx, vy, vz) = (2 * x + dx, 2 * y + dy, 2 * z + dz);
                                        group[dx + 2 * dy + 4 * dz] =
                                            materials[vx + vy * BLOCK_VOXELS + vz * BLOCK_VOXELS * BLOCK_VOXELS];
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

/// Factor-16 (block scale) packing: ONE 32³ grid per 16×16-chunk x/z tile and
/// 16-sub-chunk vertical band, group key `[cx.div_euclid(16),
/// sub_y.div_euclid(16), cz.div_euclid(16)]` — instead of one
/// `downsample_blocks` grid per 2×2×2-chunk group. A block cell is 16 world
/// units, so a 16-chunk tile fills a grid's 32×32-cell x/z span exactly (the
/// grid's full 512-unit span) and 32 cells of height cover one 16-sub-chunk
/// band (512 units). Mapping: chunk `(cx, sub_y, cz)`, block `(bx, by, bz)` →
/// cell `(cx.rem_euclid(16)*2 + bx, sub_y.rem_euclid(16)*2 + by,
/// cz.rem_euclid(16)*2 + bz)`. With `REGION_BLOCKS = 64` a region spans 2×2
/// tiles per band (4×4 at 64); a region smaller than one tile leaves the grid's excess
/// x/z volume as air (the corner-packing trade the plan documents for
/// `downsample_blocks`) — over-draw from the shared LOD `Aabb`, never
/// under-draw. Chiseled blocks count as their block id — chisel detail is
/// invisible at 16×, matching `downsample_blocks`.
pub fn block_scale_cells(chunks: Vec<(ChunkCoords, Chunk)>) -> Vec<LodCell> {
    let mut bands: BTreeMap<[i32; 3], Vec<Voxel>> = BTreeMap::new();
    for (coords, chunk) in chunks {
        let (cx, cz) = (coords.x as i32, coords.z as i32);
        let group = [cx.div_euclid(16), coords.y.div_euclid(16), cz.div_euclid(16)];
        let band =
            bands.entry(group).or_insert_with(|| vec![Voxel::default(); BRICK_VOXEL_COUNT]);
        let lx = cx.rem_euclid(16) as usize;
        let ly = coords.y.rem_euclid(16) as usize;
        let lz = cz.rem_euclid(16) as usize;
        for bz in 0..2 {
            for by in 0..2 {
                for bx in 0..2 {
                    let block = chunk.block_at(bx, by, bz);
                    if block.id != BlockId::AIR {
                        let idx = (lx * 2 + bx) + 32 * (ly * 2 + by) + 32 * 32 * (lz * 2 + bz);
                        band[idx] = Voxel::new(block.id);
                    }
                }
            }
        }
    }
    bands.into_iter().map(|(group, grid)| LodCell { group, grid }).collect()
}

/// Every streamable LOD factor. The server validates request factors
/// against this list; `downsample_chunks` supports exactly these. 256 stays
/// streamable (custom `VOXEL_LOD_RINGS` tables, a future longer horizon) but
/// the production table below never reaches it.
pub const LOD_FACTORS: [u32; 7] = [2, 4, 8, 16, 32, 64, 256];

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

/// Home-region LOD factor for large cell-mode regions: a factor-`f` LOD cell
/// is `f` bricks (32 voxels) wide, so `f <= CELL_BLOCKS / BRICK_BLOCKS` keeps every home LOD cell inside
/// one game cell — the renderer can then hide exactly the ones under active
/// cells (a coarser factor spans several cells and draws over full-detail
/// terrain: overlapping rendering, live 2026-09-25).
pub const LARGE_HOME_FACTOR: u32 = {
    let c = (crate::CELL_BLOCKS / crate::BRICK_BLOCKS) as u32;
    if c >= 16 { 16 } else if c >= 8 { 8 } else if c >= 4 { 4 } else { 2 }
};

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
/// regions of 1024, 8 of 2048, 4 of 4096). `ring_table_keeps_cells_within_the_pixel_budget`
/// checks whichever table the build ships, `ring_table_ends_at_the_horizon`
/// that it stops at the horizon.
pub const LOD_RING_TABLE: &[(i32, u32)] = match crate::REGION_BLOCKS {
    64 => &[(2, 2), (3, 4), (6, 8), (13, 16), (16, 32)],
    128 => &[(2, 4), (3, 8), (6, 16), (8, 32)],
    256 => &[(2, 8), (3, 16), (4, 32)],
    // Cell-mode regions above 256 blocks (up to 65.5 km): ONLY the viewer's
    // own region, at LARGE_HOME_FACTOR — no neighbouring regions load in any
    // world preset (user, 2026-09-25: the region is the whole world now).
    // The pixel budget and 1 km horizon do not hold here.
    _ => &[(0, LARGE_HOME_FACTOR)],
};

/// How far the LOD rings reach, in world units: the last ring's Chebyshev
/// bound × `REGION_SIZE`, the same at every region size. It is also the
/// NEAREST the horizon edge can be (the viewer may stand on their region's
/// edge), which is where the client's fog goes opaque. 16 384 units = 1 km;
/// it was 2 km (32 768).
pub const LOD_HORIZON_UNITS: f32 = 16_384.0;

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

/// Names one streamable LOD unit: a region (factor <= 16) or a tile of
/// `tile_regions(factor)`² regions (64/256). `coords` are region coords
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
        2 => build_far_lod_cells(chunks),
        4 => downsample_cells(build_far_lod_cells(chunks)),
        8 => downsample_cells(downsample_cells(build_far_lod_cells(chunks))),
        16 => block_scale_cells(chunks),
        32 => downsample_cells(block_scale_cells(chunks)),
        64 => downsample_cells(downsample_cells(block_scale_cells(chunks))),
        256 => downsample_cells(downsample_cells(downsample_cells(downsample_cells(
            block_scale_cells(chunks),
        )))),
        _ => panic!("unsupported LOD factor {factor} (must be one of {LOD_FACTORS:?})"),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Block;

    fn grid_with(cells: &[([usize; 3], u16)]) -> Vec<Voxel> {
        let mut v = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        for ([x, y, z], id) in cells {
            v[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(*id));
        }
        v
    }

    #[test]
    fn empty_sources_downsample_to_air() {
        let out = downsample2(&[None; 8]);
        assert!(out.iter().all(|v| v.is_air()));
        assert_eq!(out.len(), BRICK_VOXEL_COUNT);
    }

    #[test]
    fn any_solid_source_voxel_makes_the_coarse_cell_solid() {
        // Source grid 0 (the -x-y-z octant), voxel (0,0,0) solid: coarse (0,0,0) solid.
        let g = grid_with(&[([0, 0, 0], 1)]);
        let mut sources: [Option<&[Voxel]>; 8] = [None; 8];
        sources[0] = Some(&g);
        let out = downsample2(&sources);
        assert_eq!(out[0].block, BlockId(1));
        assert!(out[1..].iter().all(|v| v.is_air()), "exactly one coarse cell");
    }

    #[test]
    fn octant_placement_maps_source_grids_to_coarse_sixteenths() {
        // Source grid 7 (+x+y+z octant), voxel (31,31,31): coarse (31,31,31).
        let g = grid_with(&[([31, 31, 31], 3)]);
        let mut sources: [Option<&[Voxel]>; 8] = [None; 8];
        sources[7] = Some(&g);
        let out = downsample2(&sources);
        assert_eq!(out[31 + 32 * 31 + 32 * 32 * 31].block, BlockId(3));
    }

    #[test]
    fn column_top_ties_break_to_lowest_id() {
        // Column tops of one coarse cell: id2, id5, id5, id2 -> 2-2 tie -> id2.
        let g = grid_with(&[([0, 0, 0], 2), ([1, 1, 0], 5), ([0, 0, 1], 5), ([1, 0, 1], 2)]);
        let mut sources: [Option<&[Voxel]>; 8] = [None; 8];
        sources[0] = Some(&g);
        let out = downsample2(&sources);
        assert_eq!(out[0].block, BlockId(2), "tie broken to lowest id");
    }

    #[test]
    fn surface_material_survives_the_material_under_it() {
        // Grass (3) over dirt (1), 4 voxels each: a volume majority ties 4-4
        // and the lowest id (dirt) used to win. Seen from above it is grass.
        let mut group = [BlockId(1); 8];
        for dz in 0..2 {
            for dx in 0..2 {
                group[dx + 2 + 4 * dz] = BlockId(3);
            }
        }
        assert_eq!(merge_group(&group).block, BlockId(3));

        // A lone voxel over a full lower layer colours one column top of
        // four, so the ground it stands on still wins.
        let mut lone = [BlockId::AIR; 8];
        for dz in 0..2 {
            for dx in 0..2 {
                lone[dx + 4 * dz] = BlockId(3);
            }
        }
        lone[2] = BlockId(7);
        assert_eq!(merge_group(&lone).block, BlockId(3));

        // Any-solid occupancy is unchanged: one voxel keeps the cell solid.
        let mut single = [BlockId::AIR; 8];
        single[5] = BlockId(4);
        assert_eq!(merge_group(&single).block, BlockId(4));
        assert!(merge_group(&[BlockId::AIR; 8]).is_air());
    }

    #[test]
    fn grass_over_dirt_stays_grass_at_every_multi_block_factor() {
        // A 2-chunk-deep slab: dirt everywhere, grass on the top block layer,
        // placed so the surface block is the LOWER of its factor-32 pair
        // (the alignment that used to flip to dirt).
        let mut chunks = Vec::new();
        for cy in 0..2 {
            let mut c = Chunk::default();
            for bz in 0..2 {
                for by in 0..2 {
                    for bx in 0..2 {
                        let top = cy == 1 && by == 1;
                        c.set_block(bx, by, bz, Block::new(BlockId(if top { 3 } else { 1 })));
                    }
                }
            }
            chunks.push((ChunkCoords::new(0, cy, 0), c));
        }
        for factor in [32u32, 64, 256] {
            let cells = downsample_chunks(chunks.clone(), factor);
            let top = cells
                .iter()
                .flat_map(|c| {
                    (0..32usize).rev().find_map(|y| {
                        let v = c.grid[32 * y];
                        (!v.is_air()).then_some((c.group[1] * 32 + y as i32, v.block))
                    })
                })
                .max()
                .expect("solid column");
            assert_eq!(top.1, BlockId(3), "factor {factor}: surface cell is grass");
        }
    }

    #[test]
    fn downsample_is_deterministic() {
        let g = grid_with(&[([5, 6, 7], 1), ([5, 7, 7], 2), ([4, 6, 7], 2)]);
        let mut sources: [Option<&[Voxel]>; 8] = [None; 8];
        sources[0] = Some(&g);
        assert_eq!(downsample2(&sources), downsample2(&sources));
    }

    use crate::derive_voxels;

    #[test]
    fn packed_grid_well_formedness_matches_decodability() {
        let grid = vec![Voxel::new(BlockId(3)); BRICK_VOXEL_COUNT];
        let good = PackedLodGrid::encode(&grid);
        assert!(good.is_well_formed());
        assert!(good.decode().is_some());
        for bad in [
            PackedLodGrid(vec![]),
            PackedLodGrid(vec![(100, BlockId(1))]),
            PackedLodGrid(vec![(u16::MAX, BlockId(1)), (u16::MAX, BlockId(2))]),
        ] {
            assert!(!bad.is_well_formed());
            assert!(bad.decode().is_none());
        }
    }

    /// The pre-2026-09-19 implementation, kept as the behavioural reference:
    /// dense `derive_voxels` per chunk, then `downsample2` over the octants.
    fn reference_far_lod_cells(chunks: Vec<(ChunkCoords, Chunk)>) -> Vec<LodCell> {
        let mut groups: BTreeMap<[i32; 3], [Option<Vec<Voxel>>; 8]> = BTreeMap::new();
        for (coords, chunk) in chunks {
            let cx = coords.x as i32;
            let cz = coords.z as i32;
            let group = [cx.div_euclid(2), coords.y.div_euclid(2), cz.div_euclid(2)];
            let octant =
                (cx.rem_euclid(2) + 2 * coords.y.rem_euclid(2) + 4 * cz.rem_euclid(2)) as usize;
            let entry = groups.entry(group).or_insert_with(|| std::array::from_fn(|_| None));
            entry[octant] = Some(derive_voxels(&chunk));
        }
        groups
            .into_iter()
            .map(|(group, grids)| {
                let sources: [Option<&[Voxel]>; 8] = std::array::from_fn(|i| grids[i].as_deref());
                LodCell { group, grid: downsample2(&sources) }
            })
            .collect()
    }

    /// The block-wise path must be bit-identical to the dense one: LOD
    /// payloads are cached, compared and streamed, so "close" is not enough.
    #[test]
    fn block_wise_far_lod_cells_match_the_dense_reference_exactly() {
        use crate::{BlockIndex, ChiselData};
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move |modulus: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % modulus
        };
        for round in 0..12 {
            let mut chunks = Vec::new();
            for _ in 0..(3 + next(10)) {
                let coords = ChunkCoords::new(next(4) as usize, next(5) as i32 - 2, next(4) as usize);
                let mut chunk = Chunk::default();
                for b in 0..crate::CHUNK_BLOCK_COUNT {
                    let bi = BlockIndex::from_linear(b);
                    let (bx, by, bz) = bi.xyz();
                    chunk.set_block(bx, by, bz, Block::new(BlockId(next(4) as u16)));
                    // ~1/3 of the blocks carry chisel detail, some with an AIR
                    // palette entry (occupied, air material); ~1/6 are half
                    // blocks (one cuboid).
                    match next(6) {
                        0 | 1 => {
                            let palette = vec![BlockId(1), BlockId(2), BlockId::AIR, BlockId(7)];
                            let mut grid = vec![None; crate::BLOCK_VOXEL_COUNT];
                            for _ in 0..next(3000) {
                                let i = next(crate::BLOCK_VOXEL_COUNT as u64) as usize;
                                grid[i] = (next(4) != 0).then(|| next(4) as u8);
                            }
                            chunk.chisel.insert(bi, ChiselData::from_grid(palette, &grid));
                        }
                        2 => {
                            let half = ChiselData::cuboid(BlockId(3), [0, 0, 0], [16, 8, 16]);
                            chunk.chisel.insert(bi, half);
                        }
                        _ => {}
                    }
                }
                chunks.push((coords, chunk));
            }
            let expected = reference_far_lod_cells(chunks.clone());
            let got = build_far_lod_cells(chunks);
            assert_eq!(got.len(), expected.len(), "round {round}: cell count");
            for (g, e) in got.iter().zip(&expected) {
                assert_eq!(g.group, e.group, "round {round}");
                assert!(g.grid == e.grid, "round {round}: grid of group {:?} differs", g.group);
            }
        }
    }

    #[test]
    fn build_far_lod_cells_groups_signed_y_correctly() {
        // y=-2 and y=-1 both div_euclid(2) to group y=-1 (div_euclid rounds
        // toward -inf, not zero), but land in different octant halves:
        // rem_euclid(2) is 0 for y=-2 (lower half) and 1 for y=-1 (upper
        // half) — the same ordering as the positive-y case, so a chunk
        // stack straddling y=0 groups and orders exactly like one that
        // doesn't.
        let mut lower = Chunk::default();
        lower.set_block(0, 0, 0, Block::new(BlockId(1)));
        let mut upper = Chunk::default();
        upper.set_block(0, 0, 0, Block::new(BlockId(2)));

        let chunks = vec![
            (ChunkCoords::new(0, -2, 0), lower),
            (ChunkCoords::new(0, -1, 0), upper),
        ];
        let cells = build_far_lod_cells(chunks);

        assert_eq!(cells.len(), 1, "both chunks land in the same 2x2x2 group");
        assert_eq!(cells[0].group, [0, -1, 0], "(-2).div_euclid(2) == (-1).div_euclid(2) == -1");

        // Coarse index x + 32*y + 32*32*z, at x=0, z=0: y=0 for the low half, y=16 for the high half.
        let grid = &cells[0].grid;
        assert_eq!(
            grid[0].block,
            BlockId(1),
            "y=-2 chunk (rem_euclid 0) occupies the lower octant half"
        );
        assert_eq!(
            grid[32 * 16].block,
            BlockId(2),
            "y=-1 chunk (rem_euclid 1) occupies the upper octant half"
        );
    }

    #[test]
    fn blocks_fast_path_maps_blocks_to_cells() {
        // A chunk with one solid block at block-index (1,0,1): with the chunk
        // in source slot 0, the coarse cell (1,0,1) of the 4³ corner is solid.
        let mut chunk = Chunk::default();
        chunk.set_block(1, 0, 1, Block::new(BlockId(4)));
        let mut chunks: [Option<&Chunk>; 8] = [None; 8];
        chunks[0] = Some(&chunk);
        let out = downsample_blocks(&chunks);
        let idx = |x: usize, y: usize, z: usize| x + 32 * y + 32 * 32 * z;
        assert_eq!(out[idx(1, 0, 1)].block, BlockId(4));
        assert_eq!(out.iter().filter(|v| !v.is_air()).count(), 1);
    }

    /// One solid whole block (16³ voxels) at block coords `b` of chunk `c`.
    fn chunk_with_block(b: [usize; 3], id: u16) -> Chunk {
        let mut chunk = Chunk::default();
        chunk.set_block(b[0], b[1], b[2], Block::new(BlockId(id)));
        chunk
    }

    #[test]
    fn factor4_two_pass_places_cells_at_world_voxel_over_4() {
        // Chunk (3,1,2), block (1,1,1): world voxels x 112..128, y 48..64,
        // z 80..96 → factor-4 cells x 28..32, y 12..16, z 20..24, all inside
        // group4 [0,0,0] (chunk div 4). Chunk (7,0,7), block (1,0,1): world
        // voxels x 240..256, y 0..16, z 240..256 → cells 60..64 / 0..4 /
        // 60..64 → group4 [1,0,1], local cells 28..32 / 0..4 / 28..32.
        let chunks = vec![
            (ChunkCoords::new(3, 1, 2), chunk_with_block([1, 1, 1], 7)),
            (ChunkCoords::new(7, 0, 7), chunk_with_block([1, 0, 1], 9)),
        ];
        let cells = downsample_cells(build_far_lod_cells(chunks));
        assert_eq!(cells.len(), 2, "two distinct factor-4 groups");

        let cell = |g: [i32; 3]| cells.iter().find(|c| c.group == g).expect("group present");
        let a = cell([0, 0, 0]);
        assert_eq!(a.grid[28 + 32 * 12 + 32 * 32 * 20].block, BlockId(7), "block min corner");
        assert_eq!(a.grid[31 + 32 * 15 + 32 * 32 * 23].block, BlockId(7), "block max corner");
        assert_eq!(
            a.grid.iter().filter(|v| !v.is_air()).count(),
            4 * 4 * 4,
            "a 16³-voxel block is exactly 4³ factor-4 cells"
        );
        let b = cell([1, 0, 1]);
        assert_eq!(b.grid[28 + 32 * 32 * 28].block, BlockId(9), "group-local min corner");
        assert_eq!(b.grid[31 + 32 * 3 + 32 * 32 * 31].block, BlockId(9), "group-local max corner");
        assert_eq!(b.grid.iter().filter(|v| !v.is_air()).count(), 4 * 4 * 4);
    }

    #[test]
    fn block_scale_cells_pack_the_region_into_one_grid_per_tile_and_band() {
        // Block cells are 16 world units: chunk (cx, sub_y, cz), block
        // (bx,by,bz) → grid cell (rem_euclid(cx,16)*2+bx,
        // rem_euclid(sub_y,16)*2+by, rem_euclid(cz,16)*2+bz) in group
        // [cx.div_euclid(16), sub_y.div_euclid(16), cz.div_euclid(16)].
        let chunks = vec![
            (ChunkCoords::new(5, 0, 7), chunk_with_block([1, 1, 0], 4)),
            (ChunkCoords::new(0, -1, 0), chunk_with_block([0, 0, 0], 2)),
            (ChunkCoords::new(17, 0, 20), chunk_with_block([0, 0, 0], 6)),
        ];
        let cells = block_scale_cells(chunks);
        assert_eq!(cells.len(), 3, "two bands at tile (0,0) plus band 0 of tile (1,1)");

        let cell = |g: [i32; 3]| cells.iter().find(|c| c.group == g).expect("group present");
        let band0 = cell([0, 0, 0]);
        assert_eq!(band0.grid[11 + 32 + 32 * 32 * 14].block, BlockId(4), "cell (11, 1, 14)");
        assert_eq!(band0.grid.iter().filter(|v| !v.is_air()).count(), 1);
        let band_neg = cell([0, -1, 0]);
        // rem_euclid(-1, 16) = 15 → cell y = 30 (top chunk of the band below 0).
        assert_eq!(band_neg.grid[32 * 30].block, BlockId(2));
        assert_eq!(band_neg.grid.iter().filter(|v| !v.is_air()).count(), 1);
        // Chunk (17, 0, 20) is tile-local (1, 0, 4) → cell (2, 0, 8).
        let tile11 = cell([1, 0, 1]);
        assert_eq!(tile11.grid[2 + 32 * 32 * 8].block, BlockId(6), "cell (2, 0, 8)");
        assert_eq!(tile11.grid.iter().filter(|v| !v.is_air()).count(), 1);
    }

    #[test]
    fn lod_key_region_to_tile_mapping_handles_negatives() {
        use crate::RegionCoords;
        // Factors <= 16: key coords ARE the region coords.
        assert_eq!(LodKey::for_region(4, RegionCoords::new(-3, 7)).coords, [-3, 7]);
        assert_eq!(tile_regions(2), 1);
        assert_eq!(tile_regions(16), 1);
        // Factor 64: one 2048-unit grid — 2x2-region tiles at the default
        // region size; div_euclid so negatives round toward -inf.
        let s64 = tile_regions(64);
        assert_eq!(s64, (2048 / crate::REGION_SIZE as i32).max(1));
        assert_eq!(
            LodKey::for_region(64, RegionCoords::new(-1, 3)).coords,
            [(-1i32).div_euclid(s64), 3i32.div_euclid(s64)]
        );
        // Factor 256: 8192-unit grids, 8x8-region tiles at the default size.
        let s256 = tile_regions(256);
        assert_eq!(s256, (8192 / crate::REGION_SIZE as i32).max(1));
        assert_eq!(
            LodKey::for_region(256, RegionCoords::new(-9, 15)).coords,
            [(-9i32).div_euclid(s256), 15i32.div_euclid(s256)]
        );
    }

    #[test]
    fn lod_key_regions_and_world_offset() {
        use crate::{RegionCoords, REGION_SIZE};
        // `span` is 2 at the default region size (1 at 128 blocks).
        let span = tile_regions(64);
        let key = LodKey { factor: 64, coords: [-1, 1] };
        assert_eq!(key.origin_region(), RegionCoords::new(-span, span));
        let regions = key.regions();
        assert_eq!(regions.len(), (span * span) as usize);
        assert!(regions.contains(&RegionCoords::new(-span, span)));
        assert!(regions.contains(&RegionCoords::new(-1, 2 * span - 1)));
        let edge = span as f32 * REGION_SIZE;
        assert_eq!(key.world_offset(), [-edge, 0.0, edge]);
        // A tile grid spans exactly 32 cells * factor world units = span
        // regions — or, once a grid is smaller than a region (factor 64 at
        // 256 blocks), the key is one region of several grids.
        for factor in [64, 256] {
            let grid = 32 * factor;
            let span = tile_regions(factor as u32);
            if grid >= REGION_SIZE as i32 {
                assert_eq!(grid, span * REGION_SIZE as i32, "factor {factor}: whole regions");
            } else {
                assert_eq!(span, 1);
                assert_eq!(REGION_SIZE as i32 % grid, 0, "factor {factor}: whole grids");
            }
        }
        // Region-keyed factor: world offset == the region's own offset.
        let rk = LodKey::for_region(4, RegionCoords::new(3, -2));
        assert_eq!(rk.world_offset(), RegionCoords::new(3, -2).world_offset());
    }

    #[test]
    fn ring_table_is_ascending_and_factors_valid() {
        // A first bound of 0 is the home-region ring of large cell-mode
        // regions; every later bound must strictly ascend.
        let mut prev = -1;
        for &(bound, factor) in LOD_RING_TABLE {
            assert!(bound > prev, "bounds ascend");
            prev = bound;
            assert!(LOD_FACTORS.contains(&factor));
        }
    }

    /// Fixture for the `ring_factor_in` tests: they pin the assignment and
    /// hysteresis ALGORITHM, so they must not move with `LOD_RING_TABLE`, which
    /// is re-cut per `REGION_BLOCKS`. (This is the default-size table.)
    const RING_FIXTURE: &[(i32, u32)] = &[(2, 2), (3, 4), (6, 8), (13, 16), (27, 32), (32, 64)];

    #[test]
    fn ring_factor_fresh_assignment_over_all_distances() {
        for d in 0..=35 {
            let expect = match d {
                0..=2 => Some(2),
                3 => Some(4),
                4..=6 => Some(8),
                7..=13 => Some(16),
                14..=27 => Some(32),
                28..=32 => Some(64),
                _ => None,
            };
            assert_eq!(
                ring_factor_in(RING_FIXTURE, LOD_STICKY_BAND, d, None),
                expect,
                "dist {d}, no current"
            );
        }
    }

    #[test]
    fn ring_factor_sticky_band_holds_factor_4_one_past_its_bound() {
        // Factor 4's fresh band is d == 3; band 1 widens the held interval to
        // 2..=4 on both edges.
        for d in 0..=8 {
            let expect = match d {
                0..=1 => Some(2),
                2..=4 => Some(4),
                5..=6 => Some(8),
                7..=8 => Some(16),
                _ => unreachable!(),
            };
            assert_eq!(
                ring_factor_in(RING_FIXTURE, LOD_STICKY_BAND, d, Some(4)),
                expect,
                "dist {d}, current 4"
            );
        }
    }

    #[test]
    fn ring_factor_sticky_band_holds_factor_16_both_directions() {
        // Factor 16's fresh band is 7..=13; band 1 widens the held interval
        // to 6..=14.
        for d in 0..=15 {
            let expect = match d {
                0..=2 => Some(2),
                3 => Some(4),
                4..=5 => Some(8),
                6..=14 => Some(16),
                15 => Some(32),
                _ => unreachable!(),
            };
            assert_eq!(
                ring_factor_in(RING_FIXTURE, LOD_STICKY_BAND, d, Some(16)),
                expect,
                "dist {d}, current 16"
            );
        }
    }

    #[test]
    fn ring_table_keeps_cells_within_the_pixel_budget() {
        if crate::REGION_BLOCKS > 256 {
            return; // placeholder table for large cell-mode regions
        }
        // Worst case for a ring: the viewer stands on the edge of their own
        // region, so a ring whose inner neighbour ends at `prev` regions
        // starts `prev` whole regions away.
        // Near radius 1 (3x3) is the tightest window any preset runs a full
        // table with; the production 5x5 only pushes the first ring further out.
        let mut prev = 1;
        for &(bound, factor) in LOD_RING_TABLE {
            let start = prev as f32 * REGION_SIZE;
            let px = factor as f32 / start * LOD_REFERENCE_FOCAL_PX;
            assert!(
                px <= LOD_MAX_CELL_PX,
                "factor {factor} starts {start} voxels out: {px:.2} px per cell"
            );
            prev = bound;
        }
    }

    #[test]
    fn a_large_cell_region_s_home_lod_fits_inside_a_cell() {
        if crate::REGION_BLOCKS <= 256 || crate::CELL_BLOCKS == 0 {
            return;
        }
        let home = ring_factor_in(LOD_RING_TABLE, 0, 0, None).expect("home ring");
        assert!(
            home as usize <= (crate::CELL_BLOCKS / crate::BRICK_BLOCKS).max(2),
            "home factor {home} > cell {} bricks",
            crate::CELL_BLOCKS / crate::BRICK_BLOCKS
        );
        assert_eq!(ring_factor_in(LOD_RING_TABLE, 0, 1, None), None, "no neighbouring regions");
    }

    #[test]
    fn ring_table_ends_at_the_horizon() {
        if crate::REGION_BLOCKS > 256 {
            return; // placeholder table for large cell-mode regions
        }
        let &(last, _) = LOD_RING_TABLE.last().unwrap();
        assert_eq!(last as f32 * REGION_SIZE, LOD_HORIZON_UNITS);
    }

    #[test]
    fn ring_factor_unknown_current_falls_back_to_fresh() {
        // A current factor not in the table (table shrank between sessions,
        // say) gets no stickiness — plain fresh assignment.
        assert_eq!(ring_factor_in(RING_FIXTURE, LOD_STICKY_BAND, 3, Some(99)), Some(4));
        assert_eq!(ring_factor_in(RING_FIXTURE, LOD_STICKY_BAND, 33, Some(99)), None);
    }

    #[test]
    fn packed_grid_round_trips() {
        // Mixed content incl. a lone voxel at the very end.
        let mut grid = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        for i in 0..5000 {
            grid[i] = Voxel::new(BlockId(1));
        }
        grid[9000] = Voxel::new(BlockId(7));
        grid[BRICK_VOXEL_COUNT - 1] = Voxel::new(BlockId(3));
        let packed = PackedLodGrid::encode(&grid);
        assert_eq!(packed.decode().expect("valid"), grid);
        // All-air compresses to one run.
        let air = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        assert_eq!(PackedLodGrid::encode(&air).0.len(), 1);
        assert_eq!(PackedLodGrid::encode(&air).decode().unwrap(), air);
        // Worst case: alternating ids still round-trips (u16 runs of 1).
        let mut alt = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        for (i, v) in alt.iter_mut().enumerate() {
            *v = Voxel::new(BlockId((i % 2) as u16));
        }
        assert_eq!(PackedLodGrid::encode(&alt).decode().unwrap(), alt);
    }

    #[test]
    fn packed_grid_rejects_wrong_total() {
        let short = PackedLodGrid(vec![(100, BlockId(1))]);
        assert!(short.decode().is_none(), "runs must sum to BRICK_VOXEL_COUNT");
        let long = PackedLodGrid(vec![(u16::MAX, BlockId(0)), (u16::MAX, BlockId(0))]);
        assert!(long.decode().is_none());
    }

    #[test]
    fn downsample_chunks_dispatches_every_factor() {
        // One solid block at chunk (0,0,0), block (0,0,0): world voxels 0..16 on
        // each axis. Expected occupancy per factor: f cells = 16/f per axis
        // (min 1), all in the grid at group [0,0,0].
        for &(factor, expect_per_axis) in
            &[(2u32, 8usize), (4, 4), (8, 2), (16, 1), (32, 1), (64, 1), (256, 1)]
        {
            let chunks = vec![(ChunkCoords::new(0, 0, 0), chunk_with_block([0, 0, 0], 5))];
            let cells = downsample_chunks(chunks, factor);
            assert_eq!(cells.len(), 1, "factor {factor}: one populated grid");
            assert_eq!(cells[0].group, [0, 0, 0]);
            let n = cells[0].grid.iter().filter(|v| !v.is_air()).count();
            assert_eq!(n, expect_per_axis.pow(3), "factor {factor}");
            assert_eq!(cells[0].grid[0].block, BlockId(5), "cell (0,0,0) solid");
        }
    }

    #[test]
    fn downsample_chunks_places_region_content_in_the_low_corner_rect() {
        // The region's far-corner SIM chunk (`REGION_CHUNKS - 1` on x and z).
        // `downsample_chunks` still treats one chunk as one 32-voxel grid
        // (`BRICK_VOXELS`), so a later task ports it to brick views; until
        // then the expected footprint is REGION_CHUNKS grids of BRICK_VOXELS.
        // At factor 64 the region spans cells 0..per (16 at 64 blocks), so
        // that chunk's far block lands in the last of them.
        let far = crate::REGION_CHUNKS - 1;
        let per = cells_per_region(64);
        assert_eq!(per, crate::REGION_CHUNKS * crate::BRICK_VOXELS / 64);
        assert_eq!(cells_per_region(256), crate::REGION_CHUNKS * crate::BRICK_VOXELS / 256);
        let chunks = vec![(ChunkCoords::new(far, 0, far), chunk_with_block([1, 0, 1], 9))];
        let cells = downsample_chunks(chunks, 64);
        assert_eq!(cells.len(), 1);
        // `per` may exceed one grid (64 at 256 blocks): then the last
        // cell sits in the last grid of the region's own group lattice.
        let (grp, local) = (((per - 1) / 32) as i32, (per - 1) % 32);
        assert_eq!(cells[0].group, [grp, 0, grp]);
        let idx = local + 32 * 0 + 32 * 32 * local;
        assert_eq!(cells[0].grid[idx].block, BlockId(9));
    }

    #[test]
    fn copy_region_cells_overlays_and_clears() {
        use crate::RegionCoords;
        // A key whose tile spans several regions: factor 64 (2x2) at the
        // default size; at 128 blocks that tile is ONE region, so 256
        // (2x2 at 128 as well).
        let factor = if tile_regions(64) > 1 { 64 } else { 256 };
        if tile_regions(factor) <= 1 {
            return; // large cell-mode regions: no tile spans several regions
        }
        let per = cells_per_region(factor); // 16 by default
        let key = LodKey { factor, coords: [0, 0] };
        // Coarse tile band 0: fully solid everywhere.
        let mut tile: BTreeMap<[i32; 3], Vec<Voxel>> = BTreeMap::new();
        tile.insert([0, 0, 0], vec![Voxel::new(BlockId(1)); BRICK_VOXEL_COUNT]);
        // Region (1, 0)'s authoritative cells: a single solid cell at its local (0,0,0).
        let mut grid = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        grid[0] = Voxel::new(BlockId(7));
        let region_cells = vec![LodCell { group: [0, 0, 0], grid }];
        copy_region_cells_into_tile(&mut tile, &region_cells, RegionCoords::new(1, 0), &key);

        let band = &tile[&[0, 0, 0]];
        // Region (1,0) maps to x cells per..2*per, z 0..per: rect cleared, one cell rewritten.
        assert_eq!(band[per].block, BlockId(7), "authoritative cell written at x offset `per`");
        assert!(band[per + 1].is_air(), "rest of the region rect cleared to authoritative air");
        assert!(band[per + 32 * 32 * (per - 1)].is_air(), "cleared across the whole z extent");
        assert_eq!(band[0].block, BlockId(1), "outside the region rect untouched");
        assert_eq!(band[32 * 32 * per].block, BlockId(1), "z >= per (other region row) untouched");
    }
}
