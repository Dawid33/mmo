#[test]
fn chunk_edge_is_a_whole_number_of_bricks() {
    assert_eq!(game::CHUNK_UNITS % game::BRICK_VOXELS, 0);
    assert_eq!(game::BRICKS_PER_CHUNK_EDGE * game::BRICK_VOXELS, game::CHUNK_UNITS);
    assert_eq!(game::BRICK_BLOCKS * game::BLOCK_VOXELS, game::BRICK_VOXELS);
}

#[test]
fn chunk_coords_round_trip_through_body_translation() {
    for c in [game::ChunkCoords::new(0, 0, 0), game::ChunkCoords::new(3, -5, 7)] {
        let u = game::CHUNK_UNITS as f32;
        let t = game::rapier3d::math::Vector::new(c.x as f32 * u, c.y as f32 * u, c.z as f32 * u);
        assert_eq!(game::chunk_coords_from_translation(t), c);
    }
}

#[test]
fn unit_constants_hold_at_every_legal_region_size() {
    use game::{
        max_decompressed_packet_bytes, pristine_cache_max, BLOCK_VOXELS, BRICK_VOXELS, CHUNK_BLOCKS,
        REGION_BLOCKS, REGION_BLOCKS_CHOICES, REGION_BRICKS, REGION_CHUNKS, REGION_SIZE,
    };
    for &b in &REGION_BLOCKS_CHOICES {
        assert!(max_decompressed_packet_bytes(b) > 0, "packet cap is 0 at {b} blocks");
        assert!(max_decompressed_packet_bytes(b) >= 256 << 20, "cap below the 64-block cap at {b}");
        assert!(pristine_cache_max(b) >= 1, "pristine cache disabled at {b} blocks");
    }
    assert_eq!(REGION_SIZE, (REGION_BLOCKS * BLOCK_VOXELS) as f32);
    assert_eq!(REGION_CHUNKS * CHUNK_BLOCKS, REGION_BLOCKS);
    assert_eq!(REGION_BRICKS * BRICK_VOXELS, REGION_SIZE as usize);

    // Today's values at the 64-block test default.
    assert_eq!(max_decompressed_packet_bytes(64), 256 << 20);
    assert_eq!(pristine_cache_max(64), 64);
    assert_eq!(pristine_cache_max(128), 16);
}
