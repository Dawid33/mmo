//! A player must spawn on top of the terrain surface at its region-local spawn
//! column, not at a hardcoded flat-floor height (which leaves it buried far
//! below real generated terrain).
use game::{Chunk, ChunkCoords, GameEventKind, Region, RegionId};

// Build a region whose only terrain is a floor in the chunk covering the
// spawn column (region-local voxel (128,128)) whose top face is at Y = 128:
// a full chunk at sub_y=3 at 2-block chunks (3*32 + 32), the bottom 128
// voxels of sub_y=0 at 16.
fn region_with_high_floor() -> Region {
    let cu = game::CHUNK_UNITS;
    let (cy, depth) = ((128 - 1) / cu, 128 - (128 - 1) / cu * cu);
    let c = 128 / cu;
    Region::from_chunks(
        RegionId::new(0, 0),
        vec![(ChunkCoords::new(c, cy as i32, c), Chunk::flat_floor(depth as u32))],
    )
}

fn player_body_y(region: &Region) -> f32 {
    let data = region.data();
    let e = *data.player_entites.get(&0).unwrap();
    let handle = *data.ecs.rigidbody.get(e);
    let body = data.physics.bodies.get(handle).unwrap();
    body.translation().y
}

/// Regression: player spawned ~1000 voxels below terrain, camera saw nothing; seen running the client on generated terrain (fix 8b18ab25 message)
#[test]
fn player_spawns_above_terrain_surface() {
    let mut region = region_with_high_floor();
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    let y = player_body_y(&region);
    // Must be clear of the surface (top face at 128), not the old hardcoded 26.
    assert!(y > 128.0, "player must spawn above the terrain surface, got y={y}");
    // Spawn math: (surface_top) + capsule_half_extent(8.0 + 6.4 = 14.4) + clear(3.6).
    // Surface top = 128 -> 128 + 18.0 = 146.0.
    assert!((y - 146.0).abs() < 0.01, "expected ~146.0, got {y}");
}
