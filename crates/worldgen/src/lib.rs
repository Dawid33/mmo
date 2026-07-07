//! Deterministic world generation. `generate_region` is a pure function of
//! region coordinates: same coords → identical output on every machine,
//! which is what makes "cycle out = park, cycle in = restore-or-regenerate"
//! safe for the multi-region world.

mod noise;
mod spline;
mod density;

use game::{
    Block, BlockId, Chunk, Column, ColumnCoords, RegionCoords, SubChunk, REGION_CHUNKS,
};

/// Floor height for a region: 8 on even `x+z`, 12 on odd — a checkerboard,
/// so region boundaries are visible as steps while roaming.
pub fn floor_height(coords: RegionCoords) -> u32 {
    if (coords.x + coords.z).rem_euclid(2) == 0 {
        8
    } else {
        12
    }
}

/// The full 8×8 column grid for one region, region-local coordinates.
/// Pure and deterministic; no clocks, no RNG. `solid` is the floor material.
/// Each column: stone sentinel below, air above, one straddling floor
/// sub-chunk at sub_y = 0.
pub fn generate_region(coords: RegionCoords, solid: BlockId) -> Vec<(ColumnCoords, Column)> {
    let depth = floor_height(coords);
    let stone = Block::new(solid);
    let air = Block::new(BlockId::AIR);
    let mut columns = Vec::with_capacity(REGION_CHUNKS * REGION_CHUNKS);
    for x in 0..REGION_CHUNKS {
        for z in 0..REGION_CHUNKS {
            let floor = SubChunk::Dense(Box::new(Chunk::flat_floor_with(depth, solid)));
            let column = Column::new(0, stone, air, vec![floor]);
            columns.push((ColumnCoords::new(x, z), column));
        }
    }
    columns
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{Column, ColumnCoords};

    const TEST_STONE: game::BlockId = game::BlockId(2);

    fn crc(cols: &[(ColumnCoords, Column)]) -> u32 {
        let mut h = crc32fast::Hasher::new();
        let bytes = bincode::serialize(cols).unwrap();
        h.update(&bytes);
        h.finalize()
    }

    #[test]
    fn produces_sixty_four_columns() {
        let cols = generate_region(RegionCoords::new(0, 0), TEST_STONE);
        assert_eq!(cols.len(), REGION_CHUNKS * REGION_CHUNKS); // 64
    }

    #[test]
    fn generation_is_deterministic() {
        let a = generate_region(RegionCoords::new(-3, 7), TEST_STONE);
        let b = generate_region(RegionCoords::new(-3, 7), TEST_STONE);
        assert_eq!(crc(&a), crc(&b));
    }

    #[test]
    fn parity_regions_differ() {
        let even = generate_region(RegionCoords::new(0, 0), TEST_STONE);
        let odd = generate_region(RegionCoords::new(1, 0), TEST_STONE);
        assert_ne!(crc(&even), crc(&odd));
    }

    #[test]
    fn parity_heights_checkerboard() {
        assert_eq!(floor_height(RegionCoords::new(0, 0)), 8);
        assert_eq!(floor_height(RegionCoords::new(1, 0)), 12);
        assert_eq!(floor_height(RegionCoords::new(-1, 0)), 12);
        assert_eq!(floor_height(RegionCoords::new(-1, -1)), 8);
    }

    #[test]
    fn generated_columns_carry_chisel_slabs() {
        // Every floor chunk straddles the floor height (8 or 12 voxels, both
        // sub-block), so its bottom blocks are chiseled.
        let cols = generate_region(RegionCoords::new(0, 0), TEST_STONE);
        assert!(
            cols.iter().all(|(_, col)| {
                col.sub_chunks().iter().any(|s| match s {
                    SubChunk::Dense(c) => !c.chisel.is_empty(),
                    SubChunk::Air => false,
                })
            }),
            "every generated floor column should have a chiseled slab"
        );
        assert!(
            cols.iter().all(|(_, col)| {
                col.sub_chunks().iter().all(|s| match s {
                    SubChunk::Dense(c) => c.blocks.len() == 8,
                    SubChunk::Air => true,
                })
            }),
            "every sub-chunk has 8 blocks"
        );
    }
}
