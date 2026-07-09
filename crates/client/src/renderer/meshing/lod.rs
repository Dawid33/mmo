use game::{BlockId, Voxel, CHUNK_VOXEL_COUNT};

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

                                if sx_coord < 32 && sy_coord < 32 && sz_coord < 32 {
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

/// Factor-16 fast path: each source chunk contributes its 2×2×2 `blocks` directly as coarse cells.
/// Chiseled blocks count as their block id — chisel detail is invisible at 16×.
/// Output: fills only the 0..4 corner of the 32³ grid (4×4×4 cells = 8 source chunks × 8 blocks each).
///
/// **Layout contract:** LOD grids are always 32³ `Vec<Voxel>` in the standard `x + 32*y + 32*32*z` order.
pub fn downsample_blocks(chunks: &[Option<&game::Chunk>; 8]) -> Vec<Voxel> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use game::{Block, BlockId, Voxel, CHUNK_VOXEL_COUNT};

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
    fn blocks_fast_path_maps_blocks_to_cells() {
        // A chunk with one solid block at block-index (1,0,1): with the chunk
        // in source slot 0, the coarse cell (1,0,1) of the 4³ corner is solid.
        let mut chunk = game::Chunk::default();
        chunk.set_block(1, 0, 1, Block::new(BlockId(4)));
        let mut chunks: [Option<&game::Chunk>; 8] = [None; 8];
        chunks[0] = Some(&chunk);
        let out = downsample_blocks(&chunks);
        assert_eq!(out[1 + 32 * 0 + 32 * 32 * 1].block, BlockId(4));
        assert_eq!(out.iter().filter(|v| !v.is_air()).count(), 1);
    }
}
