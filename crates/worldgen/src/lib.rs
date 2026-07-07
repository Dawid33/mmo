//! Deterministic world generation. `generate_region` is a pure function of
//! region coordinates: same coords → identical output on every machine,
//! which is what makes "cycle out = park, cycle in = restore-or-regenerate"
//! safe for the multi-region world.

mod noise;
mod spline;
mod density;
pub mod pipeline;
pub mod material;

use game::{
    Block, BlockId, BlockIndex, ChiselData, Chunk, Column, ColumnCoords, RegionCoords, SubChunk,
    BLOCK_VOXELS, CHUNK_BLOCKS, REGION_CHUNKS,
};

use crate::material::Materials;
use crate::pipeline::{TerrainPipeline, SEA_LEVEL};

// Region span in blocks: REGION_CHUNKS chunks * CHUNK_BLOCKS blocks/chunk.
const REGION_BLOCKS: i32 = (REGION_CHUNKS * CHUNK_BLOCKS) as i32; // 16
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

/// Highest solid block Y in a 1 m column (world block coords), or `SCAN_BOTTOM - 1`
/// if the whole column is air. Coarse per-block scan (design §4 step 1).
fn surface_block_y(p: &TerrainPipeline, wx: i32, wz: i32) -> i32 {
    let prof = p.column(block_center_m(wx), block_center_m(wz));
    let mut by = SCAN_TOP;
    while by >= SCAN_BOTTOM {
        if prof.density_at(block_center_m(by)) > 0.0 {
            return by;
        }
        by -= 1;
    }
    SCAN_BOTTOM - 1
}

/// The full 8x8 column grid for one region. Pure and deterministic.
pub fn generate_region(coords: RegionCoords, seed: u64, mats: Materials) -> Vec<(ColumnCoords, Column)> {
    let p = TerrainPipeline::new(seed);
    let sea = SEA_LEVEL as i32;
    let air = Block::new(BlockId::AIR);
    let stone = Block::new(mats.stone);
    let region_ox = coords.x * REGION_BLOCKS; // world block origin X
    let region_oz = coords.z * REGION_BLOCKS;

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
                    let s = surface_block_y(&p, col_ox + lx as i32, col_oz + lz as i32);
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
                            place_straddle(chunk, bi, wx, by, wz, &p, top_mat);
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
    p: &TerrainPipeline,
    top_mat: BlockId,
) {
    let mut chisel = ChiselData::new(vec![top_mat]);
    let mut any = false;
    for vx in 0..BLOCK_VOXELS {
        for vz in 0..BLOCK_VOXELS {
            // Meter coord of this voxel center: block index + (voxel + 0.5)/16.
            let mx = wx as f64 + (vx as f64 + 0.5) / BLOCK_VOXELS as f64;
            let mz = wz as f64 + (vz as f64 + 0.5) / BLOCK_VOXELS as f64;
            let prof = p.column(mx, mz);
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
}
