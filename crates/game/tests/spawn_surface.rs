//! A player must spawn on top of the terrain surface at its region-local spawn
//! column, not at a hardcoded flat-floor height (which leaves it buried far
//! below real generated terrain).
use game::{Chunk, ChunkCoords, GameEventKind, Region, RegionId};

// Build a region whose only terrain is a full chunk covering the spawn column
// (region-local voxel (128,128) -> chunk column (4,4)) at sub_y=3, so its
// highest solid voxel is at Y = 3*32 + 31 = 127 (top face at 128).
fn region_with_high_floor() -> Region {
    Region::from_chunks(
        RegionId::new(0, 0),
        vec![(ChunkCoords::new(4, 3, 4), Chunk::flat_floor(32))],
    )
}

fn player_body_y(region: &Region) -> f32 {
    let data = region.data();
    let e = *data.player_entites.get(&0).unwrap();
    let handle = *data.ecs.rigidbody.get(e);
    let body = data.physics.bodies.get(handle).unwrap();
    body.translation().y.0
}

#[test]
fn player_spawns_above_terrain_surface() {
    let mut region = region_with_high_floor();
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    let y = player_body_y(&region);
    // Must be clear of the surface (top face at 128), not the old hardcoded 26.
    assert!(y > 128.0, "player must spawn above the terrain surface, got y={y}");
    // Old flat-floor math: (surface_top) + capsule_half_extent(14.4) + clear(3.6).
    // Surface top = 128 -> 128 + 18 = 146.
    assert!((y - 146.0).abs() < 0.01, "expected ~146, got {y}");
}
