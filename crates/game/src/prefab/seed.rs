//! Worldgen's way in: a Build prefab written into a region's seed with the same layout
//! the runtime stamp uses.

use super::{layout_build, ResolvedBuild};
use crate::{RegionCoords, StudChunkCoords, StudRegionSeed};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeedStampReport {
    /// Non-box parts left out because they would straddle a chunk edge (whole prefab,
    /// not only this region's share).
    pub skipped_split: u32,
    /// Chunks of this region the prefab reaches.
    pub chunks_in_region: u32,
}

/// Lays `prefab` out at world cell `anchor` and appends the chunks that fall in `region`
/// to `seed.stamps`. A generator that places prefabs must be a pure function of world
/// coordinates and must also evaluate placements anchored in neighbouring regions
/// (within the prefab bounds), so a prefab on a region edge comes out whole on both sides.
pub fn stamp_build_into_seed(
    seed: &mut StudRegionSeed,
    region: RegionCoords,
    prefab: &ResolvedBuild,
    anchor: [i64; 3],
    turn: u8,
) -> SeedStampReport {
    let layout = layout_build(prefab, anchor, turn);
    let mut report = SeedStampReport { skipped_split: layout.skipped_split, chunks_in_region: 0 };
    for (chunk, stamp) in layout.chunks {
        let (rc, c) = chunk.to_local();
        if rc == region && crate::stud::stud_layer_ok(c.y) {
            seed.stamps.push((StudChunkCoords { x: c.x, y: c.y, z: c.z }, stamp));
            report.chunks_in_region += 1;
        }
    }
    report
}
