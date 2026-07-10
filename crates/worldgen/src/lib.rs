//! Deterministic world generation. `generate_region` is a pure function of
//! region coordinates: same coords → identical output on every machine,
//! which is what makes "cycle out = park, cycle in = restore-or-regenerate"
//! safe for the multi-region world.

mod noise;
mod spline;
mod density;
pub mod pipeline;
pub mod material;

/// The world seed shared by every `generate_region` caller (server + offline
/// client) so they produce the SAME world. Kept in one place so the server and
/// client can never silently drift to different seeds.
// Picked by examples/seed_scan.rs: dry flat spawn (y=70, sea=64) and a
// mostly-land ±256 m neighborhood. Re-run the scan if terrain shaping changes.
pub const WORLD_SEED: u64 = 0x00D5_C0DE_8D78;

use game::{
    Block, BlockId, BlockIndex, ChiselData, Chunk, Column, ColumnCoords, RegionCoords, SubChunk,
    BLOCK_VOXELS, CHUNK_BLOCKS, CHUNK_VOXEL_COUNT, REGION_CHUNKS,
};

use crate::material::Materials;
use crate::pipeline::{ProfileGrid, TerrainPipeline, SEA_LEVEL};

// Region span in blocks: REGION_CHUNKS chunks * CHUNK_BLOCKS blocks/chunk.
const REGION_BLOCKS: i32 = (REGION_CHUNKS * CHUNK_BLOCKS) as i32; // 64
// Coarse vertical scan bounds (blocks). Terrain lives well inside this band.
const SCAN_TOP: i32 = 320;
const SCAN_BOTTOM: i32 = -64;
// Deepest non-stone material_at depth (dirt/sand band). Keep in sync with
// material.rs's `depth_below_surface <= 3` bands.
const MAX_SOIL_DEPTH: i32 = 3;

/// Meter coordinate of a block center at world block index `b`.
fn block_center_m(b: i32) -> f64 {
    b as f64 + 0.5
}

/// Highest solid block Y for a precomputed profile, or `SCAN_BOTTOM - 1`
/// if the whole column is air. Shared by full-res and coarse generation.
fn surface_from_profile(prof: &crate::pipeline::ColumnProfile) -> i32 {
    let mut by = SCAN_TOP;
    while by >= SCAN_BOTTOM {
        if prof.density_at(block_center_m(by)) > 0.0 {
            return by;
        }
        by -= 1;
    }
    SCAN_BOTTOM - 1
}

/// Highest solid block Y in a 1 m column (world block coords), or `SCAN_BOTTOM - 1`
/// if the whole column is air. Coarse per-block scan (design §4 step 1).
fn surface_block_y(grid: &ProfileGrid, wx: i32, wz: i32) -> i32 {
    surface_from_profile(&grid.profile_at(block_center_m(wx), block_center_m(wz)))
}

/// The full REGION_CHUNKS x REGION_CHUNKS column grid for one region. Pure and deterministic.
pub fn generate_region(coords: RegionCoords, seed: u64, mats: Materials) -> Vec<(ColumnCoords, Column)> {
    let p = TerrainPipeline::new(seed);
    let region_ox = coords.x * REGION_BLOCKS; // world block origin X
    let region_oz = coords.z * REGION_BLOCKS;
    let grid = p.profile_grid(region_ox as f64, region_oz as f64, REGION_BLOCKS as f64);
    let sea = SEA_LEVEL as i32;
    let air = Block::new(BlockId::AIR);
    let stone = Block::new(mats.stone);

    let mut columns = Vec::with_capacity(REGION_CHUNKS * REGION_CHUNKS);
    for cx in 0..REGION_CHUNKS {
        for cz in 0..REGION_CHUNKS {
            // This Column covers CHUNK_BLOCKS x CHUNK_BLOCKS blocks.
            let col_ox = region_ox + (cx * CHUNK_BLOCKS) as i32;
            let col_oz = region_oz + (cz * CHUNK_BLOCKS) as i32;

            // Per-block-column surface heights within this footprint.
            let mut surfaces = [[SCAN_BOTTOM - 1; CHUNK_BLOCKS]; CHUNK_BLOCKS];
            let mut lo = SCAN_TOP;
            let mut hi = SCAN_BOTTOM - 1;
            for lx in 0..CHUNK_BLOCKS {
                for lz in 0..CHUNK_BLOCKS {
                    let s = surface_block_y(&grid, col_ox + lx as i32, col_oz + lz as i32);
                    surfaces[lx][lz] = s;
                    lo = lo.min(s);
                    hi = hi.max(s.max(sea)); // include the water column up to sea level
                }
            }
            // Extend the band bottom past the shallowest surface in the footprint
            // so the deepest soil (dirt/sand) of the lowest column is still
            // materialized, rather than falling below the band and reading as
            // the `below = stone` sentinel.
            lo -= MAX_SOIL_DEPTH;

            // Materialize block Ys [lo ..= hi]; below reads as stone sentinel,
            // above as air. Build per-block, chiseling the surface straddle.
            let base_sub_y = lo.div_euclid(CHUNK_BLOCKS as i32);
            let top_sub_y = hi.div_euclid(CHUNK_BLOCKS as i32);
            let n_sub = (top_sub_y - base_sub_y + 1) as usize;
            let mut chunks: Vec<Chunk> = (0..n_sub).map(|_| Chunk::default()).collect();

            for lx in 0..CHUNK_BLOCKS {
                for lz in 0..CHUNK_BLOCKS {
                    let wx = col_ox + lx as i32;
                    let wz = col_oz + lz as i32;
                    let surface = surfaces[lx][lz];
                    for by in lo..=hi {
                        let sub_i = (by.div_euclid(CHUNK_BLOCKS as i32) - base_sub_y) as usize;
                        let local_by = by.rem_euclid(CHUNK_BLOCKS as i32) as usize;
                        let bi = BlockIndex::from_xyz(lx, local_by, lz);
                        let chunk = &mut chunks[sub_i];
                        if by < surface {
                            // Solid interior/subsurface.
                            let depth = surface - by;
                            chunk.blocks[bi.0 as usize] = Block::new(mats.material_at(depth, by));
                        } else if by == surface {
                            // Surface straddle: chisel at voxel resolution (design §4 step 2).
                            let top_mat = mats.material_at(0, by);
                            place_straddle(chunk, bi, wx, by, wz, &grid, top_mat);
                        } else if by <= sea {
                            // Above ground but below sea level -> water.
                            chunk.blocks[bi.0 as usize] = Block::new(mats.water);
                        }
                        // else: air (Chunk::default leaves it air).
                    }
                }
            }

            let subs: Vec<SubChunk> = chunks.into_iter().map(|c| SubChunk::Dense(Box::new(c))).collect();
            // `below = stone` so normalize() trims full-stone bottom slices to the
            // sentinel; `above = air` trims air tops.
            let column = Column::new(base_sub_y, stone, air, subs);
            columns.push((ColumnCoords::new(cx, cz), column));
        }
    }
    columns
}

/// Fill one block at voxel resolution from the density field: solid voxels below
/// the local surface become `top_mat`, the rest air. Produces a chiseled block.
fn place_straddle(
    chunk: &mut Chunk,
    bi: BlockIndex,
    wx: i32,
    by: i32,
    wz: i32,
    grid: &ProfileGrid,
    top_mat: BlockId,
) {
    let mut chisel = ChiselData::new(vec![top_mat]);
    let mut any = false;
    for vx in 0..BLOCK_VOXELS {
        for vz in 0..BLOCK_VOXELS {
            // Meter coord of this voxel center: block index + (voxel + 0.5)/16.
            let mx = wx as f64 + (vx as f64 + 0.5) / BLOCK_VOXELS as f64;
            let mz = wz as f64 + (vz as f64 + 0.5) / BLOCK_VOXELS as f64;
            let prof = grid.profile_at(mx, mz);
            for vy in 0..BLOCK_VOXELS {
                let my = by as f64 + (vy as f64 + 0.5) / BLOCK_VOXELS as f64;
                let solid = prof.density_at(my) > 0.0;
                chisel.set(vx, vy, vz, solid, 0);
                any |= solid;
            }
        }
    }
    if any {
        // A chiseled block carries a non-air `Block` id + the chisel mask.
        chunk.blocks[bi.0 as usize] = Block::new(top_mat);
        chunk.chisel.insert(bi, chisel);
    }
}

/// Cheap coarse LOD generation for one 64/256 tile: one profile sample per
/// cell column (32×32), surface scan, then solid-below-surface + water to
/// sea level. No block materialization, no chiseling. Pure and
/// deterministic: same (key, seed, mats) → identical cells everywhere.
/// Point-sampling is NOT bit-consistent with downsampled full generation
/// (accepted, sub-pixel at these distances — see the design spec §3);
/// `coarse_tile_tracks_downsampled_full_generation` bounds the divergence.
pub fn coarse_tile(
    key: &game::lod::LodKey,
    seed: u64,
    mats: Materials,
) -> Vec<game::lod::LodCell> {
    use game::lod::LodCell;
    use game::Voxel;

    // Blocks per cell edge: 4 at factor 64, 16 at factor 256.
    let cb = (key.factor / BLOCK_VOXELS as u32) as i32;
    let span_blocks = key.region_span() * REGION_BLOCKS;
    debug_assert_eq!(span_blocks, 32 * cb, "a tile is exactly one 32³ grid");

    let p = TerrainPipeline::new(seed);
    let origin = key.origin_region();
    let ox = origin.x * REGION_BLOCKS;
    let oz = origin.z * REGION_BLOCKS;
    let grid = p.profile_grid(ox as f64, oz as f64, span_blocks as f64);
    let sea = SEA_LEVEL as i32;

    let mut bands: std::collections::BTreeMap<i32, Vec<Voxel>> = Default::default();
    for cz in 0..32i32 {
        for cx in 0..32i32 {
            let mx = (ox + cx * cb) as f64 + cb as f64 * 0.5;
            let mz = (oz + cz * cb) as f64 + cb as f64 * 0.5;
            let prof = grid.profile_at(mx, mz);
            let surface = surface_from_profile(&prof);

            let solid_lo = (surface - MAX_SOIL_DEPTH).div_euclid(cb);
            let solid_hi = surface.div_euclid(cb);
            let top_cell = surface.max(sea).div_euclid(cb);
            for cy in solid_lo..=top_cell {
                let is_solid = cy <= solid_hi;
                // Water only where the cell's bottom block is at/below sea.
                if !is_solid && cy * cb > sea {
                    continue;
                }
                let mat = if is_solid {
                    let mid = cy * cb + cb / 2;
                    mats.material_at((surface - mid).max(0), mid)
                } else {
                    mats.water
                };
                let band = cy.div_euclid(32);
                let ly = cy.rem_euclid(32) as usize;
                let g = bands
                    .entry(band)
                    .or_insert_with(|| vec![Voxel::default(); CHUNK_VOXEL_COUNT]);
                g[cx as usize + 32 * ly + 32 * 32 * cz as usize] = Voxel::new(mat);
            }
        }
    }
    bands
        .into_iter()
        .map(|(band, grid)| LodCell { group: [0, band, 0], grid })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{BlockId, Column, ColumnCoords, ReadVol, BlockPos, SubChunk, REGION_CHUNKS};

    fn mats() -> crate::material::Materials {
        crate::material::Materials {
            stone: BlockId(2),
            dirt: BlockId(1),
            grass: BlockId(3),
            sand: BlockId(4),
            water: BlockId(5),
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

    #[test]
    fn columns_contain_solid_ground() {
        // At least one column has a materialized (chiseled) surface straddle
        // block, proving the surface-find/straddle path actually ran and
        // wrote real terrain — not just the `below = stone` sentinel, which
        // a naive `get()` probe can't distinguish from generated ground.
        let cols = generate_region(RegionCoords::new(5, 5), 3, mats());
        let any_chiseled = cols.iter().any(|(_, col)| {
            col.sub_chunks().iter().any(|sub| match sub {
                SubChunk::Dense(chunk) => !chunk.chisel.is_empty(),
                SubChunk::Air => false,
            })
        });
        assert!(any_chiseled, "expected a materialized surface straddle in the region");

        // Sanity: `get()` still reads solid ground somewhere too.
        let any_solid = cols.iter().any(|(_, col)| {
            (0..64).any(|y| col.get(BlockPos::new(0, y, 0)).id != BlockId::AIR)
        });
        assert!(any_solid, "expected some solid ground in the region");
    }

    #[test]
    fn coarse_tile_is_deterministic_and_key_local() {
        let key = game::lod::LodKey { factor: 64, coords: [0, 0] };
        let a = coarse_tile(&key, WORLD_SEED, mats());
        let b = coarse_tile(&key, WORLD_SEED, mats());
        assert_eq!(a.len(), b.len());
        for (ca, cb) in a.iter().zip(&b) {
            assert_eq!(ca.group, cb.group);
            assert_eq!(ca.grid, cb.grid);
            assert_eq!((ca.group[0], ca.group[2]), (0, 0), "tile-local groups");
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
    #[ignore = "slow: >100ms — run via just test-full"]
    fn coarse_tile_tracks_downsampled_full_generation() {
        let key = game::lod::LodKey { factor: 64, coords: [0, 0] };
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
