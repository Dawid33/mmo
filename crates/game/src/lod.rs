//! LOD cell data model + downsampling, shared by the server (builds streamed
//! LOD) and the client (renders it). Pure data code — no Bevy, no I/O.
//! **Layout contract:** LOD grids are always 32³ `Vec<Voxel>` in
//! `x + 32*y + 32*32*z` order.

use std::collections::BTreeMap;

use crate::{derive_voxels, BlockId, Chunk, ChunkCoords, RegionCoords, Voxel, CHUNK_VOXEL_COUNT, REGION_SIZE};

/// Merge up to 2×2×2 source 32³ grids (index `sx + 2*sy + 4*sz`; `None` = all-air) into one 32³ coarse grid at 2× the cell size.
/// Each coarse cell (x,y,z) draws from source grid `(x/16, y/16, z/16)`'s 2×2×2 voxel block at `((x%16)*2, (y%16)*2, (z%16)*2)`.
/// Solid iff any source voxel solid; material = majority BlockId among solid sources, ties → lowest BlockId.
///
/// **Layout contract:** LOD grids are always 32³ `Vec<Voxel>` in the standard `x + 32*y + 32*32*z` order.
pub fn downsample2(sources: &[Option<&[Voxel]>; 8]) -> Vec<Voxel> {
    let mut output = vec![Voxel::default(); CHUNK_VOXEL_COUNT];

    // Iterate all 32³ coarse cells
    for z in 0..32 {
        for y in 0..32 {
            for x in 0..32 {
                // Determine which source octant and offset within that octant
                let sx = x / 16; // 0 or 1
                let sy = y / 16; // 0 or 1
                let sz = z / 16; // 0 or 1
                let source_idx = sx + 2 * sy + 4 * sz;

                let ox = (x % 16) * 2; // 0..30
                let oy = (y % 16) * 2; // 0..30
                let oz = (z % 16) * 2; // 0..30

                // Gather the 2×2×2 source voxels from this octant's grid
                let mut block_counts: [(BlockId, u8); 8] = [(BlockId(0), 0); 8];
                let mut block_count_len = 0;
                let mut has_solid = false;

                if let Some(source_grid) = sources[source_idx] {
                    for dz in 0..2 {
                        for dy in 0..2 {
                            for dx in 0..2 {
                                let sx_coord = ox + dx;
                                let sy_coord = oy + dy;
                                let sz_coord = oz + dz;
                                // ox/oy/oz top out at 30 and dx/dy/dz at 1.
                                debug_assert!(sx_coord < 32 && sy_coord < 32 && sz_coord < 32);

                                let idx = sx_coord + 32 * sy_coord + 32 * 32 * sz_coord;
                                let voxel = source_grid[idx];

                                if !voxel.is_air() {
                                    has_solid = true;
                                    let block_id = voxel.block;

                                    // Find or add this block_id to our count array
                                    let mut found = false;
                                    for (bid, count) in block_counts[..block_count_len].iter_mut() {
                                        if *bid == block_id {
                                            *count = count.saturating_add(1);
                                            found = true;
                                            break;
                                        }
                                    }
                                    if !found && block_count_len < 8 {
                                        block_counts[block_count_len] = (block_id, 1);
                                        block_count_len += 1;
                                    }
                                }
                            }
                        }
                    }
                }

                // Determine the output material
                if has_solid {
                    // Find the majority material; ties break to lowest BlockId
                    let mut max_count = 0;
                    let mut winner = BlockId(u16::MAX); // sentinel

                    for (bid, count) in block_counts[..block_count_len].iter() {
                        if *count > max_count || (*count == max_count && *bid < winner) {
                            max_count = *count;
                            winner = *bid;
                        }
                    }

                    output[x + 32 * y + 32 * 32 * z] = Voxel::new(winner);
                }
            }
        }
    }

    output
}

/// Block-scale (factor-16) downsample: each source chunk contributes its
/// 2×2×2 `blocks` directly as coarse cells. Chiseled blocks count as their
/// block id — chisel detail is invisible at 16×.
/// Output: fills only the 0..4 corner of the 32³ grid (4×4×4 cells = 8 source chunks × 8 blocks each).
///
/// NOTE: production factor-16 goes through `worldgen_rings::block_scale_cells`
/// (which packs a whole region per grid); this is the test-covered reference
/// implementation of the per-group block-read rule.
///
/// **Layout contract:** LOD grids are always 32³ `Vec<Voxel>` in the standard `x + 32*y + 32*32*z` order.
pub fn downsample_blocks(chunks: &[Option<&Chunk>; 8]) -> Vec<Voxel> {
    let mut output = vec![Voxel::default(); CHUNK_VOXEL_COUNT];

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

/// One 2×2×2-chunk-group cell produced by a far region's async LOD build: the
/// downsampled 32³ grid plus the region-local group coordinate it belongs to
/// (chunk coords `div_euclid`'d by 2 on every axis, y included — see
/// `build_far_lod_cells`).
pub struct LodCell {
    pub group: [i32; 3],
    pub grid: Vec<Voxel>,
}

/// Group a far region's `(ChunkCoords, Chunk)` list into 2×2×2 clusters and
/// downsample each cluster into one `LodCell`. `coords.x`/`coords.z` are
/// region-local `0..REGION_CHUNKS` (unsigned) but `coords.y` is signed (chunks stack both
/// above and below y=0 as the world grows) — every axis uses
/// `div_euclid`/`rem_euclid` uniformly so the grouping and octant math stay
/// correct regardless of sign, matching `downsample2`'s `sx + 2*sy + 4*sz`
/// octant convention (more negative y lands in the lower octant half, same as
/// positive y — `div_euclid`/`rem_euclid` round toward negative infinity, not
/// toward zero, so this holds across the y=0 boundary).
pub fn build_far_lod_cells(chunks: Vec<(ChunkCoords, Chunk)>) -> Vec<LodCell> {
    let mut groups: BTreeMap<[i32; 3], [Option<Vec<Voxel>>; 8]> = BTreeMap::new();
    for (coords, chunk) in chunks {
        let cx = coords.x as i32;
        let cz = coords.z as i32;
        let group = [cx.div_euclid(2), coords.y.div_euclid(2), cz.div_euclid(2)];
        let octant =
            (cx.rem_euclid(2) + 2 * coords.y.rem_euclid(2) + 4 * cz.rem_euclid(2)) as usize;
        let entry = groups.entry(group).or_insert_with(|| std::array::from_fn(|_| None));
        entry[octant] = Some(derive_voxels(&chunk.blocks, &chunk.chisel));
    }
    groups
        .into_iter()
        .map(|(group, grids)| {
            let sources: [Option<&[Voxel]>; 8] = std::array::from_fn(|i| grids[i].as_deref());
            LodCell { group, grid: downsample2(&sources) }
        })
        .collect()
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
/// cz.rem_euclid(16)*2 + bz)`. With `REGION_CHUNKS = 32` a region spans 2×2
/// tiles per band; a region smaller than one tile leaves the grid's excess
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
            bands.entry(group).or_insert_with(|| vec![Voxel::default(); CHUNK_VOXEL_COUNT]);
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
/// against this list; `downsample_chunks` supports exactly these.
pub const LOD_FACTORS: [u32; 5] = [2, 4, 16, 64, 256];

/// Ring table: `(chebyshev_bound, lod_factor)` with ascending bounds. A
/// region at Chebyshev distance `d` from the viewer region renders at the
/// factor of the first entry with `d <= bound`; None past the last bound.
pub const LOD_RING_TABLE: &[(i32, u32)] = &[(2, 2), (4, 4), (8, 16), (16, 64), (32, 256)];

/// Hysteresis band for `ring_factor_in` (see that function).
pub const LOD_STICKY_BAND: i32 = 1;

/// Tile edge length in regions for a factor's `LodKey`: factors whose 32³
/// grid fits inside one region key per-region; 64/256 key per-tile so one
/// key is exactly one grid footprint (32 * factor == span * REGION_SIZE).
pub fn tile_regions(factor: u32) -> i32 {
    ((32 * factor) as i32 / REGION_SIZE as i32).max(1)
}

/// Names one streamable LOD unit: a region (factor <= 16) or a tile of
/// `tile_regions(factor)`² regions (64/256). `coords` are region coords
/// divided (euclid) by the tile span, so they're plain region coords for
/// the small factors.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
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
/// cells, so any grid fits in <= CHUNK_VOXEL_COUNT runs).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PackedLodGrid(pub Vec<(u16, BlockId)>);

impl PackedLodGrid {
    pub fn encode(grid: &[Voxel]) -> Self {
        debug_assert_eq!(grid.len(), CHUNK_VOXEL_COUNT);
        let mut runs: Vec<(u16, BlockId)> = Vec::new();
        for v in grid {
            match runs.last_mut() {
                Some((count, block)) if *block == v.block && *count < u16::MAX => *count += 1,
                _ => runs.push((1, v.block)),
            }
        }
        Self(runs)
    }

    /// None when the run total is not exactly one grid — wire data is
    /// untrusted, so corrupt payloads drop instead of panicking.
    pub fn decode(&self) -> Option<Vec<Voxel>> {
        let total: usize = self.0.iter().map(|&(n, _)| n as usize).sum();
        if total != CHUNK_VOXEL_COUNT {
            return None;
        }
        let mut grid = Vec::with_capacity(CHUNK_VOXEL_COUNT);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Block;

    fn grid_with(cells: &[([usize; 3], u16)]) -> Vec<Voxel> {
        let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for ([x, y, z], id) in cells {
            v[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(*id));
        }
        v
    }

    #[test]
    fn empty_sources_downsample_to_air() {
        let out = downsample2(&[None; 8]);
        assert!(out.iter().all(|v| v.is_air()));
        assert_eq!(out.len(), CHUNK_VOXEL_COUNT);
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
    fn majority_material_wins_ties_to_lowest() {
        // One coarse cell: 8 source voxels = 3×id2, 3×id5, 2×air → tie 3-3 → id2.
        let mut g = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for (i, (x, y, z)) in [(0, 0, 0), (1, 0, 0), (0, 1, 0)].into_iter().enumerate() {
            let _ = i;
            g[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(2));
        }
        for (x, y, z) in [(1, 1, 0), (0, 0, 1), (1, 0, 1)] {
            g[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(5));
        }
        let mut sources: [Option<&[Voxel]>; 8] = [None; 8];
        sources[0] = Some(&g);
        let out = downsample2(&sources);
        assert_eq!(out[0].block, BlockId(2), "tie broken to lowest id");
    }

    #[test]
    fn downsample_is_deterministic() {
        let g = grid_with(&[([5, 6, 7], 1), ([5, 7, 7], 2), ([4, 6, 7], 2)]);
        let mut sources: [Option<&[Voxel]>; 8] = [None; 8];
        sources[0] = Some(&g);
        assert_eq!(downsample2(&sources), downsample2(&sources));
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
        // Factor 64: 2x2-region tiles, div_euclid so negatives round toward -inf.
        assert_eq!(tile_regions(64), 2);
        assert_eq!(LodKey::for_region(64, RegionCoords::new(-1, 3)).coords, [-1, 1]);
        // Factor 256: 8x8-region tiles.
        assert_eq!(tile_regions(256), 8);
        assert_eq!(LodKey::for_region(256, RegionCoords::new(-9, 15)).coords, [-2, 1]);
    }

    #[test]
    fn lod_key_regions_and_world_offset() {
        use crate::{RegionCoords, REGION_SIZE};
        let key = LodKey { factor: 64, coords: [-1, 1] };
        assert_eq!(key.origin_region(), RegionCoords::new(-2, 2));
        let regions = key.regions();
        assert_eq!(regions.len(), 4);
        assert!(regions.contains(&RegionCoords::new(-2, 2)));
        assert!(regions.contains(&RegionCoords::new(-1, 3)));
        assert_eq!(key.world_offset(), [-2.0 * REGION_SIZE, 0.0, 2.0 * REGION_SIZE]);
        // A tile grid spans exactly 32 cells * factor world units = span regions.
        assert_eq!(32 * 64, 2 * REGION_SIZE as i32);
        assert_eq!(32 * 256, 8 * REGION_SIZE as i32);
        // Region-keyed factor: world offset == the region's own offset.
        let rk = LodKey::for_region(4, RegionCoords::new(3, -2));
        assert_eq!(rk.world_offset(), RegionCoords::new(3, -2).world_offset());
    }

    #[test]
    fn ring_table_is_ascending_and_factors_valid() {
        let mut prev = 0;
        for &(bound, factor) in LOD_RING_TABLE {
            assert!(bound > prev, "bounds ascend");
            prev = bound;
            assert!(LOD_FACTORS.contains(&factor));
        }
    }

    #[test]
    fn ring_factor_fresh_assignment_over_all_distances() {
        for d in 0..=35 {
            let expect = match d {
                0..=2 => Some(2),
                3..=4 => Some(4),
                5..=8 => Some(16),
                9..=16 => Some(64),
                17..=32 => Some(256),
                _ => None,
            };
            assert_eq!(
                ring_factor_in(LOD_RING_TABLE, LOD_STICKY_BAND, d, None),
                expect,
                "dist {d}, no current"
            );
        }
    }

    #[test]
    fn ring_factor_sticky_band_holds_factor_4_one_past_its_bound() {
        // Factor 4's fresh band is d in 3..=4; band 1 widens the held
        // interval to 2..=5 on both edges. Unlike the old 2-entry client
        // table, factor 4 is no longer the innermost ring here (factor 2 is),
        // so its lower edge is a real boundary too, not `i32::MIN`.
        for d in 0..=10 {
            let expect = match d {
                0..=1 => Some(2),
                2..=5 => Some(4),
                6..=8 => Some(16),
                9..=10 => Some(64),
                _ => unreachable!(),
            };
            assert_eq!(
                ring_factor_in(LOD_RING_TABLE, LOD_STICKY_BAND, d, Some(4)),
                expect,
                "dist {d}, current 4"
            );
        }
    }

    #[test]
    fn ring_factor_sticky_band_holds_factor_16_both_directions() {
        // Factor 16's fresh band is 5..=8; band 1 widens the held interval to
        // 4..=9 (unchanged from the old table: its neighboring bounds, 4 and
        // 8, are the same in both tables).
        for d in 0..=10 {
            let expect = match d {
                0..=2 => Some(2),
                3 => Some(4),
                4..=9 => Some(16),
                10 => Some(64),
                _ => unreachable!(),
            };
            assert_eq!(
                ring_factor_in(LOD_RING_TABLE, LOD_STICKY_BAND, d, Some(16)),
                expect,
                "dist {d}, current 16"
            );
        }
    }

    #[test]
    fn ring_factor_unknown_current_falls_back_to_fresh() {
        // A current factor not in the table (table shrank between sessions,
        // say) gets no stickiness — plain fresh assignment.
        assert_eq!(ring_factor_in(LOD_RING_TABLE, LOD_STICKY_BAND, 3, Some(99)), Some(4));
        assert_eq!(ring_factor_in(LOD_RING_TABLE, LOD_STICKY_BAND, 33, Some(99)), None);
    }

    #[test]
    fn packed_grid_round_trips() {
        // Mixed content incl. a lone voxel at the very end.
        let mut grid = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for i in 0..5000 {
            grid[i] = Voxel::new(BlockId(1));
        }
        grid[9000] = Voxel::new(BlockId(7));
        grid[CHUNK_VOXEL_COUNT - 1] = Voxel::new(BlockId(3));
        let packed = PackedLodGrid::encode(&grid);
        assert_eq!(packed.decode().expect("valid"), grid);
        // All-air compresses to one run.
        let air = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        assert_eq!(PackedLodGrid::encode(&air).0.len(), 1);
        assert_eq!(PackedLodGrid::encode(&air).decode().unwrap(), air);
        // Worst case: alternating ids still round-trips (u16 runs of 1).
        let mut alt = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for (i, v) in alt.iter_mut().enumerate() {
            *v = Voxel::new(BlockId((i % 2) as u16));
        }
        assert_eq!(PackedLodGrid::encode(&alt).decode().unwrap(), alt);
    }

    #[test]
    fn packed_grid_rejects_wrong_total() {
        let short = PackedLodGrid(vec![(100, BlockId(1))]);
        assert!(short.decode().is_none(), "runs must sum to CHUNK_VOXEL_COUNT");
        let long = PackedLodGrid(vec![(u16::MAX, BlockId(0)), (u16::MAX, BlockId(0))]);
        assert!(long.decode().is_none());
    }
}
