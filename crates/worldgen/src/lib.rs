//! Deterministic world generation. `generate_region` is a pure function of
//! region coordinates: same coords → identical output on every machine,
//! which is what makes "cycle out = park, cycle in = restore-or-regenerate"
//! safe for the multi-region world.

use game::{Chunk, ChunkCoords, RegionCoords, REGION_CHUNKS};

/// Floor height for a region: 8 on even `x+z`, 12 on odd — a checkerboard,
/// so region boundaries are visible as steps while roaming.
pub fn floor_height(coords: RegionCoords) -> u32 {
    if (coords.x + coords.z).rem_euclid(2) == 0 {
        8
    } else {
        12
    }
}

/// The full 8×8 chunk grid for one region, region-local coordinates.
/// Pure and deterministic; no clocks, no RNG.
pub fn generate_region(coords: RegionCoords) -> Vec<(ChunkCoords, Chunk)> {
    let depth = floor_height(coords);
    let mut chunks = Vec::with_capacity(REGION_CHUNKS * REGION_CHUNKS);
    for x in 0..REGION_CHUNKS {
        for z in 0..REGION_CHUNKS {
            chunks.push((ChunkCoords::new(x, 0, z), Chunk::flat_floor(depth)));
        }
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::Hash;

    fn crc(chunks: &[(ChunkCoords, Chunk)]) -> u32 {
        let mut h = crc32fast::Hasher::new();
        for (_, c) in chunks {
            c.hash(&mut h);
        }
        h.finalize()
    }

    #[test]
    fn generation_is_pure() {
        let a = generate_region(RegionCoords::new(-3, 7));
        let b = generate_region(RegionCoords::new(-3, 7));
        assert_eq!(a.len(), 64);
        assert_eq!(crc(&a), crc(&b));
    }

    #[test]
    fn parity_heights_checkerboard() {
        assert_eq!(floor_height(RegionCoords::new(0, 0)), 8);
        assert_eq!(floor_height(RegionCoords::new(1, 0)), 12);
        assert_eq!(floor_height(RegionCoords::new(-1, 0)), 12);
        assert_eq!(floor_height(RegionCoords::new(-1, -1)), 8);
    }

    #[test]
    fn neighbouring_regions_differ() {
        let even = generate_region(RegionCoords::new(0, 0));
        let odd = generate_region(RegionCoords::new(1, 0));
        assert_ne!(crc(&even), crc(&odd));
    }
}
