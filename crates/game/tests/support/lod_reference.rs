//! The pre-brick LOD downsampler (2026-10-29), kept verbatim as the reference
//! the brick-view path (`game::lod::downsample_chunks`) must match byte for
//! byte. It reads a chunk as ONE 32³ grid of 2×2×2 blocks, so it is only a
//! valid reference while `CHUNK_BLOCKS == BRICK_BLOCKS`; callers skip it
//! otherwise. Shared by `crates/game/tests/lod_streaming.rs` and
//! `crates/worldgen/tests/lod_bricks.rs` through `#[path]` (integration tests
//! cannot see the lib's `#[cfg(test)]` items).

use std::collections::BTreeMap;

use game::lod::{downsample_cells, merge_group, LodCell};
use game::{BlockId, Chunk, ChunkCoords, Voxel, BRICK_VOXEL_COUNT};

/// Whether this build's chunks are exactly one brick (the reference's premise).
pub fn reference_applies() -> bool {
    game::CHUNK_BLOCKS == game::BRICK_BLOCKS
}

/// `downsample_chunks` as it was before chunks were read as brick views.
pub fn downsample_reference(chunks: Vec<(ChunkCoords, Chunk)>, factor: u32) -> Vec<LodCell> {
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
        _ => panic!("unsupported LOD factor {factor}"),
    }
}

fn build_far_lod_cells(chunks: Vec<(ChunkCoords, Chunk)>) -> Vec<LodCell> {
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

fn write_chunk_at_half_scale(grid: &mut [Voxel], octant: [usize; 3], chunk: &Chunk) {
    use game::BLOCK_VOXELS;
    let half = BLOCK_VOXELS / 2; // coarse cells per block axis (8)
    for (bi, block) in chunk.blocks.iter() {
        let (bx, by, bz) = bi.xyz();
        let base = [
            octant[0] * 16 + bx * half,
            octant[1] * 16 + by * half,
            octant[2] * 16 + bz * half,
        ];
        // Decoded once per chiseled block: grid order, x fastest.
        let chisel = chunk.chisel.get(&bi).map(game::ChiselData::materials);
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

fn block_scale_cells(chunks: Vec<(ChunkCoords, Chunk)>) -> Vec<LodCell> {
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

/// Cells as comparable bytes: group + `PackedLodGrid` runs, in output order.
pub fn packed(cells: &[LodCell]) -> Vec<([i32; 3], game::lod::PackedLodGrid)> {
    cells.iter().map(|c| (c.group, game::lod::PackedLodGrid::encode(&c.grid))).collect()
}
