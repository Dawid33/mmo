//! Deterministic world generation. `generate_region` is a pure function of
//! region coordinates: same coords → identical output on every machine,
//! which is what makes "cycle out = park, cycle in = restore-or-regenerate"
//! safe for the multi-region world.

mod noise;
mod spline;
mod density;
pub mod cache;
pub mod forge;
pub mod forged;
pub mod islands;
pub mod pipeline;
pub mod material;

/// The world seed shared by every `generate_region` caller (server + offline
/// client) so they produce the SAME world. Kept in one place so the server and
/// client can never silently drift to different seeds.
// Picked by examples/seed_scan.rs (re-picked for FEATURE_SCALE = 0.5): dry
// flat spawn (y=77, sea=64) on a coastal plain, open sea ~0.5 km north and a
// mountain belt (peak y=261) ~1 km south — both inside the LOD horizon.
// Re-run the scan if terrain shaping changes.
pub const WORLD_SEED: u64 = 0x00D5_C0DE_007B;

use game::{
    Block, BlockId, BlockIndex, ChiselData, Chunk, Column, ColumnCoords, RegionCoords, SubChunk,
    BLOCK_VOXELS, HALF_BLOCK_VOXELS, CHUNK_BLOCKS, CHUNK_VOXEL_COUNT, REGION_CHUNKS,
};

use crate::material::Materials;
use crate::pipeline::{ProfileGrid, TerrainPipeline, PROFILE_LATTICE_BLOCKS, SEA_TOP_BLOCK};

// Region span in blocks: REGION_CHUNKS chunks * CHUNK_BLOCKS blocks/chunk.
const REGION_BLOCKS: i32 = (REGION_CHUNKS * CHUNK_BLOCKS) as i32; // 64
// Coarse vertical scan bounds (blocks). Terrain lives well inside this band.
// They bound surfaces only: the ground below a surface is the column's
// `ColumnGround` recipe, which has no lower bound.
const SCAN_TOP: i32 = 320;
const SCAN_BOTTOM: i32 = -64;
/// Margin (blocks) a region or cell patch covers past its window on every
/// side, so the cliff rule can read the surfaces just outside it. One whole
/// lattice step keeps the window on the world-aligned profile lattice, so the
/// surfaces inside are bit-identical to a window without the margin.
const PATCH_MARGIN: i32 = PROFILE_LATTICE_BLOCKS as i32;
// Deepest non-stone material_at depth (dirt/sand band). Keep in sync with
// material.rs's `depth_below_surface <= 3` bands.
const MAX_SOIL_DEPTH: i32 = 3;

/// Meter coordinate of a block center at world block index `b`.
fn block_center_m(b: i32) -> f64 {
    b as f64 + 0.5
}

/// Worldgen terrain is whole blocks and half blocks only — never finer voxel
/// detail. A half block is an ordinary one-cuboid chisel (editable at voxel
/// scale like any other); the continuous surface is rounded to the nearest half block: a
/// block holds a half block once the surface is a quarter of the way up it, and is
/// whole once it is three quarters up. Density falls monotonically with
/// height, so every block under a half-or-better block is whole.
const HALF_TEST_M: f64 = 0.25;
const FULL_TEST_M: f64 = 0.75;

/// Highest block that is at least half full for a precomputed profile — the
/// surface block — or `SCAN_BOTTOM - 1` if the whole column is air. Shared by
/// full-res and coarse generation, so any-solid LOD cells agree with the
/// generated blocks.
fn surface_from_profile(prof: &crate::pipeline::ColumnProfile) -> i32 {
    let mut by = SCAN_TOP;
    while by >= SCAN_BOTTOM {
        if prof.density_at(by as f64 + HALF_TEST_M) > 0.0 {
            return by;
        }
        by -= 1;
    }
    SCAN_BOTTOM - 1
}

/// Whether the surface block `surface` is whole rather than a half block.
fn surface_is_full(prof: &crate::pipeline::ColumnProfile, surface: i32) -> bool {
    prof.density_at(surface as f64 + FULL_TEST_M) > 0.0
}

/// The terrain of one region- or LOD-tile-sized window, whatever it comes
/// from: the noise pipeline, or a forged world file (`forged.rs`).
pub(crate) trait Patch {
    /// Column profile at world meters (x, z), and the topmost WATER block of
    /// that column: `SEA_TOP_BLOCK` everywhere in a procedural world, higher
    /// over a forged world's lakes and rivers.
    fn sample(&self, x: f64, z: f64) -> (crate::pipeline::ColumnProfile, i32);
}

struct ProceduralPatch(ProfileGrid);

impl Patch for ProceduralPatch {
    fn sample(&self, x: f64, z: f64) -> (crate::pipeline::ColumnProfile, i32) {
        (self.0.profile_at(x, z), SEA_TOP_BLOCK)
    }
}

/// The full REGION_CHUNKS x REGION_CHUNKS column grid for one region. Pure and
/// deterministic. With `VOXEL_WORLD_FILE` set the region comes from that
/// forged world and `seed` is ignored (see `forged.rs`).
pub fn generate_region(coords: RegionCoords, seed: u64, mats: Materials) -> Vec<(ColumnCoords, Column)> {
    if let Some(world) = forged::active() {
        return generate_region_forged(world, coords, mats);
    }
    let (ox, oz, span) = patch_window(coords.x * REGION_BLOCKS, coords.z * REGION_BLOCKS, REGION_BLOCKS);
    let grid = TerrainPipeline::new(seed).profile_grid(ox, oz, span);
    region_from_patch(&ProceduralPatch(grid), coords, mats)
}

/// Patch origin and span (meters) for the window of `span` blocks at world
/// block `(x, z)`, margin included.
fn patch_window(x: i32, z: i32, span: i32) -> (f64, f64, f64) {
    ((x - PATCH_MARGIN) as f64, (z - PATCH_MARGIN) as f64, (span + 2 * PATCH_MARGIN) as f64)
}

/// One cell of `generate_region`: the `cell_chunks²` columns of `cell`
/// (region-local `ColumnCoords`, same order), bit-identical to that slice of
/// the whole-region build — the lattice is world-aligned and the cell edge is
/// a multiple of it. Cell-mode regions generate lazily, one cell at a time
/// (docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md §3).
pub fn generate_cell(
    coords: RegionCoords,
    cell: game::CellCoords,
    cell_chunks: usize,
    seed: u64,
    mats: Materials,
) -> Vec<(ColumnCoords, Column)> {
    let (x0, z0) = (cell.x as usize * cell_chunks, cell.z as usize * cell_chunks);
    let (ox, oz, span) = patch_window(
        coords.x * REGION_BLOCKS + (x0 * CHUNK_BLOCKS) as i32,
        coords.z * REGION_BLOCKS + (z0 * CHUNK_BLOCKS) as i32,
        (cell_chunks * CHUNK_BLOCKS) as i32,
    );
    let xs = x0..x0 + cell_chunks;
    let zs = z0..z0 + cell_chunks;
    if let Some(world) = forged::active() {
        return columns_from_patch(&world.patch(ox, oz, span), coords, xs, zs, mats);
    }
    let grid = TerrainPipeline::new(seed).profile_grid(ox, oz, span);
    columns_from_patch(&ProceduralPatch(grid), coords, xs, zs, mats)
}

/// `generate_region` for an explicit forged world (tests, tools).
pub fn generate_region_forged(
    world: &forged::ForgedWorld,
    coords: RegionCoords,
    mats: Materials,
) -> Vec<(ColumnCoords, Column)> {
    let (ox, oz, span) = patch_window(coords.x * REGION_BLOCKS, coords.z * REGION_BLOCKS, REGION_BLOCKS);
    region_from_patch(&world.patch(ox, oz, span), coords, mats)
}

/// Material of a solid block: sand under standing water (lake and river
/// beds, like the sea floor), else the depth/height rule.
fn solid_material(mats: &Materials, depth: i32, by: i32, surface: i32, water_top: i32) -> BlockId {
    if depth <= MAX_SOIL_DEPTH && surface < water_top && water_top > SEA_TOP_BLOCK {
        mats.sand
    } else {
        mats.material_at(depth, by)
    }
}

fn region_from_patch(patch: &dyn Patch, coords: RegionCoords, mats: Materials) -> Vec<(ColumnCoords, Column)> {
    columns_from_patch(patch, coords, 0..REGION_CHUNKS, 0..REGION_CHUNKS, mats)
}

/// The columns `xs × zs` (region-local chunk columns) of region `coords`,
/// sampled from `patch` — which must cover them plus `PATCH_MARGIN`.
/// `region_from_patch` is the whole range; `generate_cell` one cell's.
///
/// Every column carries a `ColumnGround` recipe for the ground below its band
/// (the sim reads missing chunks under the band from it), and the band's
/// solids deeper than soil are that recipe's blocks, so band and implied
/// ground never disagree. The band reaches down far enough that no air or
/// water block faces implied ground: past the footprint's deepest soil, and
/// to the lowest open block of every face-neighbouring block column (the
/// cliff rule — a cliff's face is materialised, not implied).
fn columns_from_patch(
    patch: &dyn Patch,
    coords: RegionCoords,
    xs: std::ops::Range<usize>,
    zs: std::ops::Range<usize>,
    mats: Materials,
) -> Vec<(ColumnCoords, Column)> {
    let cb = CHUNK_BLOCKS as i32;
    let air = Block::new(BlockId::AIR);
    let stone = Block::new(mats.stone);

    // Per-block-column surface block, whether it is whole (else a half block)
    // and water top, over the window plus one block on every side (the
    // cliff rule's neighbours). Index `[bx + 1][bz + 1]`, window-local blocks.
    let (wx, wz) = (coords.x * REGION_BLOCKS + xs.start as i32 * cb, coords.z * REGION_BLOCKS + zs.start as i32 * cb);
    let (nx, nz) = (xs.len() * CHUNK_BLOCKS + 2, zs.len() * CHUNK_BLOCKS + 2);
    let mut samples = vec![(0i32, true, 0i32); nx * nz];
    for i in 0..nx {
        for j in 0..nz {
            let (bx, bz) = (wx + i as i32 - 1, wz + j as i32 - 1);
            let (prof, water_top) = patch.sample(block_center_m(bx), block_center_m(bz));
            let s = surface_from_profile(&prof);
            debug_assert!(s >= SCAN_BOTTOM, "surface below the scan at block ({bx}, {bz})");
            // Under water the surface block is always whole: a half block's
            // empty upper half would be an air pocket.
            let full = surface_is_full(&prof, s) || s <= water_top;
            samples[i * nz + j] = (s, full, water_top);
        }
    }
    let at = |bx: i32, bz: i32| samples[(bx + 1) as usize * nz + (bz + 1) as usize];

    let mut columns = Vec::with_capacity(xs.len() * zs.len());
    for cx in xs.clone() {
        for cz in zs.clone() {
            // This Column covers CHUNK_BLOCKS x CHUNK_BLOCKS blocks, from
            // window-local block (fx, fz).
            let fx = (cx - xs.start) as i32 * cb;
            let fz = (cz - zs.start) as i32 * cb;

            let mut lo = SCAN_TOP;
            let mut hi = SCAN_BOTTOM - 1;
            let mut open_lo = SCAN_TOP;
            for lx in 0..cb {
                for lz in 0..cb {
                    let (s, _, water_top) = at(fx + lx, fz + lz);
                    lo = lo.min(s);
                    hi = hi.max(s.max(water_top)); // include the water column up to its surface
                    // Cliff rule: the lowest block a face neighbour leaves open
                    // (its half block's empty upper half included).
                    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                        let (ns, nfull, _) = at(fx + lx + dx, fz + lz + dz);
                        open_lo = open_lo.min(if nfull { ns + 1 } else { ns });
                    }
                }
            }
            // Extend the band bottom past the shallowest surface in the footprint
            // so the deepest soil (dirt/sand) of the lowest column is still
            // materialized, and down to every open neighbour block.
            lo = (lo - MAX_SOIL_DEPTH).min(open_lo);

            // Materialize whole sub-chunks [base_sub_y ..= top_sub_y]; below is
            // the ground recipe, above air. Whole blocks, and a half-block
            // chisel (one cuboid) where the surface rounds to half a block.
            let base_sub_y = lo.div_euclid(cb);
            let top_sub_y = hi.div_euclid(cb);
            let n_sub = (top_sub_y - base_sub_y + 1) as usize;
            let mut chunks: Vec<Chunk> = (0..n_sub).map(|_| Chunk::default()).collect();

            let mut surface = [0i16; CHUNK_BLOCKS * CHUNK_BLOCKS];
            for lx in 0..CHUNK_BLOCKS {
                for lz in 0..CHUNK_BLOCKS {
                    surface[lx * CHUNK_BLOCKS + lz] = at(fx + lx as i32, fz + lz as i32).0 as i16;
                }
            }
            let ground = game::ColumnGround {
                surface,
                geo: [0; CHUNK_BLOCKS * CHUNK_BLOCKS],
                strata: vec![(i16::MAX, mats.stone)],
                bedrock: mats.bedrock,
                band_base_sub_y: base_sub_y,
            };

            for lx in 0..CHUNK_BLOCKS {
                for lz in 0..CHUNK_BLOCKS {
                    let (surface, full, water_top) = at(fx + lx as i32, fz + lz as i32);
                    for by in base_sub_y * cb..=hi {
                        let sub_i = (by.div_euclid(cb) - base_sub_y) as usize;
                        let local_by = by.rem_euclid(cb) as usize;
                        let bi = BlockIndex::from_xyz(lx, local_by, lz);
                        let chunk = &mut chunks[sub_i];
                        let depth = surface - by;
                        if depth > MAX_SOIL_DEPTH {
                            // Below the soil: the recipe's rock (or bedrock).
                            chunk.blocks[bi.0 as usize] = ground.block_at(lx, lz, by);
                        } else if by <= surface {
                            // Solid: whole blocks, topped by a whole or half block.
                            let mat = solid_material(&mats, depth, by, surface, water_top);
                            chunk.blocks[bi.0 as usize] = Block::new(mat);
                            if by == surface && !full {
                                let top = [BLOCK_VOXELS, HALF_BLOCK_VOXELS, BLOCK_VOXELS];
                                chunk.chisel.insert(bi, ChiselData::cuboid(mat, [0, 0, 0], top));
                            }
                        } else if by <= water_top {
                            // Above ground, below the water surface -> water.
                            chunk.blocks[bi.0 as usize] = Block::new(mats.water);
                        }
                        // else: air (Chunk::default leaves it air).
                    }
                }
            }

            let subs: Vec<SubChunk> = chunks.into_iter().map(|c| SubChunk::Dense(Box::new(c))).collect();
            // `above = air` trims air tops; the bottom is kept (`grounded`).
            let column = Column::grounded(base_sub_y, stone, air, subs, ground);
            columns.push((ColumnCoords::new(cx, cz), column));
        }
    }
    columns
}

/// Cheap coarse LOD generation for one block-scale-or-coarser key
/// (`game::lod::COARSE_MIN_FACTOR` and up): one profile sample per cell
/// column, surface scan, then solid-below-surface + water to sea level. No
/// block materialization, no chiseling. Pure and deterministic: same (key,
/// seed, mats) → identical cells everywhere. A 64/256 tile is exactly one 32³
/// grid per vertical band; a factor-16 region is 2×2 of them, grouped like
/// `game::lod::block_scale_cells`.
/// Point-sampling is NOT bit-consistent with downsampled full generation
/// (accepted, sub-pixel at these distances — see the design spec §3);
/// `coarse_tile_tracks_downsampled_full_generation` bounds the divergence.
pub fn coarse_tile(
    key: &game::lod::LodKey,
    seed: u64,
    mats: Materials,
) -> Vec<game::lod::LodCell> {
    if let Some(world) = forged::active() {
        return coarse_tile_forged(world, key, mats);
    }
    let (ox, oz, span_blocks) = coarse_window(key);
    let grid = TerrainPipeline::new(seed).profile_grid(ox as f64, oz as f64, span_blocks as f64);
    coarse_from_patch(&ProceduralPatch(grid), key, mats)
}

/// `coarse_tile` for an explicit forged world (tests, tools). A coarse cell is
/// point-sampled at its center, so rivers narrower than a cell come and go
/// with distance; lakes and valleys survive every ring.
pub fn coarse_tile_forged(
    world: &forged::ForgedWorld,
    key: &game::lod::LodKey,
    mats: Materials,
) -> Vec<game::lod::LodCell> {
    let (ox, oz, span_blocks) = coarse_window(key);
    coarse_from_patch(&world.patch(ox as f64, oz as f64, span_blocks as f64), key, mats)
}

/// World block origin and span of a coarse key.
fn coarse_window(key: &game::lod::LodKey) -> (i32, i32, i32) {
    let origin = key.origin_region();
    (origin.x * REGION_BLOCKS, origin.z * REGION_BLOCKS, key.region_span() * REGION_BLOCKS)
}

fn coarse_from_patch(patch: &dyn Patch, key: &game::lod::LodKey, mats: Materials) -> Vec<game::lod::LodCell> {
    use game::lod::LodCell;
    use game::Voxel;

    // Blocks per cell edge: 1 at factor 16, 2 at 32, 4 at 64, 16 at 256.
    debug_assert!(key.factor >= game::lod::COARSE_MIN_FACTOR, "sub-block factor {}", key.factor);
    let cb = (key.factor / BLOCK_VOXELS as u32) as i32;
    let span_blocks = key.region_span() * REGION_BLOCKS;
    let cells = span_blocks / cb; // cell columns per key edge: 64 at factor 16, else 32
    debug_assert_eq!(cells % 32, 0, "a key is a whole number of 32³ grids");

    let (ox, oz, _) = coarse_window(key);

    let mut grids: std::collections::BTreeMap<[i32; 3], Vec<Voxel>> = Default::default();
    for cz in 0..cells {
        for cx in 0..cells {
            let mx = (ox + cx * cb) as f64 + cb as f64 * 0.5;
            let mz = (oz + cz * cb) as f64 + cb as f64 * 0.5;
            let (prof, water_top) = patch.sample(mx, mz);
            let surface = surface_from_profile(&prof);

            let solid_lo = (surface - MAX_SOIL_DEPTH).div_euclid(cb);
            let solid_hi = surface.div_euclid(cb);
            let top_cell = surface.max(water_top).div_euclid(cb);
            for cy in solid_lo..=top_cell {
                let is_solid = cy <= solid_hi;
                // Water only where the cell's bottom block is at/below the surface.
                if !is_solid && cy * cb > water_top {
                    continue;
                }
                let mat = if is_solid {
                    // The cell's topmost block of terrain — what shows from
                    // above, matching `game::lod::merge_group`.
                    let top = surface.min(cy * cb + cb - 1);
                    solid_material(&mats, surface - top, top, surface, water_top)
                } else {
                    mats.water
                };
                let group = [cx.div_euclid(32), cy.div_euclid(32), cz.div_euclid(32)];
                let (lx, ly, lz) = ((cx % 32) as usize, cy.rem_euclid(32) as usize, (cz % 32) as usize);
                let g = grids
                    .entry(group)
                    .or_insert_with(|| vec![Voxel::default(); CHUNK_VOXEL_COUNT]);
                g[lx + 32 * ly + 32 * 32 * lz] = Voxel::new(mat);
            }
        }
    }
    grids
        .into_iter()
        .map(|(group, grid)| LodCell { group, grid })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{BlockId, Column, ColumnCoords, ReadVol, BlockPos, SubChunk, REGION_CHUNKS};

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn a_cell_generated_alone_matches_the_whole_region_build() {
        let rc = RegionCoords::new(1, -2);
        let cc = 8;
        let whole = generate_region(rc, WORLD_SEED, mats());
        let n = (REGION_CHUNKS / cc) as u32;
        for x in 0..n {
            for z in 0..n {
                let cell = game::CellCoords::new(x, z);
                let alone = generate_cell(rc, cell, cc, WORLD_SEED, mats());
                let slice: Vec<_> = whole
                    .iter()
                    .filter(|(c, _)| game::cell_of_chunk(game::ChunkCoords::new(c.x, 0, c.z), cc) == cell)
                    .collect();
                assert_eq!(alone.len(), cc * cc, "cell {cell:?} column count");
                assert_eq!(
                    bincode::serialize(&alone).unwrap(),
                    bincode::serialize(&slice).unwrap(),
                    "cell {cell:?} must be bit-identical to the region build"
                );
            }
        }
    }

    fn mats() -> crate::material::Materials {
        crate::material::Materials {
            stone: BlockId(2),
            dirt: BlockId(1),
            grass: BlockId(3),
            sand: BlockId(4),
            water: BlockId(5),
            bedrock: game::BlockId(6),
            rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
        }
    }

    fn crc(cols: &[(ColumnCoords, Column)]) -> u32 {
        let mut h = crc32fast::Hasher::new();
        h.update(&bincode::serialize(cols).unwrap());
        h.finalize()
    }

    #[test]
    fn produces_sixty_four_columns() {
        let cols = generate_region(RegionCoords::new(0, 0), 1, mats());
        assert_eq!(cols.len(), REGION_CHUNKS * REGION_CHUNKS);
    }

    #[test]
    fn deterministic_same_seed() {
        let a = generate_region(RegionCoords::new(-3, 7), 99, mats());
        let b = generate_region(RegionCoords::new(-3, 7), 99, mats());
        assert_eq!(crc(&a), crc(&b));
    }

    #[test]
    fn different_seed_differs() {
        let a = generate_region(RegionCoords::new(0, 0), 1, mats());
        let b = generate_region(RegionCoords::new(0, 0), 2, mats());
        assert_ne!(crc(&a), crc(&b));
    }

    /// Worldgen emits whole blocks and half blocks only: every chisel is the
    /// one lower-half cuboid of its block's own material, there are some (so
    /// the half-block rounding actually ran and wrote real terrain, not just
    /// the `below = stone` sentinel), and none is under water, where its
    /// empty upper half would be an air pocket.
    #[test]
    fn terrain_is_whole_blocks_and_dry_half_blocks_only() {
        // Coast north-east of spawn: land, beach and open sea.
        let rc = RegionCoords::from_world(5632.0, -2560.0);
        let cols = generate_region(rc, WORLD_SEED, mats());
        let mut halves = 0;
        for (_, col) in &cols {
            for (sub_y, sub) in (col.base_sub_y()..).zip(col.sub_chunks()) {
                let SubChunk::Dense(chunk) = sub else { continue };
                for (bi, chisel) in &chunk.chisel {
                    let (bx, by, bz) = bi.xyz();
                    let mat = chunk.block_at(bx, by, bz).id;
                    let cuboids: Vec<_> = chisel.cuboids().collect();
                    assert_eq!(cuboids, vec![([0, 0, 0], [16, 8, 16], mat)], "not a half block");
                    halves += 1;
                    let y = sub_y * CHUNK_BLOCKS as i32 + by as i32;
                    let above = col.get(BlockPos::new(bx as i32, y + 1, bz as i32));
                    assert_ne!(above.id, mats().water, "half block under water at block y {y}");
                }
            }
        }
        assert!(halves > 0, "expected half-block steps on the coast");
    }

    #[test]
    fn columns_contain_solid_ground() {
        let cols = generate_region(RegionCoords::new(5, 5), 3, mats());

        // Sanity: `get()` still reads solid ground somewhere too.
        let any_solid = cols.iter().any(|(_, col)| {
            (0..64).any(|y| col.get(BlockPos::new(0, y, 0)).id != BlockId::AIR)
        });
        assert!(any_solid, "expected some solid ground in the region");
    }

    #[test]
    fn coarse_tile_is_deterministic_and_key_local() {
        let key = game::lod::LodKey { factor: 64, coords: [0, 0] };
        // One grid per band when the factor-64 grid covers the whole key;
        // several (2x2 at REGION_CHUNKS = 128) when a region is bigger than it.
        let grids_per_axis = ((game::REGION_SIZE as i32) / (32 * 64)).max(1);
        let a = coarse_tile(&key, WORLD_SEED, mats());
        let b = coarse_tile(&key, WORLD_SEED, mats());
        assert_eq!(a.len(), b.len());
        for (ca, cb) in a.iter().zip(&b) {
            assert_eq!(ca.group, cb.group);
            assert_eq!(ca.grid, cb.grid);
            assert!(
                (0..grids_per_axis).contains(&ca.group[0]) && (0..grids_per_axis).contains(&ca.group[2]),
                "key-local groups: {:?}",
                ca.group
            );
        }
        assert!(!a.is_empty(), "sea-level world always materializes at least one band");
        // Sea level (block 64) must be covered: some cell is water or solid.
        assert!(a.iter().any(|c| c.grid.iter().any(|v| !v.is_air())));
    }

    /// Coarse point-sampling vs exact generate+downsample: per cell column the
    /// highest solid cell must match within one cell for the vast majority of
    /// columns. This bounds the accepted §3 inexactness; tighten the threshold
    /// only with measurement.
    #[test]
    fn sea_surface_sits_on_every_lod_lattice() {
        let surface_voxels = (SEA_TOP_BLOCK + 1) as u32 * BLOCK_VOXELS as u32;
        for factor in game::lod::LOD_FACTORS {
            assert_eq!(surface_voxels % factor, 0, "factor {factor} would round the sea up");
        }
    }

    /// The sea is one flat plane in every ring: coarse sampling puts its top
    /// cell boundary exactly on the real water surface at every coarse factor.
    #[test]
    fn coarse_sea_surface_is_exact_at_every_factor() {
        let surface_voxels = (SEA_TOP_BLOCK + 1) * BLOCK_VOXELS as i32;
        // Coast north-east of spawn: grass, beach and open sea. Named by WORLD position
        // — region (5, -3) at the default size — so it is the same coast at any
        // `REGION_CHUNKS`.
        let rc = RegionCoords::from_world(5632.0, -2560.0);
        for factor in [16u32, 32, 64, 256] {
            let key = game::lod::LodKey::for_region(factor, rc);
            let mut water_columns = 0;
            for cell in coarse_tile(&key, WORLD_SEED, mats()) {
                for z in 0..32usize {
                    for x in 0..32usize {
                        let top = (0..32usize)
                            .rev()
                            .find(|&y| !cell.grid[x + 32 * y + 32 * 32 * z].is_air());
                        let Some(y) = top else { continue };
                        if cell.grid[x + 32 * y + 32 * 32 * z].block != mats().water {
                            continue;
                        }
                        water_columns += 1;
                        let top_voxels = (cell.group[1] * 32 + y as i32 + 1) * factor as i32;
                        assert_eq!(top_voxels, surface_voxels, "factor {factor}");
                    }
                }
            }
            assert!(water_columns > 0, "factor {factor}: the coast key has open water");
        }
    }

    /// The region-keyed coarse factors replace full generation for pristine
    /// terrain, so they must reproduce it, not merely resemble it: same
    /// surface cell and same visible material in nearly every column.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn region_coarse_factors_match_downsampled_full_generation() {
        let rc = RegionCoords::from_world(5632.0, -2560.0); // coast north-east of spawn (region (5, -3) at the default size): grass, beach and open sea
        let started = std::time::Instant::now();
        let chunks: Vec<(game::ChunkCoords, game::Chunk)> = generate_region(rc, WORLD_SEED, mats())
            .into_iter()
            .flat_map(|(cc, col)| {
                col.materialized_sub_chunks()
                    .into_iter()
                    .map(move |(sub_y, ch)| (game::ChunkCoords::new(cc.x, sub_y, cc.z), ch))
            })
            .collect();
        println!("generate_region: {:?}", started.elapsed());

        // Topmost solid cell (absolute cell y, block id) per cell column.
        let tops = |cells: &[game::lod::LodCell], per: usize| {
            let mut out = std::collections::BTreeMap::new();
            for c in cells {
                for z in 0..32usize {
                    for x in 0..32usize {
                        let col = (c.group[0] as usize * 32 + x, c.group[2] as usize * 32 + z);
                        if col.0 >= per || col.1 >= per {
                            continue;
                        }
                        for y in (0..32usize).rev() {
                            let v = c.grid[x + 32 * y + 32 * 32 * z];
                            if !v.is_air() {
                                let top = (c.group[1] * 32 + y as i32, v.block);
                                let e = out.entry(col).or_insert(top);
                                *e = (*e).max(top);
                                break;
                            }
                        }
                    }
                }
            }
            out
        };

        for factor in [16u32, 32] {
            let key = game::lod::LodKey::for_region(factor, rc);
            let started = std::time::Instant::now();
            let coarse = coarse_tile(&key, WORLD_SEED, mats());
            println!("coarse_tile factor {factor}: {:?}", started.elapsed());
            let exact = game::lod::downsample_chunks(chunks.clone(), factor);

            let per = game::lod::cells_per_region(factor);
            let (a, b) = (tops(&coarse, per), tops(&exact, per));
            assert_eq!(a.len(), per * per, "factor {factor}: every column has terrain or sea");
            let (mut height_hits, mut material_hits) = (0, 0);
            for (col, (ay, amat)) in &a {
                let (by, bmat) = b[col];
                height_hits += (*ay == by) as usize;
                material_hits += (*ay == by && *amat == bmat) as usize;
            }
            println!("factor {factor}: height {height_hits}/{}, material {material_hits}", a.len());
            assert!(height_hits * 100 >= a.len() * 95, "factor {factor}: surface cell matches");
            assert!(material_hits * 100 >= a.len() * 90, "factor {factor}: visible material matches");
        }
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn coarse_tile_tracks_downsampled_full_generation() {
        // A tile spanning several regions: factor 64 (2x2) at the default
        // region size, 256 once a factor-64 grid is one region or less.
        let factor = if game::lod::tile_regions(64) > 1 { 64 } else { 256 };
        if game::lod::tile_regions(factor) <= 1 {
            return; // large cell-mode regions: no tile spans several regions
        }
        let key = game::lod::LodKey { factor, coords: [0, 0] };
        let coarse = coarse_tile(&key, WORLD_SEED, mats());

        // Exact: generate each covered region, downsample to 64, overlay into a tile.
        let mut exact: std::collections::BTreeMap<[i32; 3], Vec<game::Voxel>> = Default::default();
        for rc in key.regions() {
            let columns = generate_region(rc, WORLD_SEED, mats());
            let chunks: Vec<(game::ChunkCoords, game::Chunk)> = columns
                .into_iter()
                .flat_map(|(cc, col)| {
                    col.materialized_sub_chunks()
                        .into_iter()
                        .map(move |(sub_y, ch)| (game::ChunkCoords::new(cc.x, sub_y, cc.z), ch))
                })
                .collect();
            let cells = game::lod::downsample_chunks(chunks, key.factor);
            game::lod::copy_region_cells_into_tile(&mut exact, &cells, rc, &key);
        }

        let top_of = |grids: &std::collections::BTreeMap<[i32; 3], Vec<game::Voxel>>,
                      x: usize,
                      z: usize| {
            grids
                .iter()
                .flat_map(|(g, grid)| {
                    (0..32).rev().find_map(|y| {
                        (!grid[x + 32 * y + 32 * 32 * z].is_air())
                            .then(|| g[1] * 32 + y as i32)
                    })
                })
                .max()
        };
        let coarse_map: std::collections::BTreeMap<[i32; 3], Vec<game::Voxel>> =
            coarse.iter().map(|c| (c.group, c.grid.clone())).collect();

        let (mut hits, mut total) = (0, 0);
        for x in 0..32 {
            for z in 0..32 {
                let (a, b) = (top_of(&coarse_map, x, z), top_of(&exact, x, z));
                if let (Some(a), Some(b)) = (a, b) {
                    total += 1;
                    if (a - b).abs() <= 1 {
                        hits += 1;
                    }
                }
            }
        }
        assert!(total > 200, "most columns have terrain (total={total})");
        assert!(
            hits * 10 >= total * 9,
            "coarse surface within ±1 cell for >=90% of columns ({hits}/{total})"
        );
    }
}
