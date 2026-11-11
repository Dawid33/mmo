//! The prefab editor's world: a flat baseplate over the 3 x 3 regions around the
//! origin and nothing else — no features, no water, nothing persisted (the server
//! serves it like the `studs` test world, without a store).
//!
//! `region_seed` is a pure function of the region coordinates.

use game::{
    ColumnCoords, RegionCoords, StudColumnGround, StudProfile, StudRegionSeed, REGION_BLOCKS, REGION_CHUNKS,
    STUD_CHUNK_PLATES, STUD_CHUNK_STUDS, TILE_STUDS,
};

/// Regions within this many regions of the origin (Chebyshev) have a baseplate.
pub const EDITOR_WORLD_RADIUS: i32 = 1;
/// The baseplate's surface, in plates: the top of stud layer 0, so the anchor's cell is
/// the first cell of layer 1 — 90 plates of ground below it, the 180 a prefab may
/// reach up above it.
pub const EDITOR_SURFACE_PLATE: i32 = STUD_CHUNK_PLATES as i32;
/// What the baseplate is made of, all the way down: one plain material, so every brick
/// of another material in the editor world is part of the prefab.
pub const BASEPLATE: u16 = game::MAT_GRASS;
/// Bedrock: far below anything a prefab's carve can reach.
pub const EDITOR_BEDROCK_PLATE: i32 = -100 * STUD_CHUNK_PLATES as i32;
/// One brick course of real bricks on top (studs to build on); plain ground below.
const PROFILE: StudProfile = StudProfile { cover: BASEPLATE, cover_plates: 3, soil: BASEPLATE, soil_plates: 0 };

/// What region `rc` starts from: flat ground inside the radius, nothing outside.
pub fn region_seed(rc: RegionCoords) -> StudRegionSeed {
    let mut seed = StudRegionSeed::default();
    if rc.x.abs() > EDITOR_WORLD_RADIUS || rc.z.abs() > EDITOR_WORLD_RADIUS {
        return seed;
    }
    for cx in 0..REGION_CHUNKS {
        for cz in 0..REGION_CHUNKS {
            let ground = StudColumnGround::flat(EDITOR_SURFACE_PLATE, PROFILE, BASEPLATE, EDITOR_BEDROCK_PLATE);
            seed.ground.push((ColumnCoords::new(cx, cz), ground));
        }
    }
    seed
}

/// The world cell (studs x, plates y, studs z) a prefab's anchor sits on in the
/// editor: the middle of the chunk at the centre of region (0, 0), on the baseplate.
/// A prefab's whole extent (`game::PREFAB_HALF_STUDS` each way) stays inside region
/// (0, 0) at every region size.
pub fn editor_anchor() -> [i64; 3] {
    let centre = (REGION_BLOCKS * TILE_STUDS / 2 + STUD_CHUNK_STUDS / 2) as i64;
    [centre, EDITOR_SURFACE_PLATE as i64, centre]
}

// The extent cap fits between the anchor and the region's edges at the smallest region.
const _: () = assert!(
    REGION_BLOCKS * TILE_STUDS / 2 + STUD_CHUNK_STUDS / 2 + game::PREFAB_HALF_STUDS as usize <= REGION_BLOCKS * TILE_STUDS
);
